use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;

#[derive(Clone)]
pub(super) struct ServerToolsPanel {
    pub(super) root: gtk::Box,
    app: adw::Application,
    server_id: String,
    feedback: gtk::Label,
    view_stack: gtk::Stack,
    tools: gtk::Box,
    resources: gtk::Box,
    prompts: gtk::Box,
    loading: Rc<Cell<bool>>,
    tool_filter: gtk::SearchEntry,
    /// The last loaded capabilities and policy, so the tool filter re-renders
    /// without reconnecting to the server.
    last_load: Rc<RefCell<Option<(crate::playground::Capabilities, ToolPolicy)>>>,
}

#[derive(Clone)]
struct ToolPolicy {
    disabled: std::collections::HashSet<String>,
    pinned: std::collections::HashSet<String>,
    overrides: std::collections::HashMap<String, crate::registry::ToolOverride>,
}

impl ServerToolsPanel {
    pub(super) fn new(app: &adw::Application, server_id: &str) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("toolport-content");
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        let page = gtk::Box::new(gtk::Orientation::Vertical, 14);
        page.add_css_class("toolport-page");
        page.set_margin_top(20);
        page.set_margin_bottom(20);
        page.set_margin_start(20);
        page.set_margin_end(20);
        let controls = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        page.append(
            &gtk::Label::builder()
                .label(
                    "Calls follow the active profile and gateway policy, including approval gates.",
                )
                .wrap(true)
                .xalign(0.0)
                .css_classes(["toolport-muted"])
                .build(),
        );
        let feedback = gtk::Label::builder()
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-feedback"])
            .build();
        feedback.set_label("Choose a server to load its capabilities.");

        let tool_filter = gtk::SearchEntry::builder()
            .placeholder_text("Filter tools")
            .css_classes(["toolport-search"])
            .hexpand(true)
            .build();
        controls.append(&tool_filter);
        page.append(&controls);
        let view_stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(120)
            .build();
        let tools = capability_list();
        let resources = capability_list();
        let prompts = capability_list();
        view_stack.add_titled(&tools, Some("tools"), "Tools");
        view_stack.add_titled(&resources, Some("resources"), "Resources");
        view_stack.add_titled(&prompts, Some("prompts"), "Prompts");
        let switcher = gtk::StackSwitcher::new();
        switcher.set_stack(Some(&view_stack));
        switcher.set_halign(gtk::Align::Start);
        page.append(&switcher);
        page.append(&feedback);
        page.append(&view_stack);
        // The filter only ever applied to tools, so on the other two tabs it was
        // a control that did nothing.
        let filter_for_tab = tool_filter.clone();
        view_stack.connect_visible_child_name_notify(move |stack| {
            filter_for_tab.set_visible(stack.visible_child_name().as_deref() == Some("tools"));
        });
        scroller.set_child(Some(&page));
        root.append(&scroller);

