//! The approval notification, sent straight to org.freedesktop.Notifications.
//!
//! GIO's freedesktop backend forgets a notification once the user clicks it, so
//! `withdraw_notification` can no longer close it. Notification servers that
//! retain urgent popup cards after sender close (as Omarchy does) need a
//! replacement to expire them. Track the server id until NotificationClosed,
//! so withdrawing a dismissed approval never creates a new handled toast.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

const BUS_NAME: &str = "org.freedesktop.Notifications";
const OBJECT_PATH: &str = "/org/freedesktop/Notifications";
/// The GIO id used when no session bus is available.
const FALLBACK_ID: &str = "toolport-approvals";

#[derive(Clone)]
pub(super) struct ApprovalNotification {
    app: adw::Application,
    connection: Option<gio::DBusConnection>,
    /// The server's id for the notification on screen, 0 when there is none.
    shown: Rc<Cell<u32>>,
    /// Bumped by every show and withdraw, so a Notify reply that lands after a
    /// newer request closes its own notification instead of leaving it behind.
    generation: Rc<Cell<u64>>,
    _signals: Rc<Vec<gio::SignalSubscription>>,
}

impl ApprovalNotification {
    pub(super) fn new(app: &adw::Application) -> Self {
        let connection = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).ok();
        let shown = Rc::new(Cell::new(0u32));
        let mut signals = Vec::new();
        if let Some(connection) = &connection {
            let shown_for_action = shown.clone();
            let app_for_action = app.downgrade();
            signals.push(connection.subscribe_to_signal(
                Some(BUS_NAME),
                Some(BUS_NAME),
                Some("ActionInvoked"),
                Some(OBJECT_PATH),
                None,
                gio::DBusSignalFlags::NONE,
                move |signal| {
                    let Some((id, _action)) = signal.parameters.get::<(u32, String)>() else {
                        return;
                    };
                    if id != 0 && id == shown_for_action.get() {
                        if let Some(app) = app_for_action.upgrade() {
                            app.activate_action("show-approvals", None);
                        }
                    }
                },
            ));
            let shown_for_close = shown.clone();
            signals.push(connection.subscribe_to_signal(
                Some(BUS_NAME),
                Some(BUS_NAME),
                Some("NotificationClosed"),
                Some(OBJECT_PATH),
                None,
                gio::DBusSignalFlags::NONE,
                move |signal| {
                    if let Some((id, _reason)) = signal.parameters.get::<(u32, u32)>() {
                        forget_closed(&shown_for_close, id);
                    }
                },
            ));
        }
        Self {
            app: app.clone(),
            connection,
            shown,
            generation: Rc::new(Cell::new(0)),
            _signals: Rc::new(signals),
        }
    }

    /// Show or replace the approval notification.
    pub(super) fn show(&self, title: &str, body: &str) {
        let Some(connection) = self.connection.clone() else {
            let notification = gio::Notification::new(title);
            notification.set_body(Some(body));
            notification.set_priority(gio::NotificationPriority::Urgent);
            notification.set_default_action("app.show-approvals");
            self.app.send_notification(Some(FALLBACK_ID), &notification);
            return;
        };
        let generation = self.bump();
        let mut hints = HashMap::<String, glib::Variant>::new();
        hints.insert("urgency".into(), 2u8.to_variant());
        hints.insert("desktop-entry".into(), super::APP_ID.to_variant());
        let parameters = (
            "Toolport",
            self.shown.get(),
            "toolport",
            title,
            body,
            vec!["default".to_string(), "Review".to_string()],
            hints,
            -1i32,
        )
            .to_variant();
        let shown = self.shown.clone();
        let current = self.generation.clone();
        glib::spawn_future_local(async move {
            let reply = connection
                .call_future(
                    Some(BUS_NAME),
                    OBJECT_PATH,
                    BUS_NAME,
                    "Notify",
                    Some(&parameters),
                    Some(glib::VariantTy::new("(u)").expect("static variant type")),
                    gio::DBusCallFlags::NONE,
                    5000,
                )
                .await;
            let id = match reply {
                Ok(reply) => reply.child_value(0).get::<u32>().unwrap_or(0),
                Err(error) => {
                    eprintln!("toolport-gtk: could not show the approval notification: {error}");
                    return;
                }
            };
            if current.get() == generation {
                shown.set(id);
            } else if id != 0 && id != shown.get() {
                // A superseded reply is not a tracked, visible prompt. Replacing
                // it could create a fresh toast if the server already closed it.
                close(&connection, id);
            }
        });
    }

    /// Withdraw only an approval the server has not reported closed.
    pub(super) fn withdraw(&self) {
        let Some(connection) = &self.connection else {
            self.app.withdraw_notification(FALLBACK_ID);
            return;
        };
        self.bump();
        if let Some(id) = take_shown(&self.shown) {
            withdraw(connection, id);
        }
    }

    fn bump(&self) -> u64 {
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        generation
    }
}

