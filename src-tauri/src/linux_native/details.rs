//! Pushed details share the main window and keep list state alive behind Back.
use super::*;
use std::{cell::RefCell, rc::Rc};

#[derive(Clone, Default)]
pub(super) struct DetailHost {
    stack: gtk::Stack,
    pub selected: Rc<RefCell<Option<String>>>,
    pub tools: Rc<std::cell::Cell<bool>>,
    focus: Rc<RefCell<Option<gtk::glib::WeakRef<gtk::Widget>>>>,
}

impl DetailHost {
    pub fn attach(&self, root: &gtk::Box) {
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        while let Some(child) = root.first_child() {
            root.remove(&child);
            list.append(&child);
        }
        self.stack.set_vexpand(true);
        self.stack.add_named(&list, Some("list"));
        root.append(&self.stack);
    }

    pub fn back(&self) {
        let Some(id) = self.selected.borrow_mut().take() else {
            return;
        };
        self.stack.set_visible_child_name("list");
        let target = self
            .stack
            .child_by_name("list")
            .and_then(|list| find_named(&list, &format!("detail-open:{id}")));
        let restored = target.is_some_and(|target| target.grab_focus())
            || self
                .focus
                .borrow()
                .as_ref()
                .and_then(|weak| weak.upgrade())
                .filter(|focus| focus.root().is_some())
                .is_some_and(|focus| focus.grab_focus());
        if !restored {
            self.stack.child_focus(gtk::DirectionType::TabForward);
        }
        if let Some(detail) = self.stack.child_by_name("detail") {
            self.stack.remove(&detail);
        }
    }

    pub fn push(&self, id: &str, root: &gtk::Box) {
        let current_focus = self
            .stack
            .root()
            .and_downcast::<gtk::Window>()
            .and_then(|w| gtk::prelude::GtkWindowExt::focus(&w));
        let same_destination = self.selected.borrow().as_deref() == Some(id);
        let preserve_external_focus = same_destination
            && current_focus.as_ref().is_some_and(|focus| {
                focus.root().is_some()
                    && self
                        .stack
                        .child_by_name("detail")
                        .is_some_and(|old| !focus.is_ancestor(&old))
            });
        let previous_path = self.stack.child_by_name("detail").and_then(|old| {
            current_focus
                .as_ref()
                .filter(|focus| focus.is_ancestor(&old))
                .map(|focus| focus_path(focus, &old))
        });
        if self.selected.borrow().is_none() {
            let focus = self
                .stack
                .root()
                .and_downcast::<gtk::Window>()
                .and_then(|w| gtk::prelude::GtkWindowExt::focus(&w));
            *self.focus.borrow_mut() = focus.map(|w| w.downgrade());
        }
        *self.selected.borrow_mut() = Some(id.to_string());
        if let Some(old) = self.stack.child_by_name("detail") {
            self.stack.remove(&old);
        }
        self.stack.add_named(root, Some("detail"));
        self.stack.set_visible_child_name("detail");
        if preserve_external_focus {
            return;
        }
        if let Some(focus) = previous_path
            .and_then(|path| follow_focus_path(root.upcast_ref(), &path))
            .filter(|focus| focus.is_sensitive() && focus.is_visible())
        {
            focus.grab_focus();
        } else if let Some(back) = find_class(root.upcast_ref(), "toolport-page-back") {
            back.grab_focus();
        }
    }

    pub fn header(&self, title: &str, clients: bool) -> page_header::PageHeader {
        let header = page_header::PageHeader::new(
            if clients { "Clients" } else { "Servers" },
            if clients { "MCP client" } else { "Overview" },
        );
        header.title.set_label(title);
        let back = header.set_back(
            if clients { "Clients" } else { "Servers" },
            if clients {
                "Back to clients"
            } else {
                "Back to servers"
            },
        );
        let host = self.clone();
        back.connect_clicked(move |_| host.back());
        header
    }
}

fn find_class(root: &gtk::Widget, class: &str) -> Option<gtk::Widget> {
    if root.has_css_class(class) {
        return Some(root.clone());
    }
    let mut child = root.first_child();
    while let Some(widget) = child {
        if let Some(found) = find_class(&widget, class) {
            return Some(found);
        }
        child = widget.next_sibling();
    }
    None
}

fn find_named(root: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
    if root.widget_name() == name {
        return Some(root.clone());
    }
    let mut child = root.first_child();
    while let Some(widget) = child {
        if let Some(found) = find_named(&widget, name) {
            return Some(found);
        }
        child = widget.next_sibling();
    }
    None
}

fn heading(source: &'static str) -> gtk::Label {
    let label = l10n::label(source);
    label.set_xalign(0.0);
    label.set_halign(gtk::Align::Start);
    label.add_css_class("toolport-section-label");
    label
}

fn text(value: &str, mono: bool) -> gtk::Label {
    let label = gtk::Label::builder()
        .label(value)
        .xalign(0.0)
        .wrap(true)
        .selectable(true)
        .build();
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.add_css_class(if mono { "monospace" } else { "toolport-muted" });
    label
}

fn message(source: &'static str) -> gtk::Label {
    let label = text("", false);
    l10n::bind_label(&label, source);
    label
}

fn formatted_message(
    source: &'static str,
    values: &[(&str, &str)],
    translated: &[(&str, &'static str)],
) -> gtk::Label {
    let values = values
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect::<Vec<_>>();
    let translated = translated
        .iter()
        .map(|(key, value)| (key.to_string(), *value))
        .collect::<Vec<_>>();
    let label = text("", false);
    l10n::bind_label_with(&label, move || {
        let mut values = values.clone();
        values.extend(
            translated
                .iter()
                .map(|(key, source)| (key.clone(), crate::i18n::lookup(source))),
        );
        let refs = values
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect::<Vec<_>>();
        crate::i18n::format_message(source, &refs)
    });
    label
}

pub(super) fn server(server: &state::ServerView, page: &ServerPage, show_tools: bool) {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let tools = ServerToolsPanel::new(&page.app, &server.id);
    let header = page.details.header(&server.name, false);
    header.add_leading(&branding::server_logo(&server.name, &server.transport));
    header
        .subtitle
        .set_label(&format!("{} · {}", server.origin_label, server.transport));
    let refresh = header_icon_button("view-refresh-symbolic");
    l10n::bind_tooltip(&refresh, "Refresh");
    let p = page.clone();
    let refresh_tools = tools.clone();
    let server_id = server.id.clone();
    refresh.connect_clicked(move |_| {
        if p.details.tools.get() {
            refresh_tools.refresh();
        }
        p.reprobe_after_auth_change(&server_id);
        p.reload_registry();
    });
    header.add_action(&refresh);
    root.append(&header.root);
    let tabs = gtk::Stack::new();
    tabs.set_vexpand(true);
    let overview = page_body(24);
    let snapshot = page.last_snapshot.borrow().clone();
    let profile = snapshot
        .as_ref()
        .map(|s| s.active_profile_id.as_str())
        .unwrap_or_default();
    // Reuse the live status/switch/auth controls and every existing action.
    let connection = row_group();
    connection.append(&server_row(server, profile, page.clone(), true));
    overview.append(&connection);
    let target = if let Some(command) = &server.command {
        let mask = crate::registry::secret_arg_mask(&server.args);
        std::iter::once(command.clone())
            .chain(server.args.iter().zip(mask).map(|(arg, secret)| {
                if secret {
                    "<redacted>".into()
                } else {
                    arg.clone()
                }
            }))
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        server.url.clone().unwrap_or_default()
    };
    overview.append(&text(&target, true));
    if let Some(cwd) = &server.cwd {
        overview.append(&text(cwd, true));
    }
    for env in &server.plain_env {
        overview.append(&text(&format!("{}={}", env.key, env.value), true));
    }
    if !server.secret_keys.is_empty() || !server.secret_references.is_empty() {
        overview.append(&heading("Credentials"));
        for key in &server.secret_keys {
            overview.append(&text(&format!("{key}: ••••••••"), true));
        }
        for (key, reference) in &server.secret_references {
            overview.append(&text(&format!("{key}: {reference}"), true));
        }
    }
    let actions = server_actions(server, page.clone(), false);
    let flow = wrapping_actions();
    while let Some(child) = actions.first_child() {
        actions.remove(&child);
        child.remove_css_class("toolport-menu-item");
        child.add_css_class("toolport-secondary-action");
        flow.insert(&child, -1);
    }
    overview.append(&flow);
    let log = gtk::Box::new(gtk::Orientation::Vertical, 8);
    log.set_widget_name(&format!("server-log:{}", server.id));
    log.append(&heading("Connection log"));
    let error = page
        .health
        .borrow()
        .result(&server.id)
        .and_then(|probe| probe.error.clone());
    log.append(&text(error.as_deref().unwrap_or_default(), true));
    log.set_visible(error.is_some());
    overview.append(&log);
    let scroll = gtk::ScrolledWindow::builder()
        .child(&overview)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    l10n::bind_stack_page_title(
        &tabs.add_titled(&scroll, Some("overview"), &crate::i18n::tr("Overview")),
        "Overview",
    );
    l10n::bind_stack_page_title(
        &tabs.add_titled(&tools.root, Some("tools"), &crate::i18n::tr("Tools")),
        "Tools",
    );
    let switcher = gtk::StackSwitcher::builder()
        .stack(&tabs)
        .halign(gtk::Align::Start)
        .margin_start(24)
        .margin_top(12)
        .margin_bottom(12)
        .build();
    root.append(&switcher);
    root.append(&tabs);
    let tools_for_tab = tools.clone();
    let selected_tools = page.details.tools.clone();
    tabs.connect_visible_child_name_notify(move |tabs| {
        let active = tabs.visible_child_name().as_deref() == Some("tools");
        selected_tools.set(active);
        if active {
            tools_for_tab.refresh();
        }
    });
    page.details.tools.set(show_tools);
    if show_tools {
        tabs.set_visible_child_name("tools");
    }
    page.details.push(&server.id, &root);
}

pub(super) fn update_server_log(page: &ServerPage, server_id: &str, error: Option<&str>) {
    let Some(detail) = page.details.stack.child_by_name("detail") else {
        return;
    };
    if let Some(log) = find_named(&detail, &format!("server-log:{server_id}")) {
        if let Some(label) = log.last_child().and_downcast::<gtk::Label>() {
            label.set_label(error.unwrap_or_default());
        }
        log.set_visible(error.is_some());
    }
}

fn bound_dropdown(control: &gtk::DropDown) {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        item.set_child(Some(
            &gtk::Label::builder()
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .max_width_chars(24)
                .xalign(0.0)
                .build(),
        ));
    });
    factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let label = item.child().and_downcast::<gtk::Label>().unwrap();
        let value = item.item().and_downcast::<gtk::StringObject>().unwrap();
        label.set_label(&value.string());
        label.set_tooltip_text(Some(&value.string()));
    });
    control.set_factory(Some(&factory));
}