        let panel = Self {
            root,
            app: app.clone(),
            server_id: server_id.to_string(),
            feedback,
            view_stack,
            tools,
            resources,
            prompts,
            loading: Rc::new(Cell::new(false)),
            tool_filter,
            last_load: Rc::new(RefCell::new(None)),
        };
        let page_for_filter = panel.clone();
        panel
            .tool_filter
            .connect_search_changed(move |_| page_for_filter.render_tools());
        panel
    }

    pub(super) fn refresh(&self) {
        self.load_selected();
    }

    fn load_selected(&self) {
        if self.loading.replace(true) {
            return;
        }
        let server_id = self.server_id.clone();
        self.set_status("Connecting to server…");
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                let capabilities = crate::playground::capabilities(&server_id)?;
                let registry = crate::registry::load()?;
                let disabled = registry
                    .servers
                    .iter()
                    .find(|server| server.id == server_id)
                    .map(|server| server.disabled_tools.iter().cloned().collect())
                    .unwrap_or_default();
                let pinned = registry
                    .pinned_tools
                    .get(&server_id)
                    .map(|tools| tools.iter().cloned().collect())
                    .unwrap_or_default();
                let overrides = registry
                    .tool_overrides
                    .get(&server_id)
                    .cloned()
                    .unwrap_or_default();
                Ok::<_, String>((
                    capabilities,
                    ToolPolicy {
                        disabled,
                        pinned,
                        overrides,
                    },
                ))
            })
            .await;
            page.loading.set(false);
            match result {
                Ok(Ok((capabilities, policy))) => page.render(capabilities, policy),
                Ok(Err(error)) => page.show_error(&error),
                Err(_) => page.show_error("the server connection stopped unexpectedly"),
            }
        });
    }

    fn render(&self, capabilities: crate::playground::Capabilities, policy: ToolPolicy) {
        self.clear_lists();
        let tool_count = capabilities.tools.len();
        let resource_count = capabilities.resources.len();
        let prompt_count = capabilities.prompts.len();
        for resource in capabilities.resources.iter().cloned() {
            self.resources.append(&resource_row(resource, self.clone()));
        }
        for prompt in capabilities.prompts.iter().cloned() {
            self.prompts.append(&prompt_row(prompt, self.clone()));
        }
        if resource_count == 0 {
            self.resources
                .append(&empty_capability("No resources advertised"));
        }
        if prompt_count == 0 {
            self.prompts
                .append(&empty_capability("No prompts advertised"));
        }
        *self.last_load.borrow_mut() = Some((capabilities, policy));
        self.render_tools();
        self.set_capability_counts(Some((tool_count, resource_count, prompt_count)));
        // The counts belong on the tabs they describe, so the status line goes
        // back to being transient and gets out of the way once a load succeeds.
        self.set_status("");
    }

    /// Tab titles carry the counts; `None` clears them back to bare names for
    /// the states where nothing is loaded.
    fn set_capability_counts(&self, counts: Option<(usize, usize, usize)>) {
        let labels = match counts {
            Some((tools, resources, prompts)) => [
                format!("Tools ({tools})"),
                format!("Resources ({resources})"),
                format!("Prompts ({prompts})"),
            ],
            None => ["Tools".into(), "Resources".into(), "Prompts".into()],
        };
        for (child, label) in [&self.tools, &self.resources, &self.prompts]
            .into_iter()
            .zip(labels)
        {
            self.view_stack.page(child).set_title(&label);
        }
    }

    /// An empty message hides the line entirely rather than leaving a banner
    /// sitting on the page with nothing in it.
    fn set_status(&self, message: &str) {
        self.feedback.set_label(message);
        self.feedback.set_visible(!message.is_empty());
        self.feedback.remove_css_class("error");
    }

    fn render_tools(&self) {
        let borrowed = self.last_load.borrow();
        let Some((capabilities, policy)) = borrowed.as_ref() else {
            return;
        };
        while let Some(child) = self.tools.first_child() {
            self.tools.remove(&child);
        }
        if capabilities.tools.is_empty() {
            self.tools.append(&empty_capability("No tools advertised"));
            return;
        }
        let query = self.tool_filter.text().to_lowercase();
        let mut shown = 0usize;
        for tool in &capabilities.tools {
            let name = tool
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            if !query.is_empty() {
                let description = tool
                    .get("description")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                if !name.to_lowercase().contains(&query)
                    && !description.to_lowercase().contains(&query)
                {
                    continue;
                }
            }
            shown += 1;
            self.tools.append(&tool_row(
                tool.clone(),
                !policy.disabled.contains(&name),
                policy.pinned.contains(&name),
                policy.overrides.get(&name).cloned(),
                self.clone(),
            ));
        }
        if shown == 0 {
            self.tools
                .append(&empty_capability("No tool matches the filter"));
        }
    }

    /// A pin or visibility change writes to the local registry, nothing more.
    /// Updating the cached policy in place keeps a later filter re-render
    /// honest without calling `load_selected`, which opens a fresh connection
    /// to the server and refetches every tool for what is a local toggle.
    fn remember_tool_enabled(&self, tool: &str, enabled: bool) {
        if let Some((_, policy)) = self.last_load.borrow_mut().as_mut() {
            if enabled {
                policy.disabled.remove(tool);
            } else {
                policy.disabled.insert(tool.to_string());
            }
        }
    }

    fn remember_tool_pinned(&self, tool: &str, pinned: bool) {
        if let Some((_, policy)) = self.last_load.borrow_mut().as_mut() {
            if pinned {
                policy.pinned.insert(tool.to_string());
            } else {
                policy.pinned.remove(tool);
            }
        }
    }

    fn selected_server_id(&self) -> Option<String> {
        Some(self.server_id.clone())
    }

    fn clear_lists(&self) {
        for list in [&self.tools, &self.resources, &self.prompts] {
            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
        }
    }

    fn show_error(&self, error: &str) {
        self.set_status(&format!("Tools error: {error}"));
        self.feedback.add_css_class("error");
    }
}

fn capability_list() -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, 10)
}

fn empty_capability(message: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(message)
        .halign(gtk::Align::Start)
        .css_classes(["toolport-muted"])
        .build()
}

