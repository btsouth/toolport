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

/// The one-line detail under a client's name, and the fuller text for its tooltip.
fn client_detail(client: &state::ClientView, style: RowStyle) -> (String, String) {
    if client.config_error {
        let text = crate::i18n::tr("Configuration could not be read safely");
        return (text.clone(), text);
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
    let tooltip = if tooltip.is_empty() {
        text.clone()
    } else {
        format!("{text}\n{}", tooltip.join("\n"))
    };
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
const ROW_MIN_TEXT: i32 = 220;

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
