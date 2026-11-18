use super::i18n as l10n;
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
    /// The server's name, for "Starting {server}…" without a registry read on
    /// the UI thread.
    name: String,
    disabled: std::collections::HashSet<String>,
    pinned: std::collections::HashSet<String>,
    overrides: std::collections::HashMap<String, crate::registry::ToolOverride>,
    /// The global switch that hides destructive tools, as React reads it.
    deny_destructive: bool,
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
                .label(crate::i18n::tr(
                    "Calls follow the active profile and gateway policy, including approval gates.",
                ))
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
        l10n::bind_text(&feedback, "Choose a server to load its capabilities.");

        let tool_filter = {
            let widget = gtk::SearchEntry::builder()
                .placeholder_text(crate::i18n::tr("Filter tools"))
                .css_classes(["toolport-search"])
                .hexpand(true)
                .build();
            l10n::bind_placeholder(&widget, "Filter tools");
            widget
        };
        controls.append(&tool_filter);
        page.append(&controls);
        let view_stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(120)
            .build();
        let tools = capability_list();
        let resources = capability_list();
        let prompts = capability_list();
        l10n::bind_stack_page_title(
            &view_stack.add_titled(&tools, Some("tools"), &crate::i18n::tr("Tools")),
            "Tools",
        );
        l10n::bind_stack_page_title(
            &view_stack.add_titled(&resources, Some("resources"), &crate::i18n::tr("Resources")),
            "Resources",
        );
        l10n::bind_stack_page_title(
            &view_stack.add_titled(&prompts, Some("prompts"), &crate::i18n::tr("Prompts")),
            "Prompts",
        );
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
        self.set_status("");
        if self.last_load.borrow().is_none() {
            for list in [&self.tools, &self.resources, &self.prompts] {
                while let Some(child) = list.first_child() {
                    list.remove(&child);
                }
                list.append(&super::skeleton_rows(crate::i18n::msg(
                    "Connecting to server…",
                )));
            }
        }
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                let capabilities = crate::playground::capabilities(&server_id)?;
                let registry = crate::registry::load()?;
                let name = registry
                    .servers
                    .iter()
                    .find(|server| server.id == server_id)
                    .map_or_else(|| server_id.clone(), |server| server.name.clone());
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
                        name,
                        disabled,
                        pinned,
                        overrides,
                        deny_destructive: registry.deny_destructive,
                    },
                ))
            })
            .await;
            page.loading.set(false);
            match result {
                Ok(Ok((capabilities, policy))) => page.render(capabilities, policy),
                Ok(Err(error)) => page.show_error(&error),
                Err(_) => page.show_error(&crate::i18n::tr(
                    "the server connection stopped unexpectedly",
                )),
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
            self.resources.append(&empty_capability(crate::i18n::msg(
                "No resources advertised",
            )));
        }
        if prompt_count == 0 {
            self.prompts
                .append(&empty_capability(crate::i18n::msg("No prompts advertised")));
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
                crate::i18n::trf("Tools ({value0})", &[("value0", &format!("{}", tools))]),
                crate::i18n::trf(
                    "Resources ({value0})",
                    &[("value0", &format!("{}", resources))],
                ),
                crate::i18n::trf("Prompts ({value0})", &[("value0", &format!("{}", prompts))]),
            ],
            None => [
                crate::i18n::tr("Tools"),
                crate::i18n::tr("Resources"),
                crate::i18n::tr("Prompts"),
            ],
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
            self.tools
                .append(&empty_capability(crate::i18n::msg("No tools advertised")));
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
                policy.deny_destructive && hints_destructive(tool),
                self.clone(),
            ));
        }
        if shown == 0 {
            self.tools.append(&empty_capability(crate::i18n::msg(
                "No tool matches the filter",
            )));
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
        if self.last_load.borrow().is_none() {
            for list in [&self.tools, &self.resources, &self.prompts] {
                while let Some(child) = list.first_child() {
                    list.remove(&child);
                }
                let page = self.clone();
                list.append(&super::error_callout(
                    crate::i18n::msg("Tools"),
                    error,
                    move || page.refresh(),
                ));
            }
            self.feedback.set_visible(false);
            return;
        }

        self.set_status(&crate::i18n::trf(
            "Tools error: {value0}",
            &[("value0", error)],
        ));
        self.feedback.add_css_class("error");
    }
}

fn capability_list() -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, 10)
}

fn empty_capability(message: &'static str) -> gtk::Box {
    super::state_card(
        "view-list-symbolic",
        crate::i18n::msg("Nothing here yet"),
        message,
        false,
    )
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

fn state_badge(text: &'static str) -> gtk::Label {
    let badge = gtk::Label::builder()
        .label(text)
        .css_classes(["toolport-badge", "caption"])
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Center)
        .build();
    l10n::bind_text(&badge, text);
    badge
}

