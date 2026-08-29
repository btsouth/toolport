use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;

/// Autostart entry name for the native preview. Deliberately NOT "Toolport":
/// while both Linux shells are installable side by side, sharing the shipping
/// shell's autostart file would silently repoint the user's login launch at
/// whichever shell toggled last. The name merges back at the cutover release.
const NATIVE_AUTOSTART_NAME: &str = "ToolportNativePreview";

#[derive(Clone)]
pub(super) struct SettingsPage {
    pub(super) root: gtk::Box,
    bridge: super::http_bridge::BridgeController,
    broker: crate::approval_broker::ApprovalBroker,
    feedback: gtk::Label,
    posture: gtk::Label,
    lazy_discovery: gtk::Switch,
    pinned_section: gtk::Box,
    pinned_list: gtk::Box,
    code_mode: gtk::Switch,
    allow_routine_writes: gtk::Switch,
    allow_agent_control: gtk::Switch,
    live_inspect: gtk::Switch,
    deny_destructive: gtk::Switch,
    confirm_destructive: gtk::Switch,
    human_approval: gtk::Switch,
    content_defense: gtk::Switch,
    quarantine_on_drift: gtk::Switch,
    block_on_injection: gtk::Switch,
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
    folder_list: gtk::Box,
    folder_button: gtk::Button,
    routine_list: gtk::Box,
    quarantine_list: gtk::Box,
    allowed_list: gtk::Box,
    refresh_button: gtk::Button,
    updating: Rc<Cell<bool>>,
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
        page.set_margin_top(28);
        page.set_margin_bottom(28);
        page.set_margin_start(28);
        page.set_margin_end(28);
        page.append(
            &gtk::Label::builder()
                .label("Safety and desktop behavior")
                .halign(gtk::Align::Start)
                .css_classes(["title-2"])
                .build(),
        );
        page.append(
            &gtk::Label::builder()
                .label("These settings are shared with the gateway and the current Toolport app. Team-enforced protections stay locked on.")
                .halign(gtk::Align::Start)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["toolport-muted"])
                .build(),
        );
        let feedback = gtk::Label::builder()
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-feedback"])
            .build();
        feedback.set_label("Open Settings to load current values.");
        page.append(&feedback);

        // Security posture in one line, before the individual switches, so the
        // overall stance is legible without reading every toggle.
        let posture = gtk::Label::builder()
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .wrap(true)
            .visible(false)
            .css_classes(["toolport-feedback"])
            .build();
        page.append(&posture);

        page.append(&settings_heading(
            "Gateway capabilities",
            "Control how every connected AI client discovers and uses Toolport.",
        ));
        let capabilities = gtk::Box::new(gtk::Orientation::Vertical, 0);
        capabilities.add_css_class("toolport-settings-group");
        let (lazy_row, lazy_discovery) = setting_switch_row(
            "Lazy discovery",
            "Expose a small discovery toolkit instead of loading the full tool catalog up front.",
        );
        capabilities.append(&lazy_row);
        let (code_row, code_mode) = setting_switch_row(
            "Code mode",
            "Let agents combine multiple scoped tool calls in one sandboxed server-side script.",
        );
        capabilities.append(&code_row);
        let (routine_row, allow_routine_writes) = setting_switch_row(
            "Allow routine writes",
            "Let agents suggest persistent routines. Saving one still requires your approval.",
        );
        capabilities.append(&routine_row);
        page.append(&capabilities);

        let pinned_section = gtk::Box::new(gtk::Orientation::Vertical, 8);
        pinned_section.append(
            &gtk::Label::builder()
                .label("Pinned prerequisites")
                .halign(gtk::Align::Start)
                .css_classes(["heading"])
                .build(),
        );
        pinned_section.append(
            &gtk::Label::builder()
                .label("Tools pinned in Playground always surface in lazy discovery with their full schema.")
                .halign(gtk::Align::Start)
                .xalign(0.0)
                .wrap(true)
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
        page.append(&pinned_section);

        page.append(&settings_heading(
            "Tool-call safety",
            "Choose how Toolport handles risky tools before they reach an AI client.",
        ));
        let safety = gtk::Box::new(gtk::Orientation::Vertical, 0);
        safety.add_css_class("toolport-settings-group");
        let (deny_row, deny_destructive) = setting_switch_row(
            "Block destructive tools",
            "Hide and reject tools marked destructive across every server.",
        );
        safety.append(&deny_row);
        let (confirm_row, confirm_destructive) = setting_switch_row(
            "Agent confirmation",
            "Require the AI client to replay a one-time confirmation token.",
        );
        safety.append(&confirm_row);
        let (approval_row, human_approval) = setting_switch_row(
            "Human approval",
            "Hold gated calls until you approve or deny them in Toolport.",
        );
        safety.append(&approval_row);
        page.append(&safety);

        page.append(&settings_heading(
            "Content protection",
            "Local defenses for server definitions, tool results, and sensitive values.",
        ));
        let protection = gtk::Box::new(gtk::Orientation::Vertical, 0);
        protection.add_css_class("toolport-settings-group");
        let (content_row, content_defense) = setting_switch_row(
            "Content defense",
            "Detect and label prompt injection in untrusted tool results.",
        );
        protection.append(&content_row);
        let (block_row, block_on_injection) = setting_switch_row(
            "Block high-confidence injection",
            "Fail closed instead of returning a result with a high-confidence hit.",
        );
        protection.append(&block_row);
        let (drift_row, quarantine_on_drift) = setting_switch_row(
            "Quarantine risky tool drift",
            "Hide high-risk tools whose definitions changed until reviewed.",
        );
        protection.append(&drift_row);
        let (pii_row, pii_redaction) = setting_switch_row(
            "Pseudonymize PII",
            "Replace detected personal values before results reach the model.",
        );
        protection.append(&pii_row);
        let (agent_row, allow_agent_control) = setting_switch_row(
            "Allow agent control",
            "Let agents turn servers on or off. Destructive-tool blocking stays user-controlled.",
        );
        protection.append(&agent_row);
        let (inspect_row, live_inspect) = setting_switch_row(
            "Live request/response inspection",
            "Capture the last 50 tool calls locally for Activity. Turning this off clears the buffer.",
        );
        protection.append(&inspect_row);
        page.append(&protection);

        page.append(&settings_heading(
            "Desktop",
            "Linux-native lifecycle preferences.",
        ));
        let desktop = gtk::Box::new(gtk::Orientation::Vertical, 0);
        desktop.add_css_class("toolport-settings-group");
        let (launch_row, launch_at_login) = setting_switch_row(
            "Launch at login",
            "Start Toolport hidden so approvals and notifications remain available.",
        );
        desktop.append(&launch_row);
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
                .halign(gtk::Align::Start)
                .xalign(0.0)
                .wrap(true)
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
                .label("System-managed updates")
                .halign(gtk::Align::Start)
                .css_classes(["heading"])
                .build(),
        );
        updates_copy.append(
            &gtk::Label::builder()
                .label("The native Linux app never downloads or replaces itself. Update Toolport through Omarchy or your normal pacman upgrade flow.")
                .halign(gtk::Align::Start)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["toolport-muted"])
                .build(),
        );
        updates.append(&updates_copy);
        desktop.append(&updates);
        page.append(&desktop);

        page.append(&settings_heading(
            "Project folder routing",
            "Automatically use the matching server profile when an MCP client reports a project root. The longest matching folder wins.",
        ));
        let folder_actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        folder_actions.set_halign(gtk::Align::End);
        let folder_button = gtk::Button::with_label("Add folder mapping");
        folder_button.add_css_class("toolport-secondary-action");
        folder_actions.append(&folder_button);
        page.append(&folder_actions);
        let folder_list = gtk::Box::new(gtk::Orientation::Vertical, 8);
        folder_list.add_css_class("toolport-settings-group");
        folder_list.append(
            &gtk::Label::builder()
                .label("Checking folder mappings…")
                .halign(gtk::Align::Start)
                .css_classes(["toolport-muted"])
                .build(),
        );
        page.append(&folder_list);

        page.append(&settings_heading(
            "Diagnostics",
            "Copy a secret-safe support report or inspect Toolport's local files.",
        ));
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
                .halign(gtk::Align::Start)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["toolport-muted"])
                .build(),
        );
        diagnostics.append(&diagnostics_copy);
        let diagnostics_actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let copy_diagnostics = gtk::Button::with_label("Copy diagnostics");
        copy_diagnostics.add_css_class("toolport-secondary-action");
        diagnostics_actions.append(&copy_diagnostics);
        let open_data = gtk::Button::with_label("Open data folder");
        open_data.add_css_class("toolport-secondary-action");
        diagnostics_actions.append(&open_data);
        diagnostics.append(&diagnostics_actions);
        page.append(&diagnostics);

        page.append(&settings_heading(
            "Shared HTTP endpoint",
            "A supervised, authenticated local endpoint for clients that cannot launch an MCP process.",
        ));
        let endpoint = gtk::Box::new(gtk::Orientation::Horizontal, 12);
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
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .wrap(true)
            .selectable(true)
            .css_classes(["toolport-muted"])
            .build();
        endpoint_copy.append(&endpoint_status);
        endpoint.append(&endpoint_copy);
        let endpoint_button = gtk::Button::with_label("Start");
        endpoint_button.add_css_class("toolport-secondary-action");
        let endpoint_actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
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
        endpoint_actions.append(&copy_endpoint);
        endpoint_actions.append(&copy_endpoint_token);
        endpoint_actions.append(&reveal_endpoint_token);
        endpoint_actions.append(&endpoint_button);
        endpoint.append(&endpoint_actions);
        let endpoint_token_value = gtk::Label::builder()
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .wrap(true)
            .selectable(true)
            .visible(false)
            .css_classes(["toolport-muted", "caption", "monospace"])
            .build();
        endpoint.append(&endpoint_token_value);
        page.append(&endpoint);
        let http_client_actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        http_client_actions.set_halign(gtk::Align::End);
        let add_http_client = gtk::Button::with_label("Add scoped HTTP client");
        add_http_client.add_css_class("toolport-secondary-action");
        add_http_client.set_sensitive(false);
        http_client_actions.append(&add_http_client);
        page.append(&http_client_actions);
        let http_client_list = gtk::Box::new(gtk::Orientation::Vertical, 8);
        http_client_list.add_css_class("toolport-settings-group");
        http_client_list.append(
            &gtk::Label::builder()
                .label("Start the endpoint to manage scoped clients.")
                .halign(gtk::Align::Start)
                .css_classes(["toolport-muted"])
                .build(),
        );
        page.append(&http_client_list);

        page.append(&settings_heading(
            "Suggested routines",
            "Review value-free workflow suggestions before saving them as executable routines.",
        ));
        let routine_list = gtk::Box::new(gtk::Orientation::Vertical, 8);
        routine_list.add_css_class("toolport-settings-group");
        routine_list.append(
            &gtk::Label::builder()
                .label("Checking for routine suggestions…")
                .halign(gtk::Align::Start)
                .css_classes(["toolport-muted"])
                .build(),
        );
        page.append(&routine_list);

        page.append(&settings_heading(
            "Quarantined tools",
            "High-risk definition changes stay blocked until you explicitly re-approve them.",
        ));
        let quarantine_list = gtk::Box::new(gtk::Orientation::Vertical, 8);
        quarantine_list.add_css_class("toolport-settings-group");
        quarantine_list.append(
            &gtk::Label::builder()
                .label("Checking for blocked tools…")
                .halign(gtk::Align::Start)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["toolport-muted"])
                .build(),
        );
        page.append(&quarantine_list);

        page.append(&settings_heading(
            "Remembered approvals",
            "Fingerprint-bound exceptions that can skip the human approval prompt.",
        ));
        let allowed_list = gtk::Box::new(gtk::Orientation::Vertical, 8);
        allowed_list.add_css_class("toolport-settings-group");
        allowed_list.append(
            &gtk::Label::builder()
                .label("Checking remembered approvals…")
                .halign(gtk::Align::Start)
                .css_classes(["toolport-muted"])
                .build(),
        );
        page.append(&allowed_list);

        scroller.set_child(Some(&page));
        root.append(&scroller);
        let settings_page = Self {
            root,
            bridge,
            broker,
            feedback,
            posture,
            lazy_discovery,
            pinned_section,
            pinned_list,
            code_mode,
            allow_routine_writes,
            allow_agent_control,
            live_inspect,
            deny_destructive,
            confirm_destructive,
            human_approval,
            content_defense,
            quarantine_on_drift,
            block_on_injection,
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
            folder_list,
            folder_button,
            routine_list,
            quarantine_list,
            allowed_list,
            refresh_button,
            updating: Rc::new(Cell::new(false)),
        };
        settings_page.connect_switches();
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

    fn render_folder_routing(&self, settings: crate::registry_controller::FolderRoutingSettings) {
        while let Some(child) = self.folder_list.first_child() {
            self.folder_list.remove(&child);
        }
        self.folder_button
            .set_sensitive(!settings.profiles.is_empty());
        if settings.profiles.is_empty() {
            self.folder_list.append(
                &gtk::Label::builder()
                    .label("Create a server profile before adding project folder routing.")
                    .halign(gtk::Align::Start)
                    .xalign(0.0)
                    .wrap(true)
                    .css_classes(["toolport-muted"])
                    .build(),
            );
            return;
        }
        if settings.mappings.is_empty() {
            self.folder_list.append(
                &gtk::Label::builder()
                    .label("No project folders are mapped yet.")
                    .halign(gtk::Align::Start)
                    .css_classes(["toolport-muted"])
                    .build(),
            );
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
            self.pinned_list.append(
                &gtk::Label::builder()
                    .label("No prerequisites are pinned. Pin a load-bearing tool from Playground when lazy discovery must always surface it.")
                    .halign(gtk::Align::Start)
                    .xalign(0.0)
                    .wrap(true)
                    .css_classes(["toolport-muted"])
                    .build(),
            );
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
            self.show_error("create a server profile before adding folder routing");
            return;
        }
        #[allow(deprecated)]
        let dialog = adw::MessageDialog::new(
            Some(&parent),
            Some("Choose a server profile"),
            Some("This profile will be selected when a client reports this folder or one of its descendants."),
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
                    page.show_error("choose a server profile");
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
            self.http_client_list.append(
                &gtk::Label::builder()
                    .label("No scoped HTTP clients are registered.")
                    .halign(gtk::Align::Start)
                    .css_classes(["toolport-muted"])
                    .build(),
            );
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
                Some("Toolport generates a bearer token that is shown once. Choose which server profile this client can access."),
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
            let mut profile_names = vec!["All enabled servers"];
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
                    let selected = profile.selected();
                    let profile_id = if selected == 0 {
                        None
                    } else {
                        profiles
                            .get(selected.saturating_sub(1) as usize)
                            .map(|(id, _)| id.clone())
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
                self.allow_routine_writes.clone(),
                crate::registry_controller::EssentialSetting::AllowRoutineWrites,
                "routine writes",
            ),
            (
                self.allow_agent_control.clone(),
                crate::registry_controller::EssentialSetting::AllowAgentControl,
                "agent control",
            ),
            (
                self.live_inspect.clone(),
                crate::registry_controller::EssentialSetting::LiveInspect,
                "live inspection",
            ),
            (
                self.deny_destructive.clone(),
                crate::registry_controller::EssentialSetting::DenyDestructive,
                "destructive-tool blocking",
            ),
            (
                self.confirm_destructive.clone(),
                crate::registry_controller::EssentialSetting::ConfirmDestructive,
                "agent confirmation",
            ),
            (
                self.human_approval.clone(),
                crate::registry_controller::EssentialSetting::HumanApproval,
                "human approval",
            ),
            (
                self.content_defense.clone(),
                crate::registry_controller::EssentialSetting::ContentDefense,
                "content defense",
            ),
            (
                self.quarantine_on_drift.clone(),
                crate::registry_controller::EssentialSetting::QuarantineOnDrift,
                "drift quarantine",
            ),
            (
                self.block_on_injection.clone(),
                crate::registry_controller::EssentialSetting::BlockOnInjection,
                "injection blocking",
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
                let page = page.clone();
                gtk::glib::spawn_future_local(async move {
                    let result = gtk::gio::spawn_blocking(move || {
                        crate::registry_controller::set_essential_setting(setting, enabled)
                    })
                    .await;
                    match result {
                        Ok(Ok(settings)) => {
                            page.render_settings(settings);
                            page.feedback.set_label(&format!(
                                "{} {label}",
                                if enabled { "Enabled" } else { "Disabled" }
                            ));
                            page.feedback.remove_css_class("error");
                            page.feedback.add_css_class("success");
                        }
                        Ok(Err(error)) => {
                            page.show_error(&error);
                            page.refresh();
                        }
                        Err(_) => {
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
                let page = page.clone();
                gtk::glib::spawn_future_local(async move {
                    let result = gtk::gio::spawn_blocking(move || {
                        if enabled {
                            crate::autostart::enable_linux(NATIVE_AUTOSTART_NAME)
                        } else {
                            crate::autostart::disable_linux(NATIVE_AUTOSTART_NAME)
                        }
                    })
                    .await;
                    match result {
                        Ok(Ok(())) => {
                            page.updating.set(true);
                            page.launch_at_login.set_active(enabled);
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

    pub(super) fn refresh(&self) {
        self.refresh_with_feedback(true)
    }

    /// Background cadence refresh: same reads, but no "Loading" flash and no
    /// success line, so a 15-second tick never talks over feedback the user is
    /// actually reading.
    pub(super) fn refresh_quietly(&self) {
        self.refresh_with_feedback(false)
    }

    fn refresh_with_feedback(&self, announce: bool) {
        if self.updating.replace(true) {
            return;
        }
        self.refresh_button.set_sensitive(false);
        if announce {
            self.feedback.set_label("Loading settings…");
        }
        let page = self.clone();
        let broker = self.broker.clone();
        let bridge = self.bridge.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                Ok::<_, String>((
                    crate::registry_controller::essential_settings()?,
                    crate::autostart::is_enabled_linux(NATIVE_AUTOSTART_NAME)?,
                    read_quarantined_tools()?,
                    read_allowed_tools(&broker)?,
                    broker.list_suggestions(),
                    crate::registry_controller::folder_routing_settings()?,
                    crate::registry_controller::http_client_settings()?,
                    crate::registry_controller::pinned_prerequisites()?,
                    bridge.restart_advice(),
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
                    suggestions,
                    folder_routing,
                    http_clients,
                    pinned,
                    restart_advice,
                ))) => {
                    page.render_settings(settings);
                    page.render_restart_advice(restart_advice);
                    page.updating.set(true);
                    page.launch_at_login.set_active(launch_at_login);
                    page.launch_at_login.set_sensitive(true);
                    page.updating.set(false);
                    page.render_endpoint(page.bridge.status());
                    page.render_quarantine(quarantined);
                    page.render_allowed(allowed);
                    page.render_routines(suggestions);
                    page.render_folder_routing(folder_routing);
                    page.render_http_clients(http_clients);
                    page.render_pinned_prerequisites(pinned);
                    if announce {
                        page.feedback.set_label("Settings are up to date.");
                        page.feedback.remove_css_class("error");
                        page.feedback.add_css_class("success");
                    }
                }
                Ok(Err(error)) => {
                    page.updating.set(false);
                    page.show_error(&error);
                }
                Err(_) => {
                    page.updating.set(false);
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
        self.posture
            .add_css_class(if guarded { "success" } else { "error" });
        self.posture.set_visible(true);
        self.updating.set(true);
        self.lazy_discovery.set_active(settings.lazy_discovery);
        self.lazy_discovery.set_sensitive(true);
        self.pinned_section.set_visible(settings.lazy_discovery);
        self.code_mode.set_active(settings.code_mode);
        self.code_mode.set_sensitive(true);
        self.allow_routine_writes
            .set_active(settings.allow_routine_writes);
        self.allow_routine_writes.set_sensitive(settings.code_mode);
        self.allow_routine_writes.set_tooltip_text(
            (!settings.code_mode).then_some("Enable code mode to allow routine writes"),
        );
        self.allow_agent_control
            .set_active(settings.allow_agent_control);
        self.allow_agent_control.set_sensitive(true);
        self.live_inspect.set_active(settings.live_inspect);
        self.live_inspect.set_sensitive(true);
        self.deny_destructive.set_active(settings.deny_destructive);
        set_team_managed(&self.deny_destructive, settings.deny_destructive_forced);
        self.confirm_destructive
            .set_active(settings.confirm_destructive);
        set_team_managed(&self.confirm_destructive, false);
        self.human_approval.set_active(settings.human_approval);
        set_team_managed(&self.human_approval, settings.human_approval_forced);
        self.content_defense.set_active(settings.content_defense);
        set_team_managed(&self.content_defense, settings.content_defense_forced);
        self.quarantine_on_drift
            .set_active(settings.quarantine_on_drift);
        set_team_managed(
            &self.quarantine_on_drift,
            settings.quarantine_on_drift_forced,
        );
        self.block_on_injection
            .set_active(settings.block_on_injection);
        set_team_managed(&self.block_on_injection, settings.block_on_injection_forced);
        self.pii_redaction.set_active(settings.pii_redaction);
        set_team_managed(&self.pii_redaction, settings.pii_redaction_forced);
        self.updating.set(false);
    }

    fn show_error(&self, error: &str) {
        self.feedback.set_label(&format!("Settings error: {error}"));
        self.feedback.remove_css_class("success");
        self.feedback.add_css_class("error");
    }

    fn render_quarantine(&self, entries: Vec<QuarantinedTool>) {
        while let Some(child) = self.quarantine_list.first_child() {
            self.quarantine_list.remove(&child);
        }
        if entries.is_empty() {
            self.quarantine_list.append(
                &gtk::Label::builder()
                    .label("No tools are quarantined.")
                    .halign(gtk::Align::Start)
                    .xalign(0.0)
                    .css_classes(["toolport-muted"])
                    .build(),
            );
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
        if entries.is_empty() {
            self.allowed_list.append(
                &gtk::Label::builder()
                    .label("No remembered approvals.")
                    .halign(gtk::Align::Start)
                    .css_classes(["toolport-muted"])
                    .build(),
            );
            return;
        }
        for entry in entries {
            self.allowed_list.append(&allowed_row(entry, self.clone()));
        }
    }

    fn render_routines(&self, suggestions: Vec<crate::routines::RoutineSuggestion>) {
        while let Some(child) = self.routine_list.first_child() {
            self.routine_list.remove(&child);
        }
        if suggestions.is_empty() {
            self.routine_list.append(
                &gtk::Label::builder()
                    .label("No routine suggestions are waiting for review.")
                    .halign(gtk::Align::Start)
                    .xalign(0.0)
                    .wrap(true)
                    .css_classes(["toolport-muted"])
                    .build(),
            );
            return;
        }
        for suggestion in suggestions {
            self.routine_list
                .append(&routine_row(suggestion, self.clone()));
        }
    }
}

fn routine_row(suggestion: crate::routines::RoutineSuggestion, page: SettingsPage) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 10);
    card.add_css_class("toolport-setting-row");

    let name = gtk::Entry::builder()
        .text(&suggestion.suggested_name)
        .placeholder_text("Routine name")
        .build();
    card.append(&name);
    let description = gtk::Entry::builder()
        .placeholder_text("Description (optional)")
        .build();
    card.append(&description);

    let dependencies = suggestion
        .evidence
        .observed_dependencies()
        .iter()
        .map(|dependency| dependency.name())
        .collect::<Vec<_>>()
        .join(", ");
    let provenance = match suggestion.evidence.provenance() {
        crate::routines::EvidenceProvenance::ImmutableRun => "executed end to end",
        crate::routines::EvidenceProvenance::SynthesizedFromObservedCalls => {
            "synthesized from observed calls"
        }
    };
    let risk = match suggestion.evidence.risk_class() {
        crate::routines::RoutineRiskClass::Low => "low",
        crate::routines::RoutineRiskClass::Medium => "medium",
        crate::routines::RoutineRiskClass::High => "high",
        crate::routines::RoutineRiskClass::Unknown => "unknown",
    };
    card.append(
        &gtk::Label::builder()
            .label(format!(
                "{} calls · {risk} risk · {provenance}\nDependencies: {dependencies}",
                suggestion.evidence.calls()
            ))
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-muted"])
            .build(),
    );

    let source_view = gtk::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::None)
        .top_margin(10)
        .bottom_margin(10)
        .left_margin(10)
        .right_margin(10)
        .build();
    source_view.buffer().set_text(&suggestion.source);
    let source_scroll = gtk::ScrolledWindow::builder()
        .child(&source_view)
        .min_content_height(140)
        .max_content_height(240)
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .build();
    let source = gtk::Expander::builder()
        .label("Review source")
        .child(&source_scroll)
        .build();
    card.append(&source);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    let dismiss = gtk::Button::with_label("Dismiss");
    dismiss.add_css_class("toolport-secondary-action");
    actions.append(&dismiss);
    let save = gtk::Button::with_label("Save routine");
    save.add_css_class("suggested-action");
    actions.append(&save);
    card.append(&actions);

    let fingerprint = suggestion.definition_fingerprint.clone();
    let page_for_dismiss = page.clone();
    dismiss.connect_clicked(move |_| {
        crate::routine_controller::dismiss_suggestion(&page_for_dismiss.broker, &fingerprint);
        page_for_dismiss
            .feedback
            .set_label("Routine suggestion dismissed.");
        page_for_dismiss.feedback.remove_css_class("error");
        page_for_dismiss.feedback.add_css_class("success");
        page_for_dismiss.refresh();
    });

    let fingerprint = suggestion.definition_fingerprint;
    save.connect_clicked(move |button| {
        let routine_name = name.text().to_string();
        let routine_description = description.text().to_string();
        let fingerprint = fingerprint.clone();
        button.set_sensitive(false);
        dismiss.set_sensitive(false);
        let broker = page.broker.clone();
        let page = page.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                crate::routine_controller::approve_suggestion(
                    &broker,
                    &fingerprint,
                    routine_name,
                    Some(routine_description),
                )
            })
            .await;
            match result {
                Ok(Ok(saved)) => {
                    page.feedback
                        .set_label(&format!("Saved routine ‘{}’.", saved.name()));
                    page.feedback.remove_css_class("error");
                    page.feedback.add_css_class("success");
                    page.refresh();
                }
                Ok(Err(error)) => {
                    page.show_error(&error);
                    page.refresh();
                }
                Err(_) => {
                    page.show_error("the routine save stopped unexpectedly");
                    page.refresh();
                }
            }
        });
    });
    card
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

/// The one-line security stance, mirroring the shipping card: a hard gate
/// (human approval, destructive deny, or injection block) reads as guarded;
/// softer measures alone read as partial; nothing reads as open.
fn posture_summary(settings: &crate::registry_controller::EssentialSettings) -> (String, bool) {
    let active: Vec<&str> = [
        (settings.human_approval, "human approval on"),
        (settings.deny_destructive, "destructive tools denied"),
        (settings.confirm_destructive, "destructive calls ask first"),
        (settings.quarantine_on_drift, "changed tools paused"),
        (settings.block_on_injection, "injection-like output blocked"),
    ]
    .into_iter()
    .filter_map(|(on, label)| on.then_some(label))
    .collect();
    let gated = settings.human_approval || settings.deny_destructive || settings.block_on_injection;
    if active.is_empty() {
        return (
            "Approval gates are off. Tool calls run without a Toolport approval or blocking gate."
                .to_string(),
            false,
        );
    }
    let label = if gated {
        "Guardrails active"
    } else {
        "Some guardrails active"
    };
    (format!("{label}. Active: {}.", active.join(", ")), gated)
}

/// Which profile scopes a re-approve-all pass must clear, in first-seen order.
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
                "Re-approved {released}. {failures} profile {} could not be re-approved: {}",
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

fn quarantine_bulk_row(entries: &[QuarantinedTool], page: SettingsPage) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.add_css_class("toolport-setting-row");
    let count = entries.len();
    let copy = gtk::Label::builder()
        .label(format!("{count} tools are blocked."))
        .halign(gtk::Align::Start)
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
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["heading"])
            .build(),
    );
    let scope = if entry.profile.is_empty() {
        "All profiles".to_string()
    } else {
        format!("Profile: {}", entry.profile)
    };
    copy.append(
        &gtk::Label::builder()
            .label(format!("{} · {}", entry.detail, scope))
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .wrap(true)
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
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .wrap(true)
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

fn settings_heading(title: &str, subtitle: &str) -> gtk::Box {
    let heading = gtk::Box::new(gtk::Orientation::Vertical, 3);
    heading.append(
        &gtk::Label::builder()
            .label(title)
            .halign(gtk::Align::Start)
            .css_classes(["heading"])
            .build(),
    );
    heading.append(
        &gtk::Label::builder()
            .label(subtitle)
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-muted"])
            .build(),
    );
    heading
}

fn setting_switch_row(title: &str, description: &str) -> (gtk::Box, gtk::Switch) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.add_css_class("toolport-setting-row");
    let copy = gtk::Box::new(gtk::Orientation::Vertical, 3);
    copy.set_hexpand(true);
    copy.append(
        &gtk::Label::builder()
            .label(title)
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["heading"])
            .build(),
    );
    copy.append(
        &gtk::Label::builder()
            .label(description)
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-muted"])
            .build(),
    );
    row.append(&copy);
    let toggle = gtk::Switch::builder().valign(gtk::Align::Center).build();
    row.append(&toggle);
    (row, toggle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry_controller::ReleaseAllSummary;

    fn blocked(profile: &str, tool: &str) -> QuarantinedTool {
        QuarantinedTool {
            profile: profile.to_string(),
            tool: tool.to_string(),
            detail: "definition changed".to_string(),
        }
    }

    #[test]
    fn posture_reads_guarded_partial_or_open_by_gate_strength() {
        let mut settings = crate::registry_controller::EssentialSettings::default();
        let (line, guarded) = posture_summary(&settings);
        assert!(line.starts_with("Approval gates are off."));
        assert!(!guarded);
        settings.confirm_destructive = true;
        let (line, guarded) = posture_summary(&settings);
        assert_eq!(
            line,
            "Some guardrails active. Active: destructive calls ask first."
        );
        assert!(!guarded);
        settings.human_approval = true;
        let (line, guarded) = posture_summary(&settings);
        assert_eq!(
            line,
            "Guardrails active. Active: human approval on, destructive calls ask first."
        );
        assert!(guarded);
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
            "Re-approved 1. 1 profile scope could not be re-approved: store locked"
        );
        assert!(is_error);
    }
}
