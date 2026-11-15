//! Clients recovery notices live on the page, never in the transient toast layer.
use super::*;

fn selectable(value: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(value)
        .xalign(0.0)
        .halign(gtk::Align::Fill)
        .hexpand(true)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .selectable(true)
        .build()
}

fn current_text(
    id: &str,
    field: fn(&crate::clients::recovery::ClientRecovery) -> &str,
    fallback: &str,
) -> String {
    crate::clients::recovery::list()
        .ok()
        .and_then(|records| records.into_iter().find(|record| record.id == id))
        .map(|record| field(&record).into())
        .unwrap_or_else(|| fallback.to_string())
}

fn focus_named(root: &impl IsA<gtk::Widget>, name: &str) -> bool {
    let root = root.as_ref();
    if root.widget_name() == name && root.is_mapped() && root.grab_focus() {
        return true;
    }
    let mut child = root.first_child();
    while let Some(widget) = child {
        if focus_named(&widget, name) {
            return true;
        }
        child = widget.next_sibling();
    }
    false
}

fn recovery_label(
    record: &crate::clients::recovery::ClientRecovery,
    field: fn(&crate::clients::recovery::ClientRecovery) -> &str,
) -> gtk::Label {
    let label = selectable("");
    let id = record.id.clone();
    let fallback = field(record).to_string();
    // Notice metadata is read without opening the vault. Re-fetching also makes
    // visible recovery prose follow a language change, like the action labels.
    l10n::bind_label_with(&label, move || current_text(&id, field, &fallback));
    label
}

impl ClientPage {
    pub(super) fn reload_recoveries(&self) {
        match crate::clients::recovery::list() {
            Ok(records) => *self.recoveries.borrow_mut() = records,
            Err(error) => self.show_error(&error),
        }
        while let Some(child) = self.recovery_box.first_child() {
            self.recovery_box.remove(&child);
        }
        self.append_recoveries(&self.recovery_box, None);
    }

    pub(super) fn refresh_open_detail(&self) {
        let selected = self.details.selected.borrow().clone();
        if let Some(client) = self
            .detail_clients
            .borrow()
            .iter()
            .find(|client| selected.as_deref() == Some(&client.id))
            .cloned()
        {
            details::client(&client, self);
        }
    }

    pub(super) fn append_recoveries(&self, parent: &gtk::Box, client: Option<&str>) {
        for record in self.recoveries.borrow().iter() {
            if client.is_some_and(|id| id != record.client_id) {
                continue;
            }
            let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
            card.add_css_class("toolport-feedback");
            card.add_css_class("warning");
            card.set_widget_name(&format!("client-recovery:{}", record.id));
            if record.dismissed {
                card.append(&l10n::label("Recovery notice dismissed"));
                card.append(&recovery_label(record, |record| &record.retention));
                let show = l10n::button("Show recovery");
                show.set_halign(gtk::Align::Start);
                show.set_widget_name(&format!("client-recovery-show:{}", record.id));
                let page = self.clone();
                let id = record.id.clone();
                show.connect_clicked(move |button| {
                    page.set_recovery_dismissed(&id, false, button);
                });
                card.append(&show);
                parent.append(&card);
                continue;
            }
            // Long lists remain fully selectable and reachable by scrolling.
            // No ellipsis/max-width character limits are used in recovery text.
            let message = recovery_label(record, |record| &record.message);
            let scroll = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vscrollbar_policy(gtk::PolicyType::Automatic)
                .propagate_natural_height(true)
                .max_content_height(240)
                .child(&message)
                .build();
            card.append(&scroll);
            card.append(&recovery_label(record, |record| &record.instructions));
            if let Some((_, feedback)) = self
                .recovery_feedback
                .borrow()
                .iter()
                .find(|(id, _)| id == &record.id)
            {
                let label = selectable(feedback);
                if feedback == "Toolport still could not read the missing values. Unlock your keyring, then choose Retry." {
                    l10n::bind_label(&label, "Toolport still could not read the missing values. Unlock your keyring, then choose Retry.");
                }
                card.append(&label);
            }
            let actions = gtk::FlowBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .column_spacing(8)
                .row_spacing(8)
                .halign(gtk::Align::Start)
                .min_children_per_line(1)
                .max_children_per_line(2)
                .build();
            let retry = l10n::button("Retry");
            retry.add_css_class("suggested-action");
            retry.set_widget_name(&format!("client-recovery-retry:{}", record.id));
            let page = self.clone();
            let record_for_retry = record.clone();
            retry.connect_clicked(move |button| {
                page.retry_recovery(&record_for_retry, button);
            });
            actions.insert(&retry, -1);
            let dismiss = l10n::button("Dismiss");
            let page = self.clone();
            let record_for_dismiss = record.clone();
            dismiss.connect_clicked(move |_| page.confirm_dismiss_recovery(&record_for_dismiss));
            actions.insert(&dismiss, -1);
            card.append(&actions);
            let details = gtk::Expander::new(None);
            details.set_label_widget(Some(&l10n::label("Details")));
            details.set_child(Some(&recovery_label(record, |record| &record.details)));
            card.append(&details);
            parent.append(&card);
        }
        for (id, name) in self.recovery_success.borrow().iter() {
            if client.is_some_and(|client| client != id) {
                continue;
            }
            let label = selectable("");
            let name = name.clone();
            l10n::bind_label_with(&label, move || {
                crate::i18n::trf(
                    "{client} config is fully restored. The missing values are back.",
                    &[("client", &name)],
                )
            });
            label.add_css_class("toolport-feedback");
            label.add_css_class("success");
            label.set_widget_name(&format!("client-recovery-success:{id}"));
            parent.append(&label);
        }
    }

