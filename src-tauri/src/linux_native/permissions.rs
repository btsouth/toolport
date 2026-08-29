use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;

use crate::registry::{GuardMode, PermissionAction, PermissionRule};

#[derive(Clone)]
pub(super) struct PermissionsPage {
    pub(super) root: gtk::Box,
    app: adw::Application,
    feedback: gtk::Label,
    enabled: gtk::Switch,
    rules: gtk::Box,
    presets: gtk::FlowBox,
    profiles: gtk::Box,
    pattern: gtk::Entry,
    action: gtk::DropDown,
    add: gtk::Button,
    guard_mode: gtk::DropDown,
    guard_status: gtk::Label,
    guard_preview: gtk::Button,
    refresh_button: gtk::Button,
    view: Rc<RefCell<Option<crate::agent_permissions::PermissionsView>>>,
    guard: Rc<RefCell<Option<crate::agent_guard::GuardView>>>,
    updating: Rc<Cell<bool>>,
}

impl PermissionsPage {
    pub(super) fn new(app: &adw::Application) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("toolport-content");
        let header = adw::HeaderBar::new();
        header.add_css_class("toolport-header");
        header.set_show_back_button(true);
        header.set_title_widget(Some(
            &gtk::Label::builder()
                .label("Agent permissions")
                .css_classes(["title"])
                .build(),
        ));
        let refresh_button = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Refresh permission policy")
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
                .label("One native-tool policy across your agents")
                .halign(gtk::Align::Start)
                .wrap(true)
                .css_classes(["title-2"])
                .build(),
        );
        content.append(&muted("Claude Code enforces these rules in its own settings. Cursor can evaluate the same policy through Toolport's guard hook."));
        let feedback = gtk::Label::builder()
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-feedback"])
            .build();
        feedback.set_label("Open Agent permissions to load the current policy.");
        content.append(&feedback);

        let policy = gtk::Box::new(gtk::Orientation::Vertical, 0);
        policy.add_css_class("toolport-settings-group");
        let policy_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        policy_row.add_css_class("toolport-setting-row");
        let policy_copy = gtk::Box::new(gtk::Orientation::Vertical, 3);
        policy_copy.set_hexpand(true);
        policy_copy.append(&heading("Enforce in Claude Code"));
        policy_copy.append(&muted("Writes only Toolport-owned rules into every Claude Code profile. Turning it off removes only those entries."));
        policy_row.append(&policy_copy);
        let enabled = gtk::Switch::builder().valign(gtk::Align::Center).build();
        policy_row.append(&enabled);
        policy.append(&policy_row);
        let policy_actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        policy_actions.add_css_class("toolport-setting-row");
        let preview = gtk::Button::with_label("Preview exact settings files");
        policy_actions.append(&preview);
        policy.append(&policy_actions);
        content.append(&policy);

        content.append(&section_title(
            "Rules",
            "Claude Code syntax such as Bash(rm -rf *), Read(./.env), or mcp__github__create_issue.",
        ));
        let rules = gtk::Box::new(gtk::Orientation::Vertical, 0);
        rules.add_css_class("toolport-settings-group");
        content.append(&rules);

        let editor = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let actions = gtk::StringList::new(&["Never", "Ask first", "Always allow"]);
        let action = gtk::DropDown::new(Some(actions), gtk::Expression::NONE);
        let pattern = gtk::Entry::builder()
            .placeholder_text("Bash(rm -rf *)")
            .hexpand(true)
            .build();
        let add = gtk::Button::with_label("Add rule");
        add.add_css_class("suggested-action");
        editor.append(&action);
        editor.append(&pattern);
        editor.append(&add);
        content.append(&editor);

        content.append(&section_title(
            "Presets",
            "Presets add rules for review. They do not enable enforcement.",
        ));
        let presets = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .column_spacing(7)
            .row_spacing(7)
            .max_children_per_line(5)
            .build();
        presets.set_halign(gtk::Align::Fill);
        content.append(&presets);

        content.append(&section_title(
            "Claude Code profiles",
            "Every detected profile is shown so stale or unreadable settings cannot look protected.",
        ));
        let profiles = gtk::Box::new(gtk::Orientation::Vertical, 0);
        profiles.add_css_class("toolport-settings-group");
        content.append(&profiles);

        content.append(&section_title(
            "Cursor guard",
            "Observe records what the policy would decide. Enforce applies Never and Ask first before shell, file-read, and MCP calls.",
        ));
        let guard_group = gtk::Box::new(gtk::Orientation::Vertical, 0);
        guard_group.add_css_class("toolport-settings-group");
        let guard_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        guard_row.add_css_class("toolport-setting-row");
        let guard_copy = gtk::Box::new(gtk::Orientation::Vertical, 3);
        guard_copy.set_hexpand(true);
        guard_copy.append(&heading("Guard mode"));
        let guard_status = muted("Loading Cursor hook status…");
        guard_status.set_selectable(true);
        guard_copy.append(&guard_status);
        guard_row.append(&guard_copy);
        let modes = gtk::StringList::new(&["Off", "Observe", "Enforce"]);
        let guard_mode = gtk::DropDown::new(Some(modes), gtk::Expression::NONE);
        guard_row.append(&guard_mode);
        guard_group.append(&guard_row);
        let guard_actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        guard_actions.add_css_class("toolport-setting-row");
        let guard_preview = gtk::Button::with_label("Preview exact hooks.json");
        guard_actions.append(&guard_preview);
        guard_group.append(&guard_actions);
        content.append(&guard_group);

        scroller.set_child(Some(&content));
        root.append(&scroller);
        let page = Self {
            root,
            app: app.clone(),
            feedback,
            enabled,
            rules,
            presets,
            profiles,
            pattern,
            action,
            add,
            guard_mode,
            guard_status,
            guard_preview,
            refresh_button,
            view: Rc::new(RefCell::new(None)),
            guard: Rc::new(RefCell::new(None)),
            updating: Rc::new(Cell::new(false)),
        };
        page.connect(preview);
        page
    }

    fn connect(&self, preview: gtk::Button) {
        let page = self.clone();
        self.refresh_button.connect_clicked(move |_| page.refresh());

        let page = self.clone();
        self.enabled.connect_state_set(move |switch, enabled| {
            if page.updating.get() {
                return gtk::glib::Propagation::Proceed;
            }
            switch.set_sensitive(false);
            page.mutate_permissions(
                if enabled {
                    "Claude Code enforcement enabled."
                } else {
                    "Claude Code enforcement disabled."
                },
                move || crate::agent_permissions::set_enabled(enabled),
            );
            gtk::glib::Propagation::Stop
        });

        let page = self.clone();
        self.add.connect_clicked(move |_| page.add_rule());
        let page = self.clone();
        self.pattern.connect_activate(move |_| page.add_rule());

        let page = self.clone();
        preview.connect_clicked(move |button| {
            button.set_sensitive(false);
            let page = page.clone();
            let button = button.clone();
            gtk::glib::spawn_future_local(async move {
                let result =
                    gtk::gio::spawn_blocking(|| crate::agent_permissions::preview(None)).await;
                button.set_sensitive(true);
                match result {
                    Ok(Ok(previews)) => page.open_preview(
                        "Claude Code permission preview",
                        previews
                            .into_iter()
                            .map(|preview| PreviewItem {
                                path: preview.path,
                                after: preview.after,
                                error: preview.error,
                            })
                            .collect(),
                        "Nothing has been written. Only the permissions key changes.",
                    ),
                    Ok(Err(error)) => page.show_error(&error),
                    Err(_) => page.show_error("the preview stopped unexpectedly"),
                }
            });
        });

        let page = self.clone();
        self.guard_mode.connect_selected_notify(move |dropdown| {
            if page.updating.get() {
                return;
            }
            let mode = selected_guard_mode(dropdown.selected());
            page.mutate_guard("Cursor guard mode updated.", move || {
                crate::agent_guard::set_cursor_mode(mode)
            });
        });

        let page = self.clone();
        self.guard_preview.connect_clicked(move |button| {
            button.set_sensitive(false);
            let mode = page
                .guard
                .borrow()
                .as_ref()
                .map(|guard| {
                    if guard.cursor_mode.is_off() {
                        GuardMode::Observe
                    } else {
                        guard.cursor_mode
                    }
                })
                .unwrap_or(GuardMode::Observe);
            let page = page.clone();
            let button = button.clone();
            gtk::glib::spawn_future_local(async move {
                let result =
                    gtk::gio::spawn_blocking(move || crate::agent_guard::preview(mode)).await;
                button.set_sensitive(true);
                match result {
                    Ok(Ok(preview)) => page.open_preview(
                        "Cursor guard preview",
                        preview
                            .into_iter()
                            .map(|preview| PreviewItem {
                                path: preview.path,
                                after: preview.after,
                                error: preview.error,
                            })
                            .collect(),
                        "Nothing has been written. Only Toolport's hooks entries change.",
                    ),
                    Ok(Err(error)) => page.show_error(&error),
                    Err(_) => page.show_error("the guard preview stopped unexpectedly"),
                }
            });
        });
    }

    pub(super) fn refresh(&self) {
        if self.updating.replace(true) {
            return;
        }
        self.refresh_button.set_sensitive(false);
        self.feedback.set_label("Loading permission policy…");
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(|| {
                (crate::agent_permissions::view(), crate::agent_guard::view())
            })
            .await;
            page.refresh_button.set_sensitive(true);
            match result {
                Ok((view, guard)) => {
                    page.render(view, guard);
                    page.show_success("Permission policy is up to date.");
                }
                Err(_) => {
                    page.updating.set(false);
                    page.show_error("the permission read stopped unexpectedly");
                }
            }
        });
    }

    fn render(
        &self,
        view: crate::agent_permissions::PermissionsView,
        guard: crate::agent_guard::GuardView,
    ) {
        self.updating.set(true);
        self.enabled.set_active(view.enabled);
        self.enabled.set_sensitive(true);
        self.render_rules(&view);
        self.render_presets(&view);
        self.render_profiles(&view);
        self.guard_mode.set_selected(match guard.cursor_mode {
            GuardMode::Off => 0,
            GuardMode::Observe => 1,
            GuardMode::Enforce => 2,
        });
        self.guard_mode.set_sensitive(guard.binary.is_some());
        self.guard_preview.set_sensitive(guard.binary.is_some());
        self.guard_status.set_label(&guard_summary(&guard));
        *self.view.borrow_mut() = Some(view);
        *self.guard.borrow_mut() = Some(guard);
        self.add.set_sensitive(true);
        self.refresh_button.set_sensitive(true);
        self.updating.set(false);
    }

    fn render_rules(&self, view: &crate::agent_permissions::PermissionsView) {
        clear_box(&self.rules);
        if view.rules.is_empty() {
            self.rules.append(&padded(&muted(
                "No rules yet. Add one below or start from a preset.",
            )));
            return;
        }
        for rule in &view.rules {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            row.add_css_class("toolport-setting-row");
            let badge = gtk::Label::new(Some(action_label(rule.action)));
            badge.add_css_class("toolport-mode-badge");
            row.append(&badge);
            row.append(
                &gtk::Label::builder()
                    .label(&rule.pattern)
                    .halign(gtk::Align::Start)
                    .xalign(0.0)
                    .ellipsize(gtk::pango::EllipsizeMode::Middle)
                    .selectable(true)
                    .hexpand(true)
                    .build(),
            );
            let remove = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .tooltip_text(format!("Remove {}", rule.pattern))
                .css_classes(["flat"])
                .build();
            let page = self.clone();
            let target = rule.clone();
            remove.connect_clicked(move |_| {
                let Some(view) = page.view.borrow().clone() else {
                    return;
                };
                let rules = view
                    .rules
                    .into_iter()
                    .filter(|rule| rule != &target)
                    .collect();
                page.mutate_permissions("Rule removed.", move || {
                    crate::agent_permissions::set_rules(rules)
                });
            });
            row.append(&remove);
            self.rules.append(&row);
        }
    }

    fn render_presets(&self, view: &crate::agent_permissions::PermissionsView) {
        while let Some(child) = self.presets.first_child() {
            self.presets.remove(&child);
        }
        for preset in &view.presets {
            let button = gtk::Button::with_label(&preset.label);
            let already = preset.rules.iter().all(|candidate| {
                view.rules
                    .iter()
                    .any(|rule| rule.pattern == candidate.pattern)
            });
            button.set_sensitive(!already);
            let page = self.clone();
            let additions = preset.rules.clone();
            button.connect_clicked(move |_| {
                let Some(view) = page.view.borrow().clone() else {
                    return;
                };
                let mut rules = view.rules;
                for addition in &additions {
                    if !rules.iter().any(|rule| rule.pattern == addition.pattern) {
                        rules.push(addition.clone());
                    }
                }
                page.mutate_permissions("Preset added for review.", move || {
                    crate::agent_permissions::set_rules(rules)
                });
            });
            self.presets.insert(&button, -1);
        }
    }

    fn render_profiles(&self, view: &crate::agent_permissions::PermissionsView) {
        clear_box(&self.profiles);
        if view.profiles.is_empty() {
            self.profiles
                .append(&padded(&muted("No Claude Code profile found.")));
            return;
        }
        for profile in &view.profiles {
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
            let status = gtk::Label::new(Some(profile_label(&profile.state)));
            status.add_css_class(if profile.state == "error" {
                "error"
            } else {
                "toolport-muted"
            });
            status.set_tooltip_text(profile.error.as_deref());
            row.append(&status);
            self.profiles.append(&row);
        }
    }

    fn add_rule(&self) {
        let pattern = self.pattern.text().trim().to_string();
        if pattern.is_empty() {
            self.show_error("enter a rule pattern first");
            return;
        }
        let Some(view) = self.view.borrow().clone() else {
            return;
        };
        let mut rules = view.rules;
        rules.push(PermissionRule {
            pattern,
            action: selected_permission_action(self.action.selected()),
        });
        self.mutate_permissions("Rule added.", move || {
            crate::agent_permissions::set_rules(rules)
        });
    }

    fn mutate_permissions(
        &self,
        success: &'static str,
        operation: impl FnOnce() -> Result<crate::agent_permissions::PermissionsView, String>
            + Send
            + 'static,
    ) {
        if self.updating.replace(true) {
            return;
        }
        self.set_controls_sensitive(false);
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(operation).await;
            match result {
                Ok(Ok(view)) => {
                    let guard = gtk::gio::spawn_blocking(crate::agent_guard::view)
                        .await
                        .ok();
                    if let Some(guard) = guard {
                        page.render(view, guard);
                    } else {
                        page.updating.set(false);
                    }
                    page.show_success(success);
                }
                Ok(Err(error)) => {
                    page.updating.set(false);
                    page.show_error(&error);
                    page.refresh();
                }
                Err(_) => {
                    page.updating.set(false);
                    page.show_error("the permission update stopped unexpectedly");
                    page.refresh();
                }
            }
        });
    }

    fn mutate_guard(
        &self,
        success: &'static str,
        operation: impl FnOnce() -> Result<crate::agent_guard::GuardView, String> + Send + 'static,
    ) {
        if self.updating.replace(true) {
            return;
        }
        self.set_controls_sensitive(false);
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(operation).await;
            match result {
                Ok(Ok(guard)) => {
                    let view = gtk::gio::spawn_blocking(crate::agent_permissions::view)
                        .await
                        .ok();
                    if let Some(view) = view {
                        page.render(view, guard);
                    } else {
                        page.updating.set(false);
                    }
                    page.show_success(success);
                }
                Ok(Err(error)) => {
                    page.updating.set(false);
                    page.show_error(&error);
                    page.refresh();
                }
                Err(_) => {
                    page.updating.set(false);
                    page.show_error("the guard update stopped unexpectedly");
                    page.refresh();
                }
            }
        });
    }

    fn set_controls_sensitive(&self, sensitive: bool) {
        self.enabled.set_sensitive(sensitive);
        self.add.set_sensitive(sensitive);
        self.guard_mode.set_sensitive(sensitive);
        self.guard_preview.set_sensitive(sensitive);
        self.refresh_button.set_sensitive(sensitive);
    }

    fn open_preview(&self, title: &str, previews: Vec<PreviewItem>, footer: &str) {
        let window = adw::Window::builder()
            .application(&self.app)
            .title(title)
            .default_width(760)
            .default_height(620)
            .modal(true)
            .build();
        if let Some(parent) = self.app.active_window() {
            window.set_transient_for(Some(&parent));
        }
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&gtk::Label::new(Some(title))));
        root.append(&header);
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.add_css_class("toolport-dialog-content");
        if previews.is_empty() {
            content.append(&muted(
                "No matching agent profile was found, so there is nothing to write.",
            ));
        }
        for preview in previews {
            content.append(&heading(&preview.path));
            let buffer = gtk::TextBuffer::new(None::<&gtk::TextTagTable>);
            buffer.set_text(&preview.error.unwrap_or(preview.after));
            let view = gtk::TextView::builder()
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
            view.set_height_request(180);
            content.append(&view);
        }
        content.append(&muted(footer));
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
            .set_label(&format!("Permission error: {error}"));
        self.feedback.remove_css_class("success");
        self.feedback.add_css_class("error");
    }
}