/// GtkLabel implements `lines` as a negative Pango layout height, and Pango
/// applies that as a line limit *per paragraph*, ellipsizing each one on its
/// own. MCP tool descriptions are routinely multi-paragraph instruction blocks,
/// so a two-line cap on a fifteen-paragraph description still rendered thirty
/// lines and one tool filled the whole page. Collapsing the text to a single
/// paragraph first is what makes the cap bind.
fn one_paragraph(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn state_badge(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .css_classes(["toolport-badge", "caption"])
        .valign(gtk::Align::Center)
        .build()
}

fn tool_row(
    tool: serde_json::Value,
    enabled: bool,
    pinned: bool,
    exposure_override: Option<crate::registry::ToolOverride>,
    page: ServerToolsPanel,
) -> gtk::Box {
    let name = tool
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("Unknown tool")
        .to_string();
    let description = tool
        .get("description")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("No description")
        .to_string();
    let summary = one_paragraph(&description);
    let schema = tool
        .get("inputSchema")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({"type": "object"}));
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.add_css_class("toolport-card");
    let copy = gtk::Box::new(gtk::Orientation::Vertical, 3);
    copy.set_hexpand(true);

    // Pinning and renaming moved into the row menu, so their state comes back
    // as badges. Both are off for most tools, so most rows show nothing here.
    let title = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    title.append(
        &gtk::Label::builder()
            .label(&name)
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .max_width_chars(48)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["heading"])
            .build(),
    );
    let pinned_badge = state_badge("Pinned");
    pinned_badge.set_visible(pinned);
    title.append(&pinned_badge);
    let renamed_badge = state_badge("Renamed");
    renamed_badge.set_visible(exposure_override.is_some());
    title.append(&renamed_badge);
    for (hint, label) in [
        ("readOnlyHint", "Read-only"),
        ("destructiveHint", "Destructive"),
    ] {
        if tool
            .pointer(&format!("/annotations/{hint}"))
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        {
            title.append(&state_badge(label));
        }
    }
    title.append(&state_badge(if enabled { "Enabled" } else { "Disabled" }));
    title.append(&state_badge(
        match tool
            .get("toolportQuarantine")
            .and_then(serde_json::Value::as_str)
        {
            Some("quarantined") => "Quarantined",
            Some("clear") => "Not quarantined",
            _ => "Quarantine unknown",
        },
    ));
    copy.append(&title);

    copy.append(
        &gtk::Label::builder()
            .label(&summary)
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .wrap(true)
            .lines(2)
            .max_width_chars(60)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["toolport-muted"])
            .build(),
    );
    row.append(&copy);

    // Built before the visibility toggle, which desensitizes it in place rather
    // than rebuilding the row.
    let run = gtk::Button::with_label("Run");
    run.add_css_class("toolport-secondary-action");
    run.set_valign(gtk::Align::Center);
    run.set_sensitive(enabled);

    // A compact icon toggle rather than a GtkSwitch: same single click, but the
    // switch was the tallest widget in the row and set the row height.
    let visibility = gtk::ToggleButton::builder()
        .icon_name(if enabled {
            "view-reveal-symbolic"
        } else {
            "view-conceal-symbolic"
        })
        .active(!enabled)
        .valign(gtk::Align::Center)
        .tooltip_text(if enabled {
            "Exposed to clients. Hide it."
        } else {
            "Hidden from clients. Expose it."
        })
        .css_classes(["flat"])
        .build();
    // Reverting a failed write re-enters this handler, so the guard keeps the
    // revert from being treated as a fresh toggle.
    let reverting = Rc::new(Cell::new(false));
    let page_for_visibility = page.clone();
    let name_for_visibility = name.clone();
    let run_for_visibility = run.clone();
    visibility.connect_toggled(move |button| {
        if reverting.get() {
            return;
        }
        let Some(server_id) = page_for_visibility.selected_server_id() else {
            return;
        };
        let enabled = !button.is_active();
        button.set_sensitive(false);
        let page = page_for_visibility.clone();
        let name = name_for_visibility.clone();
        let button = button.clone();
        let run = run_for_visibility.clone();
        let reverting = reverting.clone();
        gtk::glib::spawn_future_local(async move {
            let result = {
                let name = name.clone();
                gtk::gio::spawn_blocking(move || {
                    crate::registry_controller::set_tool_enabled(&server_id, &name, enabled)
                })
                .await
            };
            button.set_sensitive(true);
            match result {
                Ok(Ok(_)) => {
                    page.remember_tool_enabled(&name, enabled);
                    set_visibility_look(&button, enabled);
                    run.set_sensitive(enabled);
                }
                Ok(Err(error)) => {
                    // The registry rejected the change, so the control must not
                    // keep claiming it happened.
                    reverting.set(true);
                    button.set_active(enabled);
                    reverting.set(false);
                    page.show_error(&error);
                }
                Err(_) => {
                    reverting.set(true);
                    button.set_active(enabled);
                    reverting.set(false);
                    page.show_error("the tool visibility update stopped unexpectedly");
                }
            }
        });
    });

    let actions = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let pin_item = super::toolport_menu_button(pin_label(pinned));
    let rename_item = super::toolport_menu_button("Rename for clients\u{2026}");
    actions.append(&pin_item);
    actions.append(&rename_item);
    let popover = super::toolport_menu_popover();
    popover.set_child(Some(&actions));
    let menu = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .tooltip_text(format!("More actions for {name}"))
        .popover(&popover)
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();

    let is_pinned = Rc::new(Cell::new(pinned));
    let page_for_pin = page.clone();
    let name_for_pin = name.clone();
    let badge_for_pin = pinned_badge.clone();
    let popover_for_pin = popover.clone();
    pin_item.connect_clicked(move |item| {
        popover_for_pin.popdown();
        let Some(server_id) = page_for_pin.selected_server_id() else {
            return;
        };
        let pinned = !is_pinned.get();
        item.set_sensitive(false);
        let page = page_for_pin.clone();
        let name = name_for_pin.clone();
        let item = item.clone();
        let badge = badge_for_pin.clone();
        let is_pinned = is_pinned.clone();
        gtk::glib::spawn_future_local(async move {
            let result = {
                let name = name.clone();
                gtk::gio::spawn_blocking(move || {
                    crate::registry_controller::set_tool_pinned(&server_id, &name, pinned)
                })
                .await
            };
            item.set_sensitive(true);
            match result {
                Ok(Ok(_)) => {
                    page.remember_tool_pinned(&name, pinned);
                    is_pinned.set(pinned);
                    badge.set_visible(pinned);
                    if let Some(label) = item.child().and_downcast::<gtk::Label>() {
                        label.set_label(pin_label(pinned));
                    }
                }
                Ok(Err(error)) => page.show_error(&error),
                Err(_) => page.show_error("the pin update stopped unexpectedly"),
            }
        });
    });

    let page_for_exposure = page.clone();
    let name_for_exposure = name.clone();
    let description_for_exposure = description.clone();
    let popover_for_exposure = popover.clone();
    rename_item.connect_clicked(move |_| {
        popover_for_exposure.popdown();
        open_exposure_editor(
            &page_for_exposure,
            &name_for_exposure,
            &description_for_exposure,
            exposure_override.clone(),
        );
    });

    let name_for_run = name.clone();
    run.connect_clicked(move |_| {
        let name = name_for_run.clone();
        let title = format!("Run {name}");
        let tool_name = name.clone();
        open_json_action(&page, &title, &schema, move |server_id, arguments| {
            crate::playground::call_tool(&server_id, &tool_name, arguments)
        });
    });

    row.append(&visibility);
    row.append(&menu);
    row.append(&run);
    row
}

