use super::i18n as l10n;
use crate::i18n::{lookup, msg, tr, trf};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;

const SAFETY_ALL: &[&str] = &[msg("Off"), msg("Ask"), msg("Strict")];
const SAFETY_ASK: &[&str] = &[msg("Ask"), msg("Strict")];
const SAFETY_STRICT: &[&str] = &[msg("Strict")];

/// Autostart entry name. The preview used its own so it could not repoint the
/// shipping shell's login launch while both were installable; the GTK shell now
/// replaces that shell on Linux, so the names have merged as planned.
/// [`super::migrate_preview_identity`] retires the old file.
pub(super) const NATIVE_AUTOSTART_NAME: &str = "Toolport";
/// What the preview wrote. Only referenced by the migration.
pub(super) const LEGACY_AUTOSTART_NAME: &str = "ToolportNativePreview";

pub(super) const SETTINGS_PAGES: &[(&str, &str)] = &[
    (msg("General"), "user-home-symbolic"),
    (msg("Tools"), "applications-engineering-symbolic"),
    (msg("Safety"), "security-high-symbolic"),
    (msg("Access"), "dialog-password-symbolic"),
    (msg("Connections"), "network-wired-symbolic"),
    (msg("Help and data"), "help-browser-symbolic"),
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
    errors: gtk::Box,
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
    baseline_notice: gtk::Label,
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
    quarantine_release_generation: Rc<Cell<u64>>,
}

impl SettingsPage {
    pub(super) fn new(
        bridge: super::http_bridge::BridgeController,
        broker: crate::approval_broker::ApprovalBroker,
    ) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("toolport-content");
        let header = super::page_header::PageHeader::new(
            msg("Settings"),
            msg("Global discovery and security policy"),
        );
        let refresh_button = super::header_icon_button("view-refresh-symbolic");
        l10n::bind_tooltip(&refresh_button, "Refresh settings");
        header.add_action(&refresh_button);
        root.append(&header.root);

        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        let page = super::page_body(14);
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
        feedback.connect_notify_local(Some("label"), |label, _| {
            let weak = label.downgrade();
            let text = label.label().to_string();
            gtk::glib::idle_add_local_once(move || {
                if let Some(label) = weak.upgrade() {
                    if !text.is_empty()
                        && label.label() == text
                        && label.has_css_class("success")
                        && super::show_toast(&label, &text)
                    {
                        label.set_visible(false);
                    }
                }
            });
        });
        feedback.set_label("");
        feedback.set_visible(false);
        page.append(&feedback);
        let errors = gtk::Box::new(gtk::Orientation::Vertical, 0);
        page.append(&errors);

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
        capabilities.add_css_class("toolport-switch-group");
        let (lazy_row, lazy_discovery) = setting_switch_row(
            msg("Find tools as needed"),
            msg("Agents search for tools as needed instead of loading the full list. This is the default for connections without saved client settings. Choose a different behavior in Clients."),
        );
        capabilities.append(&lazy_row);
        let (code_row, code_mode) = setting_switch_row(
            msg("Code mode"),
            msg("Let agents combine several tool calls in one script to reduce back-and-forth. Each call follows your access and approval settings. Scripts run in a restricted environment, but this does not replace those settings."),
        );
        l10n::bind_tooltip(&code_row, msg("A gateway started with TOOLPORT_CODE_MODE=1 can keep scripts available even when this setting is off."));

        let pinned_section = gtk::Box::new(gtk::Orientation::Vertical, 8);
        pinned_section.append(&source_label(
            msg("Tools always included"),
            source_label(
                msg("Tools always included"),
                gtk::Label::builder()
                    .label(tr("Tools always included"))
                    .halign(gtk::Align::Start)
                    .css_classes(["heading"])
                    .build(),
            ),
        ));
        pinned_section.append(
            &source_label(msg("Tools pinned in Servers > Tools are included in every tool search, even when they do not match the search."), source_label(msg("Tools pinned in Servers > Tools are included in every tool search, even when they do not match the search."), gtk::Label::builder()
                .label(tr("Tools pinned in Servers > Tools are included in every tool search, even when they do not match the search."))
                .halign(gtk::Align::Fill)
                .xalign(0.0)
                .wrap(true)
            .hexpand(true)
                .css_classes(["toolport-muted"])
                .build())),
        );
        let pinned_list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        pinned_list.add_css_class("toolport-settings-group");
        pinned_list.append(&source_label(
            msg("Checking pinned tools…"),
            source_label(
                msg("Checking pinned tools…"),
                gtk::Label::builder()
                    .label(tr("Checking pinned tools…"))
                    .halign(gtk::Align::Start)
                    .css_classes(["toolport-muted"])
                    .build(),
            ),
        ));
        pinned_section.append(&pinned_list);

