//! Rows on the Clients page: one line per client with its controls on the right, and
//! Claude Code profiles or Codex homes grouped under one entry when a machine has several.

use super::*;

/// How a row is drawn: a row in a section's row group, or one profile inside a Claude
/// Code or Codex group.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum RowStyle {
    Line,
    Profile,
}

/// Clients that list one row per profile under a shared entry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum ProfileProduct {
    ClaudeCode,
    Codex,
}

impl ProfileProduct {
    pub(super) const ALL: [Self; 2] = [Self::ClaudeCode, Self::Codex];

    pub(super) fn of(client: &state::ClientView) -> Self {
        if client.id == "codex" || client.id.starts_with("codex@") {
            Self::Codex
        } else {
            Self::ClaudeCode
        }
    }

    fn client_id(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
        }
    }

    /// A product name, not translated.
    fn name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
        }
    }

    /// Whether this group shows its profiles. Kept across rescans.
    fn open(self, page: &ClientPage) -> &std::rc::Rc<std::cell::Cell<bool>> {
        match self {
            Self::ClaudeCode => &page.claude_open,
            Self::Codex => &page.codex_open,
        }
    }
}

/// The short discovery summary for a connected client, and the full sentence for its tooltip.
pub(super) fn discovery_summary(client: &state::ClientView) -> (String, String) {
    let capabilities = crate::clients::discovery_capabilities(&client.id);
    let chosen = client
        .discovery_mode
        .as_deref()
        .or(client.inherited_discovery.as_deref());
    if let Some(max) = capabilities.auto_full_max_tools_for(chosen) {
        // Auto lists a small catalog in full for this client, then switches to search.
        let text = crate::i18n::trf(
            "Auto: {mode}",
            &[(
                "mode",
                &crate::i18n::trf(
                    "full up to {count} tools, then lazy",
                    &[("count", &max.to_string())],
                ),
            )],
        );
        return (text.clone(), text);
    }
    match capabilities.resolve_mode(chosen) {
        "full" => (
            crate::i18n::tr("Full tool list"),
            crate::i18n::tr("Full tool list. Client per-tool permission rules need Full mode."),
        ),
        "grouped" => (
            crate::i18n::tr("Browse a server, then call tools"),
            crate::i18n::tr(
                "Browse a server, then call tools. Client per-tool permission rules need Full mode.",
            ),
        ),
        _ => (
            crate::i18n::tr("Search, then call tools"),
            crate::i18n::tr(
                "Search, then call tools. Client per-tool permission rules need Full mode.",
            ),
        ),
    }
}

/// "just now", "5m ago": the same words the React Clients page uses mid-sentence.
fn ago(timestamp_ms: u64) -> String {
    let seconds = epoch_ms().saturating_sub(timestamp_ms) / 1000;
    if seconds < 60 {
        return crate::i18n::tr("just now");
    }
    let count = if seconds < 3_600 {
        seconds / 60
    } else if seconds < 86_400 {
        seconds / 3_600
    } else {
        seconds / 86_400
    };
    crate::i18n::trf(
        if seconds < 3_600 {
            crate::i18n::msg("{count}m ago")
        } else if seconds < 86_400 {
            crate::i18n::msg("{count}h ago")
        } else {
            crate::i18n::msg("{count}d ago")
        },
        &[("count", &count.to_string())],
    )
}

/// The detail under a client's name, and what its tooltip adds to it (empty for none).
fn client_detail(client: &state::ClientView, style: RowStyle) -> (String, String) {
    if client.config_error {
        return (
            crate::i18n::tr("Configuration could not be read safely"),
            String::new(),
        );
    }
    let connected = client.gateway_state == state::ClientGatewayState::Connected;
    let mut parts = Vec::new();
    let mut tooltip = Vec::new();
    if style == RowStyle::Profile {
        if let Some(profile) = &client.claude_profile {
            // A profile without a known command is already named by its folder.
            if profile.command.is_some() {
                parts.push(profile.folder.clone());
            }
            if let Some(ms) = profile.last_used_ms {
                parts.push(crate::i18n::trf(
                    "Last active {value0}",
                    &[("value0", &ago(ms))],
                ));
            }
        }
    }
    if connected {
        if !client.uses_connectors && client.other_server_count > 0 {
            parts.push(crate::i18n::trf(
                if client.other_server_count == 1 {
                    crate::i18n::msg("{value0} other server")
                } else {
                    crate::i18n::msg("{value0} other servers")
                },
                &[("value0", &client.other_server_count.to_string())],
            ));
        }
    } else if client.uses_connectors {
        parts.push(crate::i18n::tr("Uses account connectors"));
    } else if style != RowStyle::Profile || client.server_count > 0 {
        // A profile row omits the empty case; its group header already names it.
        parts.push(if client.server_count == 0 {
            crate::i18n::tr("No local MCP servers")
        } else {
            crate::i18n::trf(
                if client.server_count == 1 {
                    crate::i18n::msg("{value0} local MCP server")
                } else {
                    crate::i18n::msg("{value0} local MCP servers")
                },
                &[("value0", &client.server_count.to_string())],
            )
        });
    }
    // No "to import" caption: Import intentionally leaves native entries in
    // place, so the native count is not what remains to import. The local
    // count above (and Connect's move count) stays accurate.
    if connected {
        if client.shared_http {
            parts.push("Shared HTTP".into());
        }
        let (short, full) = discovery_summary(client);
        parts.push(short);
        tooltip.push(full);
    }
    let text = parts.join(" · ");
    // Only what the line does not already say: a tooltip that repeats the
    // visible text is noise (it wraps, so nothing is cut off).
    let tooltip = tooltip
        .into_iter()
        .filter(|full| !text.contains(full.as_str()))
        .collect::<Vec<_>>()
        .join("\n");
    (text, tooltip)
}

/// "Connected" or "Custom configuration", as React labels it.
pub(super) fn status_badge(client: &state::ClientView) -> gtk::Label {
    let connected = client.gateway_state == state::ClientGatewayState::Connected;
    let badge = l10n::label(if connected {
        crate::i18n::msg("Connected")
    } else if client.gateway_state == state::ClientGatewayState::Customized {
        crate::i18n::msg("Custom configuration")
    } else if client.app_present || client.config_exists {
        crate::i18n::msg("Not connected")
    } else {
        crate::i18n::msg("Not installed")
    });
    let class = if connected {
        "success"
    } else if client.gateway_state == state::ClientGatewayState::Customized {
        "warning"
    } else {
        "off"
    };
    badge.add_css_class("toolport-badge");
    badge.add_css_class("caption");
    badge.add_css_class(class);
    badge.set_valign(gtk::Align::Center);
    badge.set_halign(gtk::Align::Start);
    badge
}

/// The width a row's logo and text keep before its controls wrap below them.
pub(super) const ROW_MIN_TEXT: i32 = 220;