fn insert_state_badge(badges: &gtk::FlowBox, badge: &gtk::Label) {
    badges.insert(badge, -1);
    // FlowBox's wrapper must hide with its label or it reserves an empty column.
    let child = badge.parent().unwrap();
    child.set_visible(badge.is_visible());
    let child = child.downgrade();
    badge.connect_visible_notify(move |badge| {
        if let Some(child) = child.upgrade() {
            child.set_visible(badge.is_visible());
        }
    });
}

/// A tool that says it is destructive, by the rule React's tool list uses.
fn hints_destructive(tool: &serde_json::Value) -> bool {
    [
        tool.pointer("/annotations/destructiveHint"),
        tool.get("destructiveHint"),
    ]
    .into_iter()
    .any(|hint| hint.and_then(serde_json::Value::as_bool) == Some(true))
}

/// `blocked`: the global switch hides this destructive tool whatever its own
/// switch says.
fn tool_row(
    tool: serde_json::Value,
    enabled: bool,
    pinned: bool,
    exposure_override: Option<crate::registry::ToolOverride>,
    blocked: bool,
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
    let title = gtk::Box::new(gtk::Orientation::Vertical, 3);
    title.append(
        &gtk::Label::builder()
            .label(&name)
            .tooltip_text(&name)
            .halign(gtk::Align::Fill)
            .hexpand(true)
            .xalign(0.0)
            .max_width_chars(48)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["heading"])
            .build(),
    );
    let badges = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .min_children_per_line(1)
        .max_children_per_line(5)
        .column_spacing(4)
        .row_spacing(4)
        .halign(gtk::Align::Start)
        .build();
    let pinned_badge = state_badge(crate::i18n::msg("Pinned"));
    pinned_badge.set_visible(pinned);
    insert_state_badge(&badges, &pinned_badge);
    let renamed_badge = state_badge(crate::i18n::msg("Renamed"));
    renamed_badge.set_visible(exposure_override.is_some());
    insert_state_badge(&badges, &renamed_badge);
    for (hint, label) in [
        ("readOnlyHint", crate::i18n::msg("Read-only")),
        ("destructiveHint", crate::i18n::msg("Destructive")),
    ] {
        if tool
            .pointer(&format!("/annotations/{hint}"))
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        {
            badges.insert(&state_badge(label), -1);
        }
    }
    let enabled_badge = state_badge(if enabled {
        crate::i18n::msg("On")
    } else {
        crate::i18n::msg("Off")
    });
    l10n::bind_text(
        &enabled_badge,
        if enabled {
            crate::i18n::msg("On")
        } else {
            crate::i18n::msg("Off")
        },
    );
    enabled_badge.add_css_class(if enabled { "success" } else { "off" });
    badges.insert(&enabled_badge, -1);
    if tool
        .get("toolportQuarantine")
        .and_then(serde_json::Value::as_str)
        != Some("clear")
    {
        let badge = state_badge(
            if tool
                .get("toolportQuarantine")
                .and_then(serde_json::Value::as_str)
                == Some("quarantined")
            {
                crate::i18n::msg("Quarantined")
            } else {
                crate::i18n::msg("Quarantine unknown")
            },
        );
        badge.add_css_class("warning");
        badges.insert(&badge, -1);
    }
    title.append(&badges);
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
    let run = l10n::button("Run");
    run.add_css_class("toolport-secondary-action");
    run.set_valign(gtk::Align::Center);
    run.set_sensitive(enabled);

    let visibility = gtk::Switch::builder()
        .active(enabled)
        .valign(gtk::Align::Center)
        .build();
    visibility.update_property(&[gtk::accessible::Property::Label(&name)]);
    set_visibility_look(&visibility, enabled, blocked);
    // Reverting a failed write re-enters this handler, so the guard keeps the
    // revert from being treated as a fresh toggle.
    let reverting = Rc::new(Cell::new(false));
    let page_for_visibility = page.clone();
    let name_for_visibility = name.clone();
    let run_for_visibility = run.clone();
    let badge_for_visibility = enabled_badge.clone();
    visibility.connect_active_notify(move |button| {
        if reverting.get() {
            return;
        }
        let Some(server_id) = page_for_visibility.selected_server_id() else {
            return;
        };
        let enabled = button.is_active();
        button.set_sensitive(false);
        let page = page_for_visibility.clone();
        let name = name_for_visibility.clone();
        let button = button.clone();
        let run = run_for_visibility.clone();
        let badge = badge_for_visibility.clone();
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
                    set_visibility_look(&button, enabled, blocked);
                    badge.remove_css_class(if enabled { "off" } else { "success" });
                    badge.add_css_class(if enabled { "success" } else { "off" });
                    run.set_sensitive(enabled);
                    l10n::bind_text(
                        &badge,
                        if enabled {
                            crate::i18n::msg("On")
                        } else {
                            crate::i18n::msg("Off")
                        },
                    );
                }
                Ok(Err(error)) => {
                    // The registry rejected the change, so the control must not
                    // keep claiming it happened.
                    reverting.set(true);
                    button.set_active(!enabled);
                    reverting.set(false);
                    page.show_error(&error);
                }
                Err(_) => {
                    reverting.set(true);
                    button.set_active(!enabled);
                    reverting.set(false);
                    page.show_error(&crate::i18n::tr(
                        "the tool visibility update stopped unexpectedly",
                    ));
                }
            }
        });
    });

    let actions = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let pin_item = super::toolport_menu_button(&crate::i18n::lookup(pin_label(pinned)));
    let rename_item = super::toolport_menu_button("Rename for clients\u{2026}");
    actions.append(&pin_item);
    actions.append(&rename_item);
    let popover = super::toolport_menu_popover();
    popover.set_child(Some(&actions));
    let menu = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .tooltip_text(crate::i18n::trf(
            "More actions for {value0}",
            &[("value0", &name.to_string())],
        ))
        .popover(&popover)
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    super::quiet_tooltips::guard_menu(&menu);

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
                        label.set_label(&crate::i18n::lookup(pin_label(pinned)));
                    }
                }
                Ok(Err(error)) => page.show_error(&error),
                Err(_) => page.show_error(&crate::i18n::tr("the pin update stopped unexpectedly")),
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
        let title = crate::i18n::trf("Run {value0}", &[("value0", &name.to_string())]);
        let tool_name = name.clone();
        // A Run the dialog can cancel, and watch while the server starts.
        let tool_run = crate::playground::ToolRun::new();
        let run_for_call = tool_run.clone();
        open_json_action(
            &page,
            &title,
            &schema,
            Some(tool_run),
            move |server_id, arguments| {
                crate::playground::call_tool_run(
                    &server_id,
                    &tool_name,
                    arguments,
                    &run_for_call,
                    |_| {},
                )
            },
        );
    });

    row.append(&visibility);
    row.append(&menu);
    row.append(&run);
    row
}