    fn retry_recovery(
        &self,
        record: &crate::clients::recovery::ClientRecovery,
        button: &gtk::Button,
    ) {
        button.set_sensitive(false);
        let page = self.clone();
        let record = record.clone();
        gtk::glib::spawn_future_local(async move {
            let id = record.id.clone();
            let focus_id = record.id.clone();
            let focus_client = record.client_id.clone();
            let result =
                gtk::gio::spawn_blocking(move || crate::clients::recovery::retry(&id)).await;
            page.recovery_feedback
                .borrow_mut()
                .retain(|(id, _)| id != &record.id);
            match result {
                Ok(Ok(None)) => {
                    page.recovery_success
                        .borrow_mut()
                        .retain(|(id, _)| id != &record.client_id);
                    page.recovery_success
                        .borrow_mut()
                        .push((record.client_id, record.client_name));
                }
                Ok(Ok(Some(_))) => page.recovery_feedback.borrow_mut().push((record.id,
                    "Toolport still could not read the missing values. Unlock your keyring, then choose Retry.".into())),
                Ok(Err(error)) => page.recovery_feedback.borrow_mut().push((record.id, error)),
                Err(_) => page.recovery_feedback.borrow_mut().push((record.id,
                    crate::i18n::tr("The recovery stopped unexpectedly. Choose Retry to try again."))),
            }
            page.reload_recoveries();
            page.refresh_open_detail();
            // Keep focus on an actionable control rather than the removed button.
            if !focus_named(&page.root, &format!("client-recovery-retry:{focus_id}")) {
                focus_named(
                    &page.root,
                    &format!("client-recovery-success:{focus_client}"),
                );
            }
        });
    }

    fn confirm_dismiss_recovery(&self, record: &crate::clients::recovery::ClientRecovery) {
        let Some(parent) = self.app.active_window() else {
            return;
        };
        #[allow(deprecated)]
        let dialog = adw::MessageDialog::new(
            Some(&parent),
            Some(&crate::i18n::tr("Dismiss recovery notice?")),
            Some(&record.retention),
        );
        l10n::bind_dialog_heading(&dialog, "Dismiss recovery notice?");
        dialog.add_response("cancel", &crate::i18n::tr("Cancel"));
        l10n::bind_dialog_response(&dialog, "cancel", "Cancel");
        dialog.add_response("dismiss", &crate::i18n::tr("Dismiss"));
        l10n::bind_dialog_response(&dialog, "dismiss", "Dismiss");
        dialog.set_close_response("cancel");
        dialog.set_default_response(Some("cancel"));
        let page = self.clone();
        let id = record.id.clone();
        dialog.connect_response(None, move |dialog, response| {
            if response == "dismiss" {
                // A temporary button supplies the same busy guard as Show.
                page.set_recovery_dismissed(&id, true, &gtk::Button::new());
            }
            dialog.close();
        });
        dialog.present();
    }

