use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;

#[derive(Clone)]
pub(super) struct HooksPage {
    pub(super) root: gtk::Box,
    app: adw::Application,
    feedback: gtk::Label,
    enabled: gtk::Switch,
    summary: gtk::Label,
    profiles: gtk::Box,
    recent: gtk::Box,
    preview: gtk::Button,
    refresh_button: gtk::Button,
    updating: Rc<Cell<bool>>,
}

impl HooksPage {
    pub(super) fn new(app: &adw::Application) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("toolport-content");
        let header = adw::HeaderBar::new();
        header.add_css_class("toolport-header");
        header.set_show_back_button(true);
        header.set_title_widget(Some(
            &gtk::Label::builder()
                .label("Agent activity")
                .css_classes(["title"])
                .build(),
        ));
        let refresh_button = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Refresh agent activity")
            .build();
        header.pack_end(&refresh_button);
        root.append(&header);

        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 14);
        content.add_css_class("toolport-page");
        content.set_margin_top(28);
        content.set_margin_bottom(28);
        content.set_margin_start(28);
        content.set_margin_end(28);
        content.append(
            &gtk::Label::builder()
                .label("See what agents do outside the gateway")
                .halign(gtk::Align::Start)
                .wrap(true)
                .css_classes(["title-2"])
                .build(),
        );
        content.append(&muted("A content-free local recorder can capture session, tool, and folder metadata from Claude Code without seeing commands, files, or results."));
        let feedback = gtk::Label::builder()
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-feedback"])
            .build();
        feedback.set_label("Open Agent activity to load recorder status.");
        content.append(&feedback);

        let recorder = gtk::Box::new(gtk::Orientation::Vertical, 0);
        recorder.add_css_class("toolport-settings-group");
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.add_css_class("toolport-setting-row");
        let copy = gtk::Box::new(gtk::Orientation::Vertical, 3);
        copy.set_hexpand(true);
        copy.append(&heading("Record what my agents do"));
        copy.append(&muted("Runs on session start, after native tools, and on session end. The recorder is not attached to the step that can block a call."));
        row.append(&copy);
        let enabled = gtk::Switch::builder().valign(gtk::Align::Center).build();
        row.append(&enabled);
        recorder.append(&row);
        let guarantees = gtk::Box::new(gtk::Orientation::Vertical, 5);
        guarantees.add_css_class("toolport-setting-row");
        guarantees.append(&guarantee("Cannot stop your agent"));
        guarantees.append(&guarantee("Stores no commands, file contents, or output"));
        guarantees.append(&guarantee(
            "Stays on this machine and removes only Toolport-owned hooks",
        ));
        recorder.append(&guarantees);
        content.append(&recorder);

        content.append(&section_title(
            "Claude Code profiles",
            "Each profile needs the recorder separately. Unreadable files are reported, never overwritten.",
        ));
        let profile_actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let preview = gtk::Button::with_label("Preview exact hook changes");
        profile_actions.append(&preview);
        content.append(&profile_actions);
        let summary = muted("Loading profiles…");
        content.append(&summary);
        let profiles = gtk::Box::new(gtk::Orientation::Vertical, 0);
        profiles.add_css_class("toolport-settings-group");
        content.append(&profiles);

        content.append(&section_title(
            "Recorded so far",
            "The visible fields are the full retained record, not a redacted version of richer content.",
        ));
        let recent = gtk::Box::new(gtk::Orientation::Vertical, 0);
        recent.add_css_class("toolport-settings-group");
        content.append(&recent);

        scroller.set_child(Some(&content));
        root.append(&scroller);
        let page = Self {
            root,
            app: app.clone(),
            feedback,
            enabled,
            summary,
            profiles,
            recent,
            preview,
            refresh_button,
            updating: Rc::new(Cell::new(false)),
        };
        page.connect();
        page
    }

    fn connect(&self) {
        let page = self.clone();
        self.refresh_button.connect_clicked(move |_| page.refresh());
        let page = self.clone();
        self.enabled.connect_state_set(move |switch, enabled| {
            if page.updating.get() {
                return gtk::glib::Propagation::Proceed;
            }
            switch.set_sensitive(false);
            page.set_enabled(enabled);
            gtk::glib::Propagation::Stop
        });
        let page = self.clone();
        self.preview.connect_clicked(move |button| {
            button.set_sensitive(false);
            let button = button.clone();
            let page = page.clone();
            gtk::glib::spawn_future_local(async move {
                let result = gtk::gio::spawn_blocking(crate::hooks::preview).await;
                button.set_sensitive(true);
                match result {
                    Ok(Ok(previews)) => page.open_preview(previews),
                    Ok(Err(error)) => page.show_error(&error),
                    Err(_) => page.show_error("the recorder preview stopped unexpectedly"),
                }
            });
        });
    }

    pub(super) fn refresh(&self) {
        if self.updating.replace(true) {
            return;
        }
        self.refresh_button.set_sensitive(false);
        self.feedback.set_label("Loading agent activity…");
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(|| {
                let view = crate::hooks::view();
                let recent = crate::hooks::read_recent(200)
                    .map_err(|error| format!("could not read agent activity: {error}"))?;
                Ok::<_, String>((view, recent))
            })
            .await;
            page.refresh_button.set_sensitive(true);
            match result {
                Ok(Ok((view, recent))) => {
                    page.render(view, recent);
                    page.show_success("Agent activity is up to date.");
                }
                Ok(Err(error)) => {
                    page.updating.set(false);
                    page.show_error(&error);
                }
                Err(_) => {
                    page.updating.set(false);
                    page.show_error("the agent activity read stopped unexpectedly");
                }
            }
        });
    }

    fn set_enabled(&self, enabled: bool) {
        if self.updating.replace(true) {
            return;
        }
        self.set_controls_sensitive(false);
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || crate::hooks::set_enabled(enabled)).await;
            match result {
                Ok(Ok(view)) => {
                    let recent = gtk::gio::spawn_blocking(|| crate::hooks::read_recent(200)).await;
                    match recent {
                        Ok(Ok(recent)) => page.render(view, recent),
                        _ => page.updating.set(false),
                    }
                    page.show_success(if enabled {
                        "Agent activity recording enabled."
                    } else {
                        "Agent activity recording disabled."
                    });
                }
                Ok(Err(error)) => {
                    page.updating.set(false);
                    page.show_error(&error);
                    page.refresh();
                }
                Err(_) => {
                    page.updating.set(false);
                    page.show_error("the recorder update stopped unexpectedly");
                    page.refresh();
                }
            }
        });
    }

    fn render(&self, view: crate::hooks::HooksView, recent: Vec<serde_json::Value>) {
        self.updating.set(true);
        self.enabled.set_active(view.enabled);
        self.enabled
            .set_sensitive(view.enabled || view.binary.is_some());
        self.preview.set_sensitive(view.binary.is_some());
        clear(&self.profiles);
        let readable = view
            .profiles
            .iter()
            .filter(|profile| profile.error.is_none())
            .count();
        let installed = view
            .profiles
            .iter()
            .filter(|profile| profile.error.is_none() && profile.installed)
            .count();
        let summary = if view.profiles.is_empty() {
            "No Claude Code profile found.".to_string()
        } else if readable == 0 {
            "No detected profile could be read.".to_string()
        } else {
            format!("{installed} of {readable} readable profiles carry the recorder.")
        };
        self.summary.set_label(&summary);
        for profile in view.profiles {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            row.add_css_class("toolport-setting-row");
            row.append(
                &gtk::Label::builder()
                    .label(&profile.path)
                    .halign(gtk::Align::Start)
                    .xalign(0.0)
                    .ellipsize(gtk::pango::EllipsizeMode::Middle)
                    .tooltip_text(&profile.path)
                    .hexpand(true)
                    .build(),
            );
            let status = gtk::Label::new(Some(if profile.error.is_some() {
                "Could not read"
            } else if profile.installed {
                "Recording"
            } else if view.enabled {
                "Not written yet"
            } else {
                "Off"
            }));
            status.set_tooltip_text(profile.error.as_deref());
            status.add_css_class(if profile.error.is_some() {
                "error"
            } else {
                "toolport-muted"
            });
            row.append(&status);
            self.profiles.append(&row);
        }
        if self.profiles.first_child().is_none() {
            self.profiles
                .append(&padded(&muted("No profiles to show.")));
        }

        clear(&self.recent);
        for event in recent.iter().take(12) {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            row.add_css_class("toolport-setting-row");
            let name = event
                .get("tool")
                .or_else(|| event.get("event"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            row.append(
                &gtk::Label::builder()
                    .label(name)
                    .halign(gtk::Align::Start)
                    .xalign(0.0)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .hexpand(true)
                    .build(),
            );
            let cwd = event
                .get("cwd")
                .and_then(serde_json::Value::as_str)
                .and_then(folder_name)
                .unwrap_or("—");
            row.append(&gtk::Label::new(Some(cwd)));
            let session = event
                .get("sessionId")
                .or_else(|| event.get("session_id"))
                .and_then(serde_json::Value::as_str)
                .map(short_session)
                .unwrap_or_else(|| "—".into());
            row.append(&gtk::Label::new(Some(&session)));
            self.recent.append(&row);
        }
        if self.recent.first_child().is_none() {
            self.recent.append(&padded(&muted(if view.enabled {
                "Nothing yet. Start a Claude Code session and events will appear here."
            } else {
                "Nothing recorded. Turn the recorder on to start."
            })));
        }
        self.refresh_button.set_sensitive(true);
        self.updating.set(false);
    }

    fn set_controls_sensitive(&self, sensitive: bool) {
        self.enabled.set_sensitive(sensitive);
        self.preview.set_sensitive(sensitive);
        self.refresh_button.set_sensitive(sensitive);
    }

    fn open_preview(&self, previews: Vec<crate::hooks::HooksPreview>) {
        let window = adw::Window::builder()
            .application(&self.app)
            .title("Agent recorder preview")
            .default_width(760)
            .default_height(620)
            .modal(true)
            .build();
        if let Some(parent) = self.app.active_window() {
            window.set_transient_for(Some(&parent));
        }
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&gtk::Label::new(Some("What would be written"))));
        root.append(&header);
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.add_css_class("toolport-dialog-content");
        if previews.is_empty() {
            content.append(&muted("No Claude Code profile was found."));
        }
        for preview in previews {
            content.append(&heading(&preview.path));
            let buffer = gtk::TextBuffer::new(None::<&gtk::TextTagTable>);
            buffer.set_text(&preview.error.unwrap_or(preview.after));
            let text = gtk::TextView::builder()
                .buffer(&buffer)
                .editable(false)
                .cursor_visible(false)
                .monospace(true)
                .wrap_mode(gtk::WrapMode::WordChar)
                .top_margin(10)
                .bottom_margin(10)
                .left_margin(10)
                .right_margin(10)
                .build();
            text.set_height_request(180);
            content.append(&text);
        }
        content.append(&muted(
            "Nothing has been written. Existing settings and comments stay intact.",
        ));
        scroller.set_child(Some(&content));
        root.append(&scroller);
        window.set_content(Some(&root));
        window.present();
    }

    fn show_success(&self, message: &str) {
        self.feedback.set_label(message);
        self.feedback.remove_css_class("error");
        self.feedback.add_css_class("success");
    }

    fn show_error(&self, error: &str) {
        self.feedback
            .set_label(&format!("Agent activity error: {error}"));
        self.feedback.remove_css_class("success");
        self.feedback.add_css_class("error");
    }
}

fn clear(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

fn heading(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .halign(gtk::Align::Start)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["heading"])
        .build()
}

fn muted(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .halign(gtk::Align::Start)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["toolport-muted"])
        .build()
}

fn padded(label: &gtk::Label) -> gtk::Label {
    label.set_margin_top(14);
    label.set_margin_bottom(14);
    label.set_margin_start(14);
    label.set_margin_end(14);
    label.clone()
}

fn section_title(title: &str, description: &str) -> gtk::Box {
    let section = gtk::Box::new(gtk::Orientation::Vertical, 3);
    section.append(&heading(title));
    section.append(&muted(description));
    section
}

fn guarantee(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(format!("✓  {text}"))
        .halign(gtk::Align::Start)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["toolport-muted"])
        .build()
}

fn folder_name(path: &str) -> Option<&str> {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
}

fn short_session(session: &str) -> String {
    session.chars().take(8).collect()
}
