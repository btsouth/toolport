use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;

/// Autostart entry name. The preview used its own so it could not repoint the
/// shipping shell's login launch while both were installable; the GTK shell now
/// replaces that shell on Linux, so the names have merged as planned.
/// [`super::migrate_preview_identity`] retires the old file.
pub(super) const NATIVE_AUTOSTART_NAME: &str = "Toolport";
/// What the preview wrote. Only referenced by the migration.
pub(super) const LEGACY_AUTOSTART_NAME: &str = "ToolportNativePreview";

pub(super) const SETTINGS_PAGES: &[(&str, &str)] = &[
    ("General", "emblem-system-symbolic"),
    ("Tools", "applications-engineering-symbolic"),
    ("Safety", "security-high-symbolic"),
    ("Access", "changes-prevent-symbolic"),
    ("Connections", "network-server-symbolic"),
    ("Help and data", "help-browser-symbolic"),
];

#[derive(Clone)]
pub(super) struct SettingsPage {
    pub(super) root: gtk::Box,
    pages: Vec<gtk::Box>,
    safety_cards: Vec<gtk::CheckButton>,
    pub(super) stop_stale: gtk::Button,
    bridge: super::http_bridge::BridgeController,
    broker: crate::approval_broker::ApprovalBroker,
    feedback: gtk::Label,
    posture: gtk::Label,
    safety_level: gtk::DropDown,
    safety_floor: Rc<Cell<crate::registry::SafetyLevel>>,
    safety_policy: gtk::Label,
    safety_kept: gtk::Box,
    safety_kept_note: gtk::Label,
    safety_kept_reset: gtk::Button,
    safety_current: Rc<Cell<crate::registry::SafetyLevel>>,
    lazy_discovery: gtk::Switch,
    pinned_section: gtk::Box,
    pinned_list: gtk::Box,
    code_mode: gtk::Switch,
    live_inspect: gtk::Switch,
    pii_redaction: gtk::Switch,
    launch_at_login: gtk::Switch,
    endpoint_status: gtk::Label,
    endpoint_button: gtk::Button,
    copy_endpoint: gtk::Button,
    copy_endpoint_token: gtk::Button,
    reveal_endpoint_token: gtk::ToggleButton,
    endpoint_token_value: gtk::Label,
    restart_list: gtk::Box,
    http_client_list: gtk::Box,
    add_http_client: gtk::Button,
    access_list: gtk::Box,
    folder_list: gtk::Box,
    folder_button: gtk::Button,
    quarantine_list: gtk::Box,
    allowed_list: gtk::Box,
    refresh_button: gtk::Button,
    /// True only while switches are being set programmatically, so their
    /// `state-set` handlers know not to treat it as a user action.
    updating: Rc<Cell<bool>>,
    /// Re-entrancy guard for a refresh in flight. Was `updating`, which
    /// `render_settings` clears at its end, releasing the guard mid-refresh.
    refreshing: Rc<Cell<bool>>,
    /// Bumped by every settings mutation. A refresh whose read began before the
    /// bump discards its snapshot rather than rendering a value the user has
    /// since changed: a background tick landing after a toggle used to put the
    /// old value straight back into the switch.
    mutation_generation: Rc<Cell<u64>>,
}

impl SettingsPage {
    pub(super) fn new(
        bridge: super::http_bridge::BridgeController,
        broker: crate::approval_broker::ApprovalBroker,
    ) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("toolport-content");
        let header = adw::HeaderBar::new();
        header.add_css_class("toolport-header");
        header.set_show_back_button(true);
        header.set_title_widget(Some(
            &gtk::Label::builder()
                .label("Settings")
                .css_classes(["title"])
                .build(),
        ));
        let refresh_button = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Refresh settings")
            .build();
        header.pack_end(&refresh_button);
        root.append(&header);

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
        let feedback = gtk::Label::builder()
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .hexpand(true)
            .css_classes(["toolport-feedback"])
            .build();
        // Visibility and the four-second expiry both follow the text, so every
        // writer on this page gets them for free: an empty bar can never sit
        // there as a blank painted strip, and a confirmation cannot stay up
        // indefinitely. Only messages styled as confirmations expire, matching
        // Catalog, where only `show_success` starts a timer.
        let feedback_timer: Rc<RefCell<Option<gtk::glib::SourceId>>> = Rc::new(RefCell::new(None));
        {
            let slot = feedback_timer.clone();
            feedback.connect_notify_local(Some("label"), move |label, _| {
                label.set_visible(!label.label().is_empty());
                // Cancel the outgoing timer first, so an older callback cannot
                // clear a message that has since been replaced.
                if let Some(timer) = slot.borrow_mut().take() {
                    timer.remove();
                }
                if label.label().is_empty() {
                    return;
                }
                let label = label.clone();
                let inner = slot.clone();
                let timer = gtk::glib::timeout_add_local_once(
                    std::time::Duration::from_secs(4),
                    move || {
                        // Released before clearing, because clearing re-enters
                        // this handler and it takes the same slot.
                        inner.borrow_mut().take();
                        // Only confirmations expire. Errors stay put, and so
                        // do progress lines like "Loading settings…", which
                        // carry neither class and would otherwise vanish
                        // mid-operation.
                        if label.has_css_class("success") {
                            label.set_label("");
                        }
                    },
                );
                slot.borrow_mut().replace(timer);
            });
        }
        feedback.set_label("");
        feedback.set_visible(false);
        page.append(&feedback);

        // Security posture in one line, before the individual switches, so the
        // overall stance is legible without reading every toggle.
        let posture = gtk::Label::builder()
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .hexpand(true)
            .visible(false)
            .css_classes(["toolport-feedback"])
            .build();

        let capabilities = gtk::Box::new(gtk::Orientation::Vertical, 0);
        capabilities.add_css_class("toolport-settings-group");
        let (lazy_row, lazy_discovery) = setting_switch_row(
            "Find tools as needed",
            "Agents search for tools as needed instead of loading the full list. This is the default for connections without saved client settings. Choose a different behavior in Clients.",
        );
        capabilities.append(&lazy_row);
        let (code_row, code_mode) = setting_switch_row(
            "Code mode",
            "Let agents combine several tool calls in one script to reduce back-and-forth. Each call follows your access and approval settings. Scripts run in a restricted environment, but this does not replace those settings.",
        );
        code_row.set_tooltip_text(Some("A gateway started with TOOLPORT_CODE_MODE=1 can keep scripts available even when this setting is off."));

        let pinned_section = gtk::Box::new(gtk::Orientation::Vertical, 8);
        pinned_section.append(
            &gtk::Label::builder()
                .label("Tools always included")
                .halign(gtk::Align::Start)
                .css_classes(["heading"])
                .build(),
        );
        pinned_section.append(
            &gtk::Label::builder()
                .label("Tools pinned in Servers > Tools are included in every tool search, even when they do not match the search.")
                .halign(gtk::Align::Fill)
                .xalign(0.0)
                .wrap(true)
            .hexpand(true)
                .css_classes(["toolport-muted"])
                .build(),
        );
        let pinned_list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        pinned_list.add_css_class("toolport-settings-group");
        pinned_list.append(
            &gtk::Label::builder()
                .label("Checking pinned tools…")
                .halign(gtk::Align::Start)
                .css_classes(["toolport-muted"])
                .build(),
        );
        pinned_section.append(&pinned_list);

        let safety = gtk::Box::new(gtk::Orientation::Vertical, 0);
        safety.add_css_class("toolport-settings-group");
        let safety_level = gtk::DropDown::from_strings(&["Off", "Ask", "Strict"]);
        safety_level.set_tooltip_text(Some("Ask holds destructive calls. Strict also blocks destructive tools, risky drift and high-confidence injection, and asks before untrusted calls. Labeling and integrity recording stay on."));
        safety_level.set_visible(false);
        safety.append(&safety_level);
        let cards = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let mut safety_cards: Vec<gtk::CheckButton> = Vec::new();
        for (index, (name, description)) in [
            ("Off", "No extra checks before destructive calls."),
            ("Ask", "Pause destructive calls for your approval."),
            ("Strict", "Hide destructive tools and ask before untrusted calls."),
        ].into_iter().enumerate() {
            let card = gtk::CheckButton::new();
            if let Some(first) = safety_cards.first() { card.set_group(Some(first)); }
            card.set_hexpand(true);
            card.add_css_class("toolport-safety-card");
            let copy = gtk::Box::new(gtk::Orientation::Vertical, 8);
            copy.append(&gtk::Label::builder().label(name).xalign(0.0).css_classes(["heading"]).build());
            copy.append(&gtk::Label::builder().label(description).xalign(0.0).wrap(true).max_width_chars(24).css_classes(["toolport-muted"]).build());
            card.set_child(Some(&copy));
            let control = safety_level.clone();
            card.connect_toggled(move |card| {
                if card.is_active() && card.is_sensitive() && control.is_sensitive() {
                    // The model contains only levels at or above the team floor.
                    let count = control.model().map(|model| model.n_items()).unwrap_or(3);
                    control.set_selected((index as u32).saturating_sub(3 - count));
                }
            });
            cards.append(&card);
            safety_cards.push(card);
        }
        safety.append(&cards);
        let safety_policy = gtk::Label::new(None);
        safety_policy.set_xalign(0.0);
        safety_policy.set_wrap(true);
        safety_policy.add_css_class("dim-label");
        safety.append(&safety_policy);
        let safety_kept = gtk::Box::new(gtk::Orientation::Vertical, 6);
        safety_kept.set_margin_top(8);
        let safety_kept_note = gtk::Label::new(None);
        safety_kept_note.set_xalign(0.0);
        safety_kept_note.set_wrap(true);
        safety_kept.append(&safety_kept_note);
        let safety_kept_reset = gtk::Button::builder()
            .halign(gtk::Align::Start)
            .build();
        safety_kept.append(&safety_kept_reset);
        safety_kept.set_visible(false);
        safety.append(&safety_kept);
        let protection = gtk::Box::new(gtk::Orientation::Vertical, 0);
        protection.add_css_class("toolport-settings-group");
        capabilities.append(&code_row);
        let (pii_row, pii_redaction) = setting_switch_row(
            "Pseudonymize PII",
            "Replace detected personal values before results reach the model.",
        );
        protection.append(&pii_row);
        let (inspect_row, live_inspect) = setting_switch_row(
            "Live request/response inspection",
            "Capture the last 50 tool calls locally for Activity. Turning this off clears the buffer.",
        );
        protection.append(&inspect_row);
        let approvals_heading = settings_heading(
            "Remembered approvals",
            "Tool calls you chose to always allow. They stop applying if the tool's definition changes.",
        );
        let allowed_list = gtk::Box::new(gtk::Orientation::Vertical, 8);
        allowed_list.add_css_class("toolport-settings-group");
        allowed_list.append(
            &gtk::Label::builder()
                .label("Checking remembered approvals…")
                .halign(gtk::Align::Start)
                .css_classes(["toolport-muted"])
                .build(),
        );

        let access_list = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let folder_button = gtk::Button::with_label("Add folder mapping");
        folder_button.add_css_class("toolport-secondary-action");
        folder_button.set_valign(gtk::Align::Center);
        let folder_heading = settings_heading_with_action(
            "Project folder routing",
            "Automatically use the matching access set when an MCP client reports a project root. The longest matching folder wins.",
            &folder_button,
        );
        let folder_list = gtk::Box::new(gtk::Orientation::Vertical, 8);
        folder_list.add_css_class("toolport-settings-group");
        folder_list.append(
            &gtk::Label::builder()
                .label("Checking folder mappings…")
                .halign(gtk::Align::Start)
                .css_classes(["toolport-muted"])
                .build(),
        );