fn pin_label(pinned: bool) -> &'static str {
    if pinned {
        "Unpin from discovery"
    } else {
        "Pin for discovery"
    }
}

fn set_visibility_look(button: &gtk::ToggleButton, enabled: bool) {
    button.set_icon_name(if enabled {
        "view-reveal-symbolic"
    } else {
        "view-conceal-symbolic"
    });
    button.set_tooltip_text(Some(if enabled {
        "Exposed to clients. Hide it."
    } else {
        "Hidden from clients. Expose it."
    }));
}

fn open_exposure_editor(
    page: &ServerToolsPanel,
    tool: &str,
    original_description: &str,
    current: Option<crate::registry::ToolOverride>,
) {
    let Some(parent) = page.app.active_window() else {
        return;
    };
    let window = adw::Window::builder()
        .title(format!("Tool exposure: {tool}"))
        .transient_for(&parent)
        .modal(true)
        .default_width(560)
        .default_height(420)
        .build();
    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    root.set_margin_top(18);
    root.set_margin_bottom(18);
    root.set_margin_start(18);
    root.set_margin_end(18);
    root.append(
        &gtk::Label::builder()
            .label(
                "Change only what AI clients see. Calls still route to the server's original tool.",
            )
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-muted"])
            .build(),
    );
    let name = gtk::Entry::builder()
        .placeholder_text(tool)
        .text(
            current
                .as_ref()
                .and_then(|value| value.name.as_deref())
                .unwrap_or_default(),
        )
        .build();
    root.append(&super::editor_field("Exposed name", &name));
    let description = gtk::TextView::new();
    description.set_wrap_mode(gtk::WrapMode::WordChar);
    description.buffer().set_text(
        current
            .as_ref()
            .and_then(|value| value.description.as_deref())
            .unwrap_or_default(),
    );
    let description_scroll = gtk::ScrolledWindow::builder()
        .min_content_height(120)
        .vexpand(true)
        .child(&description)
        .build();
    root.append(&super::editor_field(
        "Replacement description",
        &description_scroll,
    ));
    root.append(
        &gtk::Label::builder()
            .label(format!("Original description: {original_description}"))
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["caption", "toolport-muted"])
            .build(),
    );
    let feedback = gtk::Label::builder()
        .halign(gtk::Align::Fill)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["toolport-feedback"])
        .build();
    root.append(&feedback);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let reset = gtk::Button::with_label("Reset to original");
    reset.add_css_class("destructive-action");
    reset.set_sensitive(current.is_some());
    reset.set_tooltip_text(Some(
        "Remove the override so clients see the server's original name and description",
    ));
    let save = gtk::Button::with_label("Save exposure");
    save.add_css_class("suggested-action");
    actions.append(&cancel);
    actions.append(&reset);
    actions.append(&save);
    root.append(&actions);
    window.set_content(Some(&root));
    let window_for_cancel = window.clone();
    cancel.connect_clicked(move |_| window_for_cancel.close());
    let Some(server_id) = page.selected_server_id() else {
        return;
    };
    let tool = tool.to_string();
    {
        let server_id = server_id.clone();
        let tool = tool.clone();
        let page = page.clone();
        let window = window.clone();
        reset.connect_clicked(move |button| {
            button.set_sensitive(false);
            let server_id = server_id.clone();
            let tool = tool.clone();
            let page = page.clone();
            let window = window.clone();
            gtk::glib::spawn_future_local(async move {
                let result = gtk::gio::spawn_blocking(move || {
                    crate::registry_controller::clear_tool_override(&server_id, &tool)
                })
                .await;
                match result {
                    Ok(Ok(_)) => {
                        window.close();
                        page.load_selected();
                    }
                    Ok(Err(error)) => page.show_error(&error),
                    Err(_) => page.show_error("the override reset stopped unexpectedly"),
                }
            });
        });
    }
    let page_for_save = page.clone();
    let window_for_save = window.clone();
    save.connect_clicked(move |button| {
        button.set_sensitive(false);
        let name = name.text().to_string();
        let buffer = description.buffer();
        let description = buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), true)
            .to_string();
        let server_id = server_id.clone();
        let tool = tool.clone();
        let page = page_for_save.clone();
        let window = window_for_save.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                crate::registry_controller::set_tool_override(
                    &server_id,
                    &tool,
                    Some(&name),
                    Some(&description),
                )
            })
            .await;
            match result {
                Ok(Ok(_)) => {
                    window.close();
                    page.load_selected();
                }
                Ok(Err(error)) => page.show_error(&error),
                Err(_) => page.show_error("the exposure update stopped unexpectedly"),
            }
        });
    });
    window.present();
}