/// "Used by T3 Code: Work, Personal (off)": every T3 Code instance that runs this client.
/// "T3 Code" is a product name and stays as is in every language.
pub(super) fn t3_code_badge_text(uses: &[crate::clients::T3CodeUse]) -> String {
    let names = uses
        .iter()
        .map(|instance| {
            if instance.enabled {
                instance.display_name.clone()
            } else {
                crate::i18n::trf("{name} (off)", &[("name", &instance.display_name)])
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    crate::i18n::trf("Used by T3 Code: {names}", &[("names", &names)])
}

/// One line per instance: whether it is on in T3 Code.
pub(super) fn t3_code_badge_tooltip(uses: &[crate::clients::T3CodeUse]) -> String {
    uses.iter()
        .map(|instance| {
            if instance.enabled {
                crate::i18n::trf(
                    "{name} is on in T3 Code",
                    &[("name", &instance.display_name)],
                )
            } else {
                crate::i18n::trf(
                    "{name} is off in T3 Code",
                    &[("name", &instance.display_name)],
                )
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Let a row's chip or detail wrap onto more lines in a narrow window instead of cutting
/// its text to what only a hover tooltip shows, so keyboard users can read all of it.
pub(super) fn wrap_row_text(label: &gtk::Label) {
    label.set_ellipsize(gtk::pango::EllipsizeMode::None);
    label.set_single_line_mode(false);
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    // Ask for one line's width: by default a WordChar label asks for its
    // minimum, so a chip got split ("Unrea-chable") with room to spare.
    label.set_natural_wrap_mode(gtk::NaturalWrapMode::None);
    label.set_xalign(0.0);
    if label.has_css_class("toolport-badge") {
        // A pill on one line, a rounded box over several.
        label.add_css_class("toolport-badge-wraps");
    }
}

/// The row badge for a client that T3 Code runs as a provider. It wraps in a narrow
/// window, like the account badge; the tooltip says which instances are on.
fn t3_code_badge(uses: &[crate::clients::T3CodeUse]) -> gtk::Label {
    let badge = gtk::Label::new(None);
    badge.add_css_class("toolport-badge");
    badge.add_css_class("caption");
    wrap_row_text(&badge);
    badge.set_valign(gtk::Align::Center);
    {
        let uses = uses.to_vec();
        l10n::bind_label_with(&badge, move || t3_code_badge_text(&uses));
    }
    {
        let uses = uses.to_vec();
        l10n::bind_tooltip_with(&badge, move || t3_code_badge_tooltip(&uses));
    }
    badge
}

/// One client: name and a one-line detail on the left, its controls on the right.
pub(super) fn client_row(
    client: &state::ClientView,
    page: ClientPage,
    style: RowStyle,
) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
    card.add_css_class("toolport-card");
    card.add_css_class("toolport-client-row");
    if style == RowStyle::Profile {
        card.add_css_class("toolport-client-profile");
    }
    // The logo and text; the controls join them on one line in a HeaderWrap.
    let lead = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let logo = (style != RowStyle::Profile).then(|| {
        let icon = branding::client_logo(&client.id);
        icon.set_valign(gtk::Align::Center);
        lead.append(&icon);
        icon.upcast::<gtk::Widget>()
    });
    let copy = gtk::Box::new(gtk::Orientation::Vertical, 2);
    copy.set_hexpand(true);
    copy.set_valign(gtk::Align::Center);
    // The name first at its full width; chips that do not fit beside it wrap below.
    let title = super::wrap_box::WrapBox::new(8, 4);
    let profile = client
        .claude_profile
        .as_ref()
        .filter(|_| style == RowStyle::Profile);
    // An unreadable profile has no account to show, signed in or not. Codex keeps its
    // account next to its tokens, so it is never read.
    let account_badge = profile.filter(|_| {
        !client.config_error && ProfileProduct::of(client) == ProfileProduct::ClaudeCode
    });
    let name = gtk::Label::builder()
        .label(match profile {
            Some(profile) => profile
                .command
                .clone()
                .unwrap_or_else(|| profile.folder.clone()),
            None => client.name.clone(),
        })
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .single_line_mode(true)
        .tooltip_text(crate::i18n::trf(
            "{value0}\nClient id: {value1}",
            &[("value0", &client.name), ("value1", &client.id)],
        ))
        .css_classes(["heading"])
        .build();
    wrap_row_text(&name);
    if profile.is_some() {
        name.add_css_class("monospace");
    }
    title.append(&name);
    if let Some(profile) = account_badge {
        let account = match &profile.account {
            Some(account) => gtk::Label::new(Some(account)),
            None => l10n::label("No claude.ai sign-in"),
        };
        account.add_css_class("toolport-badge");
        account.add_css_class("caption");
        wrap_row_text(&account);
        account.set_valign(gtk::Align::Center);
        title.append(&account);
    }
    if !client.t3_code.is_empty() {
        title.append(&t3_code_badge(&client.t3_code));
    }
    let open = gtk::Button::builder()
        .child(&title)
        .css_classes(["flat", "toolport-card-title"])
        .build();
    open.set_widget_name(&format!("detail-open:{}", client.id));
    let selected = client.clone();
    let selected_page = page.clone();
    open.connect_clicked(move |_| details::client(&selected, &selected_page));
    copy.append(&open);
    let detail = gtk::Label::builder()
        .css_classes(["toolport-muted", "caption"])
        .build();
    wrap_row_text(&detail);
    {
        let client = client.clone();
        l10n::bind_label_with(&detail, move || client_detail(&client, style).0);
    }
    {
        let client = client.clone();
        l10n::bind_tooltip_with(&detail, move || client_detail(&client, style).1);
    }
    copy.append(&detail);
    lead.append(&copy);

    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    controls.add_css_class("toolport-client-controls");
    controls.set_sensitive(!client.config_error && (client.app_present || client.config_exists));
    controls.set_valign(gtk::Align::Center);
    controls.set_halign(gtk::Align::End);
    // The status chip, as on React's rows; an unconnected row's Connect button
    // already says it is not connected.
    if client.gateway_state != state::ClientGatewayState::Disconnected {
        controls.append(&status_badge(client));
    }
    if client.legacy_bearer_argv {
        let migrate = l10n::button("Review migration to stdio");
        let reviewed_client = client.clone();
        let reviewed_page = page.clone();
        migrate.connect_clicked(move |button| {
            confirm_client_reset(&reviewed_client, button.clone(), reviewed_page.clone());
        });
        controls.append(&migrate);
    }
    match client.gateway_state {
        state::ClientGatewayState::Disconnected => {
            // A repeated row action is secondary; the page has no primary (spec 5).
            let connect = l10n::button("Connect");
            connect.add_css_class("toolport-secondary-action");
            connect.set_sensitive(client.app_present && !client.config_error);
            let client_for_connect = client.clone();
            let page_for_connect = page.clone();
            connect.connect_clicked(move |_| {
                confirm_client_migrate(&client_for_connect, page_for_connect.clone());
            });
            controls.append(&connect);
        }
        state::ClientGatewayState::Customized if client_needs_reset_action(client) => {
            let reset = l10n::button("Reset");
            reset.add_css_class("toolport-secondary-action");
            let client_for_reset = client.clone();
            let page_for_reset = page.clone();
            reset.connect_clicked(move |button| {
                confirm_client_reset(&client_for_reset, button.clone(), page_for_reset.clone());
            });
            controls.append(&reset);
        }
        state::ClientGatewayState::Customized => {}
        state::ClientGatewayState::Connected => {
            controls.append(&client_scope_menu(client.clone(), page.clone()));
            controls.append(&connected_client_actions_menu(client.clone(), page.clone()));
        }
    }
    if shows_remove_location(client)
        && client.gateway_state == state::ClientGatewayState::Disconnected
    {
        controls.append(&remove_location_button(client, page.clone()));
    }
    controls.set_visible(controls.first_child().is_some());
    // Narrow windows move the controls under the text instead of cutting them off.
    let line = super::page_header::HeaderWrap::row(&lead, &controls, ROW_MIN_TEXT, logo.as_ref());
    card.prepend(&line);
    if client.legacy_bearer_argv {
        let warning = l10n::label("This older HTTP connection exposes a bearer credential in process arguments. Review migration to stdio; it stays unchanged until you confirm.");
        warning.set_wrap(true);
        warning.set_xalign(0.0);
        warning.add_css_class("toolport-muted");
        card.append(&warning);
    }
    card
}

/// Whether a client shows Remove location: when it can be forgotten, and disabled with
/// why for a T3 Code home T3 Code still lists (React's client details says the same).
pub(super) fn shows_remove_location(client: &state::ClientView) -> bool {
    client.location_removable || client.location_kept_by_t3_code
}

pub(super) fn remove_location_button(client: &state::ClientView, page: ClientPage) -> gtk::Button {
    let remove = gtk::Button::from_icon_name("user-trash-symbolic");
    remove.add_css_class("flat");
    remove.set_valign(gtk::Align::Center);
    remove.set_sensitive(!client.config_error && !client.location_kept_by_t3_code);
    let kept = client.location_kept_by_t3_code;
    // What it does, or why it is disabled.
    remove.set_tooltip_text(Some(&crate::i18n::tr(if kept {
        "T3 Code still uses this folder. Remove it from T3 Code's provider settings first."
    } else {
        "Remove location"
    })));
    l10n::bind_tooltip(
        &remove,
        if kept {
            "T3 Code still uses this folder. Remove it from T3 Code's provider settings first."
        } else {
            "Remove location"
        },
    );
    // Both follow language changes, like the tooltip.
    let name = client.name.clone();
    l10n::bind_accessible_with(&remove, move || {
        (
            crate::i18n::trf("Remove location {value0}", &[("value0", &name)]),
            kept.then(|| {
                crate::i18n::tr(
                    "T3 Code still uses this folder. Remove it from T3 Code's provider settings first.",
                )
            }),
        )
    });
    let id = client.id.clone();
    remove.connect_clicked(move |_| {
        let Some(parent) = page.root.root().and_downcast::<gtk::Window>() else {
            return;
        };
        #[allow(deprecated)]
        let dialog = adw::MessageDialog::new(
            Some(&parent),
            Some(&crate::i18n::tr("Remove client location?")),
            Some(&crate::i18n::tr("This forgets the added location. Your client config and backups are kept. Auto-detected Claude folders still appear in Clients.")),
        );
        l10n::bind_dialog_heading(&dialog, "Remove client location?");
        l10n::bind_dialog_body(&dialog, "This forgets the added location. Your client config and backups are kept. Auto-detected Claude folders still appear in Clients.");
        dialog.add_responses(&[("cancel", "Cancel"), ("remove", "Remove location")]);
        l10n::bind_dialog_response(&dialog, "cancel", "Cancel");
        l10n::bind_dialog_response(&dialog, "remove", "Remove location");
        dialog.set_close_response("cancel");
        dialog.set_default_response(Some("cancel"));
        dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
        let id = id.clone();
        let page = page.clone();
        dialog.connect_response(Some("remove"), move |_, _| {
            let id = id.clone();
            let page = page.clone();
            gtk::glib::spawn_future_local(async move {
                match gtk::gio::spawn_blocking(move || crate::clients::remove_client_location(&id))
                    .await
                {
                    Ok(Ok(())) => page
                        .refresh_with_confirmation(crate::i18n::tr("Client location removed.")),
                    Ok(Err(error)) => page.show_error(&error),
                    Err(_) => page.show_error(&crate::i18n::tr("Could not remove client location.")),
                }
            });
        });
        dialog.present();
    });
    remove
}

/// Rows an enabled T3 Code instance uses that Toolport can connect now: the app is
/// present, its config reads, and it has no Toolport entry yet. A customized entry needs
/// the row's own Reset, so it is left to the user. React's `t3CodeTargets` matches.
pub(super) fn t3_code_targets(clients: &[state::ClientView]) -> Vec<&state::ClientView> {
    clients
        .iter()
        .filter(|client| {
            client.t3_code.iter().any(|instance| instance.enabled)
                && client.app_present
                && !client.config_error
                && client.gateway_state == state::ClientGatewayState::Disconnected
        })
        .collect()
}

/// Why a row an enabled T3 Code instance uses is not connected and can't be connected as
/// it is, in the order the card lists them (the Clients sections' order).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum T3LeftOutReason {
    Customized,
    Unreadable,
    Missing,
}

/// A row left out that way: its name, why, and the enabled instances that use it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct T3LeftOut {
    pub(super) client: String,
    pub(super) reason: T3LeftOutReason,
    pub(super) names: Vec<String>,
}

/// Rows used by an enabled instance that are neither connected nor targets,
/// with why (the row's own status): customized, then unreadable, then not installed, each
/// in scan order. Rows that are not installed are included even though the page does not
/// list them. React's `t3CodeLeftOut` finds the same, in the same order.
pub(super) fn t3_code_left_out(clients: &[state::ClientView]) -> Vec<T3LeftOut> {
    let mut left = clients
        .iter()
        .filter_map(|client| {
            let names = client
                .t3_code
                .iter()
                .filter(|instance| instance.enabled)
                .map(|instance| instance.display_name.clone())
                .collect::<Vec<_>>();
            if names.is_empty() {
                return None;
            }
            let reason = if client.config_error {
                T3LeftOutReason::Unreadable
            } else if client.gateway_state == state::ClientGatewayState::Customized {
                T3LeftOutReason::Customized
            } else if client.gateway_state == state::ClientGatewayState::Connected
                || client.app_present
            {
                return None;
            } else {
                T3LeftOutReason::Missing
            };
            Some(T3LeftOut {
                client: client.name.clone(),
                reason,
                names,
            })
        })
        .collect::<Vec<_>>();
    left.sort_by_key(|row| row.reason);
    left
}

/// One line per left-out row: who uses it, why it isn't connected, what to do. React's
/// `t3CodeLeftOutLines` words them the same way.
pub(super) fn t3_code_left_out_lines(left: &[T3LeftOut]) -> Vec<String> {
    left.iter()
        .map(|row| {
            let one = row.names.len() == 1;
            let names = row.names.join(", ");
            let values = [("names", names.as_str()), ("client", row.client.as_str())];
            match row.reason {
                T3LeftOutReason::Unreadable => crate::i18n::trf(
                    if one {
                        "{names} uses {client}, but Toolport can't read its config. Open {client} below to see why, then connect it."
                    } else {
                        "{names} use {client}, but Toolport can't read its config. Open {client} below to see why, then connect it."
                    },
                    &values,
                ),
                T3LeftOutReason::Customized => crate::i18n::trf(
                    if one {
                        "{names} uses {client}, whose Toolport entry was changed by hand. Open {client} below and reset its Toolport entry."
                    } else {
                        "{names} use {client}, whose Toolport entry was changed by hand. Open {client} below and reset its Toolport entry."
                    },
                    &values,
                ),
                T3LeftOutReason::Missing => crate::i18n::trf(
                    if one {
                        "{names} uses {client}, which isn't installed. Install {client}, then connect it."
                    } else {
                        "{names} use {client}, which isn't installed. Install {client}, then connect it."
                    },
                    &values,
                ),
            }
        })
        .collect()
}

/// Enabled instances that are not connected: the targets', the left-out rows', and the
/// ones Toolport can't connect at all. Only zero means every enabled one is connected.
pub(super) fn t3_code_unconnected_count(
    targets: &[&state::ClientView],
    left: &[T3LeftOut],
    others: &[crate::clients::T3CodeOther],
) -> usize {
    t3_code_instance_count(targets)
        + left.iter().map(|row| row.names.len()).sum::<usize>()
        + others
            .iter()
            .filter(|other| other.enabled && other.gap != crate::clients::T3CodeGap::Unreadable)
            .count()
}

/// Enabled T3 Code instances on the given rows. Two instances sharing one home are one row
/// and two instances.
pub(super) fn t3_code_instance_count(rows: &[&state::ClientView]) -> usize {
    rows.iter()
        .map(|client| {
            client
                .t3_code
                .iter()
                .filter(|instance| instance.enabled)
                .count()
        })
        .sum()
}

/// The T3 Code card's status line for this many unconnected enabled instances.
pub(super) fn t3_code_status_text(count: usize) -> String {
    match count {
        0 => crate::i18n::tr("Every enabled T3 Code instance is connected."),
        1 => crate::i18n::trf(
            "{count} enabled T3 Code instance is not connected.",
            &[("count", "1")],
        ),
        _ => crate::i18n::trf(
            "{count} enabled T3 Code instances are not connected.",
            &[("count", &count.to_string())],
        ),
    }
}

/// The 32px tile with React's "layers" glyph (lucide), drawn in the theme's secondary
/// text color so it follows light and dark themes. No icon theme ships this glyph.
fn layers_tile() -> gtk::Box {
    let tile = gtk::Box::new(gtk::Orientation::Vertical, 0);
    tile.add_css_class("toolport-status-tile");
    tile.set_valign(gtk::Align::Start);
    tile.set_halign(gtk::Align::Start);
    // The glyph expands to center itself; stop that reaching the card, or the tile
    // takes half the row.
    tile.set_hexpand(false);
    tile.set_vexpand(false);
    let glyph = gtk::DrawingArea::new();
    glyph.set_content_width(16);
    glyph.set_content_height(16);
    glyph.set_halign(gtk::Align::Center);
    glyph.set_valign(gtk::Align::Center);
    glyph.set_vexpand(true);
    glyph.set_hexpand(true);
    glyph.set_accessible_role(gtk::AccessibleRole::Presentation);
    glyph.set_draw_func(|area, cr, width, height| {
        let color = area.color();
        cr.set_source_rgba(
            color.red().into(),
            color.green().into(),
            color.blue().into(),
            color.alpha().into(),
        );
        // lucide "layers" is drawn on a 24px grid with a 2px stroke.
        let scale = f64::from(width.min(height)) / 24.0;
        cr.scale(scale, scale);
        cr.set_line_width(2.0);
        cr.set_line_cap(gtk::cairo::LineCap::Round);
        cr.set_line_join(gtk::cairo::LineJoin::Round);
        cr.move_to(12.0, 2.5);
        cr.line_to(21.5, 7.0);
        cr.line_to(12.0, 11.5);
        cr.line_to(2.5, 7.0);
        cr.close_path();
        for top in [12.0, 17.0] {
            cr.move_to(2.0, top);
            cr.line_to(12.0, top + 4.5);
            cr.line_to(22.0, top);
        }
        let _ = cr.stroke();
    });
    tile.append(&glyph);
    tile
}

/// The card's lines for T3 Code instances Toolport cannot connect, one per reason.
/// React's `t3CodeGapLines` builds the same lines.
pub(super) fn t3_code_gap_lines(others: &[crate::clients::T3CodeOther]) -> Vec<String> {
    let count =
        |gap: crate::clients::T3CodeGap| others.iter().filter(|other| other.gap == gap).count();
    let names = |gap: crate::clients::T3CodeGap| {
        others
            .iter()
            .filter(|other| other.gap == gap)
            .map(|other| {
                let name = if other.provider_name.is_empty()
                    || other.display_name == other.provider_name
                {
                    other.display_name.clone()
                } else {
                    crate::i18n::trf(
                        "{name} ({provider})",
                        &[
                            ("name", &other.display_name),
                            ("provider", &other.provider_name),
                        ],
                    )
                };
                if other.enabled {
                    name
                } else {
                    crate::i18n::trf("{name} (off)", &[("name", &name)])
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut lines = Vec::new();
    let unsupported = names(crate::clients::T3CodeGap::Unsupported);
    if !unsupported.is_empty() {
        lines.push(crate::i18n::trf(
            "Not supported by Toolport yet: {names}.",
            &[("names", &unsupported)],
        ));
    }
    let private_home = names(crate::clients::T3CodeGap::PrivateHome);
    if !private_home.is_empty() {
        lines.push(crate::i18n::trf(
            "Toolport can't connect {names} yet: T3 Code gives it a private settings folder.",
            &[("names", &private_home)],
        ));
    }
    let malformed = count(crate::clients::T3CodeGap::Malformed);
    if malformed > 0 {
        lines.push(crate::i18n::trf(
            if malformed == 1 {
                "Toolport can't read {names} in T3 Code's settings. Fix this instance in T3 Code's settings."
            } else {
                "Toolport can't read {names} in T3 Code's settings. Fix these instances in T3 Code's settings."
            },
            &[("names", &names(crate::clients::T3CodeGap::Malformed))],
        ));
    }
    let hidden_home = count(crate::clients::T3CodeGap::HiddenHome);
    if hidden_home > 0 {
        lines.push(crate::i18n::trf(
            if hidden_home == 1 {
                "Toolport can't see which home folder {names} uses: it is marked sensitive or isn't plain text. Set its home folder as plain text in T3 Code's provider settings."
            } else {
                "Toolport can't see which home folders {names} use: they are marked sensitive or aren't plain text. Set each home folder as plain text in T3 Code's provider settings."
            },
            &[("names", &names(crate::clients::T3CodeGap::HiddenHome))],
        ));
    }
    let relative_home = count(crate::clients::T3CodeGap::RelativeHome);
    if relative_home > 0 {
        lines.push(crate::i18n::trf(
            if relative_home == 1 {
                "Toolport can't tell which home folder {names} uses: it is a relative path. Set it to a full path in T3 Code's provider settings."
            } else {
                "Toolport can't tell which home folders {names} use: they are relative paths. Set each to a full path in T3 Code's provider settings."
            },
            &[("names", &names(crate::clients::T3CodeGap::RelativeHome))],
        ));
    }
    let outside_home = count(crate::clients::T3CodeGap::OutsideHome);
    if outside_home > 0 {
        lines.push(crate::i18n::trf(
            if outside_home == 1 {
                "Toolport won't use the home folder {names} uses: it isn't inside your home folder. Toolport only manages configs inside it; add it by hand as a client location if you trust it."
            } else {
                "Toolport won't use the home folders {names} use: they aren't inside your home folder. Toolport only manages configs inside it; add each by hand as a client location if you trust it."
            },
            &[("names", &names(crate::clients::T3CodeGap::OutsideHome))],
        ));
    }
    let linked_home = count(crate::clients::T3CodeGap::LinkedHome);
    if linked_home > 0 {
        lines.push(crate::i18n::trf(
            if linked_home == 1 {
                "Toolport won't use the home folder {names} uses: it, or its config file, is a link or not a plain folder or file. Point it at the real folder in T3 Code's provider settings, or add it by hand as a client location if you trust it."
            } else {
                "Toolport won't use the home folders {names} use: they, or their config files, are links or not plain folders or files. Point each at the real folder in T3 Code's provider settings, or add it by hand as a client location if you trust it."
            },
            &[("names", &names(crate::clients::T3CodeGap::LinkedHome))],
        ));
    }
    let missing_home = count(crate::clients::T3CodeGap::MissingHome);
    if missing_home > 0 {
        lines.push(crate::i18n::trf(
            if missing_home == 1 {
                "Toolport can't connect {names} yet: its home folder doesn't exist yet. Start it once in T3 Code, then refresh Clients."
            } else {
                "Toolport can't connect {names} yet: their home folders don't exist yet. Start each once in T3 Code, then refresh Clients."
            },
            &[("names", &names(crate::clients::T3CodeGap::MissingHome))],
        ));
    }
    let unlisted_home = count(crate::clients::T3CodeGap::UnlistedHome);
    if unlisted_home > 0 {
        lines.push(crate::i18n::trf(
            if unlisted_home == 1 {
                "Toolport makes rows only for T3 Code's Claude Code and Codex homes. Once {names}'s home folder exists, add it by hand as a client location to connect it."
            } else {
                "Toolport makes rows only for T3 Code's Claude Code and Codex homes. Once the home folders {names} use exist, add each by hand as a client location to connect it."
            },
            &[("names", &names(crate::clients::T3CodeGap::UnlistedHome))],
        ));
    }
    if let Some(file) = others
        .iter()
        .find(|other| other.gap == crate::clients::T3CodeGap::Unreadable)
    {
        lines.push(crate::i18n::trf(
            "Toolport can't read T3 Code's settings in {path}, so it can't list T3 Code's instances.",
            &[("path", &file.display_name)],
        ));
    }
    lines
}

/// The T3 Code card at the top of Clients: what T3 Code's providers are, how many enabled
/// instances are not connected, and why for each one Toolport can't connect as it is.
/// React's ClientsView card shows the same lines in the same order.
pub(super) fn t3_code_card(
    clients: &[state::ClientView],
    others: &[crate::clients::T3CodeOther],
) -> gtk::Box {
    let has_rows = clients.iter().any(|client| !client.t3_code.is_empty());
    // Rows an enabled instance uses that can't be connected as they are, each with why and
    // what to do, so the status never says everything is connected while one is not.
    let left_out = t3_code_left_out(clients);
    let unconnected = t3_code_unconnected_count(&t3_code_targets(clients), &left_out, others);
    // A named group, like `row_group`.
    let group = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .accessible_role(gtk::AccessibleRole::Group)
        .overflow(gtk::Overflow::Hidden)
        .css_classes(["toolport-row-group", "toolport-t3-card-group"])
        .build();
    group.update_property(&[gtk::accessible::Property::Label("T3 Code")]);
    let card = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    card.add_css_class("toolport-card");
    card.add_css_class("toolport-client-row");
    card.add_css_class("toolport-t3-card");
    card.append(&layers_tile());
    let copy = gtk::Box::new(gtk::Orientation::Vertical, 2);
    copy.set_hexpand(true);
    copy.set_valign(gtk::Align::Center);
    // A product name, not translated.
    let title = gtk::Label::builder()
        .label("T3 Code")
        .xalign(0.0)
        .css_classes(["toolport-status-card-title"])
        .build();
    copy.append(&title);
    let body = l10n::label("T3 Code runs these clients as providers. Each reads its own MCP config, so Toolport connects to them, not to T3 Code.");
    body.set_xalign(0.0);
    body.set_wrap(true);
    body.add_css_class("toolport-muted");
    copy.append(&body);
    let status = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .css_classes(["caption"])
        .build();
    l10n::bind_label_with(&status, move || t3_code_status_text(unconnected));
    // With only instances Toolport cannot connect, there is nothing to count.
    status.set_visible(has_rows);
    copy.append(&status);
    // Left-out rows first, then instances Toolport can't use, as React orders them.
    let lines = |left: &[T3LeftOut], others: &[crate::clients::T3CodeOther]| {
        let mut lines = t3_code_left_out_lines(left);
        lines.extend(t3_code_gap_lines(others));
        lines
    };
    for (index, _) in lines(&left_out, others).iter().enumerate() {
        let (left, others) = (left_out.clone(), others.to_vec());
        let gap = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["caption"])
            .build();
        l10n::bind_label_with(&gap, move || {
            lines(&left, &others)
                .get(index)
                .cloned()
                .unwrap_or_default()
        });
        copy.append(&gap);
    }
    card.append(&copy);
    group.append(&card);
    group
}

/// A client's profiles under one entry: Claude Code profiles or Codex homes. Copies of the
/// default Claude Code profile (same account in `~/.claude`, read only through
/// CLAUDE_CONFIG_DIR) sit behind a note instead of a row.
pub(super) fn profile_group(
    product: ProfileProduct,
    profiles: &[&state::ClientView],
    page: ClientPage,
) -> gtk::Box {
    let group = gtk::Box::new(gtk::Orientation::Vertical, 0);
    group.add_css_class("toolport-client-group");
    let (copies, mut shown): (Vec<&state::ClientView>, Vec<&state::ClientView>) =
        profiles.iter().copied().partition(|client| {
            client
                .claude_profile
                .as_ref()
                .is_some_and(|profile| profile.same_as.is_some())
        });
    // Profiles started by a command first, by command; the rest by folder.
    shown.sort_by_key(|client| {
        let profile = client.claude_profile.as_ref();
        let command = profile.and_then(|profile| profile.command.clone());
        (
            command.is_none(),
            command.unwrap_or_else(|| profile.map(|p| p.folder.clone()).unwrap_or_default()),
        )
    });
    if let [only] = shown.as_slice() {
        // One real profile and its copies: an ordinary row, with the note under it.
        group.append(&client_row(only, page.clone(), RowStyle::Line));
        if !copies.is_empty() {
            group.append(&copies_note(&copies, &shown, page));
        }
        return group;
    }
    let connected = shown
        .iter()
        .filter(|client| client.gateway_state == state::ClientGatewayState::Connected)
        .count();

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let icon = branding::client_logo(product.client_id());
    icon.set_valign(gtk::Align::Center);
    header.append(&icon);
    let copy = gtk::Box::new(gtk::Orientation::Vertical, 2);
    copy.set_hexpand(true);
    copy.set_valign(gtk::Align::Center);
    let title = super::wrap_box::WrapBox::new(8, 4);
    let name = gtk::Label::new(Some(product.name()));
    name.add_css_class("heading");
    name.set_xalign(0.0);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    name.set_single_line_mode(true);
    title.append(&name);
    let count = l10n::formatted_label(
        if shown.len() == 1 {
            crate::i18n::msg("{value0} profile")
        } else {
            crate::i18n::msg("{value0} profiles")
        },
        &[("value0", &shown.len().to_string())],
    );
    count.add_css_class("toolport-badge");
    count.add_css_class("caption");
    count.set_valign(gtk::Align::Center);
    title.append(&count);
    copy.append(&title);
    let detail = l10n::formatted_label(
        "{connected} of {total} connected",
        &[
            ("connected", &connected.to_string()),
            ("total", &shown.len().to_string()),
        ],
    );
    detail.set_xalign(0.0);
    detail.add_css_class("toolport-muted");
    detail.add_css_class("caption");
    copy.append(&detail);
    header.append(&copy);
    let open = product.open(&page).clone();
    let chevron = gtk::Image::from_icon_name(if open.get() {
        "pan-down-symbolic"
    } else {
        "pan-end-symbolic"
    });
    chevron.add_css_class("toolport-muted");
    chevron.set_valign(gtk::Align::Center);
    header.append(&chevron);
    let toggle = gtk::Button::builder()
        .child(&header)
        .css_classes(["flat", "toolport-client-group-toggle"])
        .build();
    toggle.update_property(&[
        gtk::accessible::Property::Label(&crate::i18n::tr(if product == ProfileProduct::Codex {
            "Codex profiles"
        } else {
            "Claude Code profiles"
        })),
        gtk::accessible::Property::Description(&detail.text()),
    ]);
    toggle.update_state(&[gtk::accessible::State::Expanded(Some(open.get()))]);
    let header_card = gtk::Box::new(gtk::Orientation::Vertical, 0);
    header_card.add_css_class("toolport-card");
    header_card.add_css_class("toolport-client-row");
    header_card.append(&toggle);
    group.append(&header_card);

    let rows = gtk::Box::new(gtk::Orientation::Vertical, 0);
    for client in &shown {
        rows.append(&client_row(client, page.clone(), RowStyle::Profile));
    }
    if !copies.is_empty() {
        rows.append(&copies_note(&copies, &shown, page.clone()));
    }
    let revealer = gtk::Revealer::builder()
        .child(&rows)
        .reveal_child(open.get())
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .transition_duration(150)
        .build();
    group.append(&revealer);
    {
        toggle.connect_clicked(move |toggle| {
            let shown = !open.get();
            open.set(shown);
            revealer.set_reveal_child(shown);
            chevron.set_icon_name(Some(if shown {
                "pan-down-symbolic"
            } else {
                "pan-end-symbolic"
            }));
            toggle.update_state(&[gtk::accessible::State::Expanded(Some(shown))]);
        });
    }
    group
}

/// "Also found ~/.claude/.claude.json: the same account as claude …", with a toggle that
/// shows those copies as rows.
fn copies_note(
    copies: &[&state::ClientView],
    shown: &[&state::ClientView],
    page: ClientPage,
) -> gtk::Box {
    let note = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    line.add_css_class("toolport-card");
    line.add_css_class("toolport-client-profile");
    let folders = copies
        .iter()
        .filter_map(|client| client.claude_profile.as_ref())
        .map(|profile| profile.folder.clone())
        .collect::<Vec<_>>()
        .join(", ");
    let of = copies
        .iter()
        .filter_map(|client| client.claude_profile.as_ref()?.same_as.as_deref())
        .find_map(|id| shown.iter().find(|client| client.id == id))
        .and_then(|client| client.claude_profile.as_ref()?.command.clone())
        .unwrap_or_else(|| "claude".into());
    let text = l10n::formatted_label(
        "Also found {folder}: the same account as {command}. Claude Code only reads it when CLAUDE_CONFIG_DIR points there, as some background tools do.",
        &[("folder", &folders), ("command", &of)],
    );
    text.set_wrap(true);
    text.set_xalign(0.0);
    text.set_hexpand(true);
    text.add_css_class("toolport-muted");
    text.add_css_class("caption");
    line.append(&text);
    let show = page.claude_copies_shown.get();
    let toggle = if show {
        l10n::button("Hide")
    } else {
        l10n::button("Show")
    };
    toggle.add_css_class("flat");
    toggle.set_valign(gtk::Align::Center);
    line.append(&toggle);
    note.append(&line);
    let rows = gtk::Box::new(gtk::Orientation::Vertical, 0);
    for client in copies {
        rows.append(&client_row(client, page.clone(), RowStyle::Profile));
    }
    let revealer = gtk::Revealer::builder()
        .child(&rows)
        .reveal_child(show)
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .transition_duration(150)
        .build();
    note.append(&revealer);
    toggle.connect_clicked(move |toggle| {
        let show = !page.claude_copies_shown.get();
        page.claude_copies_shown.set(show);
        revealer.set_reveal_child(show);
        l10n::bind_button(toggle, if show { "Hide" } else { "Show" });
    });
    note
}

#[cfg(test)]
mod caption_tests {
    use super::*;

    #[test]
    fn already_imported_native_entries_are_not_reported_as_still_to_import() {
        // Codex after importing its firstuser-added server: both native entries
        // stay in config.toml, so movable_server_count is still 2.
        let mut client = state::ClientView {
            detected: None,
            imported_names: vec!["firstuser-added".into(), "keyring-many".into()],
            id: "codex".into(),
            name: "Codex".into(),
            app_present: true,
            config_exists: true,
            uses_connectors: false,
            server_count: 2,
            movable_server_count: 2,
            other_server_count: 2,
            gateway_state: state::ClientGatewayState::Disconnected,
            shared_http: false,
            legacy_bearer_argv: false,
            scope_id: None,
            scope_name: None,
            discovery_mode: None,
            inherited_discovery: None,
            config_error: false,
            location_removable: false,
            location_kept_by_t3_code: false,
            claude_profile: None,
            t3_code: Vec::new(),
        };
        for style in [RowStyle::Line, RowStyle::Profile] {
            let (text, tooltip) = client_detail(&client, style);
            assert!(text.contains("2 local MCP servers"), "{style:?}: {text}");
            assert!(!text.contains("to import"), "{style:?}: {text}");
            assert!(!tooltip.contains("to import"), "{style:?}: {tooltip}");
        }
        client.server_count = 0;
        client.movable_server_count = 0;
        client.other_server_count = 0;
        assert_eq!(
            client_detail(&client, RowStyle::Line).0,
            "No local MCP servers"
        );
        assert!(!client_detail(&client, RowStyle::Profile)
            .0
            .contains("local MCP"));
        client.server_count = 1;
        assert_eq!(
            client_detail(&client, RowStyle::Line).0,
            "1 local MCP server"
        );
        // Connected clients keep their other-server inventory.
        client.gateway_state = state::ClientGatewayState::Connected;
        client.other_server_count = 3;
        client.movable_server_count = 3;
        let (text, _) = client_detail(&client, RowStyle::Line);
        assert!(text.contains("3 other servers"), "{text}");
        assert!(!text.contains("to import"), "{text}");
        // A tooltip only says what the line does not: Auto's summary is the
        // whole sentence, so it gets none; the other modes add the Full note.
        client.id = "claude-code".into();
        client.discovery_mode = Some("auto".into());
        let (text, tooltip) = client_detail(&client, RowStyle::Line);
        assert!(text.contains("Auto: "), "{text}");
        assert_eq!(tooltip, "", "{text}");
        client.discovery_mode = Some("lazy".into());
        let (text, tooltip) = client_detail(&client, RowStyle::Line);
        assert!(
            !tooltip.is_empty() && !text.contains(&tooltip),
            "{text} / {tooltip}"
        );
        assert!(tooltip.contains("Full mode"), "{tooltip}");
        // A line with nothing more to say has no tooltip.
        client.gateway_state = state::ClientGatewayState::Disconnected;
        assert_eq!(client_detail(&client, RowStyle::Line).1, "");
        client.config_error = true;
        assert_eq!(client_detail(&client, RowStyle::Line).1, "");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instance(name: &str, enabled: bool) -> crate::clients::T3CodeUse {
        crate::clients::T3CodeUse {
            instance_id: name.to_lowercase(),
            display_name: name.into(),
            enabled,
        }
    }

    fn client(id: &str, state: state::ClientGatewayState) -> state::ClientView {
        state::ClientView {
            detected: None,
            imported_names: Vec::new(),
            id: id.into(),
            name: id.into(),
            app_present: true,
            config_exists: true,
            uses_connectors: false,
            server_count: 0,
            movable_server_count: 0,
            other_server_count: 0,
            gateway_state: state,
            shared_http: false,
            legacy_bearer_argv: false,
            scope_id: None,
            scope_name: None,
            discovery_mode: None,
            inherited_discovery: None,
            config_error: false,
            location_removable: false,
            claude_profile: None,
            t3_code: Vec::new(),
        }
    }

    fn ids<'a>(rows: &[&'a state::ClientView]) -> Vec<&'a str> {
        rows.iter().map(|client| client.id.as_str()).collect()
    }

    #[test]
    fn t3_code_gap_lines_name_each_instance_and_why() {
        use crate::clients::{T3CodeGap, T3CodeOther};
        let other = |id: &str, provider: &str, name: &str, enabled, gap| T3CodeOther {
            instance_id: id.into(),
            provider_name: provider.into(),
            display_name: name.into(),
            enabled,
            gap,
        };
        assert!(t3_code_gap_lines(&[]).is_empty());
        assert_eq!(
            t3_code_gap_lines(&[
                other("muse", "Muse Code", "Muse Code", true, T3CodeGap::Unsupported),
                other("fx", "fx", "My fx", false, T3CodeGap::Unsupported),
                other(
                    "antigravity",
                    "Antigravity",
                    "Antigravity",
                    true,
                    T3CodeGap::PrivateHome
                ),
            ]),
            [
                "Not supported by Toolport yet: Muse Code, My fx (fx) (off).",
                "Toolport can't connect Antigravity yet: T3 Code gives it a private settings folder.",
            ]
        );
        assert_eq!(
            t3_code_gap_lines(&[other(
                "",
                "",
                "~/.t3/userdata/settings.json",
                true,
                T3CodeGap::Unreadable
            )]),
            ["Toolport can't read T3 Code's settings in ~/.t3/userdata/settings.json, so it can't list T3 Code's instances."]
        );
    }

    /// React's "names each one with why and what to do" and "words several of a kind in
    /// the plural" check the same lines.
    #[test]
    fn t3_code_gap_lines_say_why_an_instance_cannot_be_used_and_what_to_do() {
        use crate::clients::{T3CodeGap, T3CodeOther};
        let other = |id: &str, provider: &str, name: &str, enabled, gap| T3CodeOther {
            instance_id: id.into(),
            provider_name: provider.into(),
            display_name: name.into(),
            enabled,
            gap,
        };
        assert_eq!(
            t3_code_gap_lines(&[
                other("text_entry", "", "text_entry", true, T3CodeGap::Malformed),
                other("pi_hidden", "Pi", "Pi Hidden", true, T3CodeGap::HiddenHome),
                other("rel", "Codex", "Relative", false, T3CodeGap::RelativeHome),
                other("bad driver", "", "bad driver", false, T3CodeGap::Unsupported),
            ]),
            [
                "Not supported by Toolport yet: bad driver (off).",
                "Toolport can't read text_entry in T3 Code's settings. Fix this instance in T3 Code's settings.",
                "Toolport can't see which home folder Pi Hidden (Pi) uses: it is marked sensitive or isn't plain text. Set its home folder as plain text in T3 Code's provider settings.",
                "Toolport can't tell which home folder Relative (Codex) (off) uses: it is a relative path. Set it to a full path in T3 Code's provider settings.",
            ]
        );
        assert_eq!(
            t3_code_gap_lines(&[
                other("a", "Claude", "Claude", true, T3CodeGap::Malformed),
                other("b", "Grok", "Broken", true, T3CodeGap::Malformed),
                other("c", "Grok", "Grok", true, T3CodeGap::HiddenHome),
                other("d", "OpenCode", "Hidden", true, T3CodeGap::HiddenHome),
                other("e", "Codex", "One", true, T3CodeGap::RelativeHome),
                other("f", "Claude", "Two", true, T3CodeGap::RelativeHome),
            ]),
            [
                "Toolport can't read Claude, Broken (Grok) in T3 Code's settings. Fix these instances in T3 Code's settings.",
                "Toolport can't see which home folders Grok, Hidden (OpenCode) use: they are marked sensitive or aren't plain text. Set each home folder as plain text in T3 Code's provider settings.",
                "Toolport can't tell which home folders One (Codex), Two (Claude) use: they are relative paths. Set each to a full path in T3 Code's provider settings.",
            ]
        );
    }

    #[test]
    fn t3_code_status_counts_instances() {
        assert_eq!(
            t3_code_status_text(0),
            "Every enabled T3 Code instance is connected."
        );
        assert_eq!(
            t3_code_status_text(1),
            "1 enabled T3 Code instance is not connected."
        );
        assert_eq!(
            t3_code_status_text(3),
            "3 enabled T3 Code instances are not connected."
        );
    }

    /// React's "counts instances Toolport can't use" and the hostAttribution home tests
    /// check the same lines: a home outside home, a linked one, a missing one and an
    /// OpenCode, Grok or Pi one no row covers, each named by its instance only, with a
    /// next step.
    #[test]
    fn t3_code_gap_lines_say_why_a_home_is_not_used_and_what_to_do() {
        use crate::clients::{T3CodeGap, T3CodeOther};
        let other = |name: &str, provider: &str, enabled, gap| T3CodeOther {
            instance_id: name.to_lowercase(),
            provider_name: provider.into(),
            display_name: name.into(),
            enabled,
            gap,
        };
        assert_eq!(
            t3_code_gap_lines(&[
                other("Away", "Codex", true, T3CodeGap::OutsideHome),
                other("Linked", "Claude", false, T3CodeGap::LinkedHome),
                other("Work", "Claude", true, T3CodeGap::MissingHome),
                other("Grok", "Grok", true, T3CodeGap::RelativeHome),
                other("Notes", "OpenCode", true, T3CodeGap::UnlistedHome),
            ]),
            [
                "Toolport can't tell which home folder Grok uses: it is a relative path. Set it to a full path in T3 Code's provider settings.",
                "Toolport won't use the home folder Away (Codex) uses: it isn't inside your home folder. Toolport only manages configs inside it; add it by hand as a client location if you trust it.",
                "Toolport won't use the home folder Linked (Claude) (off) uses: it, or its config file, is a link or not a plain folder or file. Point it at the real folder in T3 Code's provider settings, or add it by hand as a client location if you trust it.",
                "Toolport can't connect Work (Claude) yet: its home folder doesn't exist yet. Start it once in T3 Code, then refresh Clients.",
                "Toolport makes rows only for T3 Code's Claude Code and Codex homes. Once Notes (OpenCode)'s home folder exists, add it by hand as a client location to connect it.",
            ]
        );
        assert_eq!(
            t3_code_gap_lines(&[
                other("A", "Codex", true, T3CodeGap::OutsideHome),
                other("B", "Claude", true, T3CodeGap::OutsideHome),
                other("C", "Claude", true, T3CodeGap::LinkedHome),
                other("D", "Pi", true, T3CodeGap::LinkedHome),
                other("E", "Grok", true, T3CodeGap::MissingHome),
                other("F", "Codex", false, T3CodeGap::MissingHome),
                other("G", "OpenCode", true, T3CodeGap::UnlistedHome),
                other("H", "Pi", false, T3CodeGap::UnlistedHome),
            ]),
            [
                "Toolport won't use the home folders A (Codex), B (Claude) use: they aren't inside your home folder. Toolport only manages configs inside it; add each by hand as a client location if you trust it.",
                "Toolport won't use the home folders C (Claude), D (Pi) use: they, or their config files, are links or not plain folders or files. Point each at the real folder in T3 Code's provider settings, or add it by hand as a client location if you trust it.",
                "Toolport can't connect E (Grok), F (Codex) (off) yet: their home folders don't exist yet. Start each once in T3 Code, then refresh Clients.",
                "Toolport makes rows only for T3 Code's Claude Code and Codex homes. Once the home folders G (OpenCode), H (Pi) (off) use exist, add each by hand as a client location to connect it.",
            ]
        );
        // Every enabled one counts as not connected; an off one does not.
        let others = [
            other("Away", "Codex", true, T3CodeGap::OutsideHome),
            other("Linked", "Claude", false, T3CodeGap::LinkedHome),
            other("Work", "Claude", true, T3CodeGap::MissingHome),
        ];
        assert_eq!(t3_code_unconnected_count(&[], &[], &others), 2);
    }

    #[test]
    fn t3_code_left_out_names_each_unconnected_row_with_why_and_what_to_do() {
        use state::ClientGatewayState::*;
        let mut rows = vec![
            client("A", Disconnected),
            client("C", Connected),
            client("Cursor", Customized),
            client("Codex", Disconnected),
            client("Kiro", Disconnected),
            client("Pi", Disconnected),
        ];
        rows[0].t3_code = vec![instance("A", true)];
        rows[1].t3_code = vec![instance("C", true)];
        rows[2].t3_code = vec![
            instance("Work", true),
            instance("Home", true),
            instance("Old", false),
        ];
        rows[3].t3_code = vec![instance("E", true)];
        rows[3].config_error = true;
        rows[4].t3_code = vec![instance("F", true)];
        rows[4].app_present = false;
        rows[4].config_exists = false;
        rows[5].t3_code = vec![instance("G", false)];
        rows[5].app_present = false;
        let left = t3_code_left_out(&rows);
        assert_eq!(
            left.iter()
                .map(|row| (row.client.as_str(), row.reason, row.names.clone()))
                .collect::<Vec<_>>(),
            [
                (
                    "Cursor",
                    T3LeftOutReason::Customized,
                    vec!["Work".to_string(), "Home".to_string()]
                ),
                ("Codex", T3LeftOutReason::Unreadable, vec!["E".to_string()]),
                ("Kiro", T3LeftOutReason::Missing, vec!["F".to_string()]),
            ]
        );
        assert_eq!(
            t3_code_left_out_lines(&left),
            [
                "Work, Home use Cursor, whose Toolport entry was changed by hand. Open Cursor below and reset its Toolport entry.",
                "E uses Codex, but Toolport can't read its config. Open Codex below to see why, then connect it.",
                "F uses Kiro, which isn't installed. Install Kiro, then connect it.",
            ]
        );
        use crate::clients::{T3CodeGap, T3CodeOther};
        let other = |name: &str, enabled, gap| T3CodeOther {
            instance_id: name.into(),
            provider_name: name.into(),
            display_name: name.into(),
            enabled,
            gap,
        };
        let others = [
            other("Muse Code", true, T3CodeGap::Unsupported),
            other("fx", false, T3CodeGap::Unsupported),
            other("~/f", true, T3CodeGap::Unreadable),
        ];
        // A, the left-out rows' four, and the enabled instance Toolport can't connect.
        assert_eq!(
            t3_code_unconnected_count(&t3_code_targets(&rows), &left, &others),
            6
        );
        assert_eq!(t3_code_unconnected_count(&[], &[], &[]), 0);
    }

    #[test]
    fn t3_code_targets_skip_connected_customized_broken_absent_and_off_rows() {
        use state::ClientGatewayState::*;
        let mut rows = vec![
            client("connected", Connected),
            client("customized", Customized),
            client("broken", Disconnected),
            client("absent", Disconnected),
            client("off", Disconnected),
            client("plain", Disconnected),
            client("shared", Disconnected),
            client("target", Disconnected),
        ];
        for row in &mut rows {
            row.t3_code = vec![instance(&row.id, true)];
        }
        rows[2].config_error = true;
        rows[3].app_present = false;
        rows[4].t3_code = vec![instance("Off", false)];
        rows[5].t3_code.clear();
        // Two enabled instances sharing one home, and an off one beside them.
        rows[6].t3_code = vec![
            instance("A", true),
            instance("B", true),
            instance("C", false),
        ];
        let targets = t3_code_targets(&rows);
        assert_eq!(ids(&targets), ["shared", "target"]);
        // One row per home; the status line counts enabled instances.
        assert_eq!(t3_code_instance_count(&targets), 3);
        assert!(t3_code_targets(&rows[..2]).is_empty());
    }

    #[test]
    fn t3_code_badge_names_each_instance_and_marks_off_ones() {
        let uses = [instance("Work", true), instance("Personal", false)];
        assert_eq!(
            t3_code_badge_text(&uses),
            "Used by T3 Code: Work, Personal (off)"
        );
        assert_eq!(
            t3_code_badge_tooltip(&uses),
            "Work is on in T3 Code\nPersonal is off in T3 Code"
        );
        assert_eq!(
            t3_code_badge_text(&[instance("Claude", true)]),
            "Used by T3 Code: Claude"
        );
    }

    #[test]
    fn a_t3_home_t3_code_still_lists_shows_remove_location_disabled_with_why() {
        let mut client = state::ClientView {
            detected: None,
            imported_names: Vec::new(),
            id: "codex@t3-0123456789abcdef".into(),
            name: "Codex (~/.codex2)".into(),
            app_present: true,
            config_exists: false,
            uses_connectors: false,
            server_count: 0,
            movable_server_count: 0,
            other_server_count: 0,
            gateway_state: state::ClientGatewayState::Disconnected,
            shared_http: false,
            legacy_bearer_argv: false,
            scope_id: None,
            scope_name: None,
            discovery_mode: None,
            inherited_discovery: None,
            config_error: false,
            location_removable: false,
            location_kept_by_t3_code: true,
            claude_profile: None,
            t3_code: Vec::new(),
        };
        assert!(shows_remove_location(&client));
        client.location_kept_by_t3_code = false;
        assert!(
            !shows_remove_location(&client),
            "other kept rows stay hidden"
        );
        client.location_removable = true;
        assert!(shows_remove_location(&client));
    }

    #[test]
    #[ignore = "requires isolated GTK display"]
    fn t3_code_badge_wraps_in_a_narrow_row_instead_of_cutting_names() {
        gtk::init().expect("isolated GTK desktop");
        let badge = t3_code_badge(&[
            instance(
                "Claude account number 07 for the long running client work",
                true,
            ),
            instance("Personal", false),
        ]);
        assert!(badge.wraps());
        assert_eq!(badge.ellipsize(), gtk::pango::EllipsizeMode::None);
        let natural = badge.measure(gtk::Orientation::Horizontal, -1).1;
        let (_, one_line, _, _) = badge.measure(gtk::Orientation::Vertical, natural);
        let (_, narrow, _, _) = badge.measure(gtk::Orientation::Vertical, natural / 3);
        assert!(
            narrow > one_line,
            "wraps onto more lines: {narrow} vs {one_line}"
        );
    }
}