pub(super) fn client(client: &state::ClientView, page: &ClientPage) {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = page.details.header(&client.name, true);
    header.add_leading(&branding::client_logo(&client.id));
    let refresh = header_icon_button("view-refresh-symbolic");
    l10n::bind_tooltip(&refresh, "Scan clients again");
    let p = page.clone();
    refresh.connect_clicked(move |_| p.refresh());
    header.add_action(&refresh);
    root.append(&header.root);
    let body = page_body(24);
    let info = gtk::Box::new(gtk::Orientation::Vertical, 8);
    info.append(&client_rows::status_badge(client));
    if let Some(detected) = &client.detected {
        info.append(&text(&detected.config_path, true));
    } else if let Some(profile) = &client.claude_profile {
        info.append(&text(&profile.folder, true));
    }
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_valign(gtk::Align::Center);
    let connected = client.gateway_state == state::ClientGatewayState::Connected;
    let customized = client.gateway_state == state::ClientGatewayState::Customized;
    let writable = !client.config_error;
    let profiles = page.profiles.borrow().clone();
    let names = profiles.iter().map(|p| p.name.as_str()).collect::<Vec<_>>();
    let scope = gtk::DropDown::from_strings(&names);
    scope.set_selected(
        profiles
            .iter()
            .position(|p| Some(p.id.as_str()) == client.scope_id.as_deref().or(Some("")))
            .unwrap_or(0) as u32,
    );
    bound_dropdown(&scope);
    scope.set_sensitive(writable && !customized);
    scope.update_property(&[gtk::accessible::Property::Label(&crate::i18n::tr("Access"))]);
    let selected_scope = Rc::new(RefCell::new(client.scope_id.clone()));
    let saved = selected_scope.clone();
    scope.connect_selected_notify(move |control| {
        *saved.borrow_mut() = profiles
            .get(control.selected() as usize)
            .map(|p| p.id.clone());
    });
    let access_label = message("Access");
    access_label.set_wrap(false);
    access_label.set_valign(gtk::Align::Center);
    actions.append(&access_label);
    actions.append(&scope);
    if connected && !customized {
        let apply = l10n::button("Apply");
        apply.add_css_class("toolport-secondary-action");
        apply.set_sensitive(false);
        let apply_for_scope = apply.clone();
        let scope_for_apply = selected_scope.clone();
        let current_scope = client.scope_id.clone();
        scope.connect_selected_notify(move |_| {
            apply_for_scope.set_sensitive(
                writable
                    && scope_for_apply.borrow().as_deref().unwrap_or("")
                        != current_scope.as_deref().unwrap_or(""),
            )
        });
        let c = client.clone();
        let p = page.clone();
        let selected = selected_scope.clone();
        apply.connect_clicked(move |button| {
            run_client_mutation(
                &c,
                true,
                false,
                selected.borrow().clone(),
                button,
                p.clone(),
            )
        });
        actions.append(&apply);
    }
    let action = l10n::button(if customized || client.legacy_bearer_argv {
        client_reset_label(client)
    } else if connected {
        "Disconnect"
    } else {
        "Connect to Toolport"
    });
    action.add_css_class(if connected {
        "toolport-destructive"
    } else {
        "suggested-action"
    });
    action.set_sensitive(
        writable && (connected || customized || client.legacy_bearer_argv || client.app_present),
    );
    let c = client.clone();
    let p = page.clone();
    let scope_for_connect = selected_scope.clone();
    action.connect_clicked(move |button| {
        if customized || c.legacy_bearer_argv {
            confirm_client_reset(&c, button.clone(), p.clone());
        } else if connected {
            confirm_client_disconnect(&c, button.clone(), p.clone());
        } else {
            let mut c = c.clone();
            c.scope_id = scope_for_connect.borrow().clone();
            confirm_client_migrate(&c, p.clone());
        }
    });
    actions.append(&action);
    if (customized || client.legacy_bearer_argv)
        && client.gateway_state != state::ClientGatewayState::Disconnected
    {
        let disconnect = l10n::button("Disconnect");
        disconnect.add_css_class("toolport-destructive");
        disconnect.set_sensitive(writable);
        let c = client.clone();
        let p = page.clone();
        disconnect.connect_clicked(move |button| {
            confirm_client_disconnect(&c, button.clone(), p.clone())
        });
        actions.append(&disconnect);
    }
    if client.location_removable {
        let remove = client_rows::remove_location_button(client, page.clone());
        remove.set_sensitive(!client.config_error && !connected && !customized);
        actions.append(&remove);
    }
    body.append(&page_header::HeaderWrap::new(&info, &actions));
    if client.config_error {
        let p = page.clone();
        body.append(&error_callout(
            "Couldn't read this client's configuration",
            &crate::i18n::tr(
                "Could not read this client config. Check its syntax and file permissions.",
            ),
            move || p.refresh(),
        ));
    }
    if let Some((id, notice)) = page.detail_notice.borrow().as_ref() {
        if id == &client.id {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let label = text(notice, false);
            label.set_hexpand(true);
            row.append(&label);
            let dismiss = header_icon_button("window-close-symbolic");
            l10n::bind_tooltip(&dismiss, "Dismiss restart reminder");
            let p = page.clone();
            let row_for_dismiss = row.clone();
            dismiss.connect_clicked(move |_| {
                p.detail_notice.borrow_mut().take();
                row_for_dismiss.set_visible(false);
            });
            row.append(&dismiss);
            body.append(&row);
        }
    }
    if customized {
        body.append(&message("Toolport is leaving your hand-edited gateway entry as-is. Reset it only if you want Toolport to manage the entry."));
    }
    if client.legacy_bearer_argv {
        body.append(&message("This older HTTP connection exposes a bearer credential in process arguments. Review migration to stdio; it stays unchanged until you confirm."));
    }
    if !customized {
        let copy = gtk::Box::new(gtk::Orientation::Vertical, 8);
        copy.append(&heading("Discovery mode"));
        let summary = text("", false);
        let discovery_client = client.clone();
        l10n::bind_label_with(&summary, move || {
            client_rows::discovery_summary(&discovery_client).1
        });
        copy.append(&summary);
        let mode = discovery_picker(
            client,
            page,
            crate::registry_controller::set_client_discovery,
        );
        let discovery = row_group();
        let row = gtk::Box::new(gtk::Orientation::Vertical, 0);
        row.add_css_class("toolport-card");
        row.append(&page_header::HeaderWrap::new(&copy, &mode));
        discovery.append(&row);
        body.append(&discovery);
    }
    if !customized {
        let reachable = gtk::Box::new(gtk::Orientation::Vertical, 16);
        body.append(&reachable);
        let revision = Rc::new(std::cell::Cell::new(0));
        refresh_reachable(
            &reachable,
            client.scope_id.clone(),
            revision.clone(),
            client.clone(),
        );
        if !connected {
            let selected = selected_scope.clone();
            let client = client.clone();
            scope.connect_selected_notify(move |_| {
                refresh_reachable(
                    &reachable,
                    selected.borrow().clone(),
                    revision.clone(),
                    client.clone(),
                )
            });
        }
    }
    if client.uses_connectors {
        body.append(&message("Uses account connectors"));
        body.append(&formatted_message("Those live in {value1}'s Customize → Connectors and sync to your account, outside the local config files Toolport reads. Connecting Toolport adds a local gateway entry so your Toolport-managed servers appear in {value2} too.", &[("value1", &client.name), ("value2", &client.name)], &[]));
    }
    if let Some(detected) = &client.detected {
        client_imports(
            &body,
            detected,
            page,
            writable,
            &client.imported_names,
            selected_scope,
        );
    }
    let scroll = gtk::ScrolledWindow::builder()
        .child(&body)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    root.append(&scroll);
    page.details.push(&client.id, &root);
}