fn resource_row(resource: serde_json::Value, page: ServerToolsPanel) -> gtk::Box {
    let uri = resource
        .get("uri")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let name = resource
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(&uri)
        .to_string();
    let uri_for_read = uri.clone();
    action_row(&name, &uri, "Read", move || {
        let Some(server_id) = page.selected_server_id() else {
            return;
        };
        let uri = uri_for_read.clone();
        run_output(&page, "Resource result", move || {
            crate::playground::read_resource(&server_id, &uri)
        });
    })
}

fn prompt_row(prompt: serde_json::Value, page: ServerToolsPanel) -> gtk::Box {
    let name = prompt
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("Unknown prompt")
        .to_string();
    let description = prompt
        .get("description")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("No description")
        .to_string();
    let name_for_get = name.clone();
    action_row(&name, &description, "Get", move || {
        let name = name_for_get.clone();
        let title = format!("Get {name}");
        let prompt_name = name.clone();
        open_json_action(
            &page,
            &title,
            &serde_json::json!({"type": "object"}),
            move |server_id, arguments| {
                crate::playground::get_prompt(&server_id, &prompt_name, arguments)
            },
        )
    })
}

fn action_row(
    title: &str,
    description: &str,
    action: &str,
    activate: impl Fn() + 'static,
) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.add_css_class("toolport-card");
    let copy = gtk::Box::new(gtk::Orientation::Vertical, 3);
    copy.set_hexpand(true);
    copy.append(
        &gtk::Label::builder()
            .label(title)
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["heading"])
            .build(),
    );
    copy.append(
        &gtk::Label::builder()
            .label(one_paragraph(description))
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .wrap(true)
            .lines(2)
            .max_width_chars(60)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["toolport-muted"])
            .build(),
    );
    row.append(&copy);
    let button = gtk::Button::with_label(action);
    button.add_css_class("toolport-secondary-action");
    button.set_valign(gtk::Align::Center);
    button.connect_clicked(move |_| activate());
    row.append(&button);
    row
}

/// How one primitive schema property is edited in the typed form.
#[derive(Debug, Clone, PartialEq)]
enum FieldKind {
    Text,
    Number,
    Integer,
    Boolean,
    Choice(Vec<String>),
}

#[derive(Debug, Clone, PartialEq)]
struct FormField {
    name: String,
    kind: FieldKind,
    required: bool,
    default: Option<serde_json::Value>,
    description: Option<String>,
}

/// Build a typed form description from a tool's input schema, or `None` when
/// any property is not a primitive - then the whole dialog falls back to the
/// raw JSON editor rather than half-rendering a form that cannot express the
/// arguments.
fn schema_form_fields(schema: &serde_json::Value) -> Option<Vec<FormField>> {
    let properties = schema.get("properties")?.as_object()?;
    if properties.is_empty() {
        return None;
    }
    let required: std::collections::HashSet<&str> = schema
        .get("required")
        .and_then(serde_json::Value::as_array)
        .map(|required| {
            required
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect()
        })
        .unwrap_or_default();
    let mut fields = Vec::new();
    for (name, property) in properties {
        let kind = if let Some(options) = property.get("enum").and_then(serde_json::Value::as_array)
        {
            let options: Option<Vec<String>> = options
                .iter()
                .map(|value| value.as_str().map(str::to_string))
                .collect();
            FieldKind::Choice(options?)
        } else {
            match property.get("type").and_then(serde_json::Value::as_str) {
                Some("string") => FieldKind::Text,
                Some("number") => FieldKind::Number,
                Some("integer") => FieldKind::Integer,
                Some("boolean") => FieldKind::Boolean,
                _ => return None,
            }
        };
        fields.push(FormField {
            name: name.clone(),
            kind,
            required: required.contains(name.as_str()),
            default: property.get("default").cloned(),
            description: property
                .get("description")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
        });
    }
    Some(fields)
}

/// Coerce one form field's raw text into its JSON value. `Ok(None)` means the
/// optional field was left blank and stays out of the arguments entirely.
fn form_field_value(field: &FormField, raw: &str) -> Result<Option<serde_json::Value>, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return if field.required {
            Err(format!("{} is required", field.name))
        } else {
            Ok(None)
        };
    }
    match &field.kind {
        FieldKind::Text | FieldKind::Choice(_) => {
            Ok(Some(serde_json::Value::String(raw.to_string())))
        }
        FieldKind::Integer => raw
            .parse::<i64>()
            .map(|value| Some(serde_json::Value::from(value)))
            .map_err(|_| format!("{} must be a whole number", field.name)),
        FieldKind::Number => raw
            .parse::<f64>()
            .map(|value| Some(serde_json::json!(value)))
            .map_err(|_| format!("{} must be a number", field.name)),
        FieldKind::Boolean => Err(format!(
            "{} is a boolean and should not reach text coercion",
            field.name
        )),
    }
}

/// One built form row: the field description plus the live widget to read.
enum FormWidget {
    Entry(FormField, gtk::Entry),
    Switch(FormField, gtk::Switch),
    Choice(FormField, gtk::DropDown, Vec<String>),
}