fn pin_label(pinned: bool) -> &'static str {
    if pinned {
        crate::i18n::msg("Unpin from discovery")
    } else {
        crate::i18n::msg("Pin for discovery")
    }
}

fn set_visibility_look(button: &gtk::Switch, enabled: bool, blocked: bool) {
    button.set_tooltip_text(Some(&if blocked {
        // A click changes only the tool's own setting while the switch hides it.
        crate::i18n::tr(
            "Destructive tools are hidden from clients by the global switch even when individually enabled.",
        )
    } else if enabled {
        crate::i18n::tr("Exposed to clients. Hide it.")
    } else {
        crate::i18n::tr("Hidden from clients. Expose it.")
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
        .title(crate::i18n::trf(
            "Tool exposure: {value0}",
            &[("value0", tool)],
        ))
        .transient_for(&parent)
        .modal(true)
        .default_width(560)
        .default_height(420)
        .build();
    super::close_on_escape(&window);
    let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
    root.set_margin_top(18);
    root.set_margin_bottom(18);
    root.set_margin_start(18);
    root.set_margin_end(18);
    root.append(
        &gtk::Label::builder()
            .label(crate::i18n::tr(
                "Change only what AI clients see. Calls still route to the server's original tool.",
            ))
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
    root.append(&super::editor_field(
        crate::i18n::msg("Exposed name"),
        &name,
    ));
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
        crate::i18n::msg("Replacement description"),
        &description_scroll,
    ));
    root.append(&{
        let widget = gtk::Label::builder()
            .label("")
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["caption", "toolport-muted"])
            .build();
        l10n::bind_formatted_label(
            &widget,
            "Original description: {value0}",
            &[("value0", original_description)],
        );
        widget
    });
    let feedback = gtk::Label::builder()
        .halign(gtk::Align::Fill)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["toolport-feedback"])
        .build();
    root.append(&feedback);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    let cancel = l10n::button("Cancel");
    let reset = l10n::button("Reset to original");
    reset.add_css_class("toolport-destructive");
    reset.set_sensitive(current.is_some());
    reset.set_tooltip_text(Some(&crate::i18n::tr(
        "Remove the override so clients see the server's original name and description",
    )));
    let save = l10n::button("Save exposure");
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
                    Err(_) => {
                        page.show_error(&crate::i18n::tr("the override reset stopped unexpectedly"))
                    }
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
                Err(_) => {
                    page.show_error(&crate::i18n::tr("the exposure update stopped unexpectedly"))
                }
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
        let title = crate::i18n::trf("Get {value0}", &[("value0", &name.to_string())]);
        let prompt_name = name.clone();
        open_json_action(
            &page,
            &title,
            &serde_json::json!({"type": "object"}),
            None,
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
            Err(crate::i18n::trf(
                "{value0} is required",
                &[("value0", &field.name.to_string())],
            ))
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
            .map_err(|_| {
                crate::i18n::trf(
                    "{value0} must be a whole number",
                    &[("value0", &field.name.to_string())],
                )
            }),
        FieldKind::Number => raw
            .parse::<f64>()
            .map(|value| Some(serde_json::json!(value)))
            .map_err(|_| {
                crate::i18n::trf(
                    "{value0} must be a number",
                    &[("value0", &field.name.to_string())],
                )
            }),
        FieldKind::Boolean => Err(crate::i18n::trf(
            "{value0} is a boolean and should not reach text coercion",
            &[("value0", &field.name.to_string())],
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
                        Err(crate::i18n::trf(
                            "{value0} is required",
                            &[("value0", &field.name.to_string())],
                        ))
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
            crate::i18n::trf("{field} (required)", &[("field", field.name.as_str())])
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
            super::quiet_tooltips::guard_dropdown(&dropdown);
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
    tool_run: Option<crate::playground::ToolRun>,
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
    super::close_on_escape(&dialog);
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
    root.append(&{
        let widget = gtk::Label::builder()
            .label(crate::i18n::tr("Arguments"))
            .halign(gtk::Align::Start)
            .css_classes(["toolport-muted", "caption"])
            .build();
        l10n::bind_label(&widget, "Arguments");
        widget
    });

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
        let json_toggle = gtk::ToggleButton::new();
        l10n::bind_button(json_toggle.upcast_ref(), "Edit as JSON");
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
        .label(crate::i18n::tr("Input schema"))
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
    let cancel = l10n::button("Cancel");
    let run = l10n::button("Run");
    run.add_css_class("suggested-action");
    actions.append(&cancel);
    actions.append(&run);
    root.append(&actions);
    shell.append(&root);
    dialog.set_content(Some(&shell));
    // Cancelling mid-call cancels a tool Run in the gateway, including a wait
    // for the server to start; other actions only stop waiting. The completion
    // handler checks this before touching the closed dialog.
    let cancelled = Rc::new(Cell::new(false));
    let cancelled_on_close = cancelled.clone();
    let run_on_close = tool_run.clone();
    dialog.connect_close_request(move |_| {
        cancelled_on_close.set(true);
        if let Some(run) = &run_on_close {
            run.cancel();
        }
        gtk::glib::Propagation::Proceed
    });
    let dialog_for_cancel = dialog.clone();
    let cancelled_for_cancel = cancelled.clone();
    cancel.connect_clicked(move |_| {
        cancelled_for_cancel.set(true);
        dialog_for_cancel.close();
    });
    let Some(server_id) = page.selected_server_id() else {
        return;
    };
    let server_name = tool_run.as_ref().map(|_| {
        page.last_load
            .borrow()
            .as_ref()
            .map_or_else(|| server_id.clone(), |(_, policy)| policy.name.clone())
    });
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
                    set_dialog_status(
                        &feedback,
                        &crate::i18n::tr("Arguments must be a JSON object."),
                        true,
                    );
                    return;
                }
                Err(error) => {
                    set_dialog_status(
                        &feedback,
                        &crate::i18n::trf(
                            "Invalid JSON: {value0}",
                            &[("value0", &format!("{}", error))],
                        ),
                        true,
                    );
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
        set_dialog_status(&feedback, &crate::i18n::tr("Calling… 0s"), false);
        let started = std::time::Instant::now();
        let done = Rc::new(Cell::new(false));
        {
            let feedback = feedback.clone();
            let done = done.clone();
            let tool_run = tool_run.clone();
            let server_name = server_name.clone();
            // A quarter second, so "Starting" shows as soon as the gateway says
            // so; the text changes only once a second.
            gtk::glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
                if done.get() {
                    return gtk::glib::ControlFlow::Break;
                }
                let seconds = started.elapsed().as_secs().to_string();
                let text = match (&tool_run, &server_name) {
                    (Some(run), Some(name)) if run.is_starting() => crate::i18n::trf(
                        "Starting {value0}… {value1}s",
                        &[("value0", name), ("value1", &seconds)],
                    ),
                    _ => crate::i18n::trf("Calling… {seconds}s", &[("seconds", &seconds)]),
                };
                if feedback.label() != text {
                    set_dialog_status(&feedback, &text, false);
                }
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
                Ok(Ok(value)) => show_call_result(&page, value),
                Ok(Err(error)) => page.show_error(&error),
                Err(_) => page.show_error(&crate::i18n::tr("the tool call stopped unexpectedly")),
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
            Err(_) => page.show_error(&crate::i18n::tr("the tool operation stopped unexpectedly")),
        }
    });
}

/// A Run's result, shown as the React Tools tab shows it: what the tool
/// returned first, with Toolport's provenance metadata left out of the way.
fn show_call_result(page: &ServerToolsPanel, value: serde_json::Value) {
    if value.get("content").is_none() {
        // Not a tool result (a prompt's messages): show it whole.
        return show_output(page, &crate::i18n::tr("Result"), value);
    }
    let title = if value.get("isError").and_then(serde_json::Value::as_bool) == Some(true) {
        crate::i18n::tr("Tool returned an error")
    } else {
        crate::i18n::tr("Result")
    };
    show_text(page, &title, render_tool_result(&value));
}

/// The text of a tool result's content blocks, the same as React's
/// `renderResult`: text blocks as written, other blocks as JSON, and the whole
/// result as JSON when it has no content.
fn render_tool_result(value: &serde_json::Value) -> String {
    let mut value = value.clone();
    strip_provenance(&mut value);
    let value = &value;
    let blocks = value
        .get("content")
        .and_then(serde_json::Value::as_array)
        .filter(|blocks| !blocks.is_empty());
    let Some(blocks) = blocks else {
        return serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    };
    blocks
        .iter()
        .map(
            |block| match (block["type"].as_str(), block["text"].as_str()) {
                (Some("text"), Some(text)) => text.to_string(),
                _ => serde_json::to_string_pretty(block).unwrap_or_else(|_| block.to_string()),
            },
        )
        .collect::<Vec<_>>()
        .join("\n")
}

/// Remove Toolport's provenance label from every `_meta` in a result, keeping
/// the server's own `_meta` fields. React's `renderResult` does the same.
fn strip_provenance(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(object) => {
            if let Some(serde_json::Value::Object(meta)) = object.get_mut("_meta") {
                meta.remove("app.toolport/provenance");
                if meta.is_empty() {
                    object.remove("_meta");
                }
            }
            object.values_mut().for_each(strip_provenance);
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(strip_provenance),
        _ => {}
    }
}

fn show_output(page: &ServerToolsPanel, title: &str, mut value: serde_json::Value) {
    strip_provenance(&mut value);
    let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
    show_text(page, title, text);
}

fn show_text(page: &ServerToolsPanel, title: &str, text: String) {
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
    super::close_on_escape(&dialog);
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = adw::HeaderBar::new();
    let copy = l10n::button("Copy");
    copy.add_css_class("toolport-secondary-action");
    copy.set_tooltip_text(Some(&crate::i18n::tr(
        "Copy the full result to the clipboard",
    )));
    {
        let text = text.clone();
        let copy_button = copy.clone();
        copy.connect_clicked(move |_| {
            if let Some(display) = gtk::gdk::Display::default() {
                display.clipboard().set_text(&text);
                l10n::bind_text(&copy_button, "Copied");
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
    output.set_left_margin(12);
    output.set_right_margin(12);
    output.set_top_margin(12);
    output.set_bottom_margin(12);
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

    /// GTK shows a Run's result as React's `renderResult` does: the text the
    /// tool returned, without Toolport's provenance metadata.
    #[test]
    fn a_run_result_shows_what_the_tool_returned() {
        let meta =
            serde_json::json!({"app.toolport/provenance": {"server": "s", "trust": "untrusted"}});
        let result = serde_json::json!({
            "_meta": meta,
            "content": [
                {"type": "text", "text": "hello", "_meta": meta},
                {"type": "image", "data": "AAA", "mimeType": "image/png"},
            ],
            "isError": false,
        });
        let text = render_tool_result(&result);
        assert!(text.starts_with("hello\n{"), "{text}");
        assert!(!text.contains("provenance"), "{text}");
        assert!(text.contains("image/png"), "{text}");
        // No content: the whole result, as React shows it.
        let empty = serde_json::json!({"content": [], "structuredContent": {"n": 1}});
        assert!(render_tool_result(&empty).contains("structuredContent"));
        // Provenance goes from every block and the fallback; the server's own
        // `_meta` fields stay.
        let labelled =
            serde_json::json!({"app.toolport/provenance": {"trust": "untrusted"}, "own": 7});
        let mixed = serde_json::json!({
            "content": [
                {"type": "image", "data": "AAA", "_meta": labelled},
                {"type": "resource", "resource": {"uri": "a:b", "_meta": meta}},
            ],
        });
        let text = render_tool_result(&mixed);
        assert!(!text.contains("provenance"), "{text}");
        assert!(text.contains("\"own\": 7"), "{text}");
        assert!(!text.contains("\"_meta\": {}"), "{text}");
        let fallback = serde_json::json!({"_meta": labelled, "content": []});
        let text = render_tool_result(&fallback);
        assert!(!text.contains("provenance"), "{text}");
        assert!(text.contains("\"own\": 7"), "{text}");
    }

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn b4_dismissed_tool_action_does_not_show_a_late_result() {
        use gtk::glib::translate::IntoGlib;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        fn pump(duration: std::time::Duration) {
            let main = gtk::glib::MainLoop::new(None, false);
            let stop = main.clone();
            gtk::glib::timeout_add_local_once(duration, move || stop.quit());
            main.run();
        }
        fn wait_until(mut ready: impl FnMut() -> bool) {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while !ready() && std::time::Instant::now() < deadline {
                pump(std::time::Duration::from_millis(10));
            }
            assert!(ready(), "GTK operation did not complete");
        }
        fn window(title: &str) -> Option<gtk::Window> {
            gtk::Window::list_toplevels()
                .into_iter()
                .filter_map(|widget| widget.downcast::<gtk::Window>().ok())
                .find(|window| window.get_visible() && window.title().as_deref() == Some(title))
        }
        fn button(widget: &gtk::Widget, label: &str) -> Option<gtk::Button> {
            if let Some(button) = widget.downcast_ref::<gtk::Button>() {
                if button.label().as_deref() == Some(label) {
                    return Some(button.clone());
                }
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                if let Some(found) = button(&widget, label) {
                    return Some(found);
                }
                child = widget.next_sibling();
            }
            None
        }

        adw::init().unwrap();
        let app = adw::Application::builder()
            .application_id("com.tsout.Toolport.B4ToolDismissal")
            .build();
        app.register(None::<&gtk::gio::Cancellable>).unwrap();
        let parent = adw::ApplicationWindow::builder()
            .application(&app)
            .title("B4 tool dismissal fixture")
            .build();
        parent.present();
        wait_until(|| app.active_window().is_some());
        let panel = ServerToolsPanel::new(&app, "synthetic-server");
        for dismissal in ["success", "escape", "cancel", "window-close"] {
            let (release, gate) = std::sync::mpsc::sync_channel(1);
            let started = Arc::new(AtomicBool::new(false));
            let returned = Arc::new(AtomicBool::new(false));
            let did_start = started.clone();
            let did_return = returned.clone();
            open_json_action(
                &panel,
                "B4 pending tool",
                &serde_json::json!({"type": "object"}),
                None,
                move |_, _| {
                    did_start.store(true, Ordering::SeqCst);
                    gate.recv_timeout(std::time::Duration::from_secs(3))
                        .unwrap();
                    did_return.store(true, Ordering::SeqCst);
                    Ok(serde_json::json!({"synthetic": "completed"}))
                },
            );
            let dialog = window("B4 pending tool").expect("action dialog");
            button(dialog.upcast_ref(), "Run").unwrap().emit_clicked();
            wait_until(|| started.load(Ordering::SeqCst));
            match dismissal {
                "escape" => {
                    let controllers = dialog.observe_controllers();
                    let keys = (0..controllers.n_items())
                        .filter_map(|index| controllers.item(index))
                        .find_map(|item| item.downcast::<gtk::EventControllerKey>().ok())
                        .unwrap();
                    let handled: bool = keys.emit_by_name(
                        "key-pressed",
                        &[
                            &gtk::gdk::Key::Escape.into_glib(),
                            &0u32,
                            &gtk::gdk::ModifierType::empty(),
                        ],
                    );
                    assert!(handled);
                }
                "cancel" => button(dialog.upcast_ref(), "Cancel")
                    .unwrap()
                    .emit_clicked(),
                "window-close" => dialog.close(),
                _ => {}
            }
            if dismissal != "success" {
                assert!(!dialog.get_visible(), "the pending dialog must close");
            }
            release.send(()).unwrap();
            wait_until(|| returned.load(Ordering::SeqCst));
            if dismissal == "success" {
                wait_until(|| window("Result").is_some());
                window("Result").unwrap().close();
            } else {
                pump(std::time::Duration::from_millis(250));
                assert!(window("Result").is_none(), "late result after {dismissal}");
            }
        }
        parent.close();
    }

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
    #[test]
    fn destructive_tools_are_the_ones_react_marks() {
        use super::*;
        let tool = |value: serde_json::Value| hints_destructive(&value);
        assert!(tool(
            serde_json::json!({"annotations": {"destructiveHint": true}})
        ));
        assert!(tool(serde_json::json!({"destructiveHint": true})));
        assert!(!tool(
            serde_json::json!({"annotations": {"destructiveHint": false}})
        ));
        assert!(!tool(
            serde_json::json!({"annotations": {"readOnlyHint": true}})
        ));
    }

    /// While the global switch hides a destructive tool, its switch's tooltip
    /// says so whichever way it is set, as in React.
    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn a_tool_hidden_by_the_destructive_switch_says_so() {
        use super::*;
        adw::init().unwrap();
        let _data = crate::registry::DataDirTestEnv::new("tool-blocked-tooltip");
        crate::i18n::set_language("en").unwrap();
        let app = adw::Application::builder()
            .application_id("com.tsout.Toolport.BlockedToolTooltip")
            .build();
        app.register(gtk::gio::Cancellable::NONE).unwrap();
        let panel = ServerToolsPanel::new(&app, "fixture");
        let tooltip = |enabled: bool, blocked: bool| {
            let row = tool_row(
                serde_json::json!({"name":"remove", "toolportQuarantine":"clear", "annotations":{"destructiveHint":true}}),
                enabled,
                false,
                None,
                blocked,
                panel.clone(),
            );
            let mut stack = vec![row.upcast::<gtk::Widget>()];
            while let Some(widget) = stack.pop() {
                if let Some(switch) = widget.downcast_ref::<gtk::Switch>() {
                    return switch.tooltip_text().unwrap().to_string();
                }
                let mut child = widget.first_child();
                while let Some(node) = child {
                    child = node.next_sibling();
                    stack.push(node);
                }
            }
            panic!("no switch");
        };
        let blocked = "Destructive tools are hidden from clients by the global switch even when individually enabled.";
        assert_eq!(tooltip(true, true), blocked);
        assert_eq!(tooltip(false, true), blocked);
        assert_eq!(tooltip(true, false), "Exposed to clients. Hide it.");
        assert_eq!(tooltip(false, false), "Hidden from clients. Expose it.");
    }

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn b5_static_tool_badges_retranslate_live_without_touching_user_names() {
        use super::*;
        adw::init().unwrap();
        let _data = crate::registry::DataDirTestEnv::new("b5-badge-language");
        crate::i18n::set_language("en").unwrap();
        let app = adw::Application::builder()
            .application_id("com.tsout.Toolport.B5BadgeTest")
            .build();
        app.register(gtk::gio::Cancellable::NONE).unwrap();
        let panel = ServerToolsPanel::new(&app, "fixture");
        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        let row = |state: &str| {
            tool_row(
                serde_json::json!({"name":"Quarantined", "description":"User description", "toolportQuarantine":state, "annotations":{"readOnlyHint":true,"destructiveHint":true}}),
                true,
                true,
                None,
                false,
                panel.clone(),
            )
        };
        let quarantined = row("quarantined");
        let unknown = row("unknown");
        let clear = row("clear");
        for r in [&quarantined, &unknown, &clear] {
            root.append(r);
        }
        let renamed = state_badge(crate::i18n::msg("Renamed"));
        root.append(&renamed);
        fn labels(w: &gtk::Widget) -> Vec<gtk::Label> {
            let mut out = w
                .downcast_ref::<gtk::Label>()
                .cloned()
                .into_iter()
                .collect::<Vec<_>>();
            let mut child = w.first_child();
            while let Some(c) = child {
                out.extend(labels(&c));
                child = c.next_sibling();
            }
            out
        }
        let warning = |w: &gtk::Box| {
            labels(w.upcast_ref())
                .into_iter()
                .filter(|l| l.has_css_class("warning"))
                .collect::<Vec<_>>()
        };
        assert_eq!(warning(&quarantined)[0].text(), "Quarantined");
        assert_eq!(warning(&unknown)[0].text(), "Quarantine unknown");
        assert!(warning(&clear).is_empty());
        crate::i18n::set_language("fr").unwrap();
        l10n::refresh_bindings();
        assert_eq!(warning(&quarantined)[0].text(), "Mis en quarantaine");
        assert_eq!(warning(&unknown)[0].text(), "Quarantaine inconnue");
        assert!(warning(&clear).is_empty());
        assert_eq!(renamed.text(), "Renommé");
        let text = labels(quarantined.upcast_ref())
            .into_iter()
            .map(|l| l.text().to_string())
            .collect::<Vec<_>>();
        for expected in [
            "Quarantined",
            "Épinglé",
            "Lecture seule",
            "Destructeur",
            "Activé",
        ] {
            assert!(text.iter().any(|t| t == expected), "{expected}: {text:?}");
        }
        crate::i18n::set_language("en").unwrap();
        l10n::refresh_bindings();
        assert_eq!(warning(&quarantined)[0].text(), "Quarantined");
        if std::env::var_os("TOOLPORT_B5_BADGE_CAPTURE").is_some() {
            root.set_margin_top(24);
            root.set_margin_bottom(24);
            root.set_margin_start(24);
            root.set_margin_end(24);
            let window = adw::ApplicationWindow::builder()
                .application(&app)
                .title("Toolport B5 badge fixture")
                .content(&root)
                .default_width(1440)
                .default_height(900)
                .build();
            window.add_css_class("toolport-native");
            let theme = super::super::theme::ThemeController::new();
            theme.attach(&window);
            for (action_name, language) in [("capture-french", "fr"), ("capture-english", "en")] {
                let action = gtk::gio::SimpleAction::new(action_name, None);
                action.connect_activate(move |_, _| {
                    crate::i18n::set_language(language).unwrap();
                    l10n::refresh_bindings();
                });
                app.add_action(&action);
            }
            let close = gtk::gio::SimpleAction::new("close-fixture", None);
            let target = window.downgrade();
            close.connect_activate(move |_, _| {
                if let Some(w) = target.upgrade() {
                    w.close();
                }
            });
            app.add_action(&close);
            let main_loop = gtk::glib::MainLoop::new(None, false);
            let stop = main_loop.clone();
            window.connect_close_request(move |_| {
                stop.quit();
                gtk::glib::Propagation::Proceed
            });
            let stop = main_loop.clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_secs(120), move || {
                stop.quit()
            });
            window.present();
            main_loop.run();
            window.close();
        }
    }

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn b5_tool_switch_saves_exact_policy_and_rolls_back_failed_writes() {
        use super::*;
        adw::init().unwrap();
        let _data = crate::registry::DataDirTestEnv::new("b5-tool-switch");
        crate::registry::update(|registry| {
            registry.servers.push(serde_json::from_value(serde_json::json!({
                "id":"selected", "name":"Selected", "transport":"stdio", "command":"fixture", "args":[], "env":[]
            })).unwrap());
            Ok(())
        }).unwrap();
        let app = adw::Application::builder()
            .application_id("com.tsout.Toolport.B5ToolSwitchTest")
            .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(gtk::gio::Cancellable::NONE).unwrap();
        let panel = ServerToolsPanel::new(&app, "selected");
        let tool = serde_json::json!({"name":"read", "description":"Read a record", "toolportQuarantine":"clear"});
        let row = tool_row(tool.clone(), true, false, None, false, panel.clone());
        fn find_switch(root: &gtk::Widget) -> Option<gtk::Switch> {
            if let Some(switch) = root.downcast_ref::<gtk::Switch>() {
                return Some(switch.clone());
            }
            let mut child = root.first_child();
            while let Some(widget) = child {
                if let Some(switch) = find_switch(&widget) {
                    return Some(switch);
                }
                child = widget.next_sibling();
            }
            None
        }
        fn settle(switch: &gtk::Switch) {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let context = gtk::glib::MainContext::default();
            while !switch.is_sensitive() && std::time::Instant::now() < deadline {
                while context.pending() {
                    context.iteration(false);
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert!(switch.is_sensitive(), "policy update completed");
        }
        let switch = find_switch(row.upcast_ref()).unwrap();
        assert!(switch.is_active());
        switch.set_active(false);
        settle(&switch);
        assert!(!switch.is_active());
        let registry = crate::registry::load().unwrap();
        assert_eq!(registry.servers[0].disabled_tools, vec!["read"]);
        let failed_panel = ServerToolsPanel::new(&app, "missing");
        let failed_row = tool_row(tool, true, false, None, false, failed_panel.clone());
        let failed_switch = find_switch(failed_row.upcast_ref()).unwrap();
        failed_switch.set_active(false);
        settle(&failed_switch);
        assert!(
            failed_switch.is_active(),
            "a rejected policy write restores the old state"
        );
        assert_eq!(
            failed_panel.tools.first_child().unwrap().accessible_role(),
            gtk::AccessibleRole::Alert
        );
        assert_eq!(
            crate::registry::load().unwrap().servers[0].disabled_tools,
            vec!["read"]
        );
    }
}