fn discovery_picker(
    client: &state::ClientView,
    page: &ClientPage,
    write: fn(&str, Option<&str>) -> Result<crate::registry::Registry, String>,
) -> gtk::DropDown {
    let modes = [None, Some("lazy"), Some("grouped"), Some("full")];
    let mode = gtk::DropDown::from_strings(&[
        &crate::i18n::tr("Auto"),
        &crate::i18n::tr("Lazy · search"),
        &crate::i18n::tr("Grouped · per-server"),
        &crate::i18n::tr("Full · every tool"),
    ]);
    bound_dropdown(&mode);
    l10n::bind_dropdown_choices(
        &mode,
        &[
            "Auto",
            "Lazy · search",
            "Grouped · per-server",
            "Full · every tool",
        ],
    );
    mode.set_selected(
        modes
            .iter()
            .position(|m| *m == client.discovery_mode.as_deref())
            .unwrap_or(0) as u32,
    );
    mode.set_sensitive(!client.config_error);
    mode.set_valign(gtk::Align::Start);
    mode.update_property(&[gtk::accessible::Property::Label(&crate::i18n::tr(
        "Discovery mode",
    ))]);
    let updating = Rc::new(std::cell::Cell::new(false));
    let persisted = Rc::new(std::cell::Cell::new(mode.selected()));
    let c = client.clone();
    let p = page.clone();
    mode.connect_selected_notify(move |control| {
        if l10n::dropdown_rebinding(control) || updating.replace(true) {
            return;
        }
        let Some(mode) = modes.get(control.selected() as usize).copied() else {
            updating.set(false);
            return;
        };
        control.set_sensitive(false);
        let id = c.id.clone();
        let name = c.name.clone();
        let p = p.clone();
        let control = control.clone();
        let updating = updating.clone();
        let persisted = persisted.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || write(&id, mode)).await;
            match result {
                Ok(Ok(_)) => {
                    persisted.set(control.selected());
                    updating.set(false);
                    p.show_confirmation(&crate::i18n::trf(
                        "Updated discovery for {value0}.",
                        &[("value0", &name)],
                    ));
                    p.refresh();
                }
                Ok(Err(e)) => {
                    control.set_selected(persisted.get());
                    updating.set(false);
                    control.set_sensitive(true);
                    p.show_error(&e);
                }
                Err(_) => {
                    control.set_selected(persisted.get());
                    updating.set(false);
                    control.set_sensitive(true);
                    p.show_error(&crate::i18n::tr(
                        "the discovery update stopped unexpectedly",
                    ));
                }
            }
        });
    });
    mode
}

fn refresh_reachable(
    reachable: &gtk::Box,
    scope: Option<String>,
    revision: Rc<std::cell::Cell<u64>>,
    client: state::ClientView,
) {
    let request = revision.get() + 1;
    revision.set(request);
    while let Some(child) = reachable.last_child() {
        reachable.remove(&child);
    }
    reachable.append(&skeleton_rows("Loading servers… "));
    let reachable = reachable.clone();
    gtk::glib::spawn_future_local(async move {
        let result = gtk::gio::spawn_blocking(move || {
            let registry = crate::registry::load()?;
            let default_scope = registry.default_access_id();
            let scope = scope
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or(&default_scope);
            Ok::<_, String>(
                registry
                    .servers
                    .iter()
                    .filter(|s| registry.is_enabled(scope, &s.id))
                    .map(|s| s.name.clone())
                    .collect::<Vec<_>>(),
            )
        })
        .await;
        if revision.get() != request {
            return;
        }
        while let Some(child) = reachable.last_child() {
            reachable.remove(&child);
        }
        match result {
            Ok(Ok(names)) => render_reachable(&reachable, &client, &names),
            _ => reachable.append(&message("Servers unavailable")),
        }
    });
}

fn server_chips(names: &[String]) -> gtk::FlowBox {
    let chips = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .homogeneous(false)
        .min_children_per_line(1)
        .max_children_per_line(100)
        .row_spacing(6)
        .column_spacing(6)
        .build();
    for name in names {
        let chip = text(name, true);
        chip.set_halign(gtk::Align::Start);
        chip.add_css_class("toolport-server-chip");
        chips.insert(&chip, -1);
    }
    chips
}

fn render_reachable(root: &gtk::Box, client: &state::ClientView, names: &[String]) {
    if client.gateway_state == state::ClientGatewayState::Connected {
        root.append(&heading("Servers it can reach"));
        if names.is_empty() {
            root.append(&message("No enabled servers in this access set yet. Turn servers on in Servers and include them under Settings > Advanced."));
        } else {
            root.append(&server_chips(names));
        }
        return;
    }
    if names.is_empty() {
        root.append(&formatted_message("Connect {value1}, then enable servers under{value2}{value3} and they'll all route through Toolport, no per-client setup.", &[("value1", &client.name), ("value2", " ")], &[("value3", "Servers")]));
        return;
    }
    let explanation = text("", false);
    let name = client.name.clone();
    let count = names.len();
    l10n::bind_label_with(&explanation, move || {
        let count = crate::i18n::trf(
            if count == 1 {
                "{value1} managed server"
            } else {
                "{value1} managed servers"
            },
            &[("value1", &count.to_string())],
        );
        crate::i18n::trf("Connect {value1} once and it reaches your{value2}{value3}{value4}through one gateway, no re-wiring per project. Your keys stay in your keychain.", &[("value1", &name), ("value2", " "), ("value3", &count), ("value4", " ")])
    });
    root.append(&explanation);
    let flow = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .homogeneous(false)
        .min_children_per_line(1)
        .max_children_per_line(3)
        .row_spacing(16)
        .column_spacing(12)
        .halign(gtk::Align::Center)
        .build();
    let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
    frame.add_css_class("toolport-gateway-flow");
    frame.append(&flow);
    let client_step = gtk::Box::new(gtk::Orientation::Vertical, 8);
    client_step.set_halign(gtk::Align::Center);
    let logo = branding::client_logo(&client.id);
    logo.set_pixel_size(48);
    client_step.append(&logo);
    let name = text(&client.name, false);
    name.set_max_width_chars(20);
    name.set_justify(gtk::Justification::Center);
    client_step.append(&name);
    flow.insert(&client_step, -1);
    let gateway_step = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    gateway_step.append(&gtk::Image::from_icon_name("go-next-symbolic"));
    let gateway = gtk::Box::new(gtk::Orientation::Vertical, 8);
    gateway.set_halign(gtk::Align::Center);
    let logo = branding::toolport_mark();
    logo.set_pixel_size(48);
    gateway.append(&logo);
    gateway.append(&formatted_message(
        "Toolport {value1}",
        &[],
        &[("value1", "gateway")],
    ));
    gateway_step.append(&gateway);
    flow.insert(&gateway_step, -1);
    let targets = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    targets.append(&gtk::Image::from_icon_name("go-next-symbolic"));
    let list = gtk::Box::new(gtk::Orientation::Vertical, 4);
    for name in names.iter().take(4) {
        let chip = text(name, true);
        chip.add_css_class("toolport-server-chip");
        list.append(&chip);
    }
    if names.len() > 4 {
        list.append(&formatted_message(
            "+{value1} more",
            &[("value1", &(names.len() - 4).to_string())],
            &[],
        ));
    }
    targets.append(&list);
    flow.insert(&targets, -1);
    root.append(&frame);
}