        let desktop = gtk::Box::new(gtk::Orientation::Vertical, 0);
        desktop.add_css_class("toolport-settings-group");
        let (launch_row, launch_at_login) = setting_switch_row(
            "Launch at login",
            "Start Toolport hidden so approvals and notifications remain available.",
        );
        let stale_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        stale_row.add_css_class("toolport-setting-row");
        let stale_copy = gtk::Box::new(gtk::Orientation::Vertical, 3);
        stale_copy.set_hexpand(true);
        stale_copy.append(
            &gtk::Label::builder()
                .label("Old gateway processes")
                .halign(gtk::Align::Start)
                .css_classes(["heading"])
                .build(),
        );
        stale_copy.append(
            &gtk::Label::builder()
                .label("Stop gateways left behind by an upgrade without interrupting the current endpoint.")
                .halign(gtk::Align::Fill)
                .xalign(0.0)
                .wrap(true)
            .hexpand(true)
                .css_classes(["toolport-muted"])
                .build(),
        );
        stale_row.append(&stale_copy);
        let stop_stale = gtk::Button::with_label("Stop old gateways");
        stop_stale.add_css_class("toolport-secondary-action");
        stale_row.append(&stop_stale);
        desktop.append(&stale_row);
        // The durable view of which apps still spawn a superseded gateway. A
        // transient feedback line is not enough: the user acts on this list
        // app by app, possibly minutes later.
        let restart_list = gtk::Box::new(gtk::Orientation::Vertical, 4);
        restart_list.set_visible(false);
        desktop.append(&restart_list);
        let updates = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        updates.add_css_class("toolport-setting-row");
        let updates_copy = gtk::Box::new(gtk::Orientation::Vertical, 3);
        updates_copy.set_hexpand(true);
        updates_copy.append(
            &gtk::Label::builder()
                .label("Updates")
                .halign(gtk::Align::Start)
                .css_classes(["heading"])
                .build(),
        );
        let update_advice = gtk::Label::builder()
            .label(super::package_updates::generic_advice())
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .hexpand(true)
            .css_classes(["toolport-muted"])
            .build();
        updates_copy.append(&update_advice);
        gtk::glib::spawn_future_local(async move {
            if let Ok(advice) =
                gtk::gio::spawn_blocking(super::package_updates::update_advice).await
            {
                update_advice.set_label(advice);
            }
        });
        updates_copy.append(
            &gtk::LinkButton::builder()
                .label("Open release page")
                .uri(super::package_updates::RELEASE_PAGE)
                .halign(gtk::Align::Start)
                .build(),
        );
        updates.append(&updates_copy);

        let diagnostics = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        diagnostics.add_css_class("toolport-setting-row");
        let diagnostics_copy = gtk::Box::new(gtk::Orientation::Vertical, 3);
        diagnostics_copy.set_hexpand(true);
        diagnostics_copy.append(
            &gtk::Label::builder()
                .label("Support and local data")
                .halign(gtk::Align::Start)
                .css_classes(["heading"])
                .build(),
        );
        diagnostics_copy.append(
            &gtk::Label::builder()
                .label("Diagnostics redact secrets. The data folder contains your registry, audit, and gateway logs.")
                .halign(gtk::Align::Fill)
                .xalign(0.0)
                .wrap(true)
            .hexpand(true)
                .css_classes(["toolport-muted"])
                .build(),
        );
        diagnostics.append(&diagnostics_copy);
        let diagnostics_actions = action_wrap();
        let copy_diagnostics = gtk::Button::with_label("Copy diagnostics");
        copy_diagnostics.add_css_class("toolport-secondary-action");
        diagnostics_actions.insert(&copy_diagnostics, -1);
        let open_data = gtk::Button::with_label("Open data folder");
        open_data.add_css_class("toolport-secondary-action");
        diagnostics_actions.insert(&open_data, -1);
        diagnostics.append(&diagnostics_actions);

        let endpoint_heading = settings_heading(
            "Shared HTTP endpoint",
            "A supervised, authenticated local endpoint for clients that cannot launch an MCP process.",
        );
        let endpoint = gtk::Box::new(gtk::Orientation::Vertical, 8);
        endpoint.add_css_class("toolport-setting-row");
        let endpoint_copy = gtk::Box::new(gtk::Orientation::Vertical, 3);
        endpoint_copy.set_hexpand(true);
        endpoint_copy.append(
            &gtk::Label::builder()
                .label("Local HTTP gateway")
                .halign(gtk::Align::Start)
                .css_classes(["heading"])
                .build(),
        );
        let endpoint_status = gtk::Label::builder()
            .label("Checking endpoint…")
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .hexpand(true)
            .selectable(true)
            .css_classes(["toolport-muted"])
            .build();
        endpoint_copy.append(&endpoint_status);
        endpoint.append(&endpoint_copy);
        let endpoint_button = gtk::Button::with_label("Start");
        endpoint_button.add_css_class("toolport-secondary-action");
        let endpoint_actions = action_wrap();
        let copy_endpoint = gtk::Button::with_label("Copy URL");
        copy_endpoint.add_css_class("toolport-secondary-action");
        copy_endpoint.set_sensitive(false);
        let copy_endpoint_token = gtk::Button::with_label("Copy token");
        copy_endpoint_token.add_css_class("toolport-secondary-action");
        copy_endpoint_token.set_sensitive(false);
        copy_endpoint_token.set_tooltip_text(Some("Copy the private administrator bearer token"));
        let reveal_endpoint_token = gtk::ToggleButton::with_label("Show token");
        reveal_endpoint_token.add_css_class("toolport-secondary-action");
        reveal_endpoint_token.set_sensitive(false);
        reveal_endpoint_token.set_tooltip_text(Some(
            "Reveal the administrator bearer token on screen; hide it again with the same button",
        ));
        endpoint_actions.insert(&copy_endpoint, -1);
        endpoint_actions.insert(&copy_endpoint_token, -1);
        endpoint_actions.insert(&reveal_endpoint_token, -1);
        endpoint_actions.insert(&endpoint_button, -1);
        endpoint.append(&endpoint_actions);
        let endpoint_token_value = gtk::Label::builder()
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .hexpand(true)
            .selectable(true)
            .visible(false)
            .css_classes(["toolport-muted", "caption", "monospace"])
            .build();
        endpoint.append(&endpoint_token_value);
        // The scoped-client list had no heading of its own, so its empty state
        // floated under the gateway row with nothing naming it.
        let add_http_client = gtk::Button::with_label("Add scoped HTTP client");
        add_http_client.add_css_class("toolport-secondary-action");
        add_http_client.set_valign(gtk::Align::Center);
        add_http_client.set_sensitive(false);
        let http_client_heading = settings_heading_with_action(
            "Scoped HTTP clients",
            "Each gets its own bearer token and server scope, so one endpoint can serve several clients.",
            &add_http_client,
        );
        let http_client_list = gtk::Box::new(gtk::Orientation::Vertical, 8);
        http_client_list.add_css_class("toolport-settings-group");
        http_client_list.append(
            &gtk::Label::builder()
                .label("Start the endpoint to manage scoped clients.")
                .halign(gtk::Align::Fill)
                .xalign(0.0)
                .wrap(true)
                .hexpand(true)
                .css_classes(["toolport-muted"])
                .build(),
        );

        let quarantine_heading = settings_heading(
            "Quarantined tools",
            "Strict blocks retained high-risk definition changes until you explicitly re-approve them.",
        );
        let quarantine_list = gtk::Box::new(gtk::Orientation::Vertical, 8);
        quarantine_list.add_css_class("toolport-settings-group");
        quarantine_list.append(
            &gtk::Label::builder()
                .label("Checking retained quarantines…")
                .halign(gtk::Align::Fill)
                .xalign(0.0)
                .wrap(true)
                .hexpand(true)
                .css_classes(["toolport-muted"])
                .build(),
        );

        let remove_clients = gtk::Button::with_label("Remove Toolport from all clients");
        remove_clients.set_halign(gtk::Align::Start);
        remove_clients.add_css_class("destructive-action");
        let removal_results = gtk::Label::new(None);
        removal_results.set_xalign(0.0);
        removal_results.set_wrap(true);