impl FormWidget {
    fn value(&self) -> Result<Option<(String, serde_json::Value)>, String> {
        match self {
            FormWidget::Entry(field, entry) => Ok(form_field_value(field, entry.text().as_str())?
                .map(|value| (field.name.clone(), value))),
            FormWidget::Switch(field, switch) => Ok(Some((
                field.name.clone(),
                serde_json::Value::Bool(switch.is_active()),
            ))),
            FormWidget::Choice(field, dropdown, options) => {
                let selected = dropdown.selected() as usize;
                if selected == 0 {
                    return if field.required {
                        Err(format!("{} is required", field.name))
                    } else {
                        Ok(None)
                    };
                }
                Ok(options.get(selected - 1).map(|option| {
                    (
                        field.name.clone(),
                        serde_json::Value::String(option.clone()),
                    )
                }))
            }
        }
    }
}

fn build_form_row(field: &FormField) -> (gtk::Box, FormWidget) {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let label = gtk::Label::builder()
        .label(if field.required {
            format!("{} (required)", field.name)
        } else {
            field.name.clone()
        })
        .halign(gtk::Align::Start)
        .css_classes(["heading", "caption"])
        .build();
    if let Some(description) = field.description.as_deref() {
        label.set_tooltip_text(Some(description));
    }
    row.append(&label);
    let widget = match &field.kind {
        FieldKind::Boolean => {
            let switch = gtk::Switch::builder()
                .halign(gtk::Align::Start)
                .active(
                    field
                        .default
                        .as_ref()
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                )
                .build();
            row.append(&switch);
            FormWidget::Switch(field.clone(), switch)
        }
        FieldKind::Choice(options) => {
            let mut items = vec!["(unset)"];
            items.extend(options.iter().map(String::as_str));
            let dropdown = gtk::DropDown::from_strings(&items);
            let default_index = field
                .default
                .as_ref()
                .and_then(serde_json::Value::as_str)
                .and_then(|default| options.iter().position(|option| option == default))
                .map(|index| index as u32 + 1)
                .unwrap_or(0);
            dropdown.set_selected(default_index);
            dropdown.add_css_class("toolport-input");
            row.append(&dropdown);
            FormWidget::Choice(field.clone(), dropdown, options.clone())
        }
        _ => {
            let entry = gtk::Entry::builder()
                .hexpand(true)
                .css_classes(["toolport-input"])
                .build();
            if let Some(default) = field.default.as_ref() {
                entry.set_text(
                    default
                        .as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| default.to_string())
                        .as_str(),
                );
            }
            if let Some(description) = field.description.as_deref() {
                entry.set_placeholder_text(Some(description));
            }
            row.append(&entry);
            FormWidget::Entry(field.clone(), entry)
        }
    };
    (row, widget)
}

/// Empty means hidden. `toolport-feedback` paints a background, so an empty
/// label leaves a blank pill sitting in the dialog.
fn set_dialog_status(feedback: &gtk::Label, message: &str, error: bool) {
    feedback.set_label(message);
    feedback.set_visible(!message.is_empty());
    if error {
        feedback.add_css_class("error");
    } else {
        feedback.remove_css_class("error");
    }
}

