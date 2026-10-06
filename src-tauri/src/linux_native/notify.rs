//! The approval notification, sent straight to org.freedesktop.Notifications.
//!
//! GIO's freedesktop backend forgets a notification once the user clicks it, so
//! `withdraw_notification` can no longer close it. Notification servers that
//! keep a clicked urgent notification on screen (the Omarchy shell does) then
//! show an approval that was already decided. Talking to the server directly
//! keeps its id, so a resolved, expired or replaced approval always closes.

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
                        if id != 0 && id == shown_for_close.get() {
                            shown_for_close.set(0);
                        }
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
                close(&connection, id);
            }
        });
    }

    /// Close the approval notification, clicked or not.
    pub(super) fn withdraw(&self) {
        let Some(connection) = &self.connection else {
            self.app.withdraw_notification(FALLBACK_ID);
            return;
        };
        self.bump();
        let id = self.shown.replace(0);
        if id != 0 {
            close(connection, id);
        }
    }

    fn bump(&self) -> u64 {
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        generation
    }
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