        let general = settings_section(&page, "General", "Startup and updates.");
        let startup = gtk::Box::new(gtk::Orientation::Vertical, 0);
        startup.add_css_class("toolport-settings-group");
        startup.append(&launch_row);
        startup.append(&updates);
        general.append(&startup);
        let tools = settings_section(&page, "Tools", "How agents find and use your tools.");
        tools.append(&capabilities);
        tools.append(&pinned_section);
        let safety_section = settings_section(&page, "Safety", "How Toolport handles risky tool calls.");
        safety_section.append(&posture);
        safety_section.append(&safety);
        safety_section.append(&protection);
        let pending = gtk::Label::builder().label("Pending approvals appear in the approval queue.").xalign(0.0).wrap(true).css_classes(["toolport-muted"]).build();
        safety_section.append(&pending);
        safety_section.append(&listed_section(&approvals_heading, &allowed_list));
        safety_section.append(&listed_section(&quarantine_heading, &quarantine_list));
        let access = settings_section(&page, "Access", "Limit which servers and tools each client can use, by client or project folder.");
        access.append(&access_list);
        access.append(&folder_heading);
        access.append(&folder_list);
        access.append(&http_client_heading);
        access.append(&http_client_list);
        let connections = settings_section(&page, "Connections", "Your local HTTP endpoint and gateway processes.");
        connections.append(&endpoint_heading);
        connections.append(&endpoint);
        connections.append(&desktop);
        let help = settings_section(&page, "Help and data", "Support reports, local files and removing Toolport.");
        help.append(&diagnostics);
        help.append(&remove_clients);
        help.append(&removal_results);
        let pages = vec![general, tools, safety_section, access, connections, help];
        for (index, section) in pages.iter().enumerate() { section.set_visible(index == 0); }
        scroller.set_child(Some(&page));
        root.append(&scroller);
        let settings_page = Self {
            stop_stale: stop_stale.clone(),
            root,
            pages,
            safety_cards,
            bridge,
            broker,
            feedback,
            posture,
            safety_level,
            safety_floor: Rc::new(Cell::new(crate::registry::SafetyLevel::Off)),
            safety_policy,
            safety_kept,
            safety_kept_note,
            safety_kept_reset,
            safety_current: Rc::new(Cell::new(crate::registry::SafetyLevel::Off)),
            lazy_discovery,
            pinned_section,
            pinned_list,
            code_mode,
            live_inspect,
            pii_redaction,
            launch_at_login,
            endpoint_status,
            endpoint_button,
            copy_endpoint: copy_endpoint.clone(),
            copy_endpoint_token: copy_endpoint_token.clone(),
            reveal_endpoint_token: reveal_endpoint_token.clone(),
            endpoint_token_value: endpoint_token_value.clone(),
            restart_list: restart_list.clone(),
            http_client_list,
            add_http_client,
            access_list,
            folder_list,
            folder_button,
            quarantine_list,
            allowed_list,
            refresh_button,
            updating: Rc::new(Cell::new(false)),
            refreshing: Rc::new(Cell::new(false)),
            mutation_generation: Rc::new(Cell::new(0)),
        };
        settings_page.connect_switches();
        let remove_page = settings_page.clone();
        remove_clients.connect_clicked(move |button| {
            let parent = remove_page.root.root().and_downcast::<gtk::Window>();
            let dialog = adw::MessageDialog::new(parent.as_ref(), Some("Remove Toolport from all clients?"), Some("Unchanged configs return to their original bytes. Your edits are preserved and moved entries are restored. Each client result is reported here."));
            dialog.add_response("cancel", "Cancel");
            dialog.add_response("remove", "Remove from all clients");
            dialog.set_close_response("cancel");
            dialog.set_default_response(Some("cancel"));
            dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
            let page = remove_page.clone();
            let results_label = removal_results.clone();
            let button = button.clone();
            dialog.connect_response(None, move |dialog, response| {
                if response == "remove" {
                    button.set_sensitive(false);
                    let page = page.clone();
                    let results_label = results_label.clone();
                    let button = button.clone();
                    gtk::glib::spawn_future_local(async move {
                        match gtk::gio::spawn_blocking(|| crate::clients::disconnect_all(false)).await {
                            Ok(Ok(results)) => {
                                let message = if results.is_empty() { "No client connections to remove.".into() } else { results.iter().map(|result| format!("{}: {}", result.client_id, result.error.clone().unwrap_or_else(|| std::iter::once("Client configuration restored".to_string()).chain(result.warnings.iter().cloned()).collect::<Vec<_>>().join("; ")))).collect::<Vec<_>>().join("\n") };
                                results_label.set_label(&message);
                                page.begin_mutation();
                                page.refresh_quietly();
                            }
                            Ok(Err(error)) => page.show_error(&error),
                            Err(_) => page.show_error("Client removal stopped unexpectedly"),
                        }
                        button.set_sensitive(true);
                    });
                }
                dialog.close();
            });
            dialog.present();
        });
        let page_for_endpoint = settings_page.clone();
        settings_page
            .endpoint_button
            .connect_clicked(move |_| page_for_endpoint.toggle_endpoint());
        let page_for_http_client = settings_page.clone();
        settings_page
            .add_http_client
            .connect_clicked(move |_| page_for_http_client.open_http_client_editor());
        let page_for_refresh = settings_page.clone();
        settings_page
            .refresh_button
            .connect_clicked(move |_| page_for_refresh.refresh());
        let page_for_folder = settings_page.clone();
        settings_page
            .folder_button
            .connect_clicked(move |_| page_for_folder.choose_folder_mapping());
        let page_for_copy = settings_page.clone();
        copy_diagnostics.connect_clicked(move |button| {
            button.set_sensitive(false);
            page_for_copy.feedback.set_label("Preparing diagnostics…");
            let page = page_for_copy.clone();
            let button = button.clone();
            gtk::glib::spawn_future_local(async move {
                let result = gtk::gio::spawn_blocking(crate::diagnostics_controller::gather).await;
                button.set_sensitive(true);
                match result {
                    Ok(text) => {
                        if let Some(display) = gtk::gdk::Display::default() {
                            display.clipboard().set_text(&text);
                            page.feedback.set_label("Copied secret-safe diagnostics.");
                            page.feedback.remove_css_class("error");
                            page.feedback.add_css_class("success");
                        } else {
                            page.show_error("could not access the desktop clipboard");
                        }
                    }
                    Err(_) => page.show_error("the diagnostics task stopped unexpectedly"),
                }
            });
        });
        let page_for_data = settings_page.clone();
        open_data.connect_clicked(move |button| {
            button.set_sensitive(false);
            let page = page_for_data.clone();
            let button = button.clone();
            gtk::glib::spawn_future_local(async move {
                let result =
                    gtk::gio::spawn_blocking(crate::diagnostics_controller::open_data_dir).await;
                button.set_sensitive(true);
                match result {
                    Ok(Ok(())) => {
                        page.feedback.set_label("Opened the Toolport data folder.");
                        page.feedback.remove_css_class("error");
                        page.feedback.add_css_class("success");
                    }
                    Ok(Err(error)) => page.show_error(&error),
                    Err(_) => page.show_error("the file manager task stopped unexpectedly"),
                }
            });
        });
        let page_for_url = settings_page.clone();
        copy_endpoint.connect_clicked(move |_| {
            let status = page_for_url.bridge.status();
            let Some(url) = status.url else {
                page_for_url.show_error("start the Shared HTTP endpoint first");
                return;
            };
            if let Some(display) = gtk::gdk::Display::default() {
                display.clipboard().set_text(&format!("{url}/mcp"));
                page_for_url
                    .feedback
                    .set_label("Copied the Shared HTTP URL.");
                page_for_url.feedback.remove_css_class("error");
                page_for_url.feedback.add_css_class("success");
            }
        });
        let page_for_token = settings_page.clone();
        copy_endpoint_token.connect_clicked(move |_| {
            let status = page_for_token.bridge.status();
            let Some(token) = status.token else {
                page_for_token.show_error("start the Shared HTTP endpoint first");
                return;
            };
            if let Some(display) = gtk::gdk::Display::default() {
                display.clipboard().set_text(&token);
                page_for_token
                    .feedback
                    .set_label("Copied the administrator bearer token. Keep it private.");
                page_for_token.feedback.remove_css_class("error");
                page_for_token.feedback.add_css_class("success");
            }
        });
        let page_for_reveal = settings_page.clone();
        reveal_endpoint_token.connect_toggled(move |toggle| {
            if toggle.is_active() {
                let status = page_for_reveal.bridge.status();
                page_for_reveal
                    .endpoint_token_value
                    .set_label(status.token.as_deref().unwrap_or(""));
                page_for_reveal.endpoint_token_value.set_visible(true);
                toggle.set_label("Hide token");
            } else {
                page_for_reveal.endpoint_token_value.set_label("");
                page_for_reveal.endpoint_token_value.set_visible(false);
                toggle.set_label("Show token");
            }
        });
        let page_for_stale = settings_page.clone();
        stop_stale.connect_clicked(move |button| {
            button.set_sensitive(false);
            page_for_stale.feedback.set_label("Checking old gateways…");
            let page = page_for_stale.clone();
            let button = button.clone();
            let bridge = page_for_stale.bridge.clone();
            gtk::glib::spawn_future_local(async move {
                let result = gtk::gio::spawn_blocking(move || bridge.stop_stale_gateways()).await;
                button.set_sensitive(true);
                match result {
                    Ok(outcome) if !outcome.failed.is_empty() => page.show_error(&format!(
                        "Could not stop every old gateway: {}",
                        outcome.failed.join("; ")
                    )),
                    Ok(outcome) if !outcome.needs_restart.is_empty() => {
                        let clients = outcome
                            .needs_restart
                            .iter()
                            .map(|client| client.client.as_str())
                            .collect::<Vec<_>>()
                            .join(", ");
                        page.feedback.set_label(&format!(
                            "Stopped {} old gateway process(es). Restart: {clients}.",
                            outcome.killed.len()
                        ));
                        page.feedback.remove_css_class("error");
                        page.feedback.add_css_class("success");
                    }
                    Ok(outcome) => {
                        page.feedback.set_label(if outcome.killed.is_empty() {
                            "No old gateway processes found."
                        } else {
                            "Old gateway processes stopped."
                        });
                        page.feedback.remove_css_class("error");
                        page.feedback.add_css_class("success");
                    }
                    Err(_) => page.show_error("the gateway cleanup task stopped unexpectedly"),
                }
                page.render_endpoint(page.bridge.status());
            });
        });
        settings_page
    }

    fn toggle_endpoint(&self) {
        self.endpoint_button.set_sensitive(false);
        let running = self.bridge.status().running;
        self.feedback.set_label(if running {
            "Stopping Shared HTTP endpoint…"
        } else {
            "Starting Shared HTTP endpoint…"
        });
        let bridge = self.bridge.clone();
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                if running {
                    bridge.stop()
                } else {
                    bridge.start(None)
                }
            })
            .await;
            page.endpoint_button.set_sensitive(true);
            match result {
                Ok(Ok(status)) => {
                    page.render_endpoint(status);
                    page.feedback.set_label(if running {
                        "Shared HTTP endpoint stopped"
                    } else {
                        "Shared HTTP endpoint started"
                    });
                    page.feedback.remove_css_class("error");
                    page.feedback.add_css_class("success");
                }
                Ok(Err(error)) => page.show_error(&error),
                Err(_) => page.show_error("the endpoint operation stopped unexpectedly"),
            }
        });
    }

    fn render_access_sets(&self, registry: crate::registry::Registry) {
        while let Some(child) = self.access_list.first_child() {
            self.access_list.remove(&child);
        }
        self.access_list.append(&settings_heading("Access sets", "Narrow enabled servers and tools per client. Servers that are off are hidden everywhere."));
        self.access_list.append(
            &gtk::Label::builder()
                .label("Default access")
                .halign(gtk::Align::Start)
                .css_classes(["heading"])
                .build(),
        );
        let mut labels = vec!["All enabled servers".to_string()];
        labels.extend(registry.profiles.iter().map(|p| p.name.clone()));
        let default =
            gtk::DropDown::from_strings(&labels.iter().map(String::as_str).collect::<Vec<_>>());
        default.set_tooltip_text(Some(
            "Default access for clients without an explicit access set",
        ));
        default.set_selected(
            registry
                .profiles
                .iter()
                .position(|p| Some(&p.id) == registry.default_access_profile_id.as_ref())
                .map(|i| i as u32 + 1)
                .unwrap_or(0),
        );
        let profiles = registry.profiles.clone();
        let page = self.clone();
        default.connect_selected_notify(move |dropdown| {
            let profile = dropdown
                .selected()
                .checked_sub(1)
                .and_then(|i| profiles.get(i as usize))
                .map(|p| p.id.clone());
            page.mutate_access(move || {
                crate::registry_controller::set_default_access(profile.as_deref())
            });
        });
        self.access_list.append(&default);
        if registry.default_access_legacy_policy && registry.default_access_profile_id.is_none() {
            let clear = gtk::Button::with_label(
                "Use All enabled servers without the retained tool restrictions",
            );
            let page = self.clone();
            clear.connect_clicked(move |_| {
                page.mutate_access(|| crate::registry_controller::set_default_access(None))
            });
            self.access_list.append(&clear);
        }

        let creator = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let name = gtk::Entry::builder()
            .placeholder_text("Access set name")
            .hexpand(true)
            .build();
        let add = gtk::Button::with_label("Create access set");
        let page = self.clone();
        let entry = name.clone();
        add.connect_clicked(move |_| {
            let name = entry.text().to_string();
            page.mutate_access(move || crate::registry_controller::create_profile(&name));
        });
        creator.append(&name);
        creator.append(&add);
        self.access_list.append(&creator);
        for profile in &registry.profiles {
            let expander = gtk::Expander::builder().label(&profile.name).build();
            let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
            for server in registry
                .servers
                .iter()
                .filter(|s| !crate::clients::is_gateway_server(s))
            {
                let included = gtk::CheckButton::with_label(&format!(
                    "{}{}",
                    server.name,
                    if server.enabled { "" } else { " (off)" }
                ));
                included.set_active(profile.enabled_server_ids.contains(&server.id));
                let (pid, sid, page) = (profile.id.clone(), server.id.clone(), self.clone());
                included.connect_toggled(move |button| {
                    let (pid, sid, included) = (pid.clone(), sid.clone(), button.is_active());
                    page.mutate_access(move || {
                        crate::registry_controller::set_access_server(&pid, &sid, included)
                    });
                });
                content.append(&included);
                if profile.enabled_server_ids.contains(&server.id) {
                    let tools =
                        gtk::Button::with_label(&format!("Choose tools for {}", server.name));
                    let (server, pid, scope, page) = (
                        server.clone(),
                        profile.id.clone(),
                        profile.tool_scope.get(&server.id).cloned(),
                        self.clone(),
                    );
                    tools.connect_clicked(move |_| {
                        open_access_tool_scope(
                            server.clone(),
                            pid.clone(),
                            scope.clone(),
                            page.clone(),
                        )
                    });
                    content.append(&tools);
                }
            }
            let delete = gtk::Button::with_label("Delete access set");
            delete.set_sensitive(
                registry.profiles.len() > 1
                    && Some(&profile.id) != registry.default_access_profile_id.as_ref()
                    && Some(&profile.id) != registry.default_access_context_id.as_ref(),
            );
            let (pid, page) = (profile.id.clone(), self.clone());
            delete.connect_clicked(move |_| {
                let pid = pid.clone();
                page.mutate_access(move || crate::registry_controller::delete_profile(&pid));
            });
            content.append(&delete);
            expander.set_child(Some(&content));
            self.access_list.append(&expander);
        }
    }

    fn mutate_access(
        &self,
        operation: impl FnOnce() -> Result<crate::registry::Registry, String> + Send + 'static,
    ) {
        self.mutation_generation
            .set(self.mutation_generation.get().wrapping_add(1));
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            match gtk::gio::spawn_blocking(operation).await {
                Ok(Ok(registry)) => page.render_access_sets(registry),
                Ok(Err(error)) => page.show_error(&error),
                Err(_) => page.show_error("The access update stopped unexpectedly"),
            }
        });
    }

    fn render_folder_routing(&self, settings: crate::registry_controller::FolderRoutingSettings) {
        while let Some(child) = self.folder_list.first_child() {
            self.folder_list.remove(&child);
        }
        self.folder_button
            .set_sensitive(!settings.profiles.is_empty());
        if settings.profiles.is_empty() {
            self.folder_list.append(&empty_state(
                "Create an access set before adding project folder routing.",
            ));
            return;
        }
        if settings.mappings.is_empty() {
            self.folder_list
                .append(&empty_state("No project folders are mapped yet."));
            return;
        }
        for mapping in settings.mappings {
            let profile_name = settings
                .profiles
                .iter()
                .find(|(id, name)| id == &mapping.profile || name == &mapping.profile)
                .map(|(_, name)| name.as_str())
                .unwrap_or(&mapping.profile);
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            row.add_css_class("toolport-setting-row");
            let copy = gtk::Box::new(gtk::Orientation::Vertical, 3);
            copy.set_hexpand(true);
            copy.append(
                &gtk::Label::builder()
                    .label(&mapping.path)
                    .halign(gtk::Align::Start)
                    .xalign(0.0)
                    .ellipsize(gtk::pango::EllipsizeMode::Middle)
                    .css_classes(["heading"])
                    .build(),
            );
            copy.append(
                &gtk::Label::builder()
                    .label(format!("Uses {profile_name}"))
                    .halign(gtk::Align::Start)
                    .css_classes(["toolport-muted"])
                    .build(),
            );
            row.append(&copy);
            let remove = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .tooltip_text("Remove folder mapping")
                .css_classes(["flat"])
                .build();
            let path = mapping.path;
            let page = self.clone();
            remove.connect_clicked(move |button| {
                button.set_sensitive(false);
                let path = path.clone();
                let page = page.clone();
                gtk::glib::spawn_future_local(async move {
                    let result = gtk::gio::spawn_blocking(move || {
                        crate::registry_controller::remove_folder_profile(&path)
                    })
                    .await;
                    match result {
                        Ok(Ok(settings)) => page.render_folder_routing(settings),
                        Ok(Err(error)) => page.show_error(&error),
                        Err(_) => {
                            page.show_error("the folder mapping removal stopped unexpectedly")
                        }
                    }
                });
            });
            row.append(&remove);
            self.folder_list.append(&row);
        }
    }

    fn render_pinned_prerequisites(
        &self,
        pins: Vec<crate::registry_controller::PinnedPrerequisite>,
    ) {
        while let Some(child) = self.pinned_list.first_child() {
            self.pinned_list.remove(&child);
        }
        if pins.is_empty() {
            self.pinned_list.append(&empty_state("None yet. In Servers, open a server's Tools tab and pin tools your agent needs every time, such as sign-in or a required lookup."));
            return;
        }
        for pin in pins {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            row.add_css_class("toolport-setting-row");
            let copy = gtk::Box::new(gtk::Orientation::Vertical, 3);
            copy.set_hexpand(true);
            copy.append(
                &gtk::Label::builder()
                    .label(&pin.tool)
                    .halign(gtk::Align::Start)
                    .css_classes(["heading"])
                    .build(),
            );
            copy.append(
                &gtk::Label::builder()
                    .label(&pin.server)
                    .halign(gtk::Align::Start)
                    .css_classes(["toolport-muted"])
                    .build(),
            );
            row.append(&copy);
            let unpin = gtk::Button::with_label("Unpin");
            unpin.add_css_class("toolport-secondary-action");
            let page = self.clone();
            unpin.connect_clicked(move |button| {
                button.set_sensitive(false);
                let page = page.clone();
                let server_id = pin.server_id.clone();
                let tool = pin.tool.clone();
                gtk::glib::spawn_future_local(async move {
                    let result = gtk::gio::spawn_blocking(move || {
                        crate::registry_controller::set_tool_pinned(&server_id, &tool, false)
                    })
                    .await;
                    match result {
                        Ok(Ok(_)) => {
                            page.feedback.set_label("Pinned prerequisite removed.");
                            page.feedback.remove_css_class("error");
                            page.feedback.add_css_class("success");
                            page.refresh();
                        }
                        Ok(Err(error)) => page.show_error(&error),
                        Err(_) => page.show_error("the prerequisite update stopped unexpectedly"),
                    }
                });
            });
            row.append(&unpin);
            self.pinned_list.append(&row);
        }
    }

    fn choose_folder_mapping(&self) {
        let Some(parent) = self.root.root().and_downcast::<gtk::Window>() else {
            return;
        };
        let dialog = gtk::FileDialog::builder()
            .title("Choose a project folder")
            .modal(true)
            .accept_label("Choose")
            .build();
        let page = self.clone();
        dialog.select_folder(Some(&parent), gtk::gio::Cancellable::NONE, move |result| {
            let Ok(folder) = result else {
                return;
            };
            let Some(path) = folder.path() else {
                page.show_error("the selected folder does not have a local path");
                return;
            };
            page.choose_folder_profile(path);
        });
    }

    fn choose_folder_profile(&self, path: std::path::PathBuf) {
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let result =
                gtk::gio::spawn_blocking(crate::registry_controller::folder_routing_settings).await;
            match result {
                Ok(Ok(settings)) => page.show_folder_profile_dialog(path, settings),
                Ok(Err(error)) => page.show_error(&error),
                Err(_) => page.show_error("the profile read stopped unexpectedly"),
            }
        });
    }

    fn show_folder_profile_dialog(
        &self,
        path: std::path::PathBuf,
        settings: crate::registry_controller::FolderRoutingSettings,
    ) {
        let Some(parent) = self.root.root().and_downcast::<gtk::Window>() else {
            return;
        };
        if settings.profiles.is_empty() {
            self.show_error("create an access set before adding folder routing");
            return;
        }
        #[allow(deprecated)]
        let dialog = adw::MessageDialog::new(
            Some(&parent),
            Some("Choose an access set"),
            Some("This access set will be selected when a client reports this folder or one of its descendants."),
        );
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("save", "Save mapping");
        dialog.set_close_response("cancel");
        dialog.set_default_response(Some("save"));
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        let names = settings
            .profiles
            .iter()
            .map(|(_, name)| name.as_str())
            .collect::<Vec<_>>();
        let dropdown =
            gtk::DropDown::new(Some(gtk::StringList::new(&names)), gtk::Expression::NONE);
        dropdown.set_margin_top(8);
        dropdown.set_margin_bottom(8);
        dialog.set_extra_child(Some(&dropdown));
        let profiles = settings.profiles;
        let page = self.clone();
        dialog.connect_response(None, move |dialog, response| {
            if response == "save" {
                let Some((profile, _)) = profiles.get(dropdown.selected() as usize) else {
                    page.show_error("choose an access set");
                    dialog.close();
                    return;
                };
                let path = path.to_string_lossy().to_string();
                let profile = profile.clone();
                let page = page.clone();
                gtk::glib::spawn_future_local(async move {
                    let result = gtk::gio::spawn_blocking(move || {
                        crate::registry_controller::upsert_folder_profile(&path, &profile)
                    })
                    .await;
                    match result {
                        Ok(Ok(settings)) => {
                            page.render_folder_routing(settings);
                            page.feedback.set_label("Saved project folder routing.");
                            page.feedback.remove_css_class("error");
                            page.feedback.add_css_class("success");
                        }
                        Ok(Err(error)) => page.show_error(&error),
                        Err(_) => page.show_error("the folder mapping save stopped unexpectedly"),
                    }
                });
            }
            dialog.close();
        });
        dialog.present();
    }

    fn render_restart_advice(&self, advice: Vec<crate::gateway_publish::ClientNeedingRestart>) {
        while let Some(child) = self.restart_list.first_child() {
            self.restart_list.remove(&child);
        }
        self.restart_list.set_visible(!advice.is_empty());
        for client in advice {
            self.restart_list.append(
                &gtk::Label::builder()
                    .label(format!(
                        "{} (pid {}) still launches {} - restart it to pick up the upgrade",
                        client.client, client.client_pid, client.gateway
                    ))
                    .halign(gtk::Align::Start)
                    .xalign(0.0)
                    .wrap(true)
                    .css_classes(["toolport-feedback", "error"])
                    .build(),
            );
        }
    }

    fn render_endpoint(&self, status: crate::http_bridge::HttpBridgeStatus) {
        if let Some(url) = status.url {
            self.endpoint_status.set_label(&format!(
                "Running at {url}/mcp · bearer authentication required"
            ));
            self.endpoint_button.set_label("Stop");
            self.endpoint_button.remove_css_class("suggested-action");
            self.endpoint_button.add_css_class("destructive-action");
            self.copy_endpoint.set_sensitive(true);
            self.copy_endpoint_token.set_sensitive(true);
            self.reveal_endpoint_token.set_sensitive(true);
            if self.reveal_endpoint_token.is_active() {
                self.endpoint_token_value
                    .set_label(status.token.as_deref().unwrap_or(""));
                self.endpoint_token_value.set_visible(true);
            }
            self.add_http_client.set_sensitive(true);
        } else {
            self.endpoint_status.set_label("Stopped");
            self.endpoint_button.set_label("Start");
            self.endpoint_button.remove_css_class("destructive-action");
            self.endpoint_button.add_css_class("suggested-action");
            self.copy_endpoint.set_sensitive(false);
            self.copy_endpoint_token.set_sensitive(false);
            self.reveal_endpoint_token.set_sensitive(false);
            self.reveal_endpoint_token.set_active(false);
            self.endpoint_token_value.set_label("");
            self.endpoint_token_value.set_visible(false);
            self.add_http_client.set_sensitive(false);
        }
    }

    fn render_http_clients(&self, settings: crate::registry_controller::HttpClientSettings) {
        while let Some(child) = self.http_client_list.first_child() {
            self.http_client_list.remove(&child);
        }
        if settings.clients.is_empty() {
            self.http_client_list
                .append(&empty_state("No scoped HTTP clients are registered."));
            return;
        }
        for client in settings.clients {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            row.add_css_class("toolport-setting-row");
            let copy = gtk::Box::new(gtk::Orientation::Vertical, 3);
            copy.set_hexpand(true);
            copy.append(
                &gtk::Label::builder()
                    .label(&client.label)
                    .halign(gtk::Align::Start)
                    .css_classes(["heading"])
                    .build(),
            );
            let profile = if client.profile.is_empty() {
                "Full connected set".to_string()
            } else if client.profile == crate::registry::ALL_ENABLED_ACCESS {
                "All enabled servers".to_string()
            } else {
                settings
                    .profiles
                    .iter()
                    .find(|(id, name)| id == &client.profile || name == &client.profile)
                    .map(|(_, name)| format!("Only {name}"))
                    .unwrap_or_else(|| format!("Only {}", client.profile))
            };
            copy.append(
                &gtk::Label::builder()
                    .label(profile)
                    .halign(gtk::Align::Start)
                    .css_classes(["toolport-muted"])
                    .build(),
            );
            row.append(&copy);
            if client.id.starts_with("client:") {
                let badge = gtk::Label::new(Some("Managed client"));
                badge.add_css_class("toolport-badge");
                badge.add_css_class("success");
                badge.set_tooltip_text(Some("Disconnect this token from the Clients page"));
                row.append(&badge);
            } else {
                let remove = gtk::Button::builder()
                    .icon_name("user-trash-symbolic")
                    .tooltip_text(format!("Revoke {}", client.label))
                    .css_classes(["flat", "destructive-action"])
                    .build();
                let id = client.id;
                let page = self.clone();
                remove.connect_clicked(move |button| {
                    button.set_sensitive(false);
                    let id = id.clone();
                    let page = page.clone();
                    gtk::glib::spawn_future_local(async move {
                        let result = gtk::gio::spawn_blocking(move || {
                            crate::registry_controller::remove_http_client(&id)
                        })
                        .await;
                        match result {
                            Ok(Ok(settings)) => {
                                page.render_http_clients(settings);
                                page.feedback.set_label("Revoked the HTTP client token.");
                                page.feedback.remove_css_class("error");
                                page.feedback.add_css_class("success");
                            }
                            Ok(Err(error)) => page.show_error(&error),
                            Err(_) => {
                                page.show_error("the HTTP client removal stopped unexpectedly")
                            }
                        }
                    });
                });
                row.append(&remove);
            }
            self.http_client_list.append(&row);
        }
    }

    fn open_http_client_editor(&self) {
        let Some(parent) = self.root.root().and_downcast::<gtk::Window>() else {
            return;
        };
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let result =
                gtk::gio::spawn_blocking(crate::registry_controller::http_client_settings).await;
            let settings = match result {
                Ok(Ok(settings)) => settings,
                Ok(Err(error)) => {
                    page.show_error(&error);
                    return;
                }
                Err(_) => {
                    page.show_error("the HTTP client read stopped unexpectedly");
                    return;
                }
            };
            #[allow(deprecated)]
            let dialog = adw::MessageDialog::new(
                Some(&parent),
                Some("Add a scoped HTTP client"),
                Some("Toolport generates a bearer token that is shown once. Choose which access set this client can access."),
            );
            dialog.add_response("cancel", "Cancel");
            dialog.add_response("add", "Add client");
            dialog.set_close_response("cancel");
            dialog.set_default_response(Some("add"));
            dialog.set_response_appearance("add", adw::ResponseAppearance::Suggested);
            let form = gtk::Box::new(gtk::Orientation::Vertical, 8);
            let label = gtk::Entry::builder()
                .placeholder_text("Client name, for example Open WebUI")
                .css_classes(["toolport-input"])
                .build();
            form.append(&label);
            let mut profile_names = vec!["Full connected set", "All enabled servers"];
            profile_names.extend(settings.profiles.iter().map(|(_, name)| name.as_str()));
            let profile = gtk::DropDown::new(
                Some(gtk::StringList::new(&profile_names)),
                gtk::Expression::NONE,
            );
            form.append(&profile);
            dialog.set_extra_child(Some(&form));
            let profiles = settings.profiles;
            let page_for_response = page.clone();
            dialog.connect_response(None, move |dialog, response| {
                if response == "add" {
                    let name = label.text().to_string();
                    let profile_id = match http_access_choice(&profiles, profile.selected()) {
                        Ok(profile) => profile,
                        Err(error) => {
                            page_for_response.show_error(&error);
                            dialog.close();
                            return;
                        }
                    };
                    let page = page_for_response.clone();
                    gtk::glib::spawn_future_local(async move {
                        let result = gtk::gio::spawn_blocking(move || {
                            crate::registry_controller::add_http_client(
                                &name,
                                profile_id.as_deref(),
                            )
                        })
                        .await;
                        match result {
                            Ok(Ok(added)) => {
                                page.render_http_clients(added.settings);
                                page.show_http_client_token(&added.token);
                            }
                            Ok(Err(error)) => page.show_error(&error),
                            Err(_) => {
                                page.show_error("the HTTP client registration stopped unexpectedly")
                            }
                        }
                    });
                }
                dialog.close();
            });
            dialog.present();
        });
    }

    fn show_http_client_token(&self, token: &str) {
        if let Some(display) = gtk::gdk::Display::default() {
            display.clipboard().set_text(token);
        }
        let Some(parent) = self.root.root().and_downcast::<gtk::Window>() else {
            return;
        };
        #[allow(deprecated)]
        let dialog = adw::MessageDialog::new(
            Some(&parent),
            Some("Copy this token now"),
            Some("The token was copied to the clipboard. Toolport stores only its hash and cannot show it again."),
        );
        dialog.add_response("done", "Done");
        dialog.set_default_response(Some("done"));
        dialog.set_close_response("done");
        let token = gtk::Label::builder()
            .label(token)
            .selectable(true)
            .wrap(true)
            .hexpand(true)
            .xalign(0.0)
            .css_classes(["toolport-feedback", "success"])
            .build();
        dialog.set_extra_child(Some(&token));
        dialog.connect_response(None, |dialog, _| dialog.close());
        dialog.present();
        self.feedback
            .set_label("Registered the scoped HTTP client.");
        self.feedback.remove_css_class("error");
        self.feedback.add_css_class("success");
    }

    fn connect_switches(&self) {
        let page = self.clone();
        self.safety_kept_reset.connect_clicked(move |button| {
            let level = page.safety_current.get();
            page.begin_mutation();
            button.set_sensitive(false);
            let page = page.clone();
            gtk::glib::spawn_future_local(async move {
                let result = super::run_user_action(move || {
                    crate::registry_controller::set_safety_level(level)
                })
                .await;
                page.begin_mutation();
                match result {
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => page.show_error(&error),
                    Err(_) => page.show_error("the safety update stopped unexpectedly"),
                }
                page.refresh();
            });
        });
        let page = self.clone();
        self.safety_level.connect_selected_notify(move |control| {
            if page.updating.get() {
                return;
            }
            let level = match control.selected() + page.safety_floor.get() as u32 {
                0 => crate::registry::SafetyLevel::Off,
                1 => crate::registry::SafetyLevel::Ask,
                _ => crate::registry::SafetyLevel::Strict,
            };
            page.begin_mutation();
            control.set_sensitive(false);
            for card in &page.safety_cards { card.set_sensitive(false); }
            let page = page.clone();
            gtk::glib::spawn_future_local(async move {
                let result = super::run_user_action(move || {
                    crate::registry_controller::set_safety_level(level)
                })
                .await;
                page.begin_mutation();
                match result {
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => page.show_error(&error),
                    Err(_) => page.show_error("the safety update stopped unexpectedly"),
                }
                page.safety_level.set_sensitive(true);
                page.refresh();
            });
        });
        for (switch, setting, label) in [
            (
                self.lazy_discovery.clone(),
                crate::registry_controller::EssentialSetting::LazyDiscovery,
                "lazy discovery",
            ),
            (
                self.code_mode.clone(),
                crate::registry_controller::EssentialSetting::CodeMode,
                "code mode",
            ),
            (
                self.live_inspect.clone(),
                crate::registry_controller::EssentialSetting::LiveInspect,
                "live inspection",
            ),
            (
                self.pii_redaction.clone(),
                crate::registry_controller::EssentialSetting::PiiRedaction,
                "PII pseudonymization",
            ),
        ] {
            let page = self.clone();
            switch.connect_state_set(move |switch, enabled| {
                if page.updating.get() {
                    return gtk::glib::Propagation::Proceed;
                }
                switch.set_sensitive(false);
                page.begin_mutation();
                // Claimed after the bump, so a second toggle started while this
                // one is in flight makes this completion skip its render rather
                // than putting the other switch back from a stale snapshot.
                let generation = page.mutation_generation.get();
                let switch = switch.clone();
                let page = page.clone();
                gtk::glib::spawn_future_local(async move {
                    let result = super::run_user_action(move || {
                        crate::registry_controller::set_essential_setting(setting, enabled)
                    })
                    .await;
                    // Checked before the completion bump below, which would
                    // otherwise make every save look stale and leave the switch
                    // unconfirmed until the next 15-second refresh.
                    let latest = page.mutation_generation.get() == generation;
                    // Again on completion, so a read that started mid-write is
                    // discarded too.
                    page.begin_mutation();
                    match result {
                        Ok(Ok(settings)) => {
                            if latest {
                                page.render_settings(settings);
                            } else {
                                // Another toggle is in flight; confirm only this
                                // switch rather than rendering a stale snapshot.
                                page.updating.set(true);
                                set_switch(&switch, enabled);
                                page.updating.set(false);
                                switch.set_sensitive(true);
                            }
                            let outcome =
                                format!("{} {label}", if enabled { "Enabled" } else { "Disabled" });
                            page.feedback.set_label(&outcome);
                            page.feedback.remove_css_class("error");
                            page.feedback.add_css_class("success");
                        }
                        Ok(Err(error)) => {
                            // refresh() no-ops while a background tick holds the
                            // guard, and render_settings is what re-enables the
                            // switch, so restore it here rather than relying on
                            // a refresh that may never run.
                            switch.set_sensitive(true);
                            page.show_error(&error);
                            page.refresh();
                        }
                        Err(_) => {
                            switch.set_sensitive(true);
                            page.show_error("the setting update stopped unexpectedly");
                            page.refresh();
                        }
                    }
                });
                gtk::glib::Propagation::Stop
            });
        }

        let page = self.clone();
        self.launch_at_login
            .connect_state_set(move |switch, enabled| {
                if page.updating.get() {
                    return gtk::glib::Propagation::Proceed;
                }
                switch.set_sensitive(false);
                page.begin_mutation();
                let page = page.clone();
                gtk::glib::spawn_future_local(async move {
                    let result = super::run_user_action(move || {
                        if enabled {
                            crate::autostart::enable_linux(NATIVE_AUTOSTART_NAME)
                        } else {
                            crate::autostart::disable_linux(NATIVE_AUTOSTART_NAME)
                        }
                    })
                    .await;
                    page.begin_mutation();
                    match result {
                        Ok(Ok(())) => {
                            page.updating.set(true);
                            set_switch(&page.launch_at_login, enabled);
                            page.launch_at_login.set_sensitive(true);
                            page.updating.set(false);
                            page.feedback.set_label(if enabled {
                                "Toolport will launch at login"
                            } else {
                                "Toolport will not launch at login"
                            });
                            page.feedback.remove_css_class("error");
                            page.feedback.add_css_class("success");
                        }
                        Ok(Err(error)) => {
                            page.show_error(&error);
                            page.refresh();
                        }
                        Err(_) => {
                            page.show_error("the launch-at-login update stopped unexpectedly");
                            page.refresh();
                        }
                    }
                });
                gtk::glib::Propagation::Stop
            });
    }

    pub(super) fn select_page(&self, index: usize) {
        for (current, page) in self.pages.iter().enumerate() { page.set_visible(current == index); }
    }

    pub(super) fn pending_count(&self) -> usize { self.broker.list().len() }

    pub(super) fn refresh(&self) {
        self.refresh_with_feedback(true)
    }

    /// Background cadence refresh: same reads, but no "Loading" flash and no
    /// success line, so a 15-second tick never talks over feedback the user is
    /// actually reading.
    pub(super) fn refresh_quietly(&self) {
        self.refresh_with_feedback(false)
    }

    /// Claim a new generation. Any refresh already reading is now stale and will
    /// drop its snapshot instead of rendering it over this change.
    fn begin_mutation(&self) {
        self.mutation_generation
            .set(self.mutation_generation.get().wrapping_add(1));
    }

    fn refresh_with_feedback(&self, announce: bool) {
        if self.refreshing.replace(true) {
            return;
        }
        let generation = self.mutation_generation.get();
        self.refresh_button.set_sensitive(false);
        if announce {
            self.set_status("Loading settings…");
        }
        let page = self.clone();
        let broker = self.broker.clone();
        let bridge = self.bridge.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                Ok::<_, String>((
                    crate::registry_controller::essential_settings()?,
                    crate::autostart::is_enabled_linux(NATIVE_AUTOSTART_NAME)?,
                    read_quarantined_tools(),
                    read_allowed_tools(&broker)?,
                    crate::registry_controller::folder_routing_settings()?,
                    crate::registry_controller::http_client_settings()?,
                    crate::registry_controller::pinned_prerequisites()?,
                    bridge.restart_advice(),
                    crate::registry::load()?,
                ))
            })
            .await;
            page.refresh_button.set_sensitive(true);
            match result {
                Ok(Ok((
                    settings,
                    launch_at_login,
                    quarantined,
                    allowed,
                    folder_routing,
                    http_clients,
                    pinned,
                    restart_advice,
                    access_registry,
                ))) => {
                    // A toggle landed while this read was in flight, so the
                    // snapshot is already out of date. Rendering it would put
                    // the old value back into the switch the user just moved.
                    if page.mutation_generation.get() != generation {
                        page.refreshing.set(false);
                        page.refresh_button.set_sensitive(true);
                        return;
                    }
                    page.render_access_sets(access_registry);
                    page.render_settings(settings);
                    page.render_restart_advice(restart_advice);
                    page.updating.set(true);
                    set_switch(&page.launch_at_login, launch_at_login);
                    page.launch_at_login.set_sensitive(true);
                    page.updating.set(false);
                    page.render_endpoint(page.bridge.status());
                    page.render_quarantine(quarantined);
                    page.render_allowed(allowed);
                    page.render_folder_routing(folder_routing);
                    page.render_http_clients(http_clients);
                    page.render_pinned_prerequisites(pinned);
                    if announce {
                        // Clearing, not announcing: the load succeeding is not
                        // news, and a green "up to date" bar stacked above the
                        // red approval-gates warning competed with it.
                        page.set_status("");
                    }
                    page.refreshing.set(false);
                }
                Ok(Err(error)) => {
                    page.refreshing.set(false);
                    page.show_error(&error);
                }
                Err(_) => {
                    page.refreshing.set(false);
                    page.show_error("the settings read stopped unexpectedly");
                }
            }
        });
    }

    fn render_settings(&self, settings: crate::registry_controller::EssentialSettings) {
        let (line, guarded) = posture_summary(&settings);
        self.posture.set_label(&line);
        self.posture.remove_css_class("success");
        self.posture.remove_css_class("error");
        if guarded {
            self.posture.add_css_class("success");
        }
        self.posture.set_visible(true);
        self.updating.set(true);
        set_switch(&self.lazy_discovery, settings.lazy_discovery);
        self.lazy_discovery.set_sensitive(true);
        self.pinned_section.set_visible(settings.lazy_discovery);
        set_switch(&self.code_mode, settings.code_mode);
        self.code_mode.set_sensitive(true);
        set_switch(&self.live_inspect, settings.live_inspect);
        self.live_inspect.set_sensitive(true);
        self.safety_floor.set(settings.team_min_safety_level);
        let choices = match settings.team_min_safety_level {
            crate::registry::SafetyLevel::Off => vec!["Off", "Ask", "Strict"],
            crate::registry::SafetyLevel::Ask => vec!["Ask", "Strict"],
            crate::registry::SafetyLevel::Strict => vec!["Strict"],
        };
        self.safety_level
            .set_model(Some(&gtk::StringList::new(&choices)));
        self.safety_level
            .set_selected(settings.safety_level as u32 - settings.team_min_safety_level as u32);
        let floor = match settings.team_min_safety_level {
            crate::registry::SafetyLevel::Off => "Off",
            crate::registry::SafetyLevel::Ask => "Ask",
            crate::registry::SafetyLevel::Strict => "Strict",
        };
        let mut policy = format!("Team minimum safety level: {floor}.");
        if settings.quarantine_on_drift_forced {
            policy.push_str(" Team also enforces quarantine on drift.");
        }
        if settings.block_on_injection_forced {
            policy.push_str(" Team also enforces block on injection.");
        }
        self.safety_policy.set_label(&policy);
        self.safety_policy.set_visible(
            settings.team_min_safety_level != crate::registry::SafetyLevel::Off
                || settings.quarantine_on_drift_forced
                || settings.block_on_injection_forced,
        );
        self.safety_level.set_sensitive(true);
        for (index, card) in self.safety_cards.iter().enumerate() {
            card.set_sensitive(index >= settings.team_min_safety_level as usize);
            card.set_active(index == settings.safety_level as usize);
        }
        self.safety_current.set(settings.safety_level);
        let kept = settings.kept_v1_safety.summary();
        self.safety_kept.set_visible(kept.is_some());
        if let Some(kept) = kept {
            self.safety_kept_note.set_label(&format!(
                "{kept} Choosing a level replaces these with that level's protections."
            ));
            self.safety_kept_reset
                .set_label(&format!("Use standard {}", level_name(settings.safety_level)));
            self.safety_kept_reset.set_sensitive(true);
        }
        set_switch(&self.pii_redaction, settings.pii_redaction);
        set_team_managed(&self.pii_redaction, settings.pii_redaction_forced);
        self.updating.set(false);
    }

    /// Visibility is handled by the `notify::label` hook installed in `new`, so
    /// this only has to clear the styling an earlier message left behind.
    fn set_status(&self, message: &str) {
        self.feedback.set_label(message);
        self.feedback.remove_css_class("error");
        self.feedback.remove_css_class("success");
    }

    fn show_error(&self, error: &str) {
        self.set_status(&format!("Settings error: {error}"));
        self.feedback.add_css_class("error");
    }

    fn render_quarantine(&self, entries: Result<Vec<QuarantinedTool>, String>) {
        while let Some(child) = self.quarantine_list.first_child() {
            self.quarantine_list.remove(&child);
        }
        let entries = match entries {
            Ok(entries) => entries,
            Err(error) => {
                let row = gtk::Box::new(gtk::Orientation::Vertical, 0);
                row.add_css_class("toolport-setting-row");
                row.append(
                    &gtk::Label::builder()
                        .label(format!("Blocked-tool state is unknown: {error}"))
                        .halign(gtk::Align::Fill)
                        .xalign(0.0)
                        .wrap(true)
                        .hexpand(true)
                        .css_classes(["error"])
                        .build(),
                );
                self.quarantine_list.append(&row);
                if let Some(section) = self.quarantine_list.parent() {
                    section.set_visible(true);
                }
                return;
            }
        };
        if let Some(section) = self.quarantine_list.parent() {
            section.set_visible(!entries.is_empty());
        }
        if entries.is_empty() {
            self.quarantine_list
                .append(&empty_state("No tools are quarantined."));
            return;
        }
        if entries.len() > 1 {
            self.quarantine_list
                .append(&quarantine_bulk_row(&entries, self.clone()));
        }
        for entry in entries {
            self.quarantine_list
                .append(&quarantine_row(entry, self.clone()));
        }
    }

    fn render_allowed(&self, entries: Vec<AllowedTool>) {
        while let Some(child) = self.allowed_list.first_child() {
            self.allowed_list.remove(&child);
        }
        if let Some(section) = self.allowed_list.parent() {
            section.set_visible(!entries.is_empty());
        }
        if entries.is_empty() {
            self.allowed_list
                .append(&empty_state("No remembered approvals."));
            return;
        }
        for entry in entries {
            self.allowed_list.append(&allowed_row(entry, self.clone()));
        }
    }
}