fn open_json_action(
    page: &ServerToolsPanel,
    title: &str,
    schema: &serde_json::Value,
    execute: impl FnOnce(String, serde_json::Value) -> Result<serde_json::Value, String>
        + Send
        + 'static,
) {
    let Some(parent) = page.app.active_window() else {
        return;
    };
    // No default height: the window sizes to its content, and the scrollers
    // below cap how far that can go. A fixed 540 left a one-field form sitting
    // above a screenful of nothing.
    let dialog = adw::Window::builder()
        .title(title)
        .transient_for(&parent)
        .modal(true)
        .default_width(560)
        .build();
    let shell = gtk::Box::new(gtk::Orientation::Vertical, 0);
    // The window title was set but never shown, because nothing here drew a
    // header bar the way the result dialog does.
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(
        &gtk::Label::builder()
            .label(title)
            .css_classes(["heading"])
            .build(),
    ));
    shell.append(&header);
    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    root.set_margin_top(18);
    root.set_margin_bottom(18);
    root.set_margin_start(18);
    root.set_margin_end(18);
    root.append(
        &gtk::Label::builder()
            .label("Arguments")
            .halign(gtk::Align::Start)
            .css_classes(["toolport-muted", "caption"])
            .build(),
    );

    // Typed form when the schema is all primitives; raw JSON otherwise, and as
    // an explicit opt-out either way.
    let fields = schema_form_fields(schema).unwrap_or_default();
    let form = gtk::Box::new(gtk::Orientation::Vertical, 10);
    let mut widgets: Vec<FormWidget> = Vec::new();
    for field in &fields {
        let (row, widget) = build_form_row(field);
        form.append(&row);
        widgets.push(widget);
    }
    let widgets = Rc::new(widgets);
    // propagate_natural_height plus a cap is what makes these size to their
    // content: min_content_height with vexpand demanded 150px and then took
    // every spare pixel, which is where the dead space came from.
    let form_scroll = gtk::ScrolledWindow::builder()
        .propagate_natural_height(true)
        .max_content_height(300)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&form)
        .build();
    let editor = gtk::TextView::new();
    editor.set_monospace(true);
    editor.set_wrap_mode(gtk::WrapMode::WordChar);
    editor.buffer().set_text("{}");
    let editor_scroll = gtk::ScrolledWindow::builder()
        .propagate_natural_height(true)
        .min_content_height(120)
        .max_content_height(300)
        .child(&editor)
        .build();
    let has_form = !widgets.is_empty();
    let json_mode = Rc::new(Cell::new(!has_form));
    form_scroll.set_visible(has_form);
    editor_scroll.set_visible(!has_form);
    root.append(&form_scroll);
    root.append(&editor_scroll);
    if has_form {
        let json_toggle = gtk::ToggleButton::with_label("Edit as JSON");
        json_toggle.add_css_class("flat");
        json_toggle.set_halign(gtk::Align::Start);
        let widgets_for_toggle = widgets.clone();
        let form_scroll = form_scroll.clone();
        let editor_scroll = editor_scroll.clone();
        let editor_for_toggle = editor.clone();
        let json_mode_for_toggle = json_mode.clone();
        json_toggle.connect_toggled(move |toggle| {
            let json = toggle.is_active();
            json_mode_for_toggle.set(json);
            form_scroll.set_visible(!json);
            editor_scroll.set_visible(json);
            if json {
                // Carry the current form values over so switching modes never
                // silently discards what was typed.
                let mut object = serde_json::Map::new();
                for widget in widgets_for_toggle.iter() {
                    if let Ok(Some((name, value))) = widget.value() {
                        object.insert(name, value);
                    }
                }
                let text = serde_json::to_string_pretty(&serde_json::Value::Object(object))
                    .unwrap_or_else(|_| "{}".into());
                editor_for_toggle.buffer().set_text(&text);
            }
        });
        root.append(&json_toggle);
    }
    // Kept, because it is the only place the raw contract is visible, but
    // collapsed: it was a wall of JSON taking half the dialog by default.
    let schema_text = serde_json::to_string_pretty(schema).unwrap_or_else(|_| "{}".into());
    let schema_view = gtk::ScrolledWindow::builder()
        .propagate_natural_height(true)
        .max_content_height(220)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(
            &gtk::Label::builder()
                .label(&schema_text)
                .halign(gtk::Align::Fill)
                .xalign(0.0)
                .selectable(true)
                .wrap(true)
                .css_classes(["caption", "toolport-muted", "monospace"])
                .build(),
        )
        .build();
    let schema_expander = gtk::Expander::builder()
        .label("Input schema")
        .child(&schema_view)
        .css_classes(["toolport-details-expander"])
        .build();
    root.append(&schema_expander);
    let feedback = gtk::Label::builder()
        .halign(gtk::Align::Fill)
        .xalign(0.0)
        .wrap(true)
        .visible(false)
        .css_classes(["toolport-feedback"])
        .build();
    root.append(&feedback);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let run = gtk::Button::with_label("Run");
    run.add_css_class("suggested-action");
    actions.append(&cancel);
    actions.append(&run);
    root.append(&actions);
    shell.append(&root);
    dialog.set_content(Some(&shell));
    // Cancelling mid-call abandons the wait (the server finishes on its own);
    // the completion handler checks this before touching the closed dialog.
    let cancelled = Rc::new(Cell::new(false));
    let dialog_for_cancel = dialog.clone();
    let cancelled_for_cancel = cancelled.clone();
    cancel.connect_clicked(move |_| {
        cancelled_for_cancel.set(true);
        dialog_for_cancel.close();
    });
    let Some(server_id) = page.selected_server_id() else {
        return;
    };
    let execute = Rc::new(RefCell::new(Some(execute)));
    let execute_for_run = execute.clone();
    let dialog_for_run = dialog.clone();
    let page_for_run = page.clone();
    run.connect_clicked(move |button| {
        let arguments = if json_mode.get() {
            let buffer = editor.buffer();
            let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
            match serde_json::from_str::<serde_json::Value>(&text) {
                Ok(serde_json::Value::Object(object)) => serde_json::Value::Object(object),
                Ok(_) => {
                    set_dialog_status(&feedback, "Arguments must be a JSON object.", true);
                    return;
                }
                Err(error) => {
                    set_dialog_status(&feedback, &format!("Invalid JSON: {error}"), true);
                    return;
                }
            }
        } else {
            let mut object = serde_json::Map::new();
            for widget in widgets.iter() {
                match widget.value() {
                    Ok(Some((name, value))) => {
                        object.insert(name, value);
                    }
                    Ok(None) => {}
                    Err(error) => {
                        set_dialog_status(&feedback, &error, true);
                        return;
                    }
                }
            }
            serde_json::Value::Object(object)
        };
        let Some(execute) = execute_for_run.borrow_mut().take() else {
            return;
        };
        button.set_sensitive(false);
        set_dialog_status(&feedback, "Calling… 0s", false);
        let started = std::time::Instant::now();
        let done = Rc::new(Cell::new(false));
        {
            let feedback = feedback.clone();
            let done = done.clone();
            gtk::glib::timeout_add_local(std::time::Duration::from_secs(1), move || {
                if done.get() {
                    return gtk::glib::ControlFlow::Break;
                }
                set_dialog_status(
                    &feedback,
                    &format!("Calling… {}s", started.elapsed().as_secs()),
                    false,
                );
                gtk::glib::ControlFlow::Continue
            });
        }
        let server_id = server_id.clone();
        let page = page_for_run.clone();
        let dialog = dialog_for_run.clone();
        let cancelled = cancelled.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || execute(server_id, arguments)).await;
            done.set(true);
            if cancelled.get() {
                return;
            }
            dialog.close();
            match result {
                Ok(Ok(value)) => show_output(&page, "Tools result", value),
                Ok(Err(error)) => page.show_error(&error),
                Err(_) => page.show_error("the tool call stopped unexpectedly"),
            }
        });
    });
    dialog.present();
}

