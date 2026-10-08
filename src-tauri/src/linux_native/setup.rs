//! Shared review window for client cutover, Collections and multi-server paste.
use crate::registry_controller::SetupItem;
use adw::prelude::*;

pub(super) fn review(
    parent: &gtk::Window,
    title: &str,
    items: Vec<SetupItem>,
    disclosure: &str,
    confirm_label: &str,
    action: impl Fn(Vec<String>) -> Result<String, String> + Send + Sync + 'static,
    finished: impl Fn() + 'static,
) {
    let dialog = adw::Window::builder()
        .transient_for(parent)
        .modal(true)
        .title(title)
        .default_width(680)
        .default_height(600)
        .build();
    dialog.add_css_class("toolport-editor");
    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    root.add_css_class("toolport-editor-body");
    let heading = gtk::Label::builder()
        .label(title)
        .xalign(0.0)
        .css_classes(["title-2"])
        .build();
    root.append(&heading);
    let lede = gtk::Label::builder()
        .label(disclosure)
        .xalign(0.0)
        .wrap(true)
        .build();
    root.append(&lede);
    let rows = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let mut selected = Vec::new();
    for item in items {
        let command = item
            .command
            .as_ref()
            .map(|c| format!("{c} {}", item.args.join(" ")))
            .or(item.url.clone())
            .unwrap_or_else(|| "Needs an endpoint URL".into());
        let row = gtk::CheckButton::builder().active(true).build();
        let label = gtk::Label::builder()
            .label(format!(
                "{}\n{}\n{}",
                item.name,
                command,
                if item.env_keys.is_empty() {
                    String::new()
                } else {
                    format!("Setup inputs: {}", item.env_keys.join(", "))
                }
            ))
            .xalign(0.0)
            .wrap(true)
            .build();
        row.set_child(Some(&label));
        rows.append(&row);
        selected.push((row, item.key));
    }
    let scroller = gtk::ScrolledWindow::builder()
        .child(&rows)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    root.append(&scroller);
    let feedback = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .selectable(true)
        .visible(false)
        .build();
    root.append(&feedback);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let cancel = gtk::Button::with_label("Cancel");
    let confirm = gtk::Button::with_label(confirm_label);
    confirm.add_css_class("suggested-action");
    actions.append(&cancel);
    actions.append(&confirm);
    root.append(&actions);
    dialog.set_content(Some(&root));
    let closing = dialog.clone();
    cancel.connect_clicked(move |_| closing.close());
    let action = std::sync::Arc::new(action);
    let finished = std::rc::Rc::new(finished);
    confirm.connect_clicked(move |button| {
        let keys = selected
            .iter()
            .filter(|(row, _)| row.is_active())
            .map(|(_, key)| key.clone())
            .collect::<Vec<_>>();
        button.set_sensitive(false);
        cancel.set_sensitive(false);
        scroller.set_sensitive(false);
        feedback.set_label("Checking setup...");
        feedback.set_visible(true);
        let action = action.clone();
        let finished = finished.clone();
        let feedback = feedback.clone();
        let button = button.clone();
        let cancel = cancel.clone();
        let scroller = scroller.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || action(keys)).await;
            cancel.set_sensitive(true);
            scroller.set_sensitive(true);
            match result {
                Ok(Ok(message)) => {
                    feedback.set_label(&message);
                    scroller.set_visible(false);
                    cancel.set_label("Done");
                    finished();
                }
                Ok(Err(error)) => {
                    feedback.set_label(&error);
                    button.set_sensitive(true);
                }
                Err(_) => {
                    feedback.set_label("Setup stopped. Client config unchanged. Retry.");
                    button.set_sensitive(true);
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
) {
    let parent = parent.clone();
    gtk::glib::spawn_future_local(async move {
        let id = client_id.clone();
        let preview =
            gtk::gio::spawn_blocking(move || crate::registry_controller::preview_client_setup(&id))
                .await;
        match preview {
            Ok(Ok(preview)) => {
                let disclosure = format!("Config: {}\nBackup directory: {}\nSelected entries move after gateway verification. Unchecked entries and plugin servers stay in place.{}", preview.config_path, preview.backup_dir, if force { " This replaces the customized Toolport entry." } else { "" });
                review(
                    &parent,
                    "Review and connect",
                    preview.items,
                    &disclosure,
                    "Connect to Toolport",
                    move |selected| {
                        let outcome = crate::registry_controller::migrate_client_reviewed(
                            &client_id,
                            profile.as_deref(),
                            force,
                            &selected,
                            &preview.revision,
                        )?;
                        Ok(format!("Connected. Restart the client.\nConfig: {}\nBackup: {}\nGateway tools your agent will see:\n{}", outcome.result.outcome.path, outcome.result.outcome.backup.as_deref().unwrap_or("No previous config"), outcome.tools.iter().filter_map(|t| t["name"].as_str()).collect::<Vec<_>>().join("\n")))
                    },
                    finished,
                );
            }
            Ok(Err(error)) => review(
                &parent,
                "Could not review setup",
                Vec::new(),
                &error,
                "Close",
                |_| Err("Fix the client config and retry.".into()),
                || {},
            ),
            Err(_) => review(
                &parent,
                "Could not review setup",
                Vec::new(),
                "Client review stopped.",
                "Close",
                |_| Err("Retry from Clients.".into()),
                || {},
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
        })
        .collect();
    review(parent,&format!("Review {name}"),items,"Review what each server runs. Valid servers turn on. Servers needing credentials or launch values stay off until setup is complete.","Add selected servers",move |keys| {
        let selected=entries.iter().enumerate().filter(|(i,_)|keys.contains(&i.to_string())).map(|(_,e)|e.clone()).collect();
        let (_,added)=crate::registry_controller::add_catalog_stack(selected)?;
        Ok(format!("Added {added} servers. Check status and complete any missing setup inputs under Servers."))
    },finished);
}