#[derive(Clone)]
struct QuarantinedTool {
    profile: String,
    tool: String,
    detail: String,
}

fn read_quarantined_tools() -> Result<Vec<QuarantinedTool>, String> {
    Ok(crate::integrity::all_quarantined()?
        .into_iter()
        .map(|value| QuarantinedTool {
            profile: value
                .get("profile")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            tool: value
                .get("tool")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("Unknown tool")
                .to_string(),
            detail: value
                .get("detail")
                .and_then(serde_json::Value::as_str)
                .or_else(|| value.get("reason").and_then(serde_json::Value::as_str))
                .unwrap_or("High-risk definition change")
                .to_string(),
        })
        .collect())
}

/// Describe the effective policy, including the team's enforced level.
fn posture_summary(settings: &crate::registry_controller::EssentialSettings) -> (String, bool) {
    use crate::registry::SafetyLevel;
    let (mut line, guarded) = match settings.safety_level {
        SafetyLevel::Off => ("Safety is set to Off. Toolport does not ask before destructive calls. Server sign-in and client permissions may still ask for approval.".to_string(), false),
        SafetyLevel::Ask => ("Safety is set to Ask. Destructive calls need your approval before they run.".to_string(), true),
        SafetyLevel::Strict => ("Safety is set to Strict. Destructive tools are hidden and untrusted calls need your approval.".to_string(), true),
    };
    if settings.quarantine_on_drift_forced || settings.block_on_injection_forced {
        line.push_str(
            " Your team also requires protection against risky tool changes or injection.",
        );
    }
    (line, guarded)
}