fn run_output(
    page: &ServerToolsPanel,
    title: &str,
    operation: impl FnOnce() -> Result<serde_json::Value, String> + Send + 'static,
) {
    let page = page.clone();
    let title = title.to_string();
    gtk::glib::spawn_future_local(async move {
        match gtk::gio::spawn_blocking(operation).await {
            Ok(Ok(value)) => show_output(&page, &title, value),
            Ok(Err(error)) => page.show_error(&error),
            Err(_) => page.show_error("the tool operation stopped unexpectedly"),
        }
    });
}

fn show_output(page: &ServerToolsPanel, title: &str, value: serde_json::Value) {
    let Some(parent) = page.app.active_window() else {
        return;
    };
    let dialog = adw::Window::builder()
        .title(title)
        .transient_for(&parent)
        .modal(true)
        .default_width(720)
        .default_height(560)
        .build();
    let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = adw::HeaderBar::new();
    let copy = gtk::Button::with_label("Copy");
    copy.add_css_class("toolport-secondary-action");
    copy.set_tooltip_text(Some("Copy the full result to the clipboard"));
    {
        let text = text.clone();
        let copy_button = copy.clone();
        copy.connect_clicked(move |_| {
            if let Some(display) = gtk::gdk::Display::default() {
                display.clipboard().set_text(&text);
                copy_button.set_label("Copied");
            }
        });
    }
    header.pack_end(&copy);
    root.append(&header);
    let output = gtk::TextView::new();
    output.set_editable(false);
    output.set_cursor_visible(false);
    output.set_monospace(true);
    output.set_wrap_mode(gtk::WrapMode::WordChar);
    output.buffer().set_text(&text);
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&output)
        .build();
    root.append(&scroller);
    dialog.set_content(Some(&root));
    dialog.present();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A multi-paragraph tool description has to become one paragraph before
    /// the label's two-line cap can bind, because Pango limits lines per
    /// paragraph rather than per layout.
    #[test]
    fn multi_paragraph_descriptions_collapse_to_a_single_paragraph() {
        let description = "Prepare a direct Linear file upload.\n\nWorkflow:\n1. Call this tool.\n2. Upload raw bytes.\n\nExample:\n  curl -X PUT";
        let collapsed = one_paragraph(description);
        assert!(!collapsed.contains('\n'), "newlines survived: {collapsed}");
        assert!(
            !collapsed.contains("  "),
            "runs of spaces survived: {collapsed}"
        );
        assert!(collapsed.starts_with("Prepare a direct Linear file upload. Workflow:"));
        assert!(collapsed.ends_with("Example: curl -X PUT"));
    }

    #[test]
    fn a_single_line_description_is_left_alone() {
        assert_eq!(
            one_paragraph("Retrieve an attachment's content by ID."),
            "Retrieve an attachment's content by ID."
        );
    }

    #[test]
    fn primitive_schemas_become_typed_forms_and_complex_ones_fall_back() {
        let schema = serde_json::json!({
            "type": "object",
            "required": ["query"],
            "properties": {
                "query": {"type": "string", "description": "What to search"},
                "limit": {"type": "integer", "default": 10},
                "strict": {"type": "boolean"},
                "order": {"enum": ["asc", "desc"], "default": "desc"},
            }
        });
        let fields = schema_form_fields(&schema).unwrap();
        assert_eq!(fields.len(), 4);
        let query = fields.iter().find(|field| field.name == "query").unwrap();
        assert!(query.required && query.kind == FieldKind::Text);
        let order = fields.iter().find(|field| field.name == "order").unwrap();
        assert_eq!(
            order.kind,
            FieldKind::Choice(vec!["asc".to_string(), "desc".to_string()])
        );

        // One complex property sends the whole dialog to the JSON editor.
        let complex = serde_json::json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"},
                "filters": {"type": "object"},
            }
        });
        assert_eq!(schema_form_fields(&complex), None);
        assert_eq!(
            schema_form_fields(&serde_json::json!({"type": "object"})),
            None
        );
    }

    #[test]
    fn field_values_respect_required_blank_and_type_coercion() {
        let field = |name: &str, kind: FieldKind, required: bool| FormField {
            name: name.to_string(),
            kind,
            required,
            default: None,
            description: None,
        };
        let optional = field("limit", FieldKind::Integer, false);
        assert_eq!(form_field_value(&optional, "  "), Ok(None));
        assert_eq!(
            form_field_value(&optional, "25"),
            Ok(Some(serde_json::json!(25)))
        );
        assert!(form_field_value(&optional, "abc").is_err());
        let required = field("query", FieldKind::Text, true);
        assert!(form_field_value(&required, "").is_err());
        assert_eq!(
            form_field_value(&required, "hello"),
            Ok(Some(serde_json::json!("hello")))
        );
        let number = field("scale", FieldKind::Number, false);
        assert_eq!(
            form_field_value(&number, "1.5"),
            Ok(Some(serde_json::json!(1.5)))
        );
    }
}