        let safety = gtk::Box::new(gtk::Orientation::Vertical, 0);
        safety.add_css_class("toolport-settings-group");
        let safety_level = gtk::DropDown::new(None::<gtk::StringList>, gtk::Expression::NONE);
        l10n::bind_dropdown_choices(&safety_level, SAFETY_ALL);
        l10n::bind_tooltip(&safety_level, msg("Ask holds destructive calls. Strict also blocks destructive tools, risky drift and high-confidence injection, and asks before untrusted calls. Labeling and integrity recording stay on."));
        safety_level.set_visible(false);
        safety.append(&safety_level);
        let cards = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let mut safety_cards: Vec<gtk::CheckButton> = Vec::new();
        for (index, (name, description)) in [
            (msg("Off"), msg("No extra checks before destructive calls.")),
            (
                msg("Ask"),
                msg("Pause destructive calls for your approval."),
            ),
            (
                msg("Strict"),
                msg("Hide destructive tools and ask before untrusted calls."),
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let card = gtk::CheckButton::new();
            if let Some(first) = safety_cards.first() {
                card.set_group(Some(first));
            }
            card.set_hexpand(true);
            card.add_css_class("toolport-safety-card");
            let copy = gtk::Box::new(gtk::Orientation::Vertical, 8);
            copy.append(&source_label(
                name,
                gtk::Label::builder()
                    .label(name)
                    .xalign(0.0)
                    .css_classes(["heading"])
                    .build(),
            ));
            copy.append(&source_label(
                description,
                gtk::Label::builder()
                    .label(description)
                    .xalign(0.0)
                    .wrap(true)
                    .max_width_chars(24)
                    .css_classes(["toolport-muted"])
                    .build(),
            ));
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
        let safety_kept_reset = gtk::Button::builder().halign(gtk::Align::Start).build();
        safety_kept.append(&safety_kept_reset);
        safety_kept.set_visible(false);
        safety.append(&safety_kept);
        let protection = gtk::Box::new(gtk::Orientation::Vertical, 0);
        protection.add_css_class("toolport-settings-group");
        protection.add_css_class("toolport-switch-group");
        capabilities.append(&code_row);
        let (pii_row, pii_redaction) = setting_switch_row(
            msg("Pseudonymize PII"),
            msg("Replace detected personal values before results reach the model."),
        );
        protection.append(&pii_row);
        let (inspect_row, live_inspect) = setting_switch_row(
            msg("Live request/response inspection"),
            msg("Capture the last 50 tool calls locally for Activity. Turning this off clears the buffer."),
        );
        protection.append(&inspect_row);
        let approvals_heading = settings_heading(
            msg("Remembered approvals"),
            msg("Tool calls you chose to always allow. They stop applying if the tool's definition changes."),
        );
        let allowed_list = gtk::Box::new(gtk::Orientation::Vertical, 8);
        allowed_list.add_css_class("toolport-settings-group");
        allowed_list.append(&source_label(
            msg("Checking remembered approvals…"),
            source_label(
                msg("Checking remembered approvals…"),
                gtk::Label::builder()
                    .label(tr("Checking remembered approvals…"))
                    .halign(gtk::Align::Start)
                    .css_classes(["toolport-muted"])
                    .build(),
            ),
        ));

        let access_list = gtk::Box::new(gtk::Orientation::Vertical, 12);
        let folder_button = l10n::button("Add folder mapping");
        folder_button.add_css_class("toolport-secondary-action");
        folder_button.set_valign(gtk::Align::Center);
        let folder_heading = settings_heading_with_action(
            msg("Project folder routing"),
            msg("Automatically use the matching access set when an MCP client reports a project root. The longest matching folder wins."),
            &folder_button,
        );
        // Framed like the access set cards above it.
        let folder_list = access_group();
        folder_list.append(&source_label(
            msg("Checking folder mappings…"),
            source_label(
                msg("Checking folder mappings…"),
                gtk::Label::builder()
                    .label(tr("Checking folder mappings…"))
                    .halign(gtk::Align::Fill)
                    .xalign(0.0)
                    .css_classes(["toolport-muted", "toolport-setting-row"])
                    .build(),
            ),
        ));

        let desktop = gtk::Box::new(gtk::Orientation::Vertical, 0);
        desktop.add_css_class("toolport-settings-group");
        let (launch_row, launch_at_login) = setting_switch_row(
            msg("Launch at login"),
            msg("Start Toolport hidden so approvals and notifications remain available."),
        );
        let stale_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        stale_row.add_css_class("toolport-setting-row");
        let stale_copy = gtk::Box::new(gtk::Orientation::Vertical, 3);
        stale_copy.set_hexpand(true);
        stale_copy.append(&source_label(
            msg("Old gateway processes"),
            source_label(
                msg("Old gateway processes"),
                gtk::Label::builder()
                    .label(tr("Old gateway processes"))
                    .halign(gtk::Align::Start)
                    .css_classes(["heading"])
                    .build(),
            ),
        ));
        stale_copy.append(
            &source_label(msg("Stop gateways left behind by an upgrade without interrupting the current endpoint."), source_label(msg("Stop gateways left behind by an upgrade without interrupting the current endpoint."), gtk::Label::builder()
                .label(tr("Stop gateways left behind by an upgrade without interrupting the current endpoint."))
                .halign(gtk::Align::Fill)
                .xalign(0.0)
                .wrap(true)
            .hexpand(true)
                .css_classes(["toolport-muted"])
                .build())),
        );
        stale_row.append(&stale_copy);
        let stop_stale = l10n::button("Stop old gateways");
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
        updates_copy.append(&source_label(
            msg("Updates"),
            source_label(
                msg("Updates"),
                gtk::Label::builder()
                    .label(tr("Updates"))
                    .halign(gtk::Align::Start)
                    .css_classes(["heading"])
                    .build(),
            ),
        ));
        let update_advice = gtk::Label::builder()
            .label(lookup(super::package_updates::generic_advice()))
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .hexpand(true)
            .css_classes(["toolport-muted"])
            .build();
        l10n::bind_label(&update_advice, super::package_updates::generic_advice());
        updates_copy.append(&update_advice);
        gtk::glib::spawn_future_local(async move {
            if let Ok(advice) =
                gtk::gio::spawn_blocking(super::package_updates::update_advice).await
            {
                l10n::bind_label(&update_advice, advice);
            }
        });
        updates_copy.append(&source_button(
            msg("Open release page"),
            gtk::LinkButton::builder()
                .label(tr("Open release page"))
                .uri(super::package_updates::RELEASE_PAGE)
                .halign(gtk::Align::Start)
                .build(),
        ));
        updates.append(&updates_copy);

        let diagnostics = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        diagnostics.add_css_class("toolport-setting-row");
        let diagnostics_copy = gtk::Box::new(gtk::Orientation::Vertical, 3);
        diagnostics_copy.set_hexpand(true);
        diagnostics_copy.append(&source_label(
            msg("Support and local data"),
            source_label(
                msg("Support and local data"),
                gtk::Label::builder()
                    .label(tr("Support and local data"))
                    .halign(gtk::Align::Start)
                    .css_classes(["heading"])
                    .build(),
            ),
        ));
        diagnostics_copy.append(
            &source_label(msg("Diagnostics redact secrets. The data folder contains your registry, audit, and gateway logs."), source_label(msg("Diagnostics redact secrets. The data folder contains your registry, audit, and gateway logs."), gtk::Label::builder()
                .label(tr("Diagnostics redact secrets. The data folder contains your registry, audit, and gateway logs."))
                .halign(gtk::Align::Fill)
                .xalign(0.0)
                .wrap(true)
            .hexpand(true)
                .css_classes(["toolport-muted"])
                .build())),
        );
        diagnostics.append(&diagnostics_copy);
        let diagnostics_actions = action_wrap();
        let copy_diagnostics = l10n::button("Copy diagnostics");
        copy_diagnostics.add_css_class("toolport-secondary-action");
        diagnostics_actions.insert(&copy_diagnostics, -1);
        let open_data = l10n::button("Open data folder");
        open_data.add_css_class("toolport-secondary-action");
        diagnostics_actions.insert(&open_data, -1);
        diagnostics.append(&diagnostics_actions);

        let endpoint_heading = settings_heading(
            msg("Shared HTTP endpoint"),
            msg("A supervised, authenticated local endpoint for clients that cannot launch an MCP process."),
        );
        let endpoint = gtk::Box::new(gtk::Orientation::Vertical, 8);
        endpoint.add_css_class("toolport-setting-row");
        let endpoint_copy = gtk::Box::new(gtk::Orientation::Vertical, 3);
        endpoint_copy.set_hexpand(true);
        endpoint_copy.append(&source_label(
            msg("Local HTTP gateway"),
            source_label(
                msg("Local HTTP gateway"),
                gtk::Label::builder()
                    .label(tr("Local HTTP gateway"))
                    .halign(gtk::Align::Start)
                    .css_classes(["heading"])
                    .build(),
            ),
        ));
        let endpoint_status = source_label(
            msg("Checking endpoint…"),
            source_label(
                msg("Checking endpoint…"),
                gtk::Label::builder()
                    .label(tr("Checking endpoint…"))
                    .halign(gtk::Align::Fill)
                    .xalign(0.0)
                    .wrap(true)
                    .hexpand(true)
                    .selectable(true)
                    .css_classes(["toolport-muted"])
                    .build(),
            ),
        );
        endpoint_copy.append(&endpoint_status);
        endpoint.append(&endpoint_copy);
        let endpoint_button = l10n::button("Start");
        endpoint_button.add_css_class("suggested-action");
        let endpoint_actions = action_wrap();
        let copy_endpoint = l10n::button("Copy URL");
        copy_endpoint.add_css_class("toolport-secondary-action");
        copy_endpoint.set_sensitive(false);
        let copy_endpoint_token = l10n::button("Copy token");
        copy_endpoint_token.add_css_class("toolport-secondary-action");
        copy_endpoint_token.set_sensitive(false);
        copy_endpoint_token
            .set_tooltip_text(Some(&tr("Copy the private administrator bearer token")));
        let reveal_endpoint_token = source_button(
            msg("Show token"),
            gtk::ToggleButton::with_label("Show token"),
        );
        reveal_endpoint_token.add_css_class("toolport-secondary-action");
        reveal_endpoint_token.set_sensitive(false);
        reveal_endpoint_token.set_tooltip_text(Some(&tr(
            "Reveal the administrator bearer token on screen; hide it again with the same button",
        )));
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
        let add_http_client = l10n::button("Add scoped HTTP client");
        add_http_client.add_css_class("toolport-secondary-action");
        add_http_client.set_valign(gtk::Align::Center);
        add_http_client.set_sensitive(false);
        let http_client_heading = settings_heading_with_action(
            msg("Scoped HTTP clients"),
            msg("Each gets its own bearer token and server scope, so one endpoint can serve several clients."),
            &add_http_client,
        );
        let http_client_list = access_group();
        http_client_list.append(&source_label(
            msg("Start the endpoint to manage scoped clients."),
            source_label(
                msg("Start the endpoint to manage scoped clients."),
                gtk::Label::builder()
                    .label(tr("Start the endpoint to manage scoped clients."))
                    .halign(gtk::Align::Fill)
                    .xalign(0.0)
                    .wrap(true)
                    .hexpand(true)
                    .css_classes(["toolport-muted", "toolport-setting-row"])
                    .build(),
            ),
        ));

        let quarantine_heading = settings_heading(
            msg("Quarantined tools"),
            msg("Toolport keeps high-risk definition changes here until you re-approve them."),
        );
        let quarantine_list = gtk::Box::new(gtk::Orientation::Vertical, 8);
        quarantine_list.add_css_class("toolport-settings-group");
        quarantine_list.append(&source_label(
            msg("Checking retained quarantines…"),
            source_label(
                msg("Checking retained quarantines…"),
                gtk::Label::builder()
                    .label(tr("Checking retained quarantines…"))
                    .halign(gtk::Align::Fill)
                    .xalign(0.0)
                    .wrap(true)
                    .hexpand(true)
                    .css_classes(["toolport-muted"])
                    .build(),
            ),
        ));

        // Each removal sits in its own row that says what it does; the button
        // only says Remove, and its accessible name says what.
        let remove_clients = l10n::button("Remove");
        remove_clients.add_css_class("toolport-destructive");
        remove_clients.set_valign(gtk::Align::Center);
        l10n::bind_accessible_label(&remove_clients, "Remove Toolport from all clients");
        let purge_data = l10n::button("Remove");
        purge_data.add_css_class("toolport-destructive");
        purge_data.set_valign(gtk::Align::Center);
        l10n::bind_accessible_label(&purge_data, "Remove Toolport data");
        let purge_root = root.clone();
        purge_data.connect_clicked(move |_| {
            let parent = purge_root.root().and_downcast::<gtk::Window>();
            let plan = match crate::purge::plan() {
                Ok(plan) => plan,
                Err(error) => {
                    let dialog = adw::MessageDialog::new(parent.as_ref(), Some(&tr("Could not prepare data removal")), Some(&error));
l10n::bind_dialog_heading(&dialog, msg("Could not prepare data removal"));
                    dialog.add_response("close", &tr("Close"));
 l10n::bind_dialog_response(&dialog, "close", msg("Close"));
                    dialog.present();
                    return;
                }
            };
            let details = trf("{value1}\n\nResults and exact leftovers: {value2}", &[("value1", &(plan.resources.join("\n\n")).to_string()), ("value2", &(plan.report_path).to_string())]);
            let dialog = adw::MessageDialog::new(parent.as_ref(), Some(&tr("Remove Toolport data?")), Some(&tr("Normal uninstall keeps your data. Toolport will close, restore and disconnect clients, then permanently remove the resources below. Client files and native servers stay. Failed restoration or active sessions keep data for recovery. This cannot be undone.")));
l10n::bind_dialog_heading(&dialog, msg("Remove Toolport data?"));
l10n::bind_dialog_body(&dialog, msg("Normal uninstall keeps your data. Toolport will close, restore and disconnect clients, then permanently remove the resources below. Client files and native servers stay. Failed restoration or active sessions keep data for recovery. This cannot be undone."));
            let inventory = gtk::Label::builder().label(&details).wrap(true).xalign(0.0).selectable(true).build();
            let scroller = gtk::ScrolledWindow::builder().hscrollbar_policy(gtk::PolicyType::Never)
                .max_content_height(360).propagate_natural_height(true).child(&inventory).build();
            dialog.set_extra_child(Some(&scroller));
            dialog.add_response("cancel", &tr("Cancel"));
 l10n::bind_dialog_response(&dialog, "cancel", msg("Cancel"));
            dialog.add_response("remove", &tr("Close and remove data"));
 l10n::bind_dialog_response(&dialog, "remove", msg("Close and remove data"));
            dialog.set_close_response("cancel");
            dialog.set_default_response(Some("cancel"));
            dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
            dialog.connect_response(None, move |dialog, response| {
                if response == "remove" {
                    match crate::purge::launch_after_exit(std::path::Path::new(&plan.report_path)) {
                        Ok(()) => if let Some(app) = gtk::gio::Application::default() { app.quit(); },
                        Err(error) => {
                            let error_dialog = adw::MessageDialog::new(dialog.transient_for().as_ref(), Some(&tr("Could not start data removal")), Some(&error));
l10n::bind_dialog_heading(&error_dialog, msg("Could not start data removal"));
                            error_dialog.add_response("close", &tr("Close"));
 l10n::bind_dialog_response(&error_dialog, "close", msg("Close"));
                            error_dialog.present();
                        }
                    }
                }
            });
            dialog.present();
        });
        let removal_results = gtk::Label::new(None);
        removal_results.set_xalign(0.0);
        removal_results.set_wrap(true);

        let general = settings_section(&page, msg("General"), msg("Startup and updates."));
        let startup = gtk::Box::new(gtk::Orientation::Vertical, 0);
        startup.add_css_class("toolport-settings-group");
        startup.add_css_class("toolport-switch-group");
        let language_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        language_row.add_css_class("toolport-setting-row");
        let language_label = l10n::label("Language");
        language_label.set_hexpand(true);
        language_label.set_xalign(0.0);
        language_row.append(&language_label);
        let language_picker = gtk::DropDown::new(None::<gtk::StringList>, gtk::Expression::NONE);
        l10n::bind_language_choices(&language_picker);
        let choice = crate::i18n::language();
        let selected = crate::ui_settings::LANGUAGES
            .iter()
            .position(|code| *code == choice)
            .unwrap_or(0) as u32;
        language_picker.set_selected(selected);
        let suggestion = gtk::Box::new(gtk::Orientation::Vertical, 8);
        suggestion.add_css_class("toolport-settings-group");
        suggestion.add_css_class("toolport-padded-group");
        suggestion.set_widget_name("toolport-language-suggestion");
        suggestion.set_visible(false);
        let suggested = crate::i18n::suggested_language(true);
        if let Some((locale, name)) = suggested {
            let label = gtk::Label::builder().xalign(0.0).wrap(true).build();
            l10n::bind_formatted_label(
                &label,
                msg("Toolport is now available in {language}."),
                &[("language", name)],
            );
            suggestion.append(&label);
            let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let switch = gtk::Button::new();
            l10n::bind_button_with(&switch, move || {
                trf("Switch to {language}", &[("language", name)])
            });
            let keep = l10n::button("Keep English");
            actions.append(&switch);
            actions.append(&keep);
            suggestion.append(&actions);
            for (button, choice) in [(switch, locale), (keep, "en")] {
                let picker = language_picker.downgrade();
                let feedback = feedback.downgrade();
                let notice = suggestion.downgrade();
                button.connect_clicked(move |_| {
                    if let (Some(picker), Some(feedback), Some(notice)) =
                        (picker.upgrade(), feedback.upgrade(), notice.upgrade())
                    {
                        save_ui_language(&picker, &feedback, &notice, choice.to_owned());
                    }
                });
            }
            let notice = suggestion.downgrade();
            gtk::glib::spawn_future_local(async move {
                let pending =
                    gtk::gio::spawn_blocking(crate::ui_settings::language_suggestion_pending).await;
                if let Some(notice) = notice.upgrade() {
                    // A user may already have acted while the worker was reading.
                    if notice.widget_name() == "toolport-language-suggestion" {
                        notice.set_visible(matches!(pending, Ok(Ok(true))));
                    }
                }
            });
        }
        general.append(&suggestion);
        let feedback_for_language = feedback.clone();
        let notice_for_language = suggestion.clone();
        language_picker.connect_selected_notify(move |picker| {
            if l10n::dropdown_rebinding(picker) {
                return;
            }
            let Some(choice) = crate::ui_settings::LANGUAGES.get(picker.selected() as usize) else {
                return;
            };
            // Ignore model refresh notifications for an unchanged saved choice.
            if *choice == crate::i18n::language() {
                return;
            }
            save_ui_language(
                picker,
                &feedback_for_language,
                &notice_for_language,
                (*choice).to_owned(),
            );
        });
        language_row.append(&language_picker);
        startup.append(&language_row);
        startup.append(&launch_row);
        startup.append(&updates);
        general.append(&startup);
        let tools = settings_section(
            &page,
            msg("Tools"),
            msg("How agents find and use your tools."),
        );
        tools.append(&capabilities);
        tools.append(&pinned_section);
        let safety_section = settings_section(
            &page,
            msg("Safety"),
            msg("How Toolport handles risky tool calls."),
        );
        safety_section.append(&posture);
        safety_section.append(&safety);
        safety_section.append(&protection);
        let pending = source_label(
            msg("Pending approvals appear in the approval queue."),
            source_label(
                msg("Pending approvals appear in the approval queue."),
                gtk::Label::builder()
                    .label(tr("Pending approvals appear in the approval queue."))
                    .xalign(0.0)
                    .wrap(true)
                    .css_classes(["toolport-muted"])
                    .build(),
            ),
        );
        safety_section.append(&pending);
        let baseline_notice = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .visible(false)
            .css_classes(["toolport-muted"])
            .build();
        safety_section.append(&baseline_notice);
        safety_section.append(&listed_section(&approvals_heading, &allowed_list));
        safety_section.append(&listed_section(&quarantine_heading, &quarantine_list));
        let access = settings_section(
            &page,
            msg("Access"),
            msg("Limit which servers and tools each client can use, by client or project folder."),
        );
        access.append(&access_list);
        access.append(&folder_heading);
        access.append(&folder_list);
        access.append(&http_client_heading);
        access.append(&http_client_list);
        let connections = settings_section(
            &page,
            msg("Connections"),
            msg("Your local HTTP endpoint and gateway processes."),
        );
        connections.append(&endpoint_heading);
        connections.append(&endpoint);
        connections.append(&desktop);
        let help = settings_section(
            &page,
            msg("Help and data"),
            msg("Support reports, local files and removing Toolport."),
        );
        let help_rows = gtk::Box::new(gtk::Orientation::Vertical, 0);
        help_rows.add_css_class("toolport-settings-group");
        help_rows.add_css_class("toolport-switch-group");
        help_rows.append(&diagnostics);
        help_rows.append(&help_action_row(
            msg("Remove Toolport from all clients"),
            msg("Unchanged configs return to their original bytes. Your edits are preserved and moved entries are restored. Each client result is reported here."),
            &remove_clients,
        ));
        help_rows.append(&help_action_row(
            msg("Remove Toolport data"),
            msg("Normal uninstall keeps your data. Remove it only when you no longer need Toolport."),
            &purge_data,
        ));
        help.append(&help_rows);
        help.append(&removal_results);
        let pages = vec![general, tools, safety_section, access, connections, help];
        for (index, section) in pages.iter().enumerate() {
            section.set_visible(index == 0);
        }
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
            errors,
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
            baseline_notice,
            allowed_list,
            refresh_button,
            updating: Rc::new(Cell::new(false)),
            refreshing: Rc::new(Cell::new(false)),
            mutation_generation: Rc::new(Cell::new(0)),
            quarantine_release_generation: Rc::new(Cell::new(0)),
        };
        settings_page.connect_switches();
        let remove_page = settings_page.clone();
        remove_clients.connect_clicked(move |button| {
            let parent = remove_page.root.root().and_downcast::<gtk::Window>();
            let dialog = adw::MessageDialog::new(parent.as_ref(), Some(&tr("Remove Toolport from all clients?")), Some(&tr("Unchanged configs return to their original bytes. Your edits are preserved and moved entries are restored. Each client result is reported here.")));
l10n::bind_dialog_heading(&dialog, msg("Remove Toolport from all clients?"));
l10n::bind_dialog_body(&dialog, msg("Unchanged configs return to their original bytes. Your edits are preserved and moved entries are restored. Each client result is reported here."));
            dialog.add_response("cancel", &tr("Cancel"));
 l10n::bind_dialog_response(&dialog, "cancel", msg("Cancel"));
            dialog.add_response("remove", &tr("Remove from all clients"));
 l10n::bind_dialog_response(&dialog, "remove", msg("Remove from all clients"));
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
                                let message = if results.is_empty() { tr("No client connections to remove.") } else { results.iter().map(|result| format!("{}: {}", result.client_id, result.error.clone().unwrap_or_else(|| std::iter::once(tr("Client configuration restored")).chain(result.warnings.iter().cloned()).collect::<Vec<_>>().join("; ")))).collect::<Vec<_>>().join("\n") };
                                results_label.set_label(&message);
                                page.begin_mutation();
                                page.refresh_quietly();
                            }
                            Ok(Err(error)) => page.show_error(&error),
                            Err(_) => page.show_error(&tr("Client removal stopped unexpectedly")),
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
            page_for_copy
                .feedback
                .set_label(&tr("Preparing diagnostics…"));
            let page = page_for_copy.clone();
            let button = button.clone();
            gtk::glib::spawn_future_local(async move {
                let result = gtk::gio::spawn_blocking(crate::diagnostics_controller::gather).await;
                button.set_sensitive(true);
                match result {
                    Ok(text) => {
                        if let Some(display) = gtk::gdk::Display::default() {
                            display.clipboard().set_text(&text);
                            page.feedback
                                .set_label(&tr("Copied secret-safe diagnostics."));
                            page.feedback.remove_css_class("error");
                            page.feedback.add_css_class("success");
                        } else {
                            page.show_error(&tr("could not access the desktop clipboard"));
                        }
                    }
                    Err(_) => page.show_error(&tr("the diagnostics task stopped unexpectedly")),
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
                        page.feedback
                            .set_label(&tr("Opened the Toolport data folder."));
                        page.feedback.remove_css_class("error");
                        page.feedback.add_css_class("success");
                    }
                    Ok(Err(error)) => page.show_error(&error),
                    Err(_) => page.show_error(&tr("the file manager task stopped unexpectedly")),
                }
            });
        });
        let page_for_url = settings_page.clone();
        copy_endpoint.connect_clicked(move |_| {
            let status = page_for_url.bridge.status();
            let Some(url) = status.url else {
                page_for_url.show_error(&tr("start the Shared HTTP endpoint first"));
                return;
            };
            if let Some(display) = gtk::gdk::Display::default() {
                display.clipboard().set_text(&format!("{url}/mcp"));
                page_for_url
                    .feedback
                    .set_label(&tr("Copied the Shared HTTP URL."));
                page_for_url.feedback.remove_css_class("error");
                page_for_url.feedback.add_css_class("success");
            }
        });
        let page_for_token = settings_page.clone();
        copy_endpoint_token.connect_clicked(move |_| {
            let status = page_for_token.bridge.status();
            let Some(token) = status.token else {
                page_for_token.show_error(&tr("start the Shared HTTP endpoint first"));
                return;
            };
            if let Some(display) = gtk::gdk::Display::default() {
                display.clipboard().set_text(&token);
                page_for_token.feedback.set_label(&tr(
                    "Copied the administrator bearer token. Keep it private.",
                ));
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
                l10n::bind_button(toggle.upcast_ref::<gtk::Button>(), msg("Hide token"));
            } else {
                page_for_reveal.endpoint_token_value.set_label("");
                page_for_reveal.endpoint_token_value.set_visible(false);
                l10n::bind_button(toggle.upcast_ref::<gtk::Button>(), msg("Show token"));
            }
        });
        let page_for_stale = settings_page.clone();
        stop_stale.connect_clicked(move |button| {
            button.set_sensitive(false);
            page_for_stale
                .feedback
                .set_label(&tr("Checking old gateways…"));
            let page = page_for_stale.clone();
            let button = button.clone();
            let bridge = page_for_stale.bridge.clone();
            gtk::glib::spawn_future_local(async move {
                let result = gtk::gio::spawn_blocking(move || bridge.stop_stale_gateways()).await;
                button.set_sensitive(true);
                match result {
                    Ok(outcome) if !outcome.failed.is_empty() => page.show_error(&trf(
                        "Could not stop every old gateway: {value1}",
                        &[("value1", &(outcome.failed.join("; ")).to_string())],
                    )),
                    Ok(outcome) if !outcome.needs_restart.is_empty() => {
                        let clients = outcome
                            .needs_restart
                            .iter()
                            .map(|client| client.client.as_str())
                            .collect::<Vec<_>>()
                            .join(", ");
                        l10n::bind_formatted_label(
                            &page.feedback,
                            msg("Stopped {value1} old gateway process(es). Restart: {clients}."),
                            &[
                                ("value1", &(outcome.killed.len()).to_string()),
                                ("clients", &(clients).to_string()),
                            ],
                        );
                        page.feedback.remove_css_class("error");
                        page.feedback.add_css_class("success");
                    }
                    Ok(outcome) => {
                        l10n::bind_label(
                            &page.feedback,
                            if outcome.killed.is_empty() {
                                msg("No old gateway processes found.")
                            } else {
                                msg("Old gateway processes stopped.")
                            },
                        );
                        page.feedback.remove_css_class("error");
                        page.feedback.add_css_class("success");
                    }
                    Err(_) => page.show_error(&tr("the gateway cleanup task stopped unexpectedly")),
                }
                page.render_endpoint(page.bridge.status());
            });
        });
        settings_page
    }

    fn toggle_endpoint(&self) {
        self.endpoint_button.set_sensitive(false);
        let running = self.bridge.status().running;
        l10n::bind_label(
            &self.feedback,
            if running {
                msg("Stopping Shared HTTP endpoint…")
            } else {
                msg("Starting Shared HTTP endpoint…")
            },
        );
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
                    l10n::bind_label(
                        &page.feedback,
                        if running {
                            msg("Shared HTTP endpoint stopped")
                        } else {
                            msg("Shared HTTP endpoint started")
                        },
                    );
                    page.feedback.remove_css_class("error");
                    page.feedback.add_css_class("success");
                }
                Ok(Err(error)) => page.show_error(&error),
                Err(_) => page.show_error(&tr("the endpoint operation stopped unexpectedly")),
            }
        });
    }

    fn render_access_sets(&self, registry: crate::registry::Registry) {
        while let Some(child) = self.access_list.first_child() {
            self.access_list.remove(&child);
        }
        // Creating a set is the section's action, beside its heading like Add folder mapping.
        let creator = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        creator.set_valign(gtk::Align::Center);
        let name = gtk::Entry::builder()
            .placeholder_text(tr("Access set name"))
            .width_chars(22)
            .build();
        l10n::bind_placeholder(&name, msg("Access set name"));
        let add = l10n::button("Create access set");
        let page = self.clone();
        let entry = name.clone();
        let create = move || {
            let name = entry.text().to_string();
            page.mutate_access(move || crate::registry_controller::create_profile(&name));
        };
        let on_click = create.clone();
        add.connect_clicked(move |_| on_click());
        name.connect_activate(move |_| create());
        creator.append(&name);
        creator.append(&add);
        self.access_list.append(&settings_heading_with_action(
            msg("Access sets"),
            msg("Narrow enabled servers and tools per client. Servers that are off are hidden everywhere."),
            &creator,
        ));

        let defaults = access_group();
        let mut labels = vec![tr("All enabled servers")];
        labels.extend(registry.profiles.iter().map(|p| p.name.clone()));
        let default =
            gtk::DropDown::from_strings(&labels.iter().map(String::as_str).collect::<Vec<_>>());
        default.set_valign(gtk::Align::Center);
        default.update_property(&[gtk::accessible::Property::Label(&tr("Default access"))]);
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
            if l10n::dropdown_rebinding(dropdown) {
                return;
            }
            let profile = dropdown
                .selected()
                .checked_sub(1)
                .and_then(|i| profiles.get(i as usize))
                .map(|p| p.id.clone());
            page.mutate_access(move || {
                crate::registry_controller::set_default_access(profile.as_deref())
            });
        });
        let row = access_text_row(
            msg("Default access"),
            msg("Default access for clients without an explicit access set"),
        );
        row.append(&default);
        defaults.append(&row);
        if registry.default_access_legacy_policy && registry.default_access_profile_id.is_none() {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            row.add_css_class("toolport-setting-row");
            let notice = msg(
                "The upgrade retained the default access set's tool restrictions and instructions.",
            );
            row.append(&source_label(
                notice,
                gtk::Label::builder()
                    .label(lookup(notice))
                    .xalign(0.0)
                    .wrap(true)
                    .hexpand(true)
                    .css_classes(["toolport-muted"])
                    .build(),
            ));
            let clear = l10n::button("Use All enabled servers");
            clear.set_valign(gtk::Align::Center);
            let page = self.clone();
            clear.connect_clicked(move |_| {
                page.mutate_access(|| crate::registry_controller::set_default_access(None))
            });
            row.append(&clear);
            defaults.append(&row);
        }
        self.access_list.append(&defaults);

        let servers: Vec<_> = registry
            .servers
            .iter()
            .filter(|s| !crate::clients::is_gateway_server(s))
            .collect();
        for profile in &registry.profiles {
            let card = access_group();
            let head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            head.add_css_class("toolport-setting-row");
            head.append(
                &gtk::Label::builder()
                    .label(&profile.name)
                    .xalign(0.0)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .css_classes(["heading"])
                    .build(),
            );
            if Some(&profile.id) == registry.default_access_profile_id.as_ref() {
                head.append(
                    &gtk::Label::builder()
                        .label(tr("default"))
                        .valign(gtk::Align::Center)
                        .css_classes(["toolport-badge", "caption", "info"])
                        .build(),
                );
            }
            let count = servers
                .iter()
                .filter(|s| profile.enabled_server_ids.contains(&s.id))
                .count();
            head.append(
                &gtk::Label::builder()
                    .label(format!(
                        "{count} {}",
                        if count == 1 {
                            tr("server")
                        } else {
                            tr("servers")
                        }
                    ))
                    .hexpand(true)
                    .xalign(0.0)
                    .css_classes(["toolport-muted", "caption"])
                    .build(),
            );
            let delete = gtk::Button::from_icon_name("user-trash-symbolic");
            delete.add_css_class("flat");
            delete.set_valign(gtk::Align::Center);
            delete.set_tooltip_text(Some(&tr("Delete access set")));
            delete.update_property(&[gtk::accessible::Property::Label(&tr("Delete access set"))]);
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
            head.append(&delete);
            card.append(&head);
            for server in &servers {
                let included = profile.enabled_server_ids.contains(&server.id);
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
                row.add_css_class("toolport-setting-row");
                row.append(&super::branding::server_logo(
                    &server.name,
                    &server.transport,
                ));
                row.append(
                    &gtk::Label::builder()
                        .label(&server.name)
                        .xalign(0.0)
                        .ellipsize(gtk::pango::EllipsizeMode::End)
                        .tooltip_text(&server.name)
                        .build(),
                );
                if !server.enabled {
                    row.append(
                        &gtk::Label::builder()
                            .label(tr("Off"))
                            .valign(gtk::Align::Center)
                            .css_classes(["toolport-badge", "caption", "off"])
                            .build(),
                    );
                }
                let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                spacer.set_hexpand(true);
                row.append(&spacer);
                if included {
                    let scope = profile.tool_scope.get(&server.id).cloned();
                    let tools = gtk::Button::builder()
                        .valign(gtk::Align::Center)
                        .css_classes(["flat"])
                        .build();
                    let content = gtk::Box::new(gtk::Orientation::Horizontal, 4);
                    let label = gtk::Label::new(Some(&match &scope {
                        Some(list) if list.len() == 1 => tr("1 tool"),
                        Some(list) => trf("{value1} tools", &[("value1", &list.len().to_string())]),
                        None => tr("all tools"),
                    }));
                    if scope.is_some() {
                        label.add_css_class("accent");
                    } else {
                        label.add_css_class("toolport-muted");
                    }
                    content.append(&label);
                    content.append(&gtk::Image::from_icon_name("pan-end-symbolic"));
                    tools.set_child(Some(&content));
                    let choose = trf("Choose tools for {value1}", &[("value1", &server.name)]);
                    tools.set_tooltip_text(Some(&choose));
                    tools.update_property(&[gtk::accessible::Property::Label(&choose)]);
                    let (server, pid, page) = ((*server).clone(), profile.id.clone(), self.clone());
                    tools.connect_clicked(move |_| {
                        open_access_tool_scope(
                            server.clone(),
                            pid.clone(),
                            scope.clone(),
                            page.clone(),
                        )
                    });
                    row.append(&tools);
                }
                let switch = gtk::Switch::builder()
                    .active(included)
                    .valign(gtk::Align::Center)
                    .build();
                switch.update_property(&[gtk::accessible::Property::Label(&server.name)]);
                let (pid, sid, page) = (profile.id.clone(), server.id.clone(), self.clone());
                switch.connect_active_notify(move |switch| {
                    let (pid, sid, included) = (pid.clone(), sid.clone(), switch.is_active());
                    page.mutate_access(move || {
                        crate::registry_controller::set_access_server(&pid, &sid, included)
                    });
                });
                row.append(&switch);
                card.append(&row);
            }
            self.access_list.append(&card);
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
                Err(_) => page.show_error(&tr("The access update stopped unexpectedly")),
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
            self.folder_list.append(&empty_state(&tr(
                "Create an access set before adding project folder routing.",
            )));
            return;
        }
        if settings.mappings.is_empty() {
            self.folder_list
                .append(&empty_state(&tr("No project folders are mapped yet.")));
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
            copy.append(&source_formatted_label(
                msg("Uses {profile_name}"),
                &[("profile_name", (profile_name))],
                gtk::Label::builder()
                    .label(trf(
                        "Uses {profile_name}",
                        &[("profile_name", (profile_name))],
                    ))
                    .halign(gtk::Align::Start)
                    .css_classes(["toolport-muted"])
                    .build(),
            ));
            row.append(&copy);
            let remove = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .tooltip_text(tr("Remove folder mapping"))
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
                            page.show_error(&tr("the folder mapping removal stopped unexpectedly"))
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
            self.pinned_list.append(&empty_state(&tr("None yet. In Servers, open a server's Tools tab and pin tools your agent needs every time, such as sign-in or a required lookup.")));
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
            let unpin = l10n::button("Unpin");
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
                            page.feedback.set_label(&tr("Pinned prerequisite removed."));
                            page.feedback.remove_css_class("error");
                            page.feedback.add_css_class("success");
                            page.refresh();
                        }
                        Ok(Err(error)) => page.show_error(&error),
                        Err(_) => {
                            page.show_error(&tr("the prerequisite update stopped unexpectedly"))
                        }
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
            .title(tr("Choose a project folder"))
            .modal(true)
            .accept_label(tr("Choose"))
            .build();
        let page = self.clone();
        dialog.select_folder(Some(&parent), gtk::gio::Cancellable::NONE, move |result| {
            let Ok(folder) = result else {
                return;
            };
            let Some(path) = folder.path() else {
                page.show_error(&tr("the selected folder does not have a local path"));
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
                Err(_) => page.show_error(&tr("the profile read stopped unexpectedly")),
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
            self.show_error(&tr("create an access set before adding folder routing"));
            return;
        }
        #[allow(deprecated)]
        let dialog = adw::MessageDialog::new(
            Some(&parent),
            Some(&tr("Choose an access set")),
            Some(&tr("This access set will be selected when a client reports this folder or one of its descendants.")),
        );
        l10n::bind_dialog_heading(&dialog, msg("Choose an access set"));
        l10n::bind_dialog_body(&dialog, msg("This access set will be selected when a client reports this folder or one of its descendants."));
        dialog.add_response("cancel", &tr("Cancel"));
        l10n::bind_dialog_response(&dialog, "cancel", msg("Cancel"));
        dialog.add_response("save", &tr("Save mapping"));
        l10n::bind_dialog_response(&dialog, "save", msg("Save mapping"));
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
                    page.show_error(&tr("choose an access set"));
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
                            page.feedback
                                .set_label(&tr("Saved project folder routing."));
                            page.feedback.remove_css_class("error");
                            page.feedback.add_css_class("success");
                        }
                        Ok(Err(error)) => page.show_error(&error),
                        Err(_) => {
                            page.show_error(&tr("the folder mapping save stopped unexpectedly"))
                        }
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
                &source_formatted_label(msg("{value1} (pid {value2}) still launches {value3} - restart it to pick up the upgrade"), &[("value1", &(client.client).to_string()), ("value2", &(client.client_pid).to_string()), ("value3", &(client.gateway).to_string())], gtk::Label::builder()
                    .label(trf("{value1} (pid {value2}) still launches {value3} - restart it to pick up the upgrade", &[("value1", &(client.client).to_string()), ("value2", &(client.client_pid).to_string()), ("value3", &(client.gateway).to_string())]))
                    .halign(gtk::Align::Start)
                    .xalign(0.0)
                    .wrap(true)
                    .css_classes(["toolport-feedback", "error"])
                    .build()),
            );
        }
    }

    fn render_endpoint(&self, status: crate::http_bridge::HttpBridgeStatus) {
        if let Some(url) = status.url {
            l10n::bind_formatted_label(
                &self.endpoint_status,
                msg("Running at {url}/mcp · bearer authentication required"),
                &[("url", &(url).to_string())],
            );
            l10n::bind_button(&self.endpoint_button, msg("Stop"));
            self.endpoint_button.remove_css_class("suggested-action");
            self.endpoint_button.add_css_class("toolport-destructive");
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
            self.endpoint_status.set_label(&tr("Stopped"));
            l10n::bind_button(&self.endpoint_button, msg("Start"));
            self.endpoint_button
                .remove_css_class("toolport-destructive");
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
                .append(&empty_state(&tr("No scoped HTTP clients are registered.")));
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
                tr("Full connected set")
            } else if client.profile == crate::registry::ALL_ENABLED_ACCESS {
                tr("All enabled servers")
            } else {
                settings
                    .profiles
                    .iter()
                    .find(|(id, name)| id == &client.profile || name == &client.profile)
                    .map(|(_, name)| trf("Only {name}", &[("name", &(name).to_string())]))
                    .unwrap_or_else(|| {
                        trf(
                            "Only {value1}",
                            &[("value1", &(client.profile).to_string())],
                        )
                    })
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
                let badge = l10n::label(msg("Managed client"));
                badge.add_css_class("toolport-badge");
                badge.add_css_class("success");
                l10n::bind_tooltip(&badge, msg("Disconnect this token from the Clients page"));
                row.append(&badge);
            } else {
                let remove = gtk::Button::builder()
                    .icon_name("user-trash-symbolic")
                    .tooltip_text(trf(
                        "Revoke {value1}",
                        &[("value1", &(client.label).to_string())],
                    ))
                    .css_classes(["toolport-destructive"])
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
                                page.feedback
                                    .set_label(&tr("Revoked the HTTP client token."));
                                page.feedback.remove_css_class("error");
                                page.feedback.add_css_class("success");
                            }
                            Ok(Err(error)) => page.show_error(&error),
                            Err(_) => {
                                page.show_error(&tr("the HTTP client removal stopped unexpectedly"))
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
                    page.show_error(&tr("the HTTP client read stopped unexpectedly"));
                    return;
                }
            };
            #[allow(deprecated)]
            let dialog = adw::MessageDialog::new(
                Some(&parent),
                Some(&tr("Add a scoped HTTP client")),
                Some(&tr("Toolport generates a bearer token that is shown once. Choose which access set this client can access.")),
            );
            l10n::bind_dialog_heading(&dialog, msg("Add a scoped HTTP client"));
            l10n::bind_dialog_body(&dialog, msg("Toolport generates a bearer token that is shown once. Choose which access set this client can access."));
            dialog.add_response("cancel", &tr("Cancel"));
            l10n::bind_dialog_response(&dialog, "cancel", msg("Cancel"));
            dialog.add_response("add", &tr("Add client"));
            l10n::bind_dialog_response(&dialog, "add", msg("Add client"));
            dialog.set_close_response("cancel");
            dialog.set_default_response(Some("add"));
            dialog.set_response_appearance("add", adw::ResponseAppearance::Suggested);
            let form = gtk::Box::new(gtk::Orientation::Vertical, 8);
            let label = gtk::Entry::builder()
                .placeholder_text(tr("Client name, for example Open WebUI"))
                .css_classes(["toolport-input"])
                .build();
            l10n::bind_placeholder(&label, msg("Client name, for example Open WebUI"));
            form.append(&label);
            let mut profile_names = vec![tr("Full connected set"), tr("All enabled servers")];
            profile_names.extend(settings.profiles.iter().map(|(_, name)| name.clone()));
            let profile = gtk::DropDown::new(
                Some(gtk::StringList::new(
                    &profile_names.iter().map(String::as_str).collect::<Vec<_>>(),
                )),
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
                            Err(_) => page.show_error(&tr(
                                "the HTTP client registration stopped unexpectedly",
                            )),
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
            Some(&tr("Copy this token now")),
            Some(&tr("The token was copied to the clipboard. Toolport stores only its hash and cannot show it again.")),
        );
        l10n::bind_dialog_heading(&dialog, msg("Copy this token now"));
        l10n::bind_dialog_body(&dialog, msg("The token was copied to the clipboard. Toolport stores only its hash and cannot show it again."));
        dialog.add_response("done", &tr("Done"));
        l10n::bind_dialog_response(&dialog, "done", msg("Done"));
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
            .set_label(&tr("Registered the scoped HTTP client."));
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
                    Err(_) => page.show_error(&tr("the safety update stopped unexpectedly")),
                }
                page.refresh();
            });
        });
        let page = self.clone();
        self.safety_level.connect_selected_notify(move |control| {
            if l10n::dropdown_rebinding(control) {
                return;
            }
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
            for card in &page.safety_cards {
                card.set_sensitive(false);
            }
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
                    Err(_) => page.show_error(&tr("the safety update stopped unexpectedly")),
                }
                page.safety_level.set_sensitive(true);
                for (index, card) in page.safety_cards.iter().enumerate() {
                    card.set_sensitive(index >= page.safety_floor.get() as usize);
                }
                page.refresh();
            });
        });
        for (switch, setting, label) in [
            (
                self.lazy_discovery.clone(),
                crate::registry_controller::EssentialSetting::LazyDiscovery,
                msg("lazy discovery"),
            ),
            (
                self.code_mode.clone(),
                crate::registry_controller::EssentialSetting::CodeMode,
                msg("code mode"),
            ),
            (
                self.live_inspect.clone(),
                crate::registry_controller::EssentialSetting::LiveInspect,
                msg("live inspection"),
            ),
            (
                self.pii_redaction.clone(),
                crate::registry_controller::EssentialSetting::PiiRedaction,
                msg("PII pseudonymization"),
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
                            let outcome = if enabled {
                                trf("Enabled {setting}", &[("setting", &lookup(label))])
                            } else {
                                trf("Disabled {setting}", &[("setting", &lookup(label))])
                            };
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
                            page.show_error(&tr("the setting update stopped unexpectedly"));
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
                            l10n::bind_label(
                                &page.feedback,
                                if enabled {
                                    msg("Toolport will launch at login")
                                } else {
                                    msg("Toolport will not launch at login")
                                },
                            );
                            page.feedback.remove_css_class("error");
                            page.feedback.add_css_class("success");
                        }
                        Ok(Err(error)) => {
                            page.show_error(&error);
                            page.refresh();
                        }
                        Err(_) => {
                            page.show_error(&tr("the launch-at-login update stopped unexpectedly"));
                            page.refresh();
                        }
                    }
                });
                gtk::glib::Propagation::Stop
            });
    }

    pub(super) fn select_page(&self, index: usize) {
        for (current, page) in self.pages.iter().enumerate() {
            page.set_visible(current == index);
        }
    }

    pub(super) fn connect_page_selected(&self, index: usize, callback: impl Fn(bool) + 'static) {
        self.pages[index].connect_visible_notify(move |page| callback(page.is_visible()));
    }

    pub(super) fn approval_broker(&self) -> crate::approval_broker::ApprovalBroker {
        self.broker.clone()
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
            for list in [
                &self.pinned_list,
                &self.http_client_list,
                &self.access_list,
                &self.folder_list,
                &self.quarantine_list,
                &self.allowed_list,
            ] {
                if list.first_child().is_none() {
                    list.append(&super::skeleton_rows(crate::i18n::msg("Loading…")));
                }
            }
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
                    read_baseline_notice(),
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
                    baseline_notice,
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
                    while let Some(child) = page.errors.first_child() {
                        page.errors.remove(&child);
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
                    if let Ok(notice) = baseline_notice {
                        page.baseline_notice
                            .set_label(notice.as_deref().unwrap_or(""));
                        page.baseline_notice.set_visible(notice.is_some());
                    }
                    page.render_allowed(allowed);
                    page.render_folder_routing(folder_routing);
                    page.render_http_clients(http_clients);
                    page.render_pinned_prerequisites(pinned);
                    if announce && !page.feedback.has_css_class("error") {
                        // Clearing, not announcing: the load succeeding is not
                        // news, and a green "up to date" bar stacked above the
                        // red approval-gates warning competed with it.
                        page.set_status("");
                    }
                    page.refreshing.set(false);
                }
                Ok(Err(error)) => {
                    page.refreshing.set(false);
                    page.show_read_error(&error);
                }
                Err(_) => {
                    page.refreshing.set(false);
                    page.show_read_error(&tr("the settings read stopped unexpectedly"));
                }
            }
        });
    }

    fn finish_quarantine_release(&self, needed: Vec<(String, String)>, preserve_error: bool) {
        self.begin_mutation();
        // Only a newer release supersedes this wait. Unrelated settings changes
        // must still let it render the current quarantine list and final feedback.
        let generation = self.quarantine_release_generation.get().wrapping_add(1);
        self.quarantine_release_generation.set(generation);
        let page = self.clone();
        if !preserve_error {
            l10n::bind_label(
                &page.feedback,
                if needed.is_empty() {
                    msg("Approval saved. The gateway will publish it on its next refresh.")
                } else {
                    msg("Waiting for the current definition. One more review is needed.")
                },
            );
            page.feedback.remove_css_class("success");
            page.feedback.remove_css_class("error");
        }
        gtk::glib::spawn_future_local(async move {
            let expected = needed.clone();
            let result = gtk::gio::spawn_blocking(move || {
                wait_for_captured_quarantines(
                    &expected,
                    read_quarantined_tools,
                    std::time::Duration::from_secs(10),
                )
            })
            .await;
            if page.quarantine_release_generation.get() != generation {
                return;
            }
            match result {
                Ok(Ok(entries)) => {
                    let captured = needed.iter().all(|(profile, tool)| {
                        entries.iter().any(|entry| {
                            &entry.profile == profile && &entry.tool == tool && entry.has_definition
                        })
                    });
                    page.begin_mutation();
                    page.render_quarantine(Ok(entries));
                    if !preserve_error && !needed.is_empty() {
                        page.feedback.set_label(&lookup(capture_feedback(captured)));
                    }
                }
                Ok(Err(error)) => page.show_error(&error),
                Err(_) => page.show_error(&tr("the captured review could not be loaded")),
            }
        });
    }

    fn render_settings(&self, settings: crate::registry_controller::EssentialSettings) {
        let (_, guarded) = posture_summary(&settings);
        let posture_settings = settings.clone();
        l10n::bind_label_with(&self.posture, move || posture_summary(&posture_settings).0);
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
        let choices: &'static [&'static str] = match settings.team_min_safety_level {
            crate::registry::SafetyLevel::Off => SAFETY_ALL,
            crate::registry::SafetyLevel::Ask => SAFETY_ASK,
            crate::registry::SafetyLevel::Strict => SAFETY_STRICT,
        };
        l10n::bind_dropdown_choices(&self.safety_level, choices);
        self.safety_level
            .set_selected(settings.safety_level as u32 - settings.team_min_safety_level as u32);
        let policy_settings = settings.clone();
        l10n::bind_label_with(&self.safety_policy, move || {
            safety_policy_text(&policy_settings)
        });
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
            l10n::bind_formatted_label(
                &self.safety_kept_note,
                msg("{kept} Choosing a level replaces these with that level's protections."),
                &[("kept", &(kept).to_string())],
            );
            let level = settings.safety_level;
            l10n::bind_button_with(&self.safety_kept_reset, move || {
                trf(
                    "Use standard {level}",
                    &[("level", &lookup(level_name(level)))],
                )
            });
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

    fn show_read_error(&self, error: &str) {
        for list in [
            &self.pinned_list,
            &self.http_client_list,
            &self.access_list,
            &self.folder_list,
            &self.quarantine_list,
            &self.allowed_list,
        ] {
            if let Some(child) = list
                .first_child()
                .filter(|child| child.has_css_class("toolport-skeleton-group"))
            {
                list.remove(&child);
            }
        }
        while let Some(child) = self.errors.first_child() {
            self.errors.remove(&child);
        }
        let page = self.clone();
        self.errors
            .append(&super::error_callout(msg("Settings"), error, move || {
                page.refresh_with_feedback(true)
            }));
    }

    fn show_error(&self, error: &str) {
        self.set_status(&trf("Settings error: {error}", &[("error", error)]));
        self.feedback.add_css_class("error");
        self.feedback.set_visible(true);
    }

    fn render_quarantine(&self, entries: Result<Vec<QuarantinedTool>, String>) {
        while let Some(child) = self.quarantine_list.first_child() {
            self.quarantine_list.remove(&child);
        }
        let entries = match entries {
            Ok(entries) => entries,
            Err(error) => {
                let page = self.clone();
                let row = super::error_callout(msg("Quarantined tools"), &error, move || {
                    page.refresh_with_feedback(true)
                });
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
                .append(&empty_state(&tr("No tools are quarantined.")));
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
                .append(&empty_state(&tr("No remembered approvals.")));
            return;
        }
        for entry in entries {
            self.allowed_list.append(&allowed_row(entry, self.clone()));
        }
    }
}