/// Which profile scopes a re-approve-all pass must clear, in first-seen order.
fn open_access_tool_scope(
    server: crate::registry::ServerEntry,
    profile_id: String,
    current_scope: Option<Vec<String>>,
    page: SettingsPage,
) {
    let Some(parent) = page
        .root
        .root()
        .and_then(|root| root.downcast::<gtk::Window>().ok())
    else {
        return;
    };
    let window = adw::Window::builder()
        .transient_for(&parent)
        .modal(true)
        .title(format!("Tool scope for {}", server.name))
        .default_width(520)
        .default_height(560)
        .build();
    window.set_application(parent.application().as_ref());
    window.add_css_class("toolport-editor");
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = adw::HeaderBar::new();
    let cancel = gtk::Button::with_label("Cancel");
    cancel.add_css_class("toolport-secondary-action");
    header.pack_start(&cancel);
    let save = gtk::Button::with_label("Save");
    save.add_css_class("suggested-action");
    save.set_sensitive(false);
    header.pack_end(&save);
    root.append(&header);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.add_css_class("toolport-editor-body");
    content.append(&super::editor_intro(
        "view-list-symbolic",
        "Tools available in this access set",
        "Uncheck tools this access set should hide. Selecting every tool restores the server's default full scope.",
    ));
    let feedback = gtk::Label::builder()
        .label("Loading server tools…")
        .halign(gtk::Align::Fill)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["toolport-feedback"])
        .build();
    content.append(&feedback);
    let tool_list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    tool_list.add_css_class("toolport-settings-group");
    content.append(&tool_list);
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&content)
        .build();
    root.append(&scroller);
    window.set_content(Some(&root));
    let window_for_cancel = window.clone();
    cancel.connect_clicked(move |_| window_for_cancel.close());

    let selections = std::rc::Rc::new(std::cell::RefCell::new(
        Vec::<(gtk::CheckButton, String)>::new(),
    ));
    let selections_for_save = selections.clone();
    let page_for_save = page.clone();
    let window_for_save = window.clone();
    let server_id = server.id;
    let server_id_for_save = server_id.clone();
    save.connect_clicked(move |button| {
        button.set_sensitive(false);
        let selections = selections_for_save.borrow();
        let selected = selections
            .iter()
            .filter(|(check, _)| check.is_active())
            .map(|(_, tool)| tool.clone())
            .collect::<Vec<_>>();
        let tools = (selected.len() != selections.len()).then_some(selected);
        drop(selections);
        let profile_id = profile_id.clone();
        let server_id = server_id_for_save.clone();
        let page = page_for_save.clone();
        let window = window_for_save.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                crate::registry_controller::set_profile_server_tools(&profile_id, &server_id, tools)
            })
            .await;
            match result {
                Ok(Ok(registry)) => {
                    page.render_access_sets(registry);
                    window.close();
                }
                Ok(Err(error)) => page.show_error(&format!("Could not update tool scope: {error}")),
                Err(_) => page.show_error("The tool-scope update stopped unexpectedly."),
            }
        });
    });

    let current_scope =
        current_scope.map(|tools| tools.into_iter().collect::<std::collections::HashSet<_>>());
    let selections_for_load = selections;
    gtk::glib::spawn_future_local(async move {
        let result =
            gtk::gio::spawn_blocking(move || crate::playground::list_tools(&server_id)).await;
        match result {
            Ok(Ok(tools)) => {
                if tools.is_empty() {
                    feedback.set_label("This server does not advertise any tools.");
                    return;
                }
                for tool in tools {
                    let Some(name) = tool
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string)
                    else {
                        continue;
                    };
                    let check = gtk::CheckButton::builder()
                        .label(&name)
                        .active(
                            current_scope
                                .as_ref()
                                .is_none_or(|scope| scope.contains(&name)),
                        )
                        .build();
                    check.set_margin_top(8);
                    check.set_margin_bottom(8);
                    check.set_margin_start(12);
                    check.set_margin_end(12);
                    tool_list.append(&check);
                    selections_for_load.borrow_mut().push((check, name));
                }
                feedback.set_label("Changes apply only to this access set.");
                feedback.remove_css_class("error");
                feedback.add_css_class("success");
                save.set_sensitive(!selections_for_load.borrow().is_empty());
            }
            Ok(Err(error)) => {
                feedback.set_label(&format!("Could not load tools: {error}"));
                feedback.add_css_class("error");
            }
            Err(_) => {
                feedback.set_label("The server tool read stopped unexpectedly.");
                feedback.add_css_class("error");
            }
        }
    });
    window.present();
}

