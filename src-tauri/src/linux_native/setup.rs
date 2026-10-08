//! Shared review window for client cutover, Collections and multi-server paste.
use crate::registry_controller::SetupItem;
use adw::prelude::*;

#[derive(Default)]
pub(super) struct Completion {
    pub message: String,
    pub servers: Vec<crate::registry_controller::SetupServerResult>,
    pub tools: Vec<serde_json::Value>,
    pub backup: Option<String>,
}
impl From<String> for Completion {
    fn from(message: String) -> Self {
        Self {
            message,
            ..Self::default()
        }
    }
}

impl From<&str> for Completion {
    fn from(message: &str) -> Self {
        message.to_string().into()
    }
}

fn middle_ellipsize(widget: &gtk::Widget, text: &str) {
    if let Some(label) = widget.downcast_ref::<gtk::Label>() {
        if label.text() == text {
            label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            label.set_single_line_mode(true);
            label.set_tooltip_text(Some(text));
        }
    }
    let mut child = widget.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        middle_ellipsize(&widget, text);
    }
}

fn details_expander(title: &str) -> (gtk::Expander, gtk::Box) {
    let expander = gtk::Expander::new(Some(title));
    expander.add_css_class("toolport-details-expander");
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    expander.set_child(Some(&content));
    (expander, content)
}

