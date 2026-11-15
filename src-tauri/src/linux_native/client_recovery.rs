//! Clients recovery notices live on the page, never in the transient toast layer.
use super::*;

fn selectable(value: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(value)
        .xalign(0.0)
        .halign(gtk::Align::Fill)
        .hexpand(true)
        .valign(gtk::Align::Start)
        .vexpand(false)
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

fn message_label(record: &crate::clients::recovery::ClientRecovery, heading: bool) -> gtk::Label {
    if !heading {
        return recovery_label(record, |r| &r.description);
    }
    let label = selectable("");
    let id = record.id.clone();
    let fallback = record.message.clone();
    l10n::bind_label_with(&label, move || {
        current_text(&id, |r| &r.message, &fallback)
            .split("\n\n")
            .next()
            .unwrap_or("")
            .to_string()
    });
    label
}
fn values_list(
    record: &crate::clients::recovery::ClientRecovery,
    discard: bool,
) -> gtk::ScrolledWindow {
    let values = if discard {
        &record.discard_values
    } else {
        &record.value_lines
    };
    let label = selectable(
        &values
            .iter()
            .map(|s| format!("• {s}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let id = record.id.clone();
    let fallback = label.text().to_string();
    l10n::bind_label_with(&label, move || {
        crate::clients::recovery::list()
            .ok()
            .and_then(|rows| rows.into_iter().find(|r| r.id == id))
            .map(|r| {
                if discard {
                    r.discard_values
                } else {
                    r.value_lines
                }
            })
            .map(|lines| {
                lines
                    .iter()
                    .map(|s| format!("• {s}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_else(|| fallback.clone())
    });
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .propagate_natural_height(true)
        .max_content_height(240)
        .vexpand(false)
        .hexpand(true)
        .valign(gtk::Align::Start)
        .focusable(true)
        .child(&label)
        .build();
    l10n::bind_accessible_label(
        &scroll,
        if discard {
            "Saved values to discard"
        } else {
            "Missing saved values"
        },
    );
    scroll
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
        for (id, message) in self.recovery_success.borrow().iter() {
            if client.is_some_and(|client| client != id) {
                continue;
            }
            let label = selectable(message);
            label.set_focusable(true);
            label.add_css_class("toolport-feedback");
            label.add_css_class("success");
            label.set_widget_name(&format!("client-recovery-success:{id}"));
            parent.append(&label);
        }
        for record in self.recoveries.borrow().iter() {
            if client.is_some_and(|id| id != record.client_id) {
                continue;
            }
            let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
            card.add_css_class("toolport-feedback");
            card.add_css_class("warning");
            card.set_valign(gtk::Align::Start);
            card.set_vexpand(false);
            card.set_widget_name(&format!("client-recovery:{}", record.id));
            if record.dismissed {
                let heading = selectable("");
                heading.add_css_class("heading");
                l10n::bind_label(&heading, "Recovery notice dismissed");
                card.append(&heading);
                card.append(&recovery_label(record, |record| &record.retention));
                if let Some((_, message)) = self
                    .recovery_feedback
                    .borrow()
                    .iter()
                    .find(|(id, _)| id == &record.id)
                {
                    card.append(&selectable(message));
                }
                let show = l10n::button("Show recovery");
                show.set_halign(gtk::Align::Start);
                show.set_sensitive(!self.recovery_busy.get());
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
            let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            heading.set_valign(gtk::Align::Start);
            let icon = gtk::Image::from_icon_name("dialog-warning-symbolic");
            icon.add_css_class("warning");
            icon.set_valign(gtk::Align::Start);
            icon.set_pixel_size(16);
            heading.append(&icon);
            let title = message_label(record, true);
            title.add_css_class("heading");
            heading.append(&title);
            card.append(&heading);
            card.append(&message_label(record, false));
            if !record.value_lines.is_empty() {
                card.append(&values_list(record, false));
            }
            card.append(&recovery_label(record, |record| &record.instructions));
            card.append(&recovery_label(record, |record| &record.retention));
            let feedback = self
                .recovery_feedback
                .borrow()
                .iter()
                .find(|(id, _)| id == &record.id)
                .map(|(_, text)| text.clone())
                .or_else(|| {
                    record
                        .error
                        .clone()
                        .filter(|error| error != &record.description)
                });
            if let Some(feedback) = feedback {
                card.append(&selectable(&feedback));
            }
            let actions = gtk::FlowBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .column_spacing(8)
                .row_spacing(8)
                .halign(gtk::Align::Start)
                .min_children_per_line(1)
                .max_children_per_line(3)
                .build();
            let retry = l10n::button("Retry");
            retry.add_css_class("suggested-action");
            retry.set_sensitive(!self.recovery_busy.get());
            retry.set_widget_name(&format!("client-recovery-retry:{}", record.id));
            let page = self.clone();
            let record_for_retry = record.clone();
            retry.connect_clicked(move |button| {
                page.retry_recovery(&record_for_retry, button);
            });
            actions.insert(&retry, -1);
            if record.can_discard {
                let dismiss = l10n::button("Dismiss");
                dismiss.set_sensitive(!self.recovery_busy.get());
                dismiss.set_widget_name(&format!("client-recovery-dismiss:{}", record.id));
                let page = self.clone();
                let record_for_dismiss = record.clone();
                dismiss
                    .connect_clicked(move |_| page.confirm_dismiss_recovery(&record_for_dismiss));
                actions.insert(&dismiss, -1);
                let discard = l10n::button("Discard these saved values");
                discard.set_sensitive(!self.recovery_busy.get());
                discard.set_widget_name(&format!("client-recovery-discard:{}", record.id));
                let page = self.clone();
                let record = record.clone();
                discard.connect_clicked(move |_| page.confirm_discard_recovery(&record));
                actions.insert(&discard, -1);
            }
            card.append(&actions);
            let details = gtk::Expander::new(None);
            details.set_label_widget(Some(&l10n::label("Details")));
            details.set_expanded(self.recovery_expanded.borrow().contains(&record.id));
            let expanded = self.recovery_expanded.clone();
            let id = record.id.clone();
            details.connect_expanded_notify(move |details| {
                if details.is_expanded() {
                    expanded.borrow_mut().insert(id.clone());
                } else {
                    expanded.borrow_mut().remove(&id);
                }
            });
            details.set_child(Some(&recovery_label(record, |record| &record.details)));
            card.append(&details);
            parent.append(&card);
        }
    }

    fn retry_recovery(
        &self,
        record: &crate::clients::recovery::ClientRecovery,
        button: &gtk::Button,
    ) {
        if !record.can_discard {
            self.reload_recoveries();
            self.refresh_open_detail();
            if !focus_named(&self.root, &format!("client-recovery-retry:{}", record.id)) {
                self.recovery_success
                    .borrow_mut()
                    .retain(|(id, _)| id != &record.client_id);
                self.recovery_success.borrow_mut().push((
                    record.client_id.clone(),
                    crate::i18n::trf(
                        "{client}: the recovery information can be read again.",
                        &[("client", &record.client_name)],
                    ),
                ));
                self.reload_recoveries();
                self.refresh_open_detail();
                focus_named(
                    &self.root,
                    &format!("client-recovery-success:{}", record.client_id),
                );
            }
            return;
        }
        button.set_sensitive(false);
        self.recovery_expanded.borrow_mut().remove(&record.id);
        self.recovery_busy.set(true);
        self.reload_recoveries();
        self.refresh_open_detail();
        let page = self.clone();
        let record = record.clone();
        gtk::glib::spawn_future_local(async move {
            let id = record.id.clone();
            let result = gtk::gio::spawn_blocking(move || {
                crate::clients::recovery::retry_with_report(&id)
                    .map_err(|e| crate::clients::recovery::safe_action_error(&id, &e))
            })
            .await;
            page.recovery_busy.set(false);
            page.recovery_feedback
                .borrow_mut()
                .retain(|(id, _)| id != &record.id);
            match result {
                Ok(Ok(result)) if result.pending.is_none() => {
                    page.recovery_success
                        .borrow_mut()
                        .retain(|(id, _)| id != &record.client_id);
                    page.recovery_success
                        .borrow_mut()
                        .push((record.client_id.clone(), result.message));
                }
                Ok(Ok(result)) => page
                    .recovery_feedback
                    .borrow_mut()
                    .push((record.id.clone(), result.message)),
                Ok(Err(error)) => page
                    .recovery_feedback
                    .borrow_mut()
                    .push((record.id.clone(), error)),
                Err(_) => page
                    .recovery_feedback
                    .borrow_mut()
                    .push((record.id.clone(), crate::clients::recovery::worker_error())),
            }
            page.reload_recoveries();
            page.refresh_open_detail();
            if !focus_named(&page.root, &format!("client-recovery-retry:{}", record.id)) {
                focus_named(
                    &page.root,
                    &format!("client-recovery-success:{}", record.client_id),
                );
            }
        });
    }

    fn confirm_dismiss_recovery(&self, record: &crate::clients::recovery::ClientRecovery) {
        let parent = self
            .app
            .active_window()
            .or_else(|| self.root.root().and_downcast::<gtk::Window>());
        #[allow(deprecated)]
        let dialog = adw::MessageDialog::new(
            parent.as_ref(),
            Some(&crate::i18n::tr("Dismiss recovery notice?")),
            None,
        );
        dialog.set_application(Some(&self.app));
        dialog.set_extra_child(Some(&recovery_label(record, |record| &record.retention)));
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
            if response != "dismiss" {
                focus_named(&page.root, &format!("client-recovery-dismiss:{id}"));
            }
        });
        dialog.present();
    }

    fn set_recovery_dismissed(&self, id: &str, dismissed: bool, button: &gtk::Button) {
        button.set_sensitive(false);
        self.recovery_busy.set(true);
        self.reload_recoveries();
        self.refresh_open_detail();
        let page = self.clone();
        let id = id.to_string();
        let focus_id = id.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                crate::clients::recovery::dismiss(&id, dismissed)
                    .map_err(|e| crate::clients::recovery::safe_action_error(&id, &e))
            })
            .await;
            match result {
                Ok(Ok(())) => {
                    page.recovery_busy.set(false);
                    if dismissed {
                        page.recovery_expanded.borrow_mut().remove(&focus_id);
                    }
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
                Ok(Err(error)) => page.recovery_action_error(&focus_id, error),
                Err(_) => {
                    page.recovery_action_error(&focus_id, crate::clients::recovery::worker_error())
                }
            }
        });
    }
    fn recovery_action_error(&self, id: &str, error: String) {
        self.recovery_busy.set(false);
        self.recovery_feedback
            .borrow_mut()
            .retain(|(row, _)| row != id);
        self.recovery_feedback.borrow_mut().push((id.into(), error));
        self.reload_recoveries();
        self.refresh_open_detail();
        if !focus_named(&self.root, &format!("client-recovery-retry:{id}")) {
            focus_named(&self.root, &format!("client-recovery-show:{id}"));
        }
    }
    fn confirm_discard_recovery(&self, record: &crate::clients::recovery::ClientRecovery) {
        let parent = self
            .app
            .active_window()
            .or_else(|| self.root.root().and_downcast::<gtk::Window>());
        #[allow(deprecated)]
        let dialog = adw::MessageDialog::new(
            parent.as_ref(),
            Some(&crate::i18n::tr("Discard these saved values?")),
            None,
        );
        dialog.set_application(Some(&self.app));
        l10n::bind_dialog_heading(&dialog, "Discard these saved values?");
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.append(&selectable(&crate::i18n::trf("Toolport will permanently delete the saved original config and its recovery copies for {client} at {path}. The saved copies of the values listed below will be lost. The current client config and Toolport's server credentials will not change.",&[("client",&record.client_name),("path",&record.path)])));
        if !record.discard_values.is_empty() {
            content.append(&values_list(record, true));
        }
        content.append(&recovery_label(record, |r| &r.retention));
        dialog.set_extra_child(Some(&content));
        dialog.add_response("cancel", &crate::i18n::tr("Cancel"));
        dialog.add_response("discard", &crate::i18n::tr("Discard these saved values"));
        l10n::bind_dialog_response(&dialog, "cancel", "Cancel");
        l10n::bind_dialog_response(&dialog, "discard", "Discard these saved values");
        dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let page = self.clone();
        let record = record.clone();
        dialog.connect_response(None,move|dialog,response|{dialog.close();if response!="discard"{focus_named(&page.root,&format!("client-recovery-discard:{}",record.id));return;}
            page.recovery_busy.set(true);page.reload_recoveries();page.refresh_open_detail();
            let page=page.clone();let record=record.clone();gtk::glib::spawn_future_local(async move{
                let id=record.id.clone();let result=gtk::gio::spawn_blocking(move||crate::clients::recovery::discard(&id).map_err(|e|crate::clients::recovery::safe_action_error(&id,&e))).await;
                match result {Ok(Ok(()))=>{page.recovery_busy.set(false);page.recovery_success.borrow_mut().retain(|(id,_)|id!=&record.client_id);page.recovery_success.borrow_mut().push((record.client_id.clone(),crate::i18n::trf("{client}'s saved recovery copies were discarded. The current client config was not changed.",&[("client",&record.client_name)])));page.reload_recoveries();page.refresh_open_detail();focus_named(&page.root,&format!("client-recovery-success:{}",record.client_id));},Ok(Err(e))=>page.recovery_action_error(&record.id,e),Err(_)=>page.recovery_action_error(&record.id,crate::clients::recovery::worker_error())}
            });
        });
        dialog.present();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn recovery_notice_is_selectable_wrapped_and_has_explicit_actions() {
        let _data = crate::registry::DataDirTestEnv::new("gtk-recovery-notice");
        adw::init().unwrap();
        let app = adw::Application::builder()
            .application_id("com.tsout.Toolport.RecoveryNoticeTest")
            .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(gtk::gio::Cancellable::NONE).unwrap();
        let (_, servers, _) = build_content(&app, crate::approval_broker::start_native());
        let page = ClientPage::new(&app, servers);
        let values = (0..20)
            .map(|index| format!("• API_KEY_{index:02} for server-{index:02}"))
            .collect::<Vec<_>>()
            .join("\n");
        let body = format!("Its original config is back, except for values Toolport could not read because your keyring is locked or unavailable:\n\n{values}\n\nThe config has placeholders for these values for now.");
        let message = format!("Cursor is disconnected.\n\n{body}");
        let record = crate::clients::recovery::ClientRecovery {
            id: "fixture-receipt".into(),
            client_id: "cursor".into(),
            client_name: "Cursor".into(),
            path: "/fixture/cursor.json".into(),
            backup_path: "/fixture/original.json".into(),
            missing: (0..20)
                .map(|index| crate::clients::backup_storage::Reference {
                    server: format!("server-{index:02}"),
                    key: format!("API_KEY_{index:02}"),
                })
                .collect(),
            dismissed: false,
            message: message.clone(),
            description: "Its original config is back. These values still have placeholders:"
                .into(),
            value_lines: values
                .lines()
                .map(|s| s.trim_start_matches("• ").to_string())
                .collect(),
            discard_values: values.lines().map(str::to_string).collect(),
            error: None,
            can_discard: true,
            cleanup: false,
            instructions: "Unlock your keyring, then choose Retry.".into(),
            retention: "The recovery backup does not expire automatically.".into(),
            details: "TOOLPORT_MISSING_CREDENTIAL_0".into(),
        };
        page.recoveries.borrow_mut().push(record.clone());
        page.append_recoveries(&page.recovery_box, None);
        let card = page.recovery_box.first_child().unwrap();
        let heading = card.first_child().unwrap().downcast::<gtk::Box>().unwrap();
        let icon = heading
            .first_child()
            .unwrap()
            .downcast::<gtk::Image>()
            .unwrap();
        assert_eq!(icon.icon_name().as_deref(), Some("dialog-warning-symbolic"));
        let title = heading
            .last_child()
            .unwrap()
            .downcast::<gtk::Label>()
            .unwrap();
        assert_eq!(title.text(), "Cursor is disconnected.");
        assert!(title.has_css_class("heading"));
        assert_eq!(title.xalign(), 0.0);
        assert!(title.is_selectable());
        let label = heading
            .next_sibling()
            .unwrap()
            .downcast::<gtk::Label>()
            .unwrap();
        assert_eq!(label.text(), record.description);
        let scroll = label
            .next_sibling()
            .unwrap()
            .downcast::<gtk::ScrolledWindow>()
            .unwrap();
        assert_eq!(scroll.max_content_height(), 240);
        assert!(scroll.propagates_natural_height());
        let viewport = scroll.child().unwrap().downcast::<gtk::Viewport>().unwrap();
        let values_label = viewport.child().unwrap().downcast::<gtk::Label>().unwrap();
        for index in 0..20 {
            assert!(values_label.text().contains(&format!("API_KEY_{index:02}")));
        }
        assert!(values_label.is_selectable());
        assert!(values_label.wraps());
        assert!(label.is_selectable());
        assert!(label.wraps());
        assert_eq!(label.wrap_mode(), gtk::pango::WrapMode::WordChar);
        assert_eq!(label.valign(), gtk::Align::Start);
        assert_eq!(label.ellipsize(), gtk::pango::EllipsizeMode::None);
        assert!(!label.text().contains("TOOLPORT_MISSING_CREDENTIAL"));
        let mut child = card.first_child();
        while let Some(widget) = child {
            if let Ok(scroll) = widget.clone().downcast::<gtk::ScrolledWindow>() {
                assert_eq!(scroll.max_content_height(), 240);
            }
            child = widget.next_sibling();
        }
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
        let discard = actions
            .child_at_index(2)
            .unwrap()
            .child()
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap();
        assert_eq!(
            discard.label().as_deref(),
            Some("Discard these saved values")
        );
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

        // A tray/background action has no active parent window. Confirmation
        // must still open, and confirmed deletion must guard every other action.
        assert!(app.active_window().is_none());
        page.confirm_discard_recovery(&record);
        let dialog = app
            .windows()
            .into_iter()
            .find_map(|window| window.downcast::<adw::MessageDialog>().ok())
            .expect("parentless Discard confirmation");
        assert!(dialog.transient_for().is_none());
        assert_eq!(dialog.default_response().as_deref(), Some("cancel"));
        dialog.response("discard");
        assert!(
            page.recovery_busy.get(),
            "Discard must be busy before its worker starts"
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while page.recovery_busy.get() && std::time::Instant::now() < deadline {
            gtk::glib::MainContext::default().iteration(false);
        }
        assert!(
            !page.recovery_busy.get(),
            "failed Discard must release the busy guard"
        );
        assert!(page
            .recovery_feedback
            .borrow()
            .iter()
            .any(|(id, message)| id == &record.id && !message.is_empty()));
    }
}