fn distinct_profiles(entries: &[QuarantinedTool]) -> Vec<String> {
    let mut profiles: Vec<String> = Vec::new();
    for entry in entries {
        if !profiles.contains(&entry.profile) {
            profiles.push(entry.profile.clone());
        }
    }
    profiles
}

/// The user-facing outcome line for a bulk re-approval, and whether it is an error.
///
/// A skipped tool is still blocked, so any skip or scope failure must read as an
/// error; saying "done" would send the user away believing the catalog is whole.
fn release_all_feedback(summary: &crate::registry_controller::ReleaseAllSummary) -> (String, bool) {
    let released = summary.released;
    if !summary.failed.is_empty() {
        let failures = summary.failed.len();
        return (
            format!(
                "Re-approved {released}. {failures} access-set {} could not be re-approved: {}",
                if failures == 1 { "scope" } else { "scopes" },
                summary.failed[0]
            ),
            true,
        );
    }
    if !summary.skipped.is_empty() {
        let skipped = summary.skipped.len();
        return (
            format!(
                "Re-approved {released}. {skipped} could not be repaired and {} still blocked.",
                if skipped == 1 { "is" } else { "are" }
            ),
            true,
        );
    }
    (
        format!(
            "Re-approved {released} tool{}.",
            if released == 1 { "" } else { "s" }
        ),
        false,
    )
}