pub(super) fn review(
    parent: &gtk::Window,
    title: &str,
    items: Vec<SetupItem>,
    disclosure: &str,
    confirm_label: &str,
    action: impl Fn(
            Vec<String>,
            std::collections::BTreeMap<String, std::collections::BTreeMap<String, bool>>,
        ) -> Result<Completion, String>
        + Send
        + Sync
        + 'static,
    finished: impl Fn() + 'static,
    credential_page: Option<super::ServerPage>,
) {
    let dialog = adw::Window::builder()
        .transient_for(parent)
        .modal(true)
        .title(title)
        .default_width(620)
        .default_height(540)
        .build();
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = adw::HeaderBar::new();
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    let cancel = gtk::Button::with_label("Cancel");
    let confirm = gtk::Button::with_label(confirm_label);
    confirm.add_css_class("suggested-action");
    header.pack_start(&cancel);
    header.pack_end(&confirm);
    root.append(&header);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 16);
    body.set_margin_top(24);
    body.set_margin_bottom(24);
    body.set_margin_start(24);
    body.set_margin_end(24);
    let lede = gtk::Label::builder()
        .label("Review the selected servers before continuing.")
        .xalign(0.0)
        .wrap(true)
        .build();
    body.append(&lede);
    let rows = gtk::ListBox::new();
    rows.set_selection_mode(gtk::SelectionMode::None);
    rows.add_css_class("boxed-list");
    let mut selected = Vec::new();
    let mut credential_choices = Vec::new();
    for item in items {
        let command = item
            .command
            .as_ref()
            .map(|c| format!("{c} {}", item.args.join(" ")))
            .or(item.url.clone())
            .unwrap_or_else(|| "Needs an endpoint URL".into());
        let row = adw::ExpanderRow::builder()
            .title(&item.name)
            .subtitle(&command)
            .build();
        row.set_subtitle_lines(1);
        middle_ellipsize(row.upcast_ref(), &command);
        let check = gtk::CheckButton::builder()
            .active(item.unsupported.is_none())
            .valign(gtk::Align::Center)
            .build();
        row.add_prefix(&check);
        check.set_sensitive(item.unsupported.is_none());
        row.set_enable_expansion(!item.credentials.is_empty());
        let tag = gtk::Label::new(Some(if item.is_new { "New" } else { "In Toolport" }));
        tag.add_css_class("dim-label");
        row.add_suffix(&tag);
        let spinner = gtk::Spinner::new();
        spinner.set_visible(false);
        row.add_suffix(&spinner);
        if !item.credentials.is_empty() {
            let found = item.credentials.iter().all(|env| env.present);
            let secret = item.credentials.iter().any(|env| env.secret);
            let state = gtk::Label::new(Some(if !found {
                "Missing"
            } else if secret {
                "Found, goes to keychain"
            } else {
                "Found"
            }));
            state.add_css_class("warning");
            row.add_suffix(&state);
        }
        if let Some(reason) = &item.unsupported {
            row.set_subtitle(&format!("Unsupported: {reason}"));
        }
        for env in item.credentials {
            let choice = gtk::CheckButton::builder()
                .label(format!("Keep {} in keychain", env.key))
                .active(env.secret)
                .build();
            choice.set_sensitive(item.unsupported.is_none());
            let value_row = adw::ActionRow::builder()
                .title(&env.key)
                .subtitle(if env.present { "Found" } else { "Missing" })
                .build();
            value_row.add_suffix(&choice);
            value_row.set_activatable_widget(Some(&choice));
            row.add_row(&value_row);
            credential_choices.push((item.name.clone(), env.key, choice));
        }
        rows.append(&row);
        selected.push((check, row, spinner, item.key, item.name));
    }
    body.append(&rows);
    let (details, content) = details_expander("Details");
    let path = gtk::Label::builder()
        .label(disclosure)
        .wrap(true)
        .selectable(true)
        .xalign(0.0)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    content.append(&path);
    body.append(&details);
    let feedback = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .selectable(true)
        .visible(false)
        .build();
    body.append(&feedback);
    let scroller = gtk::ScrolledWindow::builder()
        .child(&body)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    root.append(&scroller);
    dialog.set_content(Some(&root));
    let busy = std::rc::Rc::new(std::cell::Cell::new(false));
    let closing = dialog.clone();
    let busy_cancel = busy.clone();
    cancel.connect_clicked(move |_| {
        if !busy_cancel.get() {
            closing.close();
        }
    });
    let keys = gtk::EventControllerKey::new();
    let closing = dialog.clone();
    let busy_keys = busy.clone();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gtk::gdk::Key::Escape && !busy_keys.get() {
            closing.close();
            return gtk::glib::Propagation::Stop;
        }
        gtk::glib::Propagation::Proceed
    });
    dialog.add_controller(keys);
    if confirm_label == "Close" {
        let closing = dialog.clone();
        confirm.connect_clicked(move |_| closing.close());
        cancel.set_visible(false);
        dialog.present();
        return;
    }
    let selected = std::rc::Rc::new(selected);
    let credential_choices = std::rc::Rc::new(credential_choices);
    let action = std::sync::Arc::new(action);
    let finished = std::rc::Rc::new(finished);
    let completed = std::rc::Rc::new(std::cell::Cell::new(false));
    let closing = dialog.clone();
    confirm.connect_clicked(move |button| {
        if completed.get() {
            closing.close();
            return;
        }
        let chosen = selected
            .iter()
            .filter(|(check, _, _, _, _)| check.is_active())
            .map(|(_, _, _, key, _)| key.clone())
            .collect::<Vec<_>>();
        let mut choices =
            std::collections::BTreeMap::<String, std::collections::BTreeMap<String, bool>>::new();
        for (name, key, choice) in credential_choices.iter() {
            choices
                .entry(name.clone())
                .or_default()
                .insert(key.clone(), choice.is_active());
            choice.set_sensitive(false);
        }
        busy.set(true);
        button.set_sensitive(false);
        cancel.set_sensitive(false);
        for (check, row, spinner, _, _) in selected.iter() {
            row.remove_css_class("error");
            check.set_sensitive(false);
            spinner.set_visible(check.is_active());
            spinner.set_spinning(check.is_active());
        }
        feedback.set_label("Checking selected servers...");
        feedback.set_visible(true);
        let (action, finished, selected, busy, completed) = (
            action.clone(),
            finished.clone(),
            selected.clone(),
            busy.clone(),
            completed.clone(),
        );
        let credential_choices = credential_choices.clone();
        let credential_page = credential_page.clone();
        let (feedback, button, cancel, body, rows, details, lede) = (
            feedback.clone(),
            button.clone(),
            cancel.clone(),
            body.clone(),
            rows.clone(),
            details.clone(),
            lede.clone(),
        );
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || action(chosen, choices)).await;
            busy.set(false);
            for (_, _, choice) in credential_choices.iter() {
                choice.set_sensitive(true);
            }
            cancel.set_sensitive(true);
            button.set_sensitive(true);
            for (check, _, spinner, _, _) in selected.iter() {
                check.set_sensitive(
                    row.subtitle()
                        .is_none_or(|s| !s.starts_with("Unsupported:")),
                );
                spinner.set_spinning(false);
                spinner.set_visible(false);
            }
            match result {
                Ok(Ok(outcome)) => {
                    completed.set(true);
                    cancel.set_visible(false);
                    button.set_label("Done");
                    rows.set_visible(false);
                    details.set_visible(false);
                    lede.set_visible(false);
                    feedback.set_visible(false);
                    let status = adw::StatusPage::builder()
                        .icon_name("emblem-ok-symbolic")
                        .title(&outcome.message)
                        .build();
                    status.add_css_class("compact");
                    status.set_vexpand(false);
                    body.prepend(&status);
                    let results = gtk::ListBox::new();
                    results.add_css_class("boxed-list");
                    results.set_selection_mode(gtk::SelectionMode::None);
                    for server in outcome.servers {
                        let credential = match server.credential_state.as_str() {
                            "stored" => "Stored in keychain",
                            "missing" => "Needs input",
                            _ => "No credentials needed",
                        };
                        let row = adw::ActionRow::builder()
                            .title(&server.name)
                            .subtitle(format!("{} tools · {}", server.tool_count, credential))
                            .build();
                        if server.credential_state != "none" {
                            if let Some(page) = &credential_page {
                                let button = gtk::Button::with_label("Open Credentials");
                                let page = page.clone();
                                let name = server.name.clone();
                                button.connect_clicked(move |_| {
                                    if let Ok(registry) =
                                        crate::registry_controller::registry_for_disconnect()
                                    {
                                        if let Some(server) =
                                            super::state::RegistrySnapshot::from_registry(registry)
                                                .servers
                                                .into_iter()
                                                .find(|s| s.name == name)
                                        {
                                            super::open_credentials_editor(server, page.clone());
                                        }
                                    }
                                });
                                row.add_suffix(&button);
                            }
                        }
                        results.append(&row);
                    }
                    body.append(&results);
                    if !outcome.tools.is_empty() {
                        let (agent, content) = details_expander("What your agent sees");
                        for tool in outcome.tools {
                            let row = adw::ActionRow::builder()
                                .title(tool["name"].as_str().unwrap_or("Tool"))
                                .build();
                            content.append(&row);
                        }
                        body.append(&agent);
                    }
                    if let Some(backup) = outcome.backup {
                        let (detail, content) = details_expander("Details");
                        let date = std::fs::metadata(&backup)
                            .ok()
                            .and_then(|m| m.modified().ok())
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .and_then(|t| {
                                gtk::glib::DateTime::from_unix_local(t.as_secs() as i64).ok()
                            })
                            .and_then(|t| t.format("%e %B %Y, %H:%M").ok())
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| "saved".into());
                        let label = gtk::Label::builder()
                            .label(format!("Backup {date}\n{backup}"))
                            .wrap(true)
                            .selectable(true)
                            .build();
                        content.append(&label);
                        body.append(&detail);
                    }
                    finished();
                }
                result => {
                    let error = match result {
                        Ok(Err(error)) => error,
                        _ => "Setup stopped. Retry.".into(),
                    };
                    feedback.set_label(&error);
                    feedback.add_css_class("error");
                    button.set_label("Retry");
                    for (_, row, _, _, name) in selected.iter() {
                        if error.contains(name) {
                            row.add_css_class("error");
                        }
                    }
                }
            }
        });
    });
    dialog.present();
}