struct PreviewItem {
    path: String,
    after: String,
    error: Option<String>,
}

fn clear_box(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

fn padded(label: &gtk::Label) -> gtk::Label {
    label.set_margin_top(14);
    label.set_margin_bottom(14);
    label.set_margin_start(14);
    label.set_margin_end(14);
    label.clone()
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

fn section_title(title: &str, description: &str) -> gtk::Box {
    let section = gtk::Box::new(gtk::Orientation::Vertical, 3);
    section.append(&heading(title));
    section.append(&muted(description));
    section
}

fn selected_permission_action(selected: u32) -> PermissionAction {
    match selected {
        1 => PermissionAction::Ask,
        2 => PermissionAction::Allow,
        _ => PermissionAction::Deny,
    }
}

fn selected_guard_mode(selected: u32) -> GuardMode {
    match selected {
        1 => GuardMode::Observe,
        2 => GuardMode::Enforce,
        _ => GuardMode::Off,
    }
}

fn action_label(action: PermissionAction) -> &'static str {
    match action {
        PermissionAction::Deny => "Never",
        PermissionAction::Ask => "Ask first",
        PermissionAction::Allow => "Always allow",
    }
}

fn profile_label(state: &str) -> &'static str {
    match state {
        "applied" => "Applied",
        "stale" => "Not applied yet",
        "off" => "Off",
        _ => "Error",
    }
}

fn guard_summary(guard: &crate::agent_guard::GuardView) -> String {
    let binary = if guard.binary.is_some() {
        "gateway available"
    } else {
        "gateway unavailable"
    };
    match &guard.cursor {
        Some(cursor) if cursor.error.is_some() => {
            format!("{} · error reading hooks.json", cursor.path)
        }
        Some(cursor) if cursor.installed => format!("{} · guard installed · {binary}", cursor.path),
        Some(cursor) => format!("{} · no guard installed · {binary}", cursor.path),
        None => format!("Cursor config folder unavailable · {binary}"),
    }
}