/// A wrapping container for a cluster of action buttons: on a narrow tile the
/// buttons flow onto the next line instead of forcing a minimum window width.
fn action_wrap() -> gtk::FlowBox {
    let flow = gtk::FlowBox::new();
    flow.set_selection_mode(gtk::SelectionMode::None);
    flow.set_column_spacing(8);
    flow.set_row_spacing(6);
    flow.set_min_children_per_line(1);
    flow.set_max_children_per_line(8);
    flow.set_valign(gtk::Align::Center);
    flow
}

fn quarantine_bulk_row(entries: &[QuarantinedTool], page: SettingsPage) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.add_css_class("toolport-setting-row");
    let count = entries.len();
    let copy = gtk::Label::builder()
        .label(format!("{count} tools are blocked."))
        .halign(gtk::Align::Fill)
        .xalign(0.0)
        .wrap(true)
        .hexpand(true)
        .css_classes(["toolport-muted"])
        .build();
    row.append(&copy);
    let release_all = gtk::Button::with_label("Re-approve all");
    release_all.add_css_class("toolport-secondary-action");
    release_all.set_tooltip_text(Some(
        "Repair every baseline in one pass. A tool whose captured definition cannot be read stays blocked.",
    ));
    let profiles = distinct_profiles(entries);
    let release_for_click = release_all.clone();
    release_all.connect_clicked(move |_| {
        let Some(parent) = page.root.root().and_downcast::<gtk::Window>() else {
            return;
        };
        #[allow(deprecated)]
        let dialog = adw::MessageDialog::new(
            Some(&parent),
            Some(&format!("Re-approve all {count} blocked tools?")),
            Some(
                "Toolport trusts each changed definition again and repairs its baseline in one pass. \
                 A tool whose captured definition cannot be read stays blocked and remains listed.",
            ),
        );
        dialog.add_response("cancel", "Keep blocked");
        dialog.add_response("approve", "Re-approve all");
        dialog.set_close_response("cancel");
        dialog.set_default_response(Some("cancel"));
        dialog.set_response_appearance("approve", adw::ResponseAppearance::Suggested);
        let page = page.clone();
        let profiles = profiles.clone();
        let button = release_for_click.clone();
        dialog.connect_response(None, move |dialog, response| {
            if response == "approve" {
                button.set_sensitive(false);
                let page = page.clone();
                let profiles = profiles.clone();
                gtk::glib::spawn_future_local(async move {
                    let result = gtk::gio::spawn_blocking(move || {
                        crate::registry_controller::release_all_quarantine(&profiles)
                    })
                    .await;
                    match result {
                        Ok(summary) => {
                            let (message, is_error) = release_all_feedback(&summary);
                            page.feedback.set_label(&message);
                            if is_error {
                                page.feedback.remove_css_class("success");
                                page.feedback.add_css_class("error");
                            } else {
                                page.feedback.remove_css_class("error");
                                page.feedback.add_css_class("success");
                            }
                            page.refresh();
                        }
                        Err(_) => page.show_error("the bulk re-approval stopped unexpectedly"),
                    }
                });
            }
            dialog.close();
        });
        dialog.present();
    });
    row.append(&release_all);
    row
}

fn quarantine_row(entry: QuarantinedTool, page: SettingsPage) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.add_css_class("toolport-setting-row");
    let copy = gtk::Box::new(gtk::Orientation::Vertical, 3);
    copy.set_hexpand(true);
    copy.append(
        &gtk::Label::builder()
            .label(&entry.tool)
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .hexpand(true)
            .css_classes(["heading"])
            .build(),
    );
    let scope = if entry.profile.is_empty() {
        "All access sets".to_string()
    } else {
        format!("Access set: {}", entry.profile)
    };
    copy.append(
        &gtk::Label::builder()
            .label(format!("{} · {}", entry.detail, scope))
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .hexpand(true)
            .css_classes(["toolport-muted"])
            .build(),
    );
    row.append(&copy);
    let release = gtk::Button::with_label("Review and approve");
    release.add_css_class("toolport-secondary-action");
    let release_for_click = release.clone();
    release.connect_clicked(move |_| {
        let Some(parent) = page.root.root().and_downcast::<gtk::Window>() else {
            return;
        };
        #[allow(deprecated)]
        let dialog = adw::MessageDialog::new(
            Some(&parent),
            Some(&format!("Re-approve {}?", entry.tool)),
            Some(&format!(
                "Toolport will trust the changed definition and expose this tool again.\n\n{}",
                entry.detail
            )),
        );
        dialog.add_response("cancel", "Keep blocked");
        dialog.add_response("approve", "Re-approve");
        dialog.set_close_response("cancel");
        dialog.set_default_response(Some("cancel"));
        dialog.set_response_appearance("approve", adw::ResponseAppearance::Suggested);
        let page = page.clone();
        let entry = entry.clone();
        let button = release_for_click.clone();
        dialog.connect_response(None, move |dialog, response| {
            if response == "approve" {
                button.set_sensitive(false);
                let page = page.clone();
                let entry = entry.clone();
                gtk::glib::spawn_future_local(async move {
                    let tool = entry.tool.clone();
                    let profile = entry.profile.clone();
                    let result = gtk::gio::spawn_blocking(move || {
                        let profile = (!profile.is_empty()).then_some(profile.as_str());
                        crate::registry_controller::release_quarantine(profile, &tool)
                    })
                    .await;
                    match result {
                        Ok(Ok(())) => {
                            page.feedback.set_label("Tool re-approved.");
                            page.feedback.remove_css_class("error");
                            page.feedback.add_css_class("success");
                            page.refresh();
                        }
                        Ok(Err(error)) => page.show_error(&error),
                        Err(_) => page.show_error("the re-approval stopped unexpectedly"),
                    }
                });
            }
            dialog.close();
        });
        dialog.present();
    });
    row.append(&release);
    row
}