pub(super) fn connect(
    parent: &gtk::Window,
    client_id: String,
    profile: Option<String>,
    force: bool,
    finished: impl Fn() + 'static,
    credential_page: super::ServerPage,
) {
    let parent = parent.clone();
    gtk::glib::spawn_future_local(async move {
        let id = client_id.clone();
        let preview =
            gtk::gio::spawn_blocking(move || crate::registry_controller::preview_client_setup(&id))
                .await;
        match preview {
            Ok(Ok(preview)) => {
                let client_name = crate::clients::detect_clients()
                    .into_iter()
                    .find(|client| client.id == client_id)
                    .map(|client| client.name)
                    .unwrap_or_else(|| "Client".into());
                let disclosure = format!("Config: {}\nBackup directory: {}\nSelected entries move after gateway verification. Unchecked entries and plugin servers stay in place.{}", preview.config_path, preview.backup_dir, if force { " This replaces the customized Toolport entry." } else { "" });
                review(
                    &parent,
                    "Review and connect",
                    preview.items,
                    &disclosure,
                    "Connect to Toolport",
                    move |selected, choices| {
                        let outcome = crate::registry_controller::migrate_client_reviewed_choices(
                            &client_id,
                            profile.as_deref(),
                            force,
                            &selected,
                            &preview.revision,
                            &choices,
                        )?;
                        Ok(Completion {
                            message: format!(
                                "{client_name} connected. Restart it to load Toolport."
                            ),
                            servers: outcome.servers,
                            tools: outcome.tools,
                            backup: outcome.result.outcome.backup,
                        })
                    },
                    finished,
                    Some(credential_page),
                );
            }
            Ok(Err(error)) => review(
                &parent,
                "Could not review setup",
                Vec::new(),
                &error,
                "Close",
                |_, _| Err("Fix the client config and retry.".into()),
                || {},
                None,
            ),
            Err(_) => review(
                &parent,
                "Could not review setup",
                Vec::new(),
                "Client review stopped.",
                "Close",
                |_, _| Err("Retry from Clients.".into()),
                || {},
                None,
            ),
        }
    });
}