#[derive(Clone)]
struct QuarantinedTool {
    record_id: String,
    profile: String,
    tool: String,
    detail: String,
    has_definition: bool,
}

fn capture_feedback(captured: bool) -> &'static str {
    if captured {
        msg("Current definition captured. One more review is needed; the tool stays blocked until then.")
    } else {
        msg("The gateway has not captured the definition yet. Refresh to review it when the gateway is running.")
    }
}

fn bulk_capture_targets(
    needed: Vec<(String, String)>,
    remaining: &[QuarantinedTool],
) -> Vec<(String, String)> {
    needed
        .into_iter()
        .filter(|(profile, tool)| {
            !remaining.iter().any(|entry| {
                &entry.profile == profile && &entry.tool == tool && !entry.has_definition
            })
        })
        .collect()
}

// Read off the GTK main thread after a release; a gateway capture can arrive later.
fn wait_for_captured_quarantines(
    needed: &[(String, String)],
    mut read: impl FnMut() -> Result<Vec<QuarantinedTool>, String>,
    timeout: std::time::Duration,
) -> Result<Vec<QuarantinedTool>, String> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let entries = read()?;
        let captured = needed.iter().all(|(profile, tool)| {
            entries.iter().any(|entry| {
                &entry.profile == profile && &entry.tool == tool && entry.has_definition
            })
        });
        if captured || std::time::Instant::now() >= deadline {
            return Ok(entries);
        }
        std::thread::sleep(
            std::time::Duration::from_millis(100)
                .min(deadline.saturating_duration_since(std::time::Instant::now())),
        );
    }
}