#[derive(Clone)]
struct AllowedTool {
    key: String,
    server: String,
    tool: String,
    persistent: bool,
}

fn read_allowed_tools(
    broker: &crate::approval_broker::ApprovalBroker,
) -> Result<Vec<AllowedTool>, String> {
    let registry = crate::registry::load()?;
    let persistent = registry.human_approval_allow;
    let parse = |key: &str| -> Option<(String, String)> {
        let mut parts = key.splitn(3, '/');
        match (parts.next(), parts.next(), parts.next()) {
            (Some(server), Some(tool), Some(_)) => Some((server.into(), tool.into())),
            _ => None,
        }
    };
    let mut entries = persistent
        .iter()
        .filter_map(|key| {
            let (server, tool) = parse(key)?;
            Some(AllowedTool {
                key: key.clone(),
                server,
                tool,
                persistent: true,
            })
        })
        .collect::<Vec<_>>();
    for key in broker.session_allowed() {
        if !persistent.contains(&key) {
            if let Some((server, tool)) = parse(&key) {
                entries.push(AllowedTool {
                    key,
                    server,
                    tool,
                    persistent: false,
                });
            }
        }
    }
    entries.sort_by(|left, right| {
        left.server
            .cmp(&right.server)
            .then(left.tool.cmp(&right.tool))
    });
    Ok(entries)
}

fn allowed_row(entry: AllowedTool, page: SettingsPage) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.add_css_class("toolport-setting-row");
    let copy = gtk::Box::new(gtk::Orientation::Vertical, 3);
    copy.set_hexpand(true);
    copy.append(
        &gtk::Label::builder()
            .label(format!("{} / {}", entry.server, entry.tool))
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .hexpand(true)
            .css_classes(["heading"])
            .build(),
    );
    copy.append(
        &gtk::Label::builder()
            .label(if entry.persistent {
                "Always allowed for this exact tool definition"
            } else {
                "Allowed for this Toolport session"
            })
            .halign(gtk::Align::Start)
            .css_classes(["toolport-muted"])
            .build(),
    );
    row.append(&copy);
    let revoke = gtk::Button::with_label("Require approval");
    revoke.add_css_class("toolport-secondary-action");
    revoke.connect_clicked(move |button| {
        button.set_sensitive(false);
        let key = entry.key.clone();
        let broker = page.broker.clone();
        let page = page.clone();
        gtk::glib::spawn_future_local(async move {
            let key_for_write = key.clone();
            let result = gtk::gio::spawn_blocking(move || {
                crate::registry::update(|registry| {
                    registry.revoke_tool(&key_for_write);
                    Ok(())
                })
            })
            .await;
            match result {
                Ok(Ok(_)) => {
                    broker.remove_session_allow(&key);
                    page.feedback.set_label("Approval exception removed.");
                    page.feedback.remove_css_class("error");
                    page.feedback.add_css_class("success");
                    page.refresh();
                }
                Ok(Err(error)) => page.show_error(&error),
                Err(_) => page.show_error("the approval update stopped unexpectedly"),
            }
        });
    });
    row.append(&revoke);
    row
}

fn set_team_managed(toggle: &gtk::Switch, forced: bool) {
    toggle.set_sensitive(!forced);
    toggle.set_tooltip_text(forced.then_some("Required by your Toolport team"));
}

/// A section heading with its own action on the same row. Both of these used to
/// sit on a line of their own, right-aligned against the whole page width.
fn settings_heading_with_action(
    title: &str,
    subtitle: &str,
    action: &impl IsA<gtk::Widget>,
) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.set_margin_top(10);
    let heading = settings_heading(title, subtitle);
    heading.set_margin_top(0);
    heading.set_hexpand(true);
    row.append(&heading);
    row.append(action);
    row
}

/// A top-level group of the page: a larger title, one line saying what is in it,
/// and the box its rows go into.
fn settings_section(page: &gtk::Box, title: &str, summary: &str) -> gtk::Box {
    let section = gtk::Box::new(gtk::Orientation::Vertical, 14);
    section.set_margin_top(12);
    section.append(&section_title(title, summary));
    page.append(&section);
    section
}

fn section_title(title: &str, summary: &str) -> gtk::Box {
    let heading = gtk::Box::new(gtk::Orientation::Vertical, 3);
    heading.append(
        &gtk::Label::builder()
            .label(title)
            .halign(gtk::Align::Start)
            .css_classes(["title-3"])
            .build(),
    );
    heading.append(
        &gtk::Label::builder()
            .label(summary)
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .hexpand(true)
            .css_classes(["toolport-muted"])
            .build(),
    );
    heading
}

/// A heading and its list, hidden together until the list has entries.
fn listed_section(heading: &gtk::Box, list: &gtk::Box) -> gtk::Box {
    let section = gtk::Box::new(gtk::Orientation::Vertical, 8);
    section.append(heading);
    section.append(list);
    section.set_visible(false);
    section
}

fn settings_heading(title: &str, subtitle: &str) -> gtk::Box {
    let heading = gtk::Box::new(gtk::Orientation::Vertical, 3);
    // The page's own 14px spacing is the gap between rows within a section, so a
    // heading sat the same distance from the section above it as from its own
    // body, and read as belonging to the block before it.
    heading.set_margin_top(10);
    heading.append(
        &gtk::Label::builder()
            .label(title)
            .halign(gtk::Align::Start)
            .css_classes(["toolport-section-label"])
            .build(),
    );
    heading.append(
        &gtk::Label::builder()
            .label(subtitle)
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .hexpand(true)
            .css_classes(["toolport-muted"])
            .build(),
    );
    heading
}

/// Empty states go straight into a `toolport-settings-group`, which carries no
/// padding of its own: the rows inside it normally supply that via
/// `toolport-setting-row`. Without this the text sits flush against the border.
/// A `GtkSwitch` draws its knob from `active` but its "on" styling from `state`.
/// The handlers here return `Propagation::Stop`, which suppresses the default
/// handler that copies one to the other, and `set_active` to a value the switch
/// already holds emits no signal to fix it up later. So a switch the user just
/// flipped slid across and stayed grey. Always set both.
pub(super) fn set_switch(switch: &gtk::Switch, on: bool) {
    switch.set_active(on);
    switch.set_state(on);
}

fn empty_state(text: &str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 0);
    row.add_css_class("toolport-setting-row");
    row.append(
        &gtk::Label::builder()
            .label(text)
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .hexpand(true)
            .css_classes(["toolport-muted"])
            .build(),
    );
    row
}

fn setting_switch_row(title: &str, description: &str) -> (gtk::Box, gtk::Switch) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.add_css_class("toolport-setting-row");
    let copy = gtk::Box::new(gtk::Orientation::Vertical, 3);
    copy.set_hexpand(true);
    copy.append(
        &gtk::Label::builder()
            .label(title)
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .hexpand(true)
            .css_classes(["heading"])
            .build(),
    );
    copy.append(
        &gtk::Label::builder()
            .label(description)
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .hexpand(true)
            .css_classes(["toolport-muted"])
            .build(),
    );
    row.append(&copy);
    let toggle = gtk::Switch::builder().valign(gtk::Align::Center).build();
    row.append(&toggle);
    (row, toggle)
}

fn http_access_choice(
    profiles: &[(String, String)],
    selected: u32,
) -> Result<Option<String>, String> {
    match selected {
        0 => Ok(None),
        1 => Ok(Some(crate::registry::ALL_ENABLED_ACCESS.into())),
        index => profiles
            .get((index - 2) as usize)
            .map(|(id, _)| Some(id.clone()))
            .ok_or_else(|| "The access set is unavailable".into()),
    }
}

fn level_name(level: crate::registry::SafetyLevel) -> &'static str {
    match level {
        crate::registry::SafetyLevel::Off => "Off",
        crate::registry::SafetyLevel::Ask => "Ask",
        crate::registry::SafetyLevel::Strict => "Strict",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_access_picker_maps_named_sets_and_never_widens_an_invalid_choice() {
        let profiles = vec![
            ("default".into(), "Default".into()),
            ("work".into(), "Work".into()),
        ];
        assert_eq!(http_access_choice(&profiles, 0).unwrap(), None);
        assert_eq!(
            http_access_choice(&profiles, 1).unwrap().as_deref(),
            Some(crate::registry::ALL_ENABLED_ACCESS)
        );
        assert_eq!(
            http_access_choice(&profiles, 2).unwrap().as_deref(),
            Some("default")
        );
        assert_eq!(
            http_access_choice(&profiles, 3).unwrap().as_deref(),
            Some("work")
        );
        assert!(http_access_choice(&profiles, 4).is_err());
    }

    use crate::registry_controller::ReleaseAllSummary;

    fn blocked(profile: &str, tool: &str) -> QuarantinedTool {
        QuarantinedTool {
            profile: profile.to_string(),
            tool: tool.to_string(),
            detail: "definition changed".to_string(),
        }
    }

    #[test]
    fn posture_describes_the_effective_level_without_an_alarm_for_off() {
        let mut settings = crate::registry_controller::EssentialSettings::default();
        settings.safety_level = crate::registry::SafetyLevel::Off;
        let (line, guarded) = posture_summary(&settings);
        assert!(line.starts_with("Safety is set to Off."));
        assert!(line.contains("Server sign-in and client permissions"));
        assert!(!guarded);
        // A leftover 1.x mirror is not an effective policy.
        settings.confirm_destructive = true;
        assert_eq!(posture_summary(&settings).0, line);
        settings.safety_level = crate::registry::SafetyLevel::Ask;
        assert!(posture_summary(&settings)
            .0
            .contains("Destructive calls need your approval"));
        assert!(posture_summary(&settings).1);
        settings.quarantine_on_drift_forced = true;
        assert!(posture_summary(&settings)
            .0
            .contains("Your team also requires"));
    }

    #[test]
    fn bulk_release_covers_each_profile_scope_once_in_first_seen_order() {
        let entries = [
            blocked("", "a"),
            blocked("work", "b"),
            blocked("", "c"),
            blocked("work", "d"),
            blocked("home", "e"),
        ];
        assert_eq!(
            distinct_profiles(&entries),
            vec!["".to_string(), "work".to_string(), "home".to_string()]
        );
    }

    #[test]
    fn a_clean_bulk_release_reads_as_success() {
        let (message, is_error) = release_all_feedback(&ReleaseAllSummary {
            released: 3,
            ..ReleaseAllSummary::default()
        });
        assert_eq!(message, "Re-approved 3 tools.");
        assert!(!is_error);
    }

    #[test]
    fn skipped_tools_keep_the_outcome_an_error_because_they_are_still_blocked() {
        let (message, is_error) = release_all_feedback(&ReleaseAllSummary {
            released: 2,
            skipped: vec!["tool".to_string()],
            failed: Vec::new(),
        });
        assert_eq!(
            message,
            "Re-approved 2. 1 could not be repaired and is still blocked."
        );
        assert!(is_error);
    }

    #[test]
    fn a_failed_scope_outranks_skips_and_reports_what_did_get_through() {
        let (message, is_error) = release_all_feedback(&ReleaseAllSummary {
            released: 1,
            skipped: vec!["tool".to_string()],
            failed: vec!["store locked".to_string()],
        });
        assert_eq!(
            message,
            "Re-approved 1. 1 access-set scope could not be re-approved: store locked"
        );
        assert!(is_error);
    }
}