fn withdrawal_parameters(id: u32) -> glib::Variant {
    let mut hints = HashMap::<String, glib::Variant>::new();
    hints.insert("urgency".into(), 0u8.to_variant());
    hints.insert("desktop-entry".into(), super::APP_ID.to_variant());
    (
        "Toolport",
        id,
        "toolport",
        "Approval handled",
        "",
        Vec::<String>::new(),
        hints,
        1000i32,
    )
        .to_variant()
}

// NotificationClosed applies to every reason (expiry, dismissal, sender close).
// An unrelated or old id must not clear a newer approval notification.
fn forget_closed(shown: &Cell<u32>, id: u32) {
    if id != 0 && id == shown.get() {
        shown.set(0);
    }
}

fn take_shown(shown: &Cell<u32>) -> Option<u32> {
    match shown.replace(0) {
        0 => None,
        id => Some(id),
    }
}

fn withdraw(connection: &gio::DBusConnection, id: u32) {
    // Omarchy ignores sender close for popup cards; a low-urgency replacement
    // expires instead. Keep close for notification servers that honor it.
    // Dispatch now rather than queueing a future after forgetting the shown id.
    let connection_for_close = connection.clone();
    connection.call(
        Some(BUS_NAME),
        OBJECT_PATH,
        BUS_NAME,
        "Notify",
        Some(&withdrawal_parameters(id)),
        Some(glib::VariantTy::new("(u)").expect("static variant type")),
        gio::DBusCallFlags::NONE,
        5000,
        gio::Cancellable::NONE,
        move |reply| {
            let close_id = match reply {
                Ok(reply) => reply
                    .child_value(0)
                    .get::<u32>()
                    .filter(|id| *id != 0)
                    .unwrap_or(id),
                Err(error) => {
                    eprintln!("toolport-gtk: could not replace the approval notification: {error}");
                    id
                }
            };
            close(&connection_for_close, close_id);
        },
    );
}

fn close(connection: &gio::DBusConnection, id: u32) {
    connection.call(
        Some(BUS_NAME),
        OBJECT_PATH,
        BUS_NAME,
        "CloseNotification",
        Some(&(id,).to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        5000,
        gio::Cancellable::NONE,
        |_| {},
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_notification_is_not_withdrawn_or_replaced() {
        let shown = Cell::new(42);
        forget_closed(&shown, 42);
        assert_eq!(take_shown(&shown), None);
    }

    #[test]
    fn unrelated_close_keeps_the_visible_notification() {
        let shown = Cell::new(42);
        forget_closed(&shown, 0);
        forget_closed(&shown, 41);
        assert_eq!(take_shown(&shown), Some(42));
        assert_eq!(take_shown(&shown), None);
    }

    #[test]
    fn old_close_does_not_forget_a_new_notification() {
        let shown = Cell::new(42);
        forget_closed(&shown, 42);
        shown.set(43);
        forget_closed(&shown, 42);
        assert_eq!(take_shown(&shown), Some(43));
    }

    #[test]
    fn withdrawal_replaces_the_pending_id_without_actions_or_critical_urgency() {
        let parameters = withdrawal_parameters(42);
        assert_eq!(parameters.type_().as_str(), "(susssasa{sv}i)");
        assert_eq!(parameters.child_value(1).get::<u32>(), Some(42));
        assert_eq!(
            parameters.child_value(3).get::<String>().as_deref(),
            Some("Approval handled")
        );
        assert!(parameters
            .child_value(5)
            .get::<Vec<String>>()
            .unwrap()
            .is_empty());
        let hints = parameters
            .child_value(6)
            .get::<HashMap<String, glib::Variant>>()
            .unwrap();
        assert_eq!(hints["urgency"].get::<u8>(), Some(0));
        assert_eq!(
            hints["desktop-entry"].get::<String>().as_deref(),
            Some(super::super::APP_ID)
        );
        assert_eq!(parameters.child_value(7).get::<i32>(), Some(1000));
    }
}