pub(super) fn collection(
    parent: &gtk::Window,
    name: &str,
    entries: Vec<crate::catalog::CatalogEntry>,
    finished: impl Fn() + 'static,
) {
    let items = entries
        .iter()
        .enumerate()
        .map(|(i, e)| SetupItem {
            key: i.to_string(),
            name: e.name.clone(),
            transport: e.transport.clone(),
            command: e.command.clone(),
            args: e.args.clone(),
            url: e.url.clone().or(e.url_hint.clone()),
            env_keys: e
                .env_keys
                .iter()
                .cloned()
                .chain(
                    e.launch
                        .iter()
                        .flat_map(|l| l.inputs.iter().map(|i| i.label.clone())),
                )
                .collect(),
            is_new: true,
            credentials: Vec::new(),
            unsupported: None,
        })
        .collect();
    review(parent,&format!("Review {name}"),items,"Review what each server runs. Valid servers turn on. Servers needing credentials or launch values stay off until setup is complete.","Add selected servers",move |keys,_choices| {
        let selected=entries.iter().enumerate().filter(|(i,_)|keys.contains(&i.to_string())).map(|(_,e)|e.clone()).collect();
        let (_,added)=crate::registry_controller::add_catalog_stack(selected)?;
        Ok(format!("Added {added} servers. Check status and complete any missing setup inputs under Servers.").into())
    },finished,None);
}

#[cfg(test)]
mod tests {
    use super::*;
    fn descendants(widget: &gtk::Widget) -> Vec<gtk::Widget> {
        let mut out = vec![widget.clone()];
        let mut child = widget.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            out.extend(descendants(&widget));
        }
        out
    }
    fn review_window() -> gtk::Window {
        gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Window>().ok())
            .find(|w| w.title().as_deref() == Some("Could not review setup"))
            .unwrap()
    }
    #[test]
    #[ignore = "requires isolated GTK display"]
    fn close_action_closes_review_error() {
        adw::init().unwrap();
        let parent = gtk::Window::new();
        review(
            &parent,
            "Could not review setup",
            Vec::new(),
            "Invalid fixture config",
            "Close",
            |_, _| Err("not an import".into()),
            || {},
            None,
        );
        let window = review_window();
        assert!(descendants(window.upcast_ref())
            .iter()
            .filter(|w| w.is::<gtk::Expander>())
            .all(|w| w.has_css_class("toolport-details-expander")));
        let button = descendants(window.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Button>().ok())
            .find(|b| b.label().as_deref() == Some("Close"))
            .unwrap();
        button.emit_clicked();
        assert!(!window.is_visible(), "Close must close the error dialog");
    }

    #[test]
    #[ignore = "manual isolated setup screenshot fixture"]
    fn setup_screenshot_fixture() {
        adw::init().unwrap();
        let parent = gtk::Window::new();
        let state = std::env::var("TOOLPORT_SETUP_FIXTURE_STATE").unwrap_or_default();
        let items = ["Notes", "Calendar"]
            .into_iter()
            .map(|name| SetupItem {
                key: name.into(),
                name: name.into(),
                transport: "stdio".into(),
                command: Some(format!("/usr/bin/fixture-{}", name.to_lowercase())),
                args: Vec::new(),
                url: None,
                env_keys: if name == "Calendar" {
                    vec!["PAT".into()]
                } else {
                    Vec::new()
                },
                is_new: true,
                credentials: if name == "Calendar" {
                    vec![crate::registry_controller::CredentialReview {
                        key: "PAT".into(),
                        secret: true,
                        present: state != "missing",
                    }]
                } else {
                    Vec::new()
                },
                unsupported: None,
            })
            .collect();
        review(
            &parent,
            "Review and connect Claude Code",
            items,
            "Config: /home/sbx/.claude.json\nBackups will be saved in Toolport/backups/claude-code",
            "Connect",
            move |_, _| {
                if state == "verifying" {
                    use std::io::Read;
                    let mut file = std::fs::File::open("/home/sbx/setup-release")
                        .map_err(|e| e.to_string())?;
                    let mut bytes = [0u8; 1];
                    let _ = file.read(&mut bytes).map_err(|e| e.to_string())?;
                }
                if state == "failure" {
                    return Err("Calendar could not start. Check its command and retry. Client config unchanged.".into());
                }
                if state == "missing" {
                    return Err("Calendar needs credentials. Open Credentials and retry. Client config unchanged.".into());
                }
                Ok(Completion {
                    message: "Claude Code connected. Restart it to load Toolport.".into(),
                    servers: vec![
                        crate::registry_controller::SetupServerResult {
                            name: "Notes".into(),
                            tool_count: 3,
                            credential_state: "none".into(),
                        },
                        crate::registry_controller::SetupServerResult {
                            name: "Calendar".into(),
                            tool_count: 5,
                            credential_state: "stored".into(),
                        },
                    ],
                    tools: vec![
                        serde_json::json!({"name":"toolport_search_tools"}),
                        serde_json::json!({"name":"toolport_call_tool"}),
                    ],
                    backup: None,
                })
            },
            || {},
            None,
        );
        gtk::glib::MainLoop::new(None, false).run();
    }
}