fn read_baseline_notice() -> Result<Option<String>, String> {
    let registry = crate::registry::load_resolved()?;
    Ok(crate::integrity::baseline_notice(&registry)?.map(|event| {
        let timestamp = event["ts"].as_i64().unwrap_or(0);
        let time = gtk::glib::DateTime::from_unix_local(timestamp / 1000)
            .and_then(|time| time.format("%c"))
            .map(|time| time.to_string())
            .unwrap_or_else(|_| timestamp.to_string());
        trf(
            "Last baseline warning: {time}: {value1}",
            &[
                ("time", &(time).to_string()),
                ("value1", (event["detail"].as_str().unwrap_or_default())),
            ],
        )
    }))
}

fn read_quarantined_tools() -> Result<Vec<QuarantinedTool>, String> {
    Ok(crate::integrity::all_quarantined_summaries()?
        .into_iter()
        .map(|value| QuarantinedTool {
            has_definition: value["has_definition"].as_bool().unwrap_or(false),
            record_id: value["record_id"].as_str().unwrap_or_default().to_string(),
            profile: value
                .get("profile")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            tool: value
                .get("tool")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| tr("Unknown tool")),
            detail: value
                .get("detail")
                .and_then(serde_json::Value::as_str)
                .or_else(|| value.get("reason").and_then(serde_json::Value::as_str))
                .map(str::to_owned)
                .unwrap_or_else(|| tr("High-risk definition change")),
        })
        .collect())
}