    fn set_recovery_dismissed(&self, id: &str, dismissed: bool, button: &gtk::Button) {
        button.set_sensitive(false);
        let page = self.clone();
        let id = id.to_string();
        let focus_id = id.clone();
        gtk::glib::spawn_future_local(async move {
            let result =
                gtk::gio::spawn_blocking(move || crate::clients::recovery::dismiss(&id, dismissed))
                    .await;
            match result {
                Ok(Ok(())) => {
                    page.reload_recoveries();
                    page.refresh_open_detail();
                    focus_named(
                        &page.root,
                        &format!(
                            "client-recovery-{}:{focus_id}",
                            if dismissed { "show" } else { "retry" }
                        ),
                    );
                }
                Ok(Err(error)) => page.show_error(&error),
                Err(_) => page.show_error(&crate::i18n::tr(
                    "The recovery stopped unexpectedly. Choose Retry to try again.",
                )),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn recovery_notice_is_selectable_wrapped_bounded_and_has_explicit_actions() {
        let _data = crate::registry::DataDirTestEnv::new("gtk-recovery-notice");
        adw::init().unwrap();
        let app = adw::Application::builder()
            .application_id("com.tsout.Toolport.RecoveryNoticeTest")
            .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(gtk::gio::Cancellable::NONE).unwrap();
        let (_, servers, _) = build_content(&app, crate::approval_broker::start_native());
        let page = ClientPage::new(&app, servers);
        let message = (0..20)
            .map(|index| format!("API_KEY_{index:02} for server-{index:02}"))
            .collect::<Vec<_>>()
            .join("; ");
        let record = crate::clients::recovery::ClientRecovery {
            id: "fixture-receipt".into(),
            client_id: "cursor".into(),
            client_name: "Cursor".into(),
            path: "/fixture/cursor.json".into(),
            backup_path: "/fixture/original.json".into(),
            missing: Vec::new(),
            dismissed: false,
            message: message.clone(),
            instructions: "Unlock your keyring, then choose Retry.".into(),
            retention: "The recovery backup does not expire automatically.".into(),
            details: "TOOLPORT_MISSING_CREDENTIAL_0".into(),
        };
        page.recoveries.borrow_mut().push(record.clone());
        page.append_recoveries(&page.recovery_box, None);
        let card = page.recovery_box.first_child().unwrap();
        let scroll = card
            .first_child()
            .unwrap()
            .downcast::<gtk::ScrolledWindow>()
            .unwrap();
        assert_eq!(scroll.hscrollbar_policy(), gtk::PolicyType::Never);
        assert_eq!(scroll.max_content_height(), 240);
        let label = scroll
            .child()
            .unwrap()
            .downcast::<gtk::Viewport>()
            .unwrap()
            .child()
            .unwrap()
            .downcast::<gtk::Label>()
            .unwrap();
        assert_eq!(label.text(), message);
        assert!(label.is_selectable());
        assert!(label.wraps());
        assert_eq!(label.ellipsize(), gtk::pango::EllipsizeMode::None);
        assert!(!label.text().contains("TOOLPORT_MISSING_CREDENTIAL"));
        let details = card
            .last_child()
            .unwrap()
            .downcast::<gtk::Expander>()
            .unwrap();
        assert!(!details.is_expanded());
        assert!(details
            .child()
            .unwrap()
            .downcast::<gtk::Label>()
            .unwrap()
            .text()
            .contains("TOOLPORT_MISSING_CREDENTIAL_0"));
        let actions = details
            .prev_sibling()
            .unwrap()
            .downcast::<gtk::FlowBox>()
            .unwrap();
        let retry = actions
            .child_at_index(0)
            .unwrap()
            .child()
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap();
        let dismiss = actions
            .child_at_index(1)
            .unwrap()
            .child()
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap();
        assert_eq!(retry.label().as_deref(), Some("Retry"));
        assert_eq!(dismiss.label().as_deref(), Some("Dismiss"));
        while let Some(child) = page.recovery_box.first_child() {
            page.recovery_box.remove(&child);
        }
        page.recoveries.borrow_mut()[0].dismissed = true;
        page.append_recoveries(&page.recovery_box, None);
        let dismissed = page.recovery_box.first_child().unwrap();
        assert_eq!(
            dismissed
                .last_child()
                .unwrap()
                .downcast::<gtk::Button>()
                .unwrap()
                .label()
                .as_deref(),
            Some("Show recovery")
        );
        assert!(dismissed
            .first_child()
            .unwrap()
            .downcast::<gtk::Label>()
            .unwrap()
            .text()
            .contains("Recovery notice dismissed"));
    }
}