fn client_imports(
    body: &gtk::Box,
    detected: &crate::clients::DetectedClient,
    page: &ClientPage,
    writable: bool,
    imported_names: &[String],
    selected_scope: Rc<RefCell<Option<String>>>,
) {
    let mut seen = std::collections::HashSet::new();
    let servers = detected
        .servers
        .iter()
        .filter(|s| !crate::clients::detected_is_gateway(s))
        .chain(detected.plugin_servers.iter())
        .filter(|server| seen.insert(server.name.to_lowercase()))
        .collect::<Vec<_>>();
    if servers.is_empty() {
        return;
    }
    body.append(&heading("Import into Toolport"));
    let explanation = gtk::Box::new(gtk::Orientation::Vertical, 8);
    explanation.append(&formatted_message(
        "{value1} copies a server into Toolport; {value2} keeps its own copy.",
        &[("value2", &detected.name)],
        &[("value1", "Import")],
    ));
    explanation.append(&formatted_message("{review} copies it, then removes it from {client}'s config so the gateway is the only source (plugin servers stay). The cutover that actually saves context.", &[("client", &detected.name)], &[("review", "Review and connect")]));
    let how = gtk::Expander::new(None);
    how.set_label_widget(Some(&message("How importing works")));
    how.set_child(Some(&explanation));
    body.append(&how);
    if detected.gateway_installed
        && servers
            .iter()
            .any(|s| !imported_names.contains(&s.name.to_lowercase()))
    {
        let warning = formatted_message("{client} is already connected to Toolport. Import on its own leaves a copy here too, so these tools load twice, once directly and once through the gateway. Use {review} to avoid that.", &[("client", &detected.name)], &[("review", "Review and connect")]);
        warning.add_css_class("toolport-feedback");
        warning.add_css_class("warning");
        body.append(&warning);
    }
    body.append(&message("Review each command and URL. Values are saved using your keychain choices. Missing inputs stay off."));
    let all = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let import = l10n::button("Import all");
    import.add_css_class("toolport-secondary-action");
    import.set_sensitive(writable);
    let keys = servers
        .iter()
        .filter(|s| !imported_names.contains(&s.name.to_lowercase()))
        .map(|s| ImportSelection::new(&detected.id, s))
        .collect::<Vec<_>>();
    import.set_sensitive(writable && !keys.is_empty());
    import.set_visible(!keys.is_empty());
    let count = keys.len();
    l10n::bind_button_with(&import, move || {
        crate::i18n::trf(
            "{value1}Import all ({value2})",
            &[("value1", ""), ("value2", &count.to_string())],
        )
    });
    let all_imported = keys.is_empty();
    let p = page.clone();
    import.connect_clicked(move |_| review_imports(&p, keys.clone()));
    all.append(&import);
    if servers
        .iter()
        .any(|s| detected.servers.iter().any(|local| std::ptr::eq(*s, local)))
    {
        let review = l10n::button("Review and connect");
        review.add_css_class("toolport-secondary-action");
        review.set_sensitive(writable);
        let p = page.clone();
        let id = detected.id.clone();
        review.connect_clicked(move |_| {
            let c = p
                .detail_clients
                .borrow()
                .iter()
                .find(|c| c.id == id)
                .cloned();
            if let Some(mut c) = c {
                c.scope_id = selected_scope.borrow().clone();
                confirm_client_migrate(&c, p.clone());
            }
        });
        all.append(&review);
    }
    body.append(&all);
    let group = row_group();
    for server in servers {
        let row = gtk::Box::new(gtk::Orientation::Vertical, 8);
        row.add_css_class("toolport-card");
        let copy = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let name = text(&server.name, false);
        name.remove_css_class("toolport-muted");
        name.add_css_class("heading");
        copy.append(&name);
        let shown = ImportSelection::new(&detected.id, server);
        let target = shown
            .command
            .map(|command| {
                std::iter::once(command)
                    .chain(shown.args)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_else(|| shown.url.unwrap_or_default());
        copy.append(&text(&target, true));
        if detected
            .plugin_servers
            .iter()
            .any(|plugin| std::ptr::eq(plugin, server))
        {
            let badge = message("plugin");
            badge.add_css_class("toolport-badge");
            badge.add_css_class("off");
            badge.set_halign(gtk::Align::Start);
            copy.append(&badge);
        }
        let imported = imported_names.contains(&server.name.to_lowercase());
        let import = l10n::button(if imported { "in Toolport" } else { "Import" });
        import.add_css_class("toolport-secondary-action");
        import.set_halign(gtk::Align::End);
        import.set_sensitive(writable && !imported);
        let key = ImportSelection::new(&detected.id, server);
        let p = page.clone();
        import.connect_clicked(move |_| review_imports(&p, vec![key.clone()]));
        row.append(&page_header::HeaderWrap::new(&copy, &import));
        group.append(&row);
    }
    body.append(&group);
    if all_imported {
        body.append(&formatted_message(
            "{value1}Everything here is already in Toolport. Manage it under{value2}{value3}",
            &[("value1", ""), ("value2", " ")],
            &[("value3", "Servers")],
        ));
        let servers = gtk::Button::new();
        l10n::bind_button_with(&servers, || {
            crate::i18n::trf("All servers {value1}", &[("value1", "→")])
        });
        servers.add_css_class("toolport-secondary-action");
        servers.set_halign(gtk::Align::Start);
        let app = page.app.clone();
        servers.connect_clicked(move |_| {
            gtk::prelude::ActionGroupExt::activate_action(&app, "show-servers", None)
        });
        body.append(&servers);
    }
}

#[derive(Clone)]
struct ImportSelection {
    client_id: String,
    name: String,
    transport: String,
    command: Option<String>,
    args: Vec<String>,
    url: Option<String>,
}

impl ImportSelection {
    fn new(client_id: &str, server: &crate::clients::McpServer) -> Self {
        Self {
            client_id: client_id.into(),
            name: server.name.clone(),
            transport: server.transport.clone(),
            command: server.command.as_ref().map(|c| {
                if crate::registry::arg_looks_secret(c) {
                    "<command>".into()
                } else {
                    c.clone()
                }
            }),
            args: crate::import_credentials::shown_args(&server.args),
            url: server
                .url
                .as_deref()
                .map(crate::import_credentials::shown_url),
        }
    }
    fn matches(&self, item: &crate::registry_controller::ClientImportCandidate) -> bool {
        self.client_id == item.client_id
            && self.name == item.name
            && self.transport == item.transport
            && self.command == item.command
            && self.args == item.args
            && self.url == item.url
    }
}

fn selected_imports(
    keys: &[ImportSelection],
    items: Vec<crate::registry_controller::ClientImportCandidate>,
) -> Result<Vec<crate::registry_controller::ClientImportCandidate>, String> {
    keys.iter()
        .map(|key| {
            items
                .iter()
                .find(|item| key.matches(item))
                .cloned()
                .ok_or_else(|| {
                    crate::i18n::tr("Server changed. Refresh the client and review again.")
                })
        })
        .collect()
}

fn review_imports(page: &ClientPage, keys: Vec<ImportSelection>) {
    let page = page.clone();
    gtk::glib::spawn_future_local(async move {
        match gtk::gio::spawn_blocking(move || {
            let id = keys.first().ok_or("Nothing selected")?.client_id.clone();
            if keys.iter().any(|key| key.client_id != id) {
                return Err("Review imports from one client at a time.".into());
            }
            crate::registry_controller::preview_client_imports_for(&id)
                .and_then(|items| selected_imports(&keys, items))
        })
        .await
        {
            Ok(Ok(items)) if !items.is_empty() => page.show_import_review(items),
            Ok(Ok(_)) => page.show_confirmation(&crate::i18n::tr("Nothing new to import")),
            Ok(Err(e)) => page.show_error(&e),
            Err(_) => page.show_error(&crate::i18n::tr("The import scan stopped unexpectedly.")),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn b5_import_selection_rejects_other_profiles_and_changed_definitions() {
        let server = crate::clients::McpServer {
            name: "Notes".into(),
            transport: "stdio".into(),
            command: Some("notes".into()),
            args: vec![],
            url: None,
            env_keys: vec![],
        };
        let selection = ImportSelection::new("claude:work", &server);
        let candidate = crate::registry_controller::ClientImportCandidate {
            client_id: "claude:work".into(),
            key: "notes".into(),
            name: "Notes".into(),
            transport: "stdio".into(),
            command: Some("notes".into()),
            args: vec![],
            url: None,
            credentials: vec![],
            unsupported: None,
        };
        assert!(
            selected_imports(std::slice::from_ref(&selection), vec![candidate.clone()]).is_ok()
        );
        let mut other = candidate.clone();
        other.client_id = "claude:personal".into();
        assert!(selected_imports(std::slice::from_ref(&selection), vec![other]).is_err());
        let secret = crate::clients::McpServer {
            args: vec!["--token".into(), "private-value".into()],
            url: Some("https://example.test/?token=private-value".into()),
            ..server
        };
        let shown = ImportSelection::new("claude:work", &secret);
        assert!(!shown.args.join(" ").contains("private-value"));
        assert!(!shown.url.unwrap().contains("private-value"));
        let mut changed = candidate.clone();
        changed.args.push("--changed".into());
        assert!(selected_imports(
            std::slice::from_ref(&selection),
            vec![candidate, changed.clone()]
        )
        .is_ok());
        assert!(selected_imports(&[selection], vec![changed]).is_err());
    }
    fn widgets(root: &gtk::Widget) -> Vec<gtk::Widget> {
        let mut all = vec![root.clone()];
        let mut child = root.first_child();
        while let Some(widget) = child {
            all.extend(widgets(&widget));
            child = widget.next_sibling();
        }
        all
    }
    fn fixture_client(id: &str) -> state::ClientView {
        state::ClientView {
            detected: None,
            imported_names: Vec::new(),
            id: id.into(),
            name: "Claude Code".into(),
            app_present: true,
            config_exists: true,
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
            location_removable: true,
            claude_profile: None,
        }
    }
    fn snapshot(clients: Vec<state::ClientView>) -> state::ClientSnapshot {
        state::ClientSnapshot {
            clients,
            profiles: vec![
                state::ProfileView {
                    id: "".into(),
                    name: "Default access".into(),
                },
                state::ProfileView {
                    id: "scope-work".into(),
                    name: "Very long work access name that must wrap safely".into(),
                },
            ],
            sessions: vec![],
            sessions_error: false,
        }
    }
    fn app() -> adw::Application {
        adw::init().unwrap();
        let app = adw::Application::builder()
            .application_id("com.tsout.Toolport.B5DetailsTest")
            .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(gtk::gio::Cancellable::NONE).unwrap();
        app
    }
    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn b5_clients_keep_exact_instance_refresh_and_safe_error_states() {
        let _data = crate::registry::DataDirTestEnv::new("b5-clients");
        let app = app();
        let (_, servers, _) = build_content(&app, crate::approval_broker::start_native());
        let page = ClientPage::new(&app, servers);
        let window = adw::ApplicationWindow::builder()
            .application(&app)
            .content(&page.root)
            .default_width(1024)
            .default_height(700)
            .build();
        let first = fixture_client("claude-code@personal");
        let second = fixture_client("codex@work");
        page.render(snapshot(vec![first.clone(), second.clone()]));
        window.present();
        let open = find_named(page.root.upcast_ref(), "detail-open:codex@work")
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap();
        open.grab_focus();
        open.emit_clicked();
        assert_eq!(
            page.details.selected.borrow().as_deref(),
            Some("codex@work")
        );
        assert_eq!(
            page.details.stack.visible_child_name().as_deref(),
            Some("detail")
        );
        let active = page.details.stack.child_by_name("detail").unwrap();
        assert!(widgets(&active)
            .iter()
            .any(|w| w.has_css_class("toolport-page-back")));
        let picker = widgets(&active)
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::DropDown>().ok())
            .next()
            .unwrap();
        picker.set_selected(1); // Choosing access before Connect must never write a config.
        assert!(crate::registry::load().unwrap().client_scopes.is_empty());
        let mut changed = second.clone();
        changed.config_error = true;
        page.render(snapshot(vec![first.clone(), changed.clone()]));
        assert_eq!(
            page.details.selected.borrow().as_deref(),
            Some("codex@work")
        );
        let active = page.details.stack.child_by_name("detail").unwrap();
        let controls = widgets(&active);
        assert!(controls
            .iter()
            .filter_map(|w| w.downcast_ref::<gtk::Button>())
            .find(|b| b.label().as_deref() == Some("Connect to Toolport"))
            .is_some_and(|b| !b.is_sensitive()));
        assert!(controls
            .iter()
            .filter_map(|w| w.downcast_ref::<gtk::DropDown>())
            .all(|c| !c.is_sensitive()));
        assert!(controls
            .iter()
            .filter_map(|w| w.downcast_ref::<gtk::Label>())
            .any(|l| l.text() == "Couldn't read this client's configuration"));
        page.render(snapshot(vec![first.clone(), second.clone()]));
        page.details.back();
        assert_eq!(
            page.details.stack.visible_child_name().as_deref(),
            Some("list")
        );
        let replacement = find_named(page.root.upcast_ref(), "detail-open:codex@work").unwrap();
        assert!(replacement.is_focus());
        client(&second, &page);
        let mut disconnected = second.clone();
        disconnected.gateway_state = state::ClientGatewayState::Connected;
        page.render(snapshot(vec![first.clone(), disconnected]));
        assert!(
            widgets(&page.details.stack.child_by_name("detail").unwrap())
                .iter()
                .filter_map(|w| w.downcast_ref::<gtk::Button>())
                .any(|b| b.label().as_deref() == Some("Disconnect"))
        );
        page.render(snapshot(vec![first]));
        assert!(page.details.selected.borrow().is_none());
        let context = gtk::glib::MainContext::default();
        while context.pending() {
            context.iteration(false);
        }
        let focus = gtk::prelude::GtkWindowExt::focus(&window)
            .expect("missing client returns focus to live list");
        let mut ancestry = Vec::new();
        let mut current = Some(focus.clone());
        while let Some(widget) = current {
            ancestry.push(format!(
                "{}:{} visible={} child_visible={}",
                widget.type_().name(),
                widget.widget_name(),
                widget.is_visible(),
                widget.is_child_visible()
            ));
            current = widget.parent();
        }
        assert!(
            focus.is_ancestor(&page.details.stack.child_by_name("list").unwrap()),
            "missing focus ancestry: {ancestry:?}"
        );
        assert_eq!(app.windows().len(), 1, "details stay in the main window");
        window.close();
    }

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn b5_server_details_push_back_refresh_and_preserve_capability_tabs() {
        let _data = crate::registry::DataDirTestEnv::new("b5-servers");
        let app = app();
        let (root, page, _) = build_content(&app, crate::approval_broker::start_native());
        let window = adw::ApplicationWindow::builder()
            .application(&app)
            .content(&root)
            .default_width(1120)
            .default_height(700)
            .build();
        let server = state::ServerView {
            id: "b5-server".into(),
            name: "A very long server title for a pushed detail page".into(),
            origin_label: "Personal".into(),
            transport: "Local stdio".into(),
            transport_id: "stdio".into(),
            command: Some("fixture-command".into()),
            args: vec!["--token".into(), "private-fixture-value".into()],
            launch: None,
            url: None,
            cwd: Some("/fixture/work".into()),
            inherit_env: false,
            hide_prompts: false,
            secret_keys: vec!["API_KEY".into()],
            plain_env: vec![],
            secret_references: Default::default(),
            client_credentials: None,
            enabled: false,
            requires_review: false,
            team_route_removed: false,
            probe_fingerprint: 1,
        };
        let snapshot = state::RegistrySnapshot {
            servers: vec![server.clone()],
            profiles: vec![],
            enabled_count: 0,
            profile_count: 0,
            active_profile_id: "default".into(),
            paired: false,
            personal_sync: false,
        };
        page.render(state::RegistryState::Ready(snapshot.clone()));
        window.present();
        let open = find_named(root.upcast_ref(), "detail-open:b5-server")
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap();
        open.grab_focus();
        open.emit_clicked();
        assert_eq!(page.details.selected.borrow().as_deref(), Some("b5-server"));
        assert_eq!(app.windows().len(), 1);
        let labels = widgets(&page.details.stack.child_by_name("detail").unwrap())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Label>().ok())
            .map(|l| l.text().to_string())
            .collect::<Vec<_>>();
        assert!(labels
            .iter()
            .any(|s| s == "fixture-command --token <redacted>"));
        assert!(!labels.iter().any(|s| s.contains("private-fixture-value")));
        for title in [
            "Overview",
            "Tools",
            "Resources",
            "Prompts",
            "Credentials",
            "Edit",
            "Duplicate",
            "Remove",
        ] {
            assert!(labels.iter().any(|s| s == title), "missing {title}");
        }
        let detail = page.details.stack.child_by_name("detail").unwrap();
        let toggle = widgets(&detail)
            .into_iter()
            .find_map(|w| w.downcast::<gtk::Switch>().ok())
            .unwrap();
        toggle.grab_focus();
        let mut changed = snapshot.clone();
        changed.servers[0].cwd = Some("/fixture/updated".into());
        page.render(state::RegistryState::Ready(changed));
        let detail = page.details.stack.child_by_name("detail").unwrap();
        assert!(
            widgets(&detail)
                .iter()
                .filter_map(|w| w.downcast_ref::<gtk::Switch>())
                .any(|switch| switch.is_focus()),
            "live detail switch retains focus across refresh"
        );
        assert!(
            widgets(&page.details.stack.child_by_name("detail").unwrap())
                .iter()
                .filter_map(|w| w.downcast_ref::<gtk::Label>())
                .any(|l| l.text() == "/fixture/updated")
        );
        page.details.back();
        assert!(find_named(root.upcast_ref(), "detail-open:b5-server")
            .unwrap()
            .is_focus());
        server_details_for_test(&server, &page);
        page.render(state::RegistryState::Ready(state::RegistrySnapshot {
            servers: vec![],
            ..snapshot
        }));
        assert!(page.details.selected.borrow().is_none());
        window.close();
    }
    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn b5_credential_save_reprobes_unchanged_detail_without_refresh() {
        let _data = crate::registry::DataDirTestEnv::new("b5-credential-refresh");
        let app = app();
        let registry = crate::registry::update(|registry| {
            registry.servers = vec![serde_json::from_value(serde_json::json!({
                "id": "b5-credential", "name": "Credential fixture", "transport": "stdio",
                "command": "/home/sbx/bin/mock-mcp-server", "args": [], "env": [], "enabled": true
            }))
            .unwrap()];
            Ok(())
        })
        .unwrap()
        .0;
        let snapshot = state::RegistrySnapshot::from_registry(registry.clone());
        let (root, page, _) = build_content(&app, crate::approval_broker::start_native());
        let window = adw::ApplicationWindow::builder()
            .application(&app)
            .content(&root)
            .build();
        let old = crate::server_runtime::ProbeResult {
            server_id: "b5-credential".into(),
            ok: false,
            tool_count: 0,
            error: Some("Old credential failure".into()),
            auth_required: false,
            failure: None,
            auth_target: None,
        };
        let now = std::time::Instant::now();
        let ticket = page
            .health
            .borrow_mut()
            .plan(
                &[("b5-credential", snapshot.servers[0].probe_fingerprint)],
                health::ProbeReason::ConfigChanged,
                now,
            )
            .remove(0);
        page.health.borrow_mut().complete(&ticket, old, now);
        page.render(state::RegistryState::Ready(snapshot.clone()));
        window.present();
        server_details_for_test(&snapshot.servers[0], &page);
        assert!(
            widgets(&page.details.stack.child_by_name("detail").unwrap())
                .iter()
                .filter_map(|w| w.downcast_ref::<gtk::Label>())
                .any(|l| l.text() == "Old credential failure")
        );
        let editor = adw::Window::new();
        finish_credential_update(
            Ok(Ok(registry)),
            "b5-credential",
            &page,
            &editor,
            &gtk::Label::new(None),
            &gtk::Button::new(),
            "Updated",
        );
        assert!(
            page.health.borrow().is_checking("b5-credential"),
            "credential completion schedules a probe even with an identical registry"
        );
        let context = gtk::glib::MainContext::default();
        let deadline = now + std::time::Duration::from_secs(10);
        while page.health.borrow().is_checking("b5-credential")
            && std::time::Instant::now() < deadline
        {
            while context.pending() {
                context.iteration(false);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            page.health
                .borrow()
                .result("b5-credential")
                .is_some_and(|p| p.ok),
            "the replacement probe succeeds without manual Refresh"
        );
        assert!(
            !widgets(&page.details.stack.child_by_name("detail").unwrap())
                .iter()
                .filter_map(|w| w.downcast_ref::<gtk::Label>())
                .any(|l| l.is_visible() && l.text() == "Old credential failure")
        );
        window.close();
    }

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn b5_onboarding_tools_activates_servers_and_loads_new_snapshot_safely() {
        let data = crate::registry::DataDirTestEnv::new("b5-onboarding-tools");
        let app = app();
        let (root, page, _) = build_content(&app, crate::approval_broker::start_native());
        page.render(state::RegistryState::Ready(
            state::RegistrySnapshot::from_registry(crate::registry::Registry::default()),
        ));
        let stack = gtk::Stack::new();
        stack.add_named(&root, Some("servers"));
        stack.add_named(&gtk::Label::new(Some("Another main page")), Some("other"));
        stack.set_visible_child_name("other");
        let action = gtk::gio::SimpleAction::new("show-servers", None);
        let target = stack.clone();
        let target_page = page.clone();
        action.connect_activate(move |_, _| {
            target_page.details.back();
            target.set_visible_child_name("servers");
        });
        app.add_action(&action);
        install_server_tools_action(&app, &page);
        let window = adw::ApplicationWindow::builder()
            .application(&app)
            .content(&stack)
            .build();
        window.present();
        let mut registry = crate::registry::Registry {
            servers: vec![serde_json::from_value(serde_json::json!({
                "id":"new-onboarding", "name":"New from onboarding", "transport":"stdio", "command":"/home/sbx/bin/mock-mcp-server", "args":[], "env":[], "enabled":true
            })).unwrap()],
            ..Default::default()
        };
        let gateway = serde_json::from_value(serde_json::json!({
            "id":"toolport", "name":"Toolport", "transport":"stdio", "command":"toolport-gateway", "args":[], "env":[], "enabled":true
        })).unwrap();
        registry.servers.insert(0, gateway);
        // The main page still holds the empty pre-onboarding snapshot.
        crate::registry::save(&registry).unwrap();
        gtk::prelude::ActionGroupExt::activate_action(&app, "show-server-tools", None);
        assert_eq!(stack.visible_child_name().as_deref(), Some("servers"));
        let context = gtk::glib::MainContext::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while page.details.selected.borrow().is_none() && std::time::Instant::now() < deadline {
            while context.pending() {
                context.iteration(false);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(
            page.details.selected.borrow().as_deref(),
            Some("new-onboarding")
        );
        assert!(page.details.tools.get());
        assert_eq!(app.windows().len(), 1);
        registry.servers.retain(|server| server.id == "toolport");
        crate::registry::save(&registry).unwrap();
        stack.set_visible_child_name("other");
        gtk::prelude::ActionGroupExt::activate_action(&app, "show-server-tools", None);
        while context.pending() {
            context.iteration(false);
        }
        // Drain the actual async load, not merely the synchronous navigation.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while page
            .last_snapshot
            .borrow()
            .as_ref()
            .is_some_and(|s| !s.servers.is_empty())
            && std::time::Instant::now() < deadline
        {
            while context.pending() {
                context.iteration(false);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(page.details.selected.borrow().is_none());
        assert_eq!(stack.visible_child_name().as_deref(), Some("servers"));
        assert!(
            page.last_snapshot
                .borrow()
                .as_ref()
                .unwrap()
                .servers
                .is_empty(),
            "gateway-only snapshot has no manageable destination"
        );
        page.feedback.set_text("");
        // Both parse and I/O failures intentionally recover valid backups.
        // Model an unreadable store with no recoverable backup, inside this
        // disposable fixture only, to exercise the action's actual error path.
        for entry in std::fs::read_dir(&data.dir).unwrap() {
            let entry = entry.unwrap();
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with("registry.json.bak")
            {
                std::fs::remove_file(entry.path()).unwrap();
            }
        }
        std::fs::remove_file(data.dir.join("registry.json")).unwrap();
        std::fs::create_dir(data.dir.join("registry.json")).unwrap();
        gtk::prelude::ActionGroupExt::activate_action(&app, "show-server-tools", None);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while page.feedback.text().is_empty() && std::time::Instant::now() < deadline {
            while context.pending() {
                context.iteration(false);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(page.details.selected.borrow().is_none());
        assert!(
            !page.feedback.text().is_empty(),
            "actual async load failure stays visible"
        );
        assert!(page.feedback.is_visible());
        window.close();
    }

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn b5_same_detail_refresh_preserves_external_focus() {
        let _data = crate::registry::DataDirTestEnv::new("b5-external-focus");
        let app = app();
        let (_, servers, _) = build_content(&app, crate::approval_broker::start_native());
        let page = ClientPage::new(&app, servers);
        let frame = gtk::Box::new(gtk::Orientation::Vertical, 0);
        frame.append(&page.root);
        let external = gtk::Button::with_label("External navigation");
        frame.append(&external);
        let window = adw::ApplicationWindow::builder()
            .application(&app)
            .content(&frame)
            .default_width(1024)
            .default_height(700)
            .build();
        let selected = fixture_client("codex@work");
        page.render(snapshot(vec![selected.clone()]));
        window.present();
        client(&selected, &page);
        let back = find_class(
            &page.details.stack.child_by_name("detail").unwrap(),
            "toolport-page-back",
        )
        .unwrap();
        assert!(back.is_focus(), "initial push focuses Back");
        external.grab_focus();
        assert!(external.is_focus());
        let mut refreshed = selected.clone();
        refreshed.gateway_state = state::ClientGatewayState::Connected;
        page.render(snapshot(vec![refreshed]));
        assert!(
            external.is_focus(),
            "same-id detail refresh preserves a still-live external control"
        );
        assert!(
            widgets(&page.details.stack.child_by_name("detail").unwrap())
                .iter()
                .filter_map(|w| w.downcast_ref::<gtk::Button>())
                .any(|button| button.label().as_deref() == Some("Disconnect")),
            "detail refreshed while external focus stayed put"
        );
        page.render(snapshot(vec![]));
        assert!(page.details.selected.borrow().is_none());
        let focus = gtk::prelude::GtkWindowExt::focus(&window).unwrap();
        assert!(
            focus.is_ancestor(&page.details.stack.child_by_name("list").unwrap()),
            "missing destination still uses live-list fallback"
        );
        window.close();
    }

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn b5_discovery_mutation_targets_only_the_selected_profile() {
        let _data = crate::registry::DataDirTestEnv::new("b5-profile-discovery");
        let app = app();
        let (_, servers, _) = build_content(&app, crate::approval_broker::start_native());
        let page = ClientPage::new(&app, servers);
        let first = fixture_client("claude-code@personal");
        let second = fixture_client("codex@work");
        page.render(snapshot(vec![first, second.clone()]));
        client(&second, &page);
        let active = page.details.stack.child_by_name("detail").unwrap();
        let dropdowns = widgets(&active)
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::DropDown>().ok())
            .collect::<Vec<_>>();
        assert_eq!(dropdowns.len(), 2);
        dropdowns[1].set_selected(3);
        let context = gtk::glib::MainContext::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            while context.pending() {
                context.iteration(false);
            }
            if crate::registry::load()
                .unwrap()
                .client_discovery
                .get("codex@work")
                .is_some_and(|mode| mode == "full")
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let registry = crate::registry::load().unwrap();
        assert_eq!(
            registry
                .client_discovery
                .get("codex@work")
                .map(String::as_str),
            Some("full")
        );
        assert!(!registry
            .client_discovery
            .contains_key("claude-code@personal"));
        assert!(
            registry.client_scopes.is_empty(),
            "discovery does not rewrite access"
        );
    }

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn b5_rejected_discovery_restores_saved_mode_without_another_write() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static WRITES: AtomicUsize = AtomicUsize::new(0);
        fn reject(id: &str, mode: Option<&str>) -> Result<crate::registry::Registry, String> {
            assert_eq!(id, "codex@work");
            assert_eq!(mode, Some("full"));
            WRITES.fetch_add(1, Ordering::SeqCst);
            Err("Synthetic rejected discovery write".into())
        }
        let _data = crate::registry::DataDirTestEnv::new("b5-discovery-failure");
        crate::registry_controller::set_client_discovery("codex@work", Some("lazy")).unwrap();
        let before = std::fs::read(crate::registry::registry_path().unwrap()).unwrap();
        let app = app();
        let (_, servers, _) = build_content(&app, crate::approval_broker::start_native());
        let page = ClientPage::new(&app, servers);
        let mut selected = fixture_client("codex@work");
        selected.discovery_mode = Some("lazy".into());
        let mode = discovery_picker(&selected, &page, reject);
        WRITES.store(0, Ordering::SeqCst);
        assert_eq!(mode.selected(), 1);
        mode.set_selected(3);
        let context = gtk::glib::MainContext::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !mode.is_sensitive() && std::time::Instant::now() < deadline {
            while context.pending() {
                context.iteration(false);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(mode.is_sensitive());
        assert_eq!(page.feedback.text(), "Synthetic rejected discovery write");
        assert!(page.feedback.has_css_class("error"));
        assert_eq!(
            mode.selected(),
            1,
            "picker shows persisted Lazy after rejection"
        );
        while context.pending() {
            context.iteration(false);
        }
        assert_eq!(
            WRITES.load(Ordering::SeqCst),
            1,
            "rollback never starts a second write"
        );
        crate::i18n::set_language("fr").unwrap();
        l10n::refresh_bindings();
        while context.pending() {
            context.iteration(false);
        }
        assert_eq!(mode.selected(), 1);
        assert_eq!(
            WRITES.load(Ordering::SeqCst),
            1,
            "retranslation never writes discovery"
        );
        crate::i18n::set_language("en").unwrap();
        l10n::refresh_bindings();
        assert_eq!(
            std::fs::read(crate::registry::registry_path().unwrap()).unwrap(),
            before
        );
    }

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn b5_gateway_preview_tracks_selected_access_and_connected_chips() {
        let _data = crate::registry::DataDirTestEnv::new("b5-gateway-preview");
        let _app = app();
        crate::registry::update(|r| {
            r.servers = ["GitHub", "Notes"].into_iter().enumerate().map(|(i, name)| serde_json::from_value(serde_json::json!({"id":format!("server-{i}"),"name":name,"transport":"stdio","command":"fixture","args":[],"env":[],"enabled":true})).unwrap()).collect();
            r.profiles.push(serde_json::from_value(serde_json::json!({"id":"work","name":"Work","enabledServerIds":["server-1"]})).unwrap());
            Ok(())
        }).unwrap();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 16);
        let selected = fixture_client("codex@work");
        let revision = Rc::new(std::cell::Cell::new(0));
        refresh_reachable(&root, Some("work".into()), revision, selected.clone());
        let context = gtk::glib::MainContext::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while find_class(root.upcast_ref(), "toolport-gateway-flow").is_none()
            && std::time::Instant::now() < deadline
        {
            while context.pending() {
                context.iteration(false);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(find_class(root.upcast_ref(), "toolport-gateway-flow").is_some());
        assert!(find_class(root.upcast_ref(), "toolport-skeleton-group").is_none());
        let labels = widgets(root.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Label>().ok())
            .map(|l| l.text().to_string())
            .collect::<Vec<_>>();
        assert!(labels
            .iter()
            .any(|l| l.contains("1 managed server") && l.contains("keys stay in your keychain")));
        assert!(labels.iter().any(|l| l == "Toolport gateway"));
        assert!(labels.iter().any(|l| l == "Notes"));
        assert!(!labels.iter().any(|l| l == "GitHub"));
        crate::i18n::set_language("fr").unwrap();
        l10n::refresh_bindings();
        let translated = widgets(root.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Label>().ok())
            .map(|l| l.text().to_string())
            .collect::<Vec<_>>();
        assert!(
            !translated
                .iter()
                .any(|l| l.contains("keys stay in your keychain")),
            "gateway explanation retranslates live"
        );
        assert!(
            translated.iter().any(|l| l == "Notes"),
            "server name stays user data"
        );
        crate::i18n::set_language("en").unwrap();
        l10n::refresh_bindings();
        assert!(
            root.measure(gtk::Orientation::Horizontal, -1).0 <= 756,
            "gateway preview fits narrow content"
        );
        let connected = gtk::Box::new(gtk::Orientation::Vertical, 16);
        let mut client = selected;
        client.gateway_state = state::ClientGatewayState::Connected;
        render_reachable(&connected, &client, &["GitHub".into(), "Notes".into()]);
        assert_eq!(
            widgets(connected.upcast_ref())
                .iter()
                .filter(|w| w.has_css_class("toolport-server-chip"))
                .count(),
            2
        );
        assert!(find_class(connected.upcast_ref(), "toolport-gateway-flow").is_none());
        let lock = std::path::PathBuf::from(format!(
            "{}.lock",
            crate::registry::registry_path().unwrap().display()
        ));
        std::fs::remove_file(&lock).unwrap();
        std::fs::create_dir(&lock).unwrap();
        refresh_reachable(
            &root,
            Some("work".into()),
            Rc::new(std::cell::Cell::new(0)),
            client,
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while find_class(root.upcast_ref(), "toolport-skeleton-group").is_some()
            && std::time::Instant::now() < deadline
        {
            while context.pending() {
                context.iteration(false);
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            find_class(root.upcast_ref(), "toolport-skeleton-group").is_none(),
            "loading cleared after registry read failure"
        );
        assert!(widgets(root.upcast_ref())
            .iter()
            .filter_map(|w| w.downcast_ref::<gtk::Label>())
            .any(|l| l.text() == "Servers unavailable"));
        std::fs::remove_dir(lock).unwrap();
    }

    /// Deterministic, synthetic detail screens for the cross-theme capture matrix.
    #[test]
    #[ignore = "interactive visual fixture; run in omabox with TOOLPORT_B5_VIEW"]
    fn b5_details_visual_fixture() {
        let _data = crate::registry::DataDirTestEnv::new("b5-visual");
        adw::init().unwrap();
        let app = adw::Application::builder()
            .application_id("com.tsout.Toolport.B5DetailsTest")
            .build();
        app.register(gtk::gio::Cancellable::NONE).unwrap();
        let view = std::env::var("TOOLPORT_B5_VIEW").unwrap_or_else(|_| "server-overview".into());
        let width = std::env::var("TOOLPORT_B5_WIDTH")
            .ok()
            .and_then(|s| s.parse::<i32>().ok())
            .unwrap_or(1440);
        let registry = crate::registry::update(|registry| {
            registry.servers = ["GitHub", "Filesystem", "Calendar"].into_iter().enumerate().map(|(i, name)| serde_json::from_value(serde_json::json!({
                "id":format!("fixture-{i}"), "name":name, "transport":"stdio", "command":if view == "server-error" && i == 0 { "/fixture/unavailable-server" } else { "/home/sbx/bin/mock-mcp-server" }, "args":[], "env":[], "enabled":true
            })).unwrap()).collect();
            registry.servers[0].disabled_tools = vec!["add".into()];
            registry.profiles.push(serde_json::from_value(serde_json::json!({"id":"scope-work", "name":"Work projects with a deliberately long access name", "enabledServerIds":["fixture-0"]})).unwrap());
            Ok(())
        }).unwrap().0;
        let (servers_root, servers, _) =
            build_content(&app, crate::approval_broker::start_native());
        servers.render(state::RegistryState::Ready(
            state::RegistrySnapshot::from_registry(registry),
        ));
        let clients = ClientPage::new(&app, servers.clone());
        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        stack.add_named(&servers_root, Some("servers"));
        stack.add_named(&clients.root, Some("clients"));
        let split = adw::NavigationSplitView::new();
        split.add_css_class("toolport-shell");
        split.set_min_sidebar_width(220.0);
        split.set_max_sidebar_width(280.0);
        let (sidebar, _, _) = build_sidebar(
            &app,
            &split,
            &stack,
            servers.clone(),
            clients.clone(),
            ActivityPage::new(&app),
            CatalogPage::new(servers.clone()),
            TeamsPage::new(&app, servers.clone()),
            SettingsPage::new(
                http_bridge::BridgeController::default(),
                crate::approval_broker::start_native(),
            ),
            false,
        );
        split.set_sidebar(Some(&adw::NavigationPage::new(&sidebar, "Navigation")));
        split.set_content(Some(&adw::NavigationPage::new(&stack, "Toolport")));
        split.set_show_content(true);
        let window = adw::ApplicationWindow::builder()
            .application(&app)
            .title("Toolport B5 fixture")
            .default_width(width)
            .default_height(900)
            .content(&split)
            .build();
        window.add_css_class("toolport-native");
        let theme = theme::ThemeController::new();
        theme.attach(&window);
        if view.starts_with("server") {
            let mut server = servers.last_snapshot.borrow().as_ref().unwrap().servers[0].clone();
            if view == "server-long" {
                server.name = "A long server title for projects, accounts and a workspace with a lengthy name".into();
            }
            if view == "server-error" {
                server.command = Some("/fixture/unavailable-server".into());
            }
            super::server(
                &server,
                &servers,
                view != "server-overview" && view != "server-long" && view != "server-error",
            );
            if view == "server-error" {
                update_server_log(&servers, &server.id, Some("Could not start the server: executable not found. Check the command and working directory, then retry."));
            }
            if view == "server-resources" || view == "server-prompts" {
                for widget in widgets(servers_root.upcast_ref()) {
                    if let Some(stack) = widget.downcast_ref::<gtk::Stack>() {
                        let tab = if view == "server-resources" {
                            "resources"
                        } else {
                            "prompts"
                        };
                        if stack.child_by_name(tab).is_some() {
                            stack.set_visible_child_name(tab);
                        }
                    }
                }
            }
        } else {
            let (id, name) = match view.as_str() {
                "client-claude-profile" => ("claude-code@work", "Claude Code (Work)"),
                "client-codex-home" => ("codex@work", "Codex (Work)"),
                "client-customized" => ("vscode", "VS Code"),
                "client-unreadable" => ("zed", "Zed"),
                "client-absent" => ("goose", "Goose"),
                "client-available" => ("opencode", "OpenCode"),
                _ => ("cursor", "Cursor"),
            };
            let mut selected = fixture_client(id);
            selected.name = name.into();
            selected.gateway_state = if view == "client-customized" {
                state::ClientGatewayState::Customized
            } else if matches!(
                view.as_str(),
                "client-connected"
                    | "client-claude-profile"
                    | "client-disconnect"
                    | "client-access"
                    | "client-discovery"
            ) {
                state::ClientGatewayState::Connected
            } else {
                state::ClientGatewayState::Disconnected
            };
            selected.config_error = view == "client-unreadable";
            if view == "client-absent" {
                selected.app_present = false;
                selected.config_exists = false;
            }
            selected.location_removable = false;
            selected.scope_id = Some("scope-work".into());
            selected.scope_name = Some("Work projects with a deliberately long access name".into());
            selected.detected = Some(state::ClientDetailData(crate::clients::DetectedClient {
                id: id.into(),
                name: name.into(),
                config_path: format!("/home/sbx/{id}/config.json"),
                app_present: selected.app_present,
                config_exists: selected.config_exists,
                uses_connectors: false,
                location_removable: false,
                discovery: crate::clients::discovery_capabilities(id),
                servers: vec![crate::clients::McpServer {
                    name: "Local notes".into(),
                    transport: "stdio".into(),
                    command: Some("notes-mcp".into()),
                    args: vec![],
                    env_keys: vec![],
                    url: None,
                }],
                plugin_servers: vec![],
                gateway_installed: selected.gateway_state
                    != state::ClientGatewayState::Disconnected,
                entry_state: if view == "client-customized" {
                    crate::clients::GatewayEntryState::Customized
                } else if selected.gateway_state == state::ClientGatewayState::Connected {
                    crate::clients::GatewayEntryState::Managed
                } else {
                    crate::clients::GatewayEntryState::Absent
                },
                error: selected
                    .config_error
                    .then(|| "Synthetic unreadable config".into()),
                claude_profile: None,
            }));
            clients.render(snapshot(vec![selected.clone()]));
            stack.set_visible_child_name("clients");
            for widget in widgets(sidebar.upcast_ref()) {
                if let Some(button) = widget.downcast_ref::<gtk::Button>() {
                    let is_clients = widgets(button.upcast_ref())
                        .iter()
                        .filter_map(|w| w.downcast_ref::<gtk::Label>())
                        .any(|l| l.text() == "Clients");
                    if is_clients {
                        button.add_css_class("selected");
                    } else {
                        button.remove_css_class("selected");
                    }
                }
            }
            super::client(&selected, &clients);
        }
        let close = gtk::gio::SimpleAction::new("close-fixture", None);
        let close_window = window.downgrade();
        close.connect_activate(move |_, _| {
            if let Some(window) = close_window.upgrade() {
                window.close();
            }
        });
        app.add_action(&close);
        window.present();
        let main_loop = gtk::glib::MainLoop::new(None, false);
        let stop = main_loop.clone();
        window.connect_close_request(move |_| {
            stop.quit();
            gtk::glib::Propagation::Proceed
        });
        let stop = main_loop.clone();
        gtk::glib::timeout_add_local_once(std::time::Duration::from_secs(120), move || stop.quit());
        main_loop.run();
        window.close();
    }

    fn server_details_for_test(server: &state::ServerView, page: &ServerPage) {
        super::server(server, page, false);
    }
}