/// Describe the effective policy, including the team's enforced level.
fn safety_policy_text(settings: &crate::registry_controller::EssentialSettings) -> String {
    let floor = lookup(level_name(settings.team_min_safety_level));
    let mut policy = trf(
        "Team minimum safety level: {floor}.",
        &[("floor", &(floor).to_string())],
    );
    if settings.quarantine_on_drift_forced {
        policy.push_str(&tr(" Team also enforces quarantine on drift."));
    }
    if settings.block_on_injection_forced {
        policy.push_str(&tr(" Team also enforces block on injection."));
    }
    policy
}

fn posture_summary(settings: &crate::registry_controller::EssentialSettings) -> (String, bool) {
    use crate::registry::SafetyLevel;
    let (mut line, guarded) = match settings.safety_level {
        SafetyLevel::Off => (tr("Safety is set to Off. Toolport does not ask before destructive calls. Server sign-in and client permissions may still ask for approval."), false),
        SafetyLevel::Ask => (tr("Safety is set to Ask. Destructive calls need your approval before they run."), true),
        SafetyLevel::Strict => (tr("Safety is set to Strict. Destructive tools are hidden and untrusted calls need your approval."), true),
    };
    if settings.quarantine_on_drift_forced || settings.block_on_injection_forced {
        line.push_str(&tr(
            " Your team also requires protection against risky tool changes or injection.",
        ));
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
        .title(trf(
            "Tool scope for {value1}",
            &[("value1", &(server.name).to_string())],
        ))
        .default_width(520)
        .default_height(560)
        .build();
    window.set_application(parent.application().as_ref());
    window.add_css_class("toolport-editor");
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = adw::HeaderBar::new();
    let cancel = l10n::button("Cancel");
    cancel.add_css_class("flat");
    let save = l10n::button("Save");
    save.add_css_class("suggested-action");
    save.set_sensitive(false);
    root.append(&header);
    super::close_on_escape(&window);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.add_css_class("toolport-editor-body");
    content.append(&super::editor_intro(
        "view-list-symbolic",
        crate::i18n::msg("Tools available in this access set"),
        crate::i18n::msg("Uncheck tools this access set should hide. Selecting every tool restores the server's default full scope."),
    ));
    let feedback = source_label(
        msg("Loading server tools…"),
        source_label(
            msg("Loading server tools…"),
            gtk::Label::builder()
                .label(tr("Loading server tools…"))
                .halign(gtk::Align::Fill)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["toolport-feedback"])
                .build(),
        ),
    );
    content.append(&feedback);
    let tool_list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    tool_list.add_css_class("toolport-settings-group");
    tool_list.append(&super::skeleton_rows(crate::i18n::msg("Loading tools…")));
    feedback.set_visible(false);
    content.append(&tool_list);
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&content)
        .build();
    root.append(&scroller);
    root.append(&super::dialog_footer(&[&cancel, &save]));
    window.set_content(Some(&root));
    let window_for_cancel = window.clone();
    cancel.connect_clicked(move |_| window_for_cancel.close());

    let selections = std::rc::Rc::new(std::cell::RefCell::new(
        Vec::<(gtk::CheckButton, String)>::new(),
    ));
    let selections_for_save = selections.clone();
    let page_for_save = page.clone();
    let window_for_save = window.clone();
    let server_id = server.id.clone();
    let server_id_for_save = server_id.clone();
    let retry_window = window.clone();
    let retry_page = page.clone();
    let retry_server = server.clone();
    let retry_profile = profile_id.clone();
    let retry_scope = current_scope.clone();
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
                Ok(Err(error)) => page.show_error(&trf(
                    "Could not update tool scope: {error}",
                    &[("error", &(error).to_string())],
                )),
                Err(_) => page.show_error(&tr("The tool-scope update stopped unexpectedly.")),
            }
        });
    });

    let current_scope =
        current_scope.map(|tools| tools.into_iter().collect::<std::collections::HashSet<_>>());
    let selections_for_load = selections;
    gtk::glib::spawn_future_local(async move {
        let result =
            gtk::gio::spawn_blocking(move || crate::playground::list_tools(&server_id)).await;
        while let Some(child) = tool_list.first_child() {
            tool_list.remove(&child);
        }
        let retry = move || {
            retry_window.close();
            open_access_tool_scope(
                retry_server.clone(),
                retry_profile.clone(),
                retry_scope.clone(),
                retry_page.clone(),
            );
        };
        match result {
            Ok(Ok(tools)) => {
                if tools.is_empty() {
                    tool_list.append(&empty_state(&tr(
                        "This server does not advertise any tools.",
                    )));
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
                feedback.set_label(&tr("Changes apply only to this access set."));
                feedback.remove_css_class("error");
                feedback.add_css_class("success");
                save.set_sensitive(!selections_for_load.borrow().is_empty());
            }
            Ok(Err(error)) => {
                tool_list.append(&super::error_callout(msg("Tools"), &error, retry));
            }
            Err(_) => {
                tool_list.append(&super::error_callout(
                    msg("Tools"),
                    &tr("The server tool read stopped unexpectedly."),
                    retry,
                ));
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
    let saved = if released == 1 {
        tr("Saved 1 approval.")
    } else {
        trf(
            "Saved {released} approvals.",
            &[("released", &(released).to_string())],
        )
    };
    if !summary.failed.is_empty() {
        let failures = summary.failed.len();
        let error = &summary.failed[0];
        let message = if failures == 1 {
            trf(
                "{saved} {failures} access-set scope could not be re-approved: {error}",
                &[
                    ("saved", &saved),
                    ("failures", &failures.to_string()),
                    ("error", error),
                ],
            )
        } else {
            trf(
                "{saved} {failures} access-set scopes could not be re-approved: {error}",
                &[
                    ("saved", &saved),
                    ("failures", &failures.to_string()),
                    ("error", error),
                ],
            )
        };
        return (message, true);
    }
    if !summary.changed.is_empty() {
        let changed = summary.changed.len();
        return (
            trf("{saved} {changed} new or changed findings remain blocked. Refresh and review them.", &[("saved", &(saved).to_string()), ("changed", &(changed).to_string())]),
            true,
        );
    }
    if !summary.skipped.is_empty() {
        let skipped = summary.skipped.len();
        let message = if skipped == 1 {
            trf(
                "{saved} {skipped} could not be repaired and is still blocked.",
                &[("saved", &saved), ("skipped", &skipped.to_string())],
            )
        } else {
            trf(
                "{saved} {skipped} could not be repaired and are still blocked.",
                &[("saved", &saved), ("skipped", &skipped.to_string())],
            )
        };
        return (message, true);
    }
    (
        trf(
            "{saved} The gateway will publish approved definitions on its next refresh.",
            &[("saved", &(saved).to_string())],
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
    let needed: Vec<_> = entries
        .iter()
        .filter(|entry| !entry.has_definition)
        .map(|entry| (entry.profile.clone(), entry.tool.clone()))
        .collect();
    let copy = source_formatted_label(
        msg("{count} tools are blocked."),
        &[("count", &(count).to_string())],
        gtk::Label::builder()
            .label(trf(
                "{count} tools are blocked.",
                &[("count", &(count).to_string())],
            ))
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .hexpand(true)
            .css_classes(["toolport-muted"])
            .build(),
    );
    row.append(&copy);
    let release_all = l10n::button("Re-approve all");
    release_all.add_css_class("toolport-secondary-action");
    release_all.set_tooltip_text(Some(
        &tr("Repair every baseline in one pass. A tool whose captured definition cannot be read stays blocked."),
    ));
    let profiles = distinct_profiles(entries);
    let mut expected =
        std::collections::BTreeMap::<String, std::collections::BTreeMap<String, String>>::new();
    for entry in entries {
        expected
            .entry(entry.profile.clone())
            .or_default()
            .insert(entry.tool.clone(), entry.record_id.clone());
    }
    let release_for_click = release_all.clone();
    release_all.connect_clicked(move |_| {
        let Some(parent) = page.root.root().and_downcast::<gtk::Window>() else {
            return;
        };
        #[allow(deprecated)]
        let dialog = adw::MessageDialog::new(
            Some(&parent),
            Some(&trf("Re-approve all {count} blocked tools?", &[("count", &(count).to_string())])),
            Some(
                &tr("Toolport approves the displayed findings and repairs their baselines in one pass. \
                 Findings without a saved definition require one more review of the current definition. \
                 A tool whose captured definition cannot be read stays blocked and remains listed."),
            ),
        );
l10n::bind_formatted_dialog_heading(&dialog, msg("Re-approve all {count} blocked tools?"), &[("count", &(count).to_string())]);
        dialog.add_response("cancel", &tr("Keep blocked"));
 l10n::bind_dialog_response(&dialog, "cancel", msg("Keep blocked"));
        dialog.add_response("approve", &tr("Re-approve all"));
 l10n::bind_dialog_response(&dialog, "approve", msg("Re-approve all"));
        dialog.set_close_response("cancel");
        dialog.set_default_response(Some("cancel"));
        dialog.set_response_appearance("approve", adw::ResponseAppearance::Suggested);
        let page = page.clone();
        let profiles = profiles.clone();
        let expected = expected.clone();
        let needed = needed.clone();
        let button = release_for_click.clone();
        dialog.connect_response(None, move |dialog, response| {
            if response == "approve" {
                button.set_sensitive(false);
                let page = page.clone();
                let profiles = profiles.clone();
                let expected = expected.clone();
                let needed = needed.clone();
                gtk::glib::spawn_future_local(async move {
                    let result = gtk::gio::spawn_blocking(move || {
                        let summary = crate::registry_controller::release_all_quarantine_records(
                            &profiles, &expected,
                        );
                        let needed = match read_quarantined_tools() {
                            Ok(remaining) => bulk_capture_targets(needed, &remaining),
                            Err(_) => Vec::new(),
                        };
                        (summary, needed)
                    })
                    .await;
                    match result {
                        Ok((summary, needed)) => {
                            let (message, is_error) = release_all_feedback(&summary);
                            page.feedback.set_label(&message);
                            if is_error {
                                page.feedback.remove_css_class("success");
                                page.feedback.add_css_class("error");
                            } else {
                                page.feedback.remove_css_class("error");
                                page.feedback.add_css_class("success");
                            }
                            page.finish_quarantine_release(needed, is_error);
                        }
                        Err(_) => page.show_error(&tr("the bulk re-approval stopped unexpectedly")),
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

fn quarantine_definition_expander(entry: &QuarantinedTool) -> gtk::Expander {
    let expander = gtk::Expander::builder()
        .label(tr("Captured definition"))
        .build();
    expander.set_child(Some(&l10n::label(msg("Expand to load definition."))));
    let loading = Rc::new(Cell::new(false));
    let loaded = Rc::new(Cell::new(false));
    let entry = entry.clone();
    expander.connect_expanded_notify(move |expander| {
        if !expander.is_expanded() || loaded.get() || loading.replace(true) {
            return;
        }
        let entry = entry.clone();
        let expander = expander.clone();
        let loading = loading.clone();
        let loaded = loaded.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                let definition = crate::integrity::quarantine_definition(
                    (!entry.profile.is_empty()).then_some(entry.profile.as_str()),
                    &entry.tool,
                    &entry.record_id,
                )?;
                serde_json::to_string_pretty(&definition).map_err(|e| e.to_string())
            })
            .await;
            let text = match result {
                Ok(Ok(definition)) => {
                    loaded.set(true);
                    definition
                }
                Ok(Err(error)) => error,
                Err(_) => tr("The captured definition could not be loaded."),
            };
            let label = gtk::Label::builder()
                .label(text)
                .xalign(0.0)
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .selectable(true)
                .css_classes(["monospace"])
                .build();
            let scroll = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .max_content_height(260)
                .propagate_natural_height(true)
                .child(&label)
                .build();
            expander.set_child(Some(&scroll));
            loading.set(false);
        });
    });
    expander
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
        tr("All access sets")
    } else {
        trf(
            "Access set: {value1}",
            &[("value1", &(entry.profile).to_string())],
        )
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
    if entry.has_definition {
        copy.append(&quarantine_definition_expander(&entry));
    } else {
        copy.append(
            &source_label(msg("No saved definition. Approving captures the current definition for one more review."), gtk::Label::builder()
                .label(tr("No saved definition. Approving captures the current definition for one more review."))
                .xalign(0.0)
                .wrap(true)
                .css_classes(["toolport-muted"])
                .build()),
        );
    }
    row.append(&copy);
    let release = l10n::button("Review and approve");
    release.add_css_class("toolport-secondary-action");
    let release_for_click = release.clone();
    release.connect_clicked(move |_| {
        let Some(parent) = page.root.root().and_downcast::<gtk::Window>() else {
            return;
        };
        #[allow(deprecated)]
        let dialog = adw::MessageDialog::new(
            Some(&parent),
            Some(&trf("Re-approve {value1}?", &[("value1", &(entry.tool).to_string())])),
            Some(&trf("Toolport will approve this blocked finding. Findings without a saved definition require one more review of the current definition.\n\n{value1}", &[("value1", &(entry.detail).to_string())])),
        );
l10n::bind_formatted_dialog_heading(&dialog, msg("Re-approve {value1}?"), &[("value1", &(entry.tool).to_string())]);
l10n::bind_formatted_dialog_body(&dialog, msg("Toolport will approve this blocked finding. Findings without a saved definition require one more review of the current definition.\n\n{value1}"), &[("value1", &(entry.detail).to_string())]);
        if entry.has_definition {
            dialog.set_extra_child(Some(&quarantine_definition_expander(&entry)));
        }
        dialog.add_response("cancel", &tr("Keep blocked"));
 l10n::bind_dialog_response(&dialog, "cancel", msg("Keep blocked"));
        dialog.add_response("approve", &tr("Re-approve"));
 l10n::bind_dialog_response(&dialog, "approve", msg("Re-approve"));
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
                    let needed = if !entry.has_definition {
                        vec![(entry.profile.clone(), entry.tool.clone())]
                    } else {
                        Vec::new()
                    };
                    let tool = entry.tool.clone();
                    let profile = entry.profile.clone();
                    let result = gtk::gio::spawn_blocking(move || {
                        let profile = (!profile.is_empty()).then_some(profile.as_str());
                        crate::integrity::release_record(profile, &tool, &entry.record_id)
                            .map(|_| ())
                    })
                    .await;
                    match result {
                        Ok(Ok(())) => {
                            page.finish_quarantine_release(needed, false);
                        }
                        Ok(Err(error)) => page.show_error(&error),
                        Err(_) => page.show_error(&tr("the re-approval stopped unexpectedly")),
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
            .label(lookup(if entry.persistent {
                msg("Always allowed for this exact tool definition")
            } else {
                msg("Allowed for this Toolport session")
            }))
            .halign(gtk::Align::Start)
            .css_classes(["toolport-muted"])
            .build(),
    );
    row.append(&copy);
    let revoke = l10n::button("Require approval");
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
                    page.feedback.set_label(&tr("Approval exception removed."));
                    page.feedback.remove_css_class("error");
                    page.feedback.add_css_class("success");
                    page.refresh();
                }
                Ok(Err(error)) => page.show_error(&error),
                Err(_) => page.show_error(&tr("the approval update stopped unexpectedly")),
            }
        });
    });
    row.append(&revoke);
    row
}

fn set_team_managed(toggle: &gtk::Switch, forced: bool) {
    toggle.set_sensitive(!forced);
    if forced {
        l10n::bind_tooltip(toggle, msg("Required by your Toolport team"));
    } else {
        l10n::unbind(toggle, "tooltip-text");
        toggle.set_tooltip_text(None);
    }
}

/// A section heading with its own action on the same row. Both of these used to
/// sit on a line of their own, right-aligned against the whole page width.
fn settings_heading_with_action(
    title: &'static str,
    subtitle: &'static str,
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

/// A framed group of access rows: the default, or one access set and its servers.
fn access_group() -> gtk::Box {
    let group = gtk::Box::new(gtk::Orientation::Vertical, 0);
    group.add_css_class("toolport-settings-group");
    group.add_css_class("toolport-switch-group");
    group
}

/// A row with a title and one line under it, following the UI language; the caller appends
/// the control.
fn access_text_row(title: &'static str, detail: &'static str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.add_css_class("toolport-setting-row");
    let copy = gtk::Box::new(gtk::Orientation::Vertical, 3);
    copy.set_hexpand(true);
    for (text, class) in [(title, "heading"), (detail, "toolport-muted")] {
        copy.append(&source_label(
            text,
            gtk::Label::builder()
                .label(lookup(text))
                .xalign(0.0)
                .wrap(true)
                .css_classes([class])
                .build(),
        ));
    }
    row.append(&copy);
    row
}

/// A top-level group of the page: a larger title, one line saying what is in it,
/// and the box its rows go into.
fn settings_section(page: &gtk::Box, title: &'static str, summary: &'static str) -> gtk::Box {
    let section = gtk::Box::new(gtk::Orientation::Vertical, 14);
    section.set_margin_top(12);
    section.append(&section_title(title, summary));
    page.append(&section);
    section
}

fn section_title(title: &'static str, summary: &'static str) -> gtk::Box {
    let heading = gtk::Box::new(gtk::Orientation::Vertical, 3);
    heading.append(&source_label(
        title,
        gtk::Label::builder()
            .label(title)
            .halign(gtk::Align::Start)
            .css_classes(["title-3"])
            .build(),
    ));
    heading.append(&source_label(
        summary,
        gtk::Label::builder()
            .label(summary)
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .hexpand(true)
            .css_classes(["toolport-muted"])
            .build(),
    ));
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

fn settings_heading(title: &'static str, subtitle: &'static str) -> gtk::Box {
    let heading = gtk::Box::new(gtk::Orientation::Vertical, 3);
    // The page's own 14px spacing is the gap between rows within a section, so a
    // heading sat the same distance from the section above it as from its own
    // body, and read as belonging to the block before it.
    heading.set_margin_top(10);
    heading.append(&source_label(
        title,
        gtk::Label::builder()
            .label(title)
            .halign(gtk::Align::Start)
            .css_classes(["toolport-section-label"])
            .build(),
    ));
    heading.append(&source_label(
        subtitle,
        gtk::Label::builder()
            .label(subtitle)
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .hexpand(true)
            .css_classes(["toolport-muted"])
            .build(),
    ));
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
    let card = super::state_card("view-list-symbolic", msg("Nothing here yet"), "", false);
    // These lists are already row groups; the card drops its own frame inside one.
    card.add_css_class("frameless");
    let detail = card.last_child().and_downcast::<gtk::Label>().unwrap();
    detail.set_label(text);
    card
}

fn setting_switch_row(title: &'static str, description: &'static str) -> (gtk::Box, gtk::Switch) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.add_css_class("toolport-setting-row");
    let copy = gtk::Box::new(gtk::Orientation::Vertical, 3);
    copy.set_hexpand(true);
    copy.append(&source_label(
        title,
        gtk::Label::builder()
            .label(title)
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .hexpand(true)
            .css_classes(["heading"])
            .build(),
    ));
    copy.append(&source_label(
        description,
        gtk::Label::builder()
            .label(description)
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .hexpand(true)
            .css_classes(["toolport-muted"])
            .build(),
    ));
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
            .ok_or_else(|| tr("The access set is unavailable")),
    }
}

fn level_name(level: crate::registry::SafetyLevel) -> &'static str {
    match level {
        crate::registry::SafetyLevel::Off => msg("Off"),
        crate::registry::SafetyLevel::Ask => msg("Ask"),
        crate::registry::SafetyLevel::Strict => msg("Strict"),
    }
}

fn source_label(source: &'static str, label: gtk::Label) -> gtk::Label {
    l10n::bind_label(&label, source);
    label
}

fn save_ui_language(
    picker: &gtk::DropDown,
    feedback: &gtk::Label,
    notice: &gtk::Box,
    choice: String,
) {
    // Mark any initial pending read stale before submitting a newer user choice.
    notice.set_widget_name("toolport-language-suggestion-changed");
    picker.set_sensitive(false);
    notice.set_sensitive(false);
    let picker = picker.downgrade();
    let feedback = feedback.downgrade();
    let notice = notice.downgrade();
    gtk::glib::spawn_future_local(async move {
        let saved = gtk::gio::spawn_blocking(move || crate::i18n::set_language(&choice))
            .await
            .unwrap_or_else(|_| Err("language preference worker stopped".to_owned()));
        if let Some(picker) = picker.upgrade() {
            // Refresh under the existing rebinding guard, including button saves.
            l10n::bind_language_choices(&picker);
            picker.set_sensitive(true);
        }
        if let Some(notice) = notice.upgrade() {
            notice.set_sensitive(true);
            if saved.is_ok() {
                notice.set_visible(false);
            }
        }
        match saved {
            Ok(()) => {
                if let Some(feedback) = feedback.upgrade() {
                    feedback.set_label("");
                    feedback.remove_css_class("error");
                }
                l10n::refresh_bindings();
                super::tray::refresh_language();
            }
            Err(error) => {
                if let Some(feedback) = feedback.upgrade() {
                    l10n::bind_formatted_label(
                        &feedback,
                        msg("Could not save language: {error}"),
                        &[("error", &error)],
                    );
                    feedback.add_css_class("error");
                }
            }
        }
    });
}

fn source_button<B: IsA<gtk::Button>>(source: &'static str, button: B) -> B {
    l10n::bind_button(button.upcast_ref::<gtk::Button>(), source);
    button
}

fn source_formatted_label(
    source: &'static str,
    values: &[(&str, &str)],
    label: gtk::Label,
) -> gtk::Label {
    l10n::bind_formatted_label(&label, source, values);
    label
}
/// A Help and data row: what the action does on the left, its button on the right.
fn help_action_row(title: &'static str, detail: &'static str, action: &gtk::Button) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.add_css_class("toolport-setting-row");
    let copy = gtk::Box::new(gtk::Orientation::Vertical, 3);
    copy.set_hexpand(true);
    let heading = l10n::label(title);
    heading.set_halign(gtk::Align::Start);
    heading.add_css_class("heading");
    copy.append(&heading);
    let text = l10n::label(detail);
    text.set_halign(gtk::Align::Fill);
    text.set_xalign(0.0);
    text.set_wrap(true);
    text.add_css_class("toolport-muted");
    copy.append(&text);
    row.append(&copy);
    row.append(action);
    row
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn endpoint_action_style_tracks_status_and_live_language() {
        let _data = crate::registry::DataDirTestEnv::new("endpoint-action-style");
        crate::registry::save(&crate::registry::Registry::default()).unwrap();
        adw::init().unwrap();
        crate::i18n::set_language("en").unwrap();
        let app = adw::Application::builder()
            .application_id("com.tsout.Toolport.EndpointStyleTest")
            .build();
        app.register(gtk::gio::Cancellable::NONE).unwrap();
        let bridge = super::super::http_bridge::BridgeController::default();
        let page = SettingsPage::new(bridge.clone(), crate::approval_broker::start_native());
        page.select_page(4);
        let window = adw::ApplicationWindow::builder()
            .application(&app)
            .title("Toolport endpoint style fixture")
            .default_width(1024)
            .default_height(700)
            .content(&page.root)
            .build();
        let theme = super::super::theme::ThemeController::new();
        theme.attach(&window);
        window.present();
        let assert_action = |running: bool, label: &str| {
            let button = &page.endpoint_button;
            assert_eq!(button.label().as_deref(), Some(label));
            assert!(button.is_visible() && button.is_sensitive());
            assert_eq!(button.has_css_class("suggested-action"), !running);
            assert_eq!(button.has_css_class("toolport-destructive"), running);
            assert!(
                !button.has_css_class("toolport-secondary-action"),
                "secondary fill must never override the primary endpoint action"
            );
            assert!(
                !button.has_css_class("destructive-action"),
                "Stop is a destructive trigger, not a filled confirmation"
            );
            assert!(
                !bridge.status().running,
                "status fixtures never start an endpoint"
            );
        };
        let stopped = || crate::http_bridge::HttpBridgeStatus::new(None, None);
        let running = || {
            crate::http_bridge::HttpBridgeStatus::new(
                Some(54321),
                Some("synthetic-fixture-token".into()),
            )
        };
        assert_action(false, "Start");
        page.render_endpoint(stopped());
        assert_action(false, "Start");
        page.render_endpoint(running());
        assert_action(true, "Stop");
        crate::i18n::set_language("fr").unwrap();
        l10n::refresh_bindings();
        assert_action(true, "Arrêter");
        page.render_endpoint(stopped());
        assert_action(false, "Commencer");
        crate::i18n::set_language("en").unwrap();
        l10n::refresh_bindings();
        assert_action(false, "Start");
        if std::env::var_os("TOOLPORT_ENDPOINT_STYLE_VISUAL").is_some() {
            // Capture both styles without invoking the endpoint lifecycle or real calls.
            for (name, running) in [("fixture-running", true), ("fixture-stopped", false)] {
                let action = gtk::gio::SimpleAction::new(name, None);
                let target = page.clone();
                action.connect_activate(move |_, _| {
                    target.render_endpoint(crate::http_bridge::HttpBridgeStatus::new(
                        running.then_some(54321),
                        None,
                    ))
                });
                app.add_action(&action);
            }
            let action = gtk::gio::SimpleAction::new("close-fixture", None);
            let target = window.downgrade();
            action.connect_activate(move |_, _| {
                if let Some(window) = target.upgrade() {
                    window.close();
                }
            });
            app.add_action(&action);
            let loop_ = gtk::glib::MainLoop::new(None, false);
            let stop = loop_.clone();
            window.connect_close_request(move |_| {
                stop.quit();
                gtk::glib::Propagation::Proceed
            });
            let stop = loop_.clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_secs(120), move || {
                stop.quit()
            });
            window.present();
            loop_.run();
        }
        window.close();
    }

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn access_sets_are_cards_of_server_switches() {
        let _data = crate::registry::DataDirTestEnv::new("access-set-cards");
        crate::registry::save(&crate::registry::Registry::default()).unwrap();
        adw::init().unwrap();
        crate::i18n::set_language("en").unwrap();
        let page = SettingsPage::new(
            super::super::http_bridge::BridgeController::default(),
            crate::approval_broker::start_native(),
        );
        let registry: crate::registry::Registry = serde_json::from_value(serde_json::json!({
            "version": 3,
            "servers": [
                {"id": "github", "name": "GitHub", "transport": "stdio", "enabled": true},
                {"id": "stripe", "name": "Stripe", "transport": "stdio", "enabled": true},
                {"id": "linear", "name": "Linear", "transport": "stdio", "enabled": false}
            ],
            "profiles": [
                {"id": "work", "name": "Work", "enabledServerIds": ["github", "stripe"],
                 "toolScope": {"stripe": ["list_customers"]}},
                {"id": "empty", "name": "Empty", "enabledServerIds": []}
            ]
        }))
        .unwrap();
        page.render_access_sets(registry);
        fn walk(widget: &gtk::Widget, out: &mut Vec<gtk::Widget>) {
            out.push(widget.clone());
            let mut child = widget.first_child();
            while let Some(c) = child {
                walk(&c, out);
                child = c.next_sibling();
            }
        }
        let mut widgets = Vec::new();
        walk(page.access_list.upcast_ref(), &mut widgets);
        // One switch per server in each set, no checkboxes.
        let switches: Vec<gtk::Switch> = widgets
            .iter()
            .filter_map(|w| w.clone().downcast().ok())
            .collect();
        assert_eq!(switches.len(), 6);
        assert_eq!(switches.iter().filter(|s| s.is_active()).count(), 2);
        assert!(!widgets.iter().any(|w| w.is::<gtk::CheckButton>()));
        // Only servers in a set get a tools button, and a narrowed one says how many.
        let tools: Vec<String> = widgets
            .iter()
            .filter_map(|w| w.tooltip_text())
            .filter(|t| t.starts_with("Choose tools for"))
            .map(|t| t.to_string())
            .collect();
        assert_eq!(
            tools,
            ["Choose tools for GitHub", "Choose tools for Stripe"]
        );
        let labels: Vec<String> = widgets
            .iter()
            .filter_map(|w| w.clone().downcast::<gtk::Label>().ok())
            .map(|l| l.label().to_string())
            .collect();
        assert!(
            labels.contains(&"1 tool".to_string()) && labels.contains(&"all tools".to_string())
        );
        assert!(
            labels.contains(&"2 servers".to_string()) && labels.contains(&"0 servers".to_string())
        );
        // Each set and the default sit in their own framed group.
        let groups = widgets
            .iter()
            .filter(|w| w.has_css_class("toolport-switch-group"))
            .count();
        assert_eq!(groups, 3);
    }

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn b6_recovery_refresh_preserves_mutation_error() {
        let _data = crate::registry::DataDirTestEnv::new("b6-settings-errors");
        crate::registry::save(&crate::registry::Registry::default()).unwrap();
        adw::init().unwrap();
        let page = SettingsPage::new(
            super::super::http_bridge::BridgeController::default(),
            crate::approval_broker::start_native(),
        );
        page.show_read_error("Temporary read failure");
        page.show_error("Fixture save failed");
        page.refresh();
        assert!(page.feedback.label().contains("Fixture save failed"));
        gtk::glib::MainContext::default().block_on(async {
            for _ in 0..100 {
                if !page.refreshing.get() {
                    break;
                }
                gtk::glib::timeout_future(std::time::Duration::from_millis(20)).await;
            }
        });
        assert!(!page.refreshing.get());
        assert!(
            page.errors.first_child().is_none(),
            "successful recovery should clear read error only"
        );
        assert!(page.feedback.is_visible());
        assert!(page.feedback.label().contains("Fixture save failed"));
    }

    #[test]
    fn settings_pages_match_the_sidebar_order() {
        assert_eq!(
            super::SETTINGS_PAGES
                .iter()
                .map(|(label, _)| *label)
                .collect::<Vec<_>>(),
            [
                "General",
                "Tools",
                "Safety",
                "Access",
                "Connections",
                "Help and data"
            ]
        );
    }

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
            record_id: "displayed-record".to_string(),
            profile: profile.to_string(),
            tool: tool.to_string(),
            detail: "definition changed".to_string(),
            has_definition: false,
        }
    }

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn native_quarantine_capture_refresh_and_lazy_definition() {
        let data = crate::registry::DataDirTestEnv::new("native-quarantine-capture");
        adw::init().unwrap();
        let broker = crate::approval_broker::start_native();
        let page = SettingsPage::new(
            super::super::http_bridge::BridgeController::default(),
            broker,
        );
        let captured = serde_json::json!({"srv__wipe": {
            "tool":"srv__wipe", "server":"srv", "change":"changed", "reason":"Current definition needs review", "ts":1,
            "definition":{"name":"srv__wipe", "description":"Captured native fixture", "inputSchema":{"type":"object"}}
        }});
        std::fs::write(data.dir.join("quarantine.json"), "{}").unwrap();
        page.finish_quarantine_release(vec![("".into(), "srv__wipe".into())], false);
        assert!(page.feedback.label().contains("One more review is needed"));
        assert!(!page.feedback.label().contains("re-approved"));
        // An unrelated settings mutation must not strand the capture wait.
        page.begin_mutation();
        let path = data.dir.join("quarantine.json");
        let writer = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(150));
            std::fs::write(path, captured.to_string()).unwrap();
        });
        let context = gtk::glib::MainContext::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !page
            .feedback
            .label()
            .starts_with("Current definition captured")
        {
            assert!(
                std::time::Instant::now() < deadline,
                "captured record did not render"
            );
            context.iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        writer.join().unwrap();
        assert!(page.quarantine_list.first_child().is_some());
        let records = read_quarantined_tools().unwrap();
        let entry = records
            .iter()
            .find(|entry| entry.profile.is_empty())
            .unwrap();
        assert!(entry.has_definition);
        let expander = quarantine_definition_expander(entry);
        assert_eq!(
            expander
                .child()
                .unwrap()
                .downcast::<gtk::Label>()
                .unwrap()
                .label(),
            "Expand to load definition."
        );
        expander.set_expanded(true);
        while expander.child().unwrap().is::<gtk::Label>() {
            assert!(
                std::time::Instant::now() < deadline,
                "definition was not fetched on expand"
            );
            context.iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let scroll = expander
            .child()
            .unwrap()
            .downcast::<gtk::ScrolledWindow>()
            .unwrap();
        let viewport = scroll.child().unwrap().downcast::<gtk::Viewport>().unwrap();
        let label = viewport.child().unwrap().downcast::<gtk::Label>().unwrap();
        assert!(label.label().contains("Captured native fixture"));
        assert!(label.label().contains("inputSchema"));
    }

    #[test]
    fn bulk_capture_targets_skip_retained_blocks_and_timeout_feedback_is_honest() {
        let needed = vec![
            ("work".into(), "srv__old".into()),
            ("work".into(), "srv__released".into()),
        ];
        let remaining = vec![blocked("work", "srv__old")];
        assert_eq!(
            bulk_capture_targets(needed, &remaining),
            vec![("work".into(), "srv__released".into())]
        );
        assert!(capture_feedback(false).contains("has not captured"));
        assert!(!capture_feedback(false).contains("Waiting"));
        assert!(capture_feedback(true).contains("One more review is needed"));
    }

    #[test]
    fn release_refresh_waits_for_delayed_capture_and_stays_bounded() {
        let mut reads = 0;
        let records = wait_for_captured_quarantines(
            &[("work".into(), "srv__wipe".into())],
            || {
                reads += 1;
                if reads == 1 {
                    Ok(Vec::new())
                } else {
                    let mut record = blocked("work", "srv__wipe");
                    record.has_definition = true;
                    Ok(vec![record])
                }
            },
            std::time::Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(
            records.len(),
            1,
            "the delayed block must appear without manual Refresh"
        );
        let started = std::time::Instant::now();
        assert!(wait_for_captured_quarantines(
            &[("work".into(), "srv__wipe".into())],
            || Ok(Vec::new()),
            std::time::Duration::from_millis(20),
        )
        .unwrap()
        .is_empty());
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn posture_describes_the_effective_level_without_an_alarm_for_off() {
        let mut settings = crate::registry_controller::EssentialSettings {
            safety_level: crate::registry::SafetyLevel::Off,
            ..Default::default()
        };
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
        assert_eq!(
            message,
            "Saved 3 approvals. The gateway will publish approved definitions on its next refresh."
        );
        assert!(!is_error);
    }

    #[test]
    fn skipped_tools_keep_the_outcome_an_error_because_they_are_still_blocked() {
        let (message, is_error) = release_all_feedback(&ReleaseAllSummary {
            released: 2,
            changed: vec![],
            skipped: vec!["tool".to_string()],
            failed: Vec::new(),
        });
        assert_eq!(
            message,
            "Saved 2 approvals. 1 could not be repaired and is still blocked."
        );
        assert!(is_error);
    }

    #[test]
    fn new_or_changed_records_are_reported_as_still_blocked() {
        let (message, is_error) = release_all_feedback(&ReleaseAllSummary {
            released: 1,
            changed: vec!["new-tool".into(), "changed-tool".into()],
            ..ReleaseAllSummary::default()
        });
        assert_eq!(
            message,
            "Saved 1 approval. 2 new or changed findings remain blocked. Refresh and review them."
        );
        assert!(is_error);
    }

    #[test]
    fn a_failed_scope_outranks_skips_and_reports_what_did_get_through() {
        let (message, is_error) = release_all_feedback(&ReleaseAllSummary {
            released: 1,
            changed: vec![],
            skipped: vec!["tool".to_string()],
            failed: vec!["store locked".to_string()],
        });
        assert_eq!(
            message,
            "Saved 1 approval. 1 access-set scope could not be re-approved: store locked"
        );
        assert!(is_error);
    }
}

#[cfg(test)]
mod purge_tests {
    use super::*;

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn purge_confirmation_visual_fixture() {
        let _data = crate::registry::DataDirTestEnv::new("gtk-purge-confirmation");
        crate::registry::save(&crate::registry::Registry::default()).unwrap();
        adw::init().unwrap();
        let app = adw::Application::builder()
            .application_id("com.tsout.Toolport.PurgeFixture")
            .build();
        app.register(None::<&gtk::gio::Cancellable>).unwrap();
        let page = SettingsPage::new(
            super::super::http_bridge::BridgeController::default(),
            crate::approval_broker::start_native(),
        );
        let window = adw::ApplicationWindow::builder()
            .application(&app)
            .title("Toolport removal fixture")
            .default_width(900)
            .default_height(900)
            .content(&page.root)
            .build();
        // The button only says Remove; its row's heading says what it removes.
        fn find_button(widget: &gtk::Widget) -> Option<gtk::Button> {
            if widget.has_css_class("toolport-setting-row") {
                let heading = widget
                    .first_child()
                    .and_then(|copy| copy.first_child())
                    .and_downcast::<gtk::Label>();
                if heading.is_some_and(|label| label.text() == "Remove Toolport data") {
                    return widget.last_child().and_downcast::<gtk::Button>();
                }
            }
            let mut child = widget.first_child();
            while let Some(current) = child {
                if let Some(button) = find_button(&current) {
                    return Some(button);
                }
                child = current.next_sibling();
            }
            None
        }
        let button = find_button(page.root.upcast_ref()).unwrap();
        window.present();
        gtk::glib::idle_add_local_once(move || button.emit_clicked());
        let main_loop = gtk::glib::MainLoop::new(None, false);
        let stop = main_loop.clone();
        window.connect_close_request(move |_| {
            stop.quit();
            gtk::glib::Propagation::Proceed
        });
        let stop = main_loop.clone();
        gtk::glib::timeout_add_local_once(std::time::Duration::from_secs(180), move || stop.quit());
        main_loop.run();
    }
}
