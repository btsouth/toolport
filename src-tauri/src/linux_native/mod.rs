//! Additive GTK4 desktop shell for Linux.
//!
//! This module is feature-gated so the existing Tauri application and every
//! non-Linux build remain unchanged while the native shell is developed.

mod catalog;
mod hooks;
mod http_bridge;
mod onboarding;
mod permissions;
mod playground;
mod settings;
mod state;
mod teams;
mod theme;
mod tray;

use adw::prelude::*;
use catalog::CatalogPage;
use gtk::glib::prelude::ToValue;
use hooks::HooksPage;
use permissions::PermissionsPage;
use playground::PlaygroundPage;
use settings::SettingsPage;
use teams::TeamsPage;

const PREVIEW_APP_ID: &str = "com.tsout.Toolport.NativePreview";

pub fn run() {
    let registry = match crate::registry::load() {
        Ok(registry) => registry,
        Err(error) => {
            run_registry_startup_failure(error);
            return;
        }
    };
    if !registry.live_inspect {
        crate::inspect::clear();
    }
    let startup_notice = std::rc::Rc::new(std::cell::RefCell::new(
        crate::registry::take_recovery_notice(),
    ));
    if !cfg!(debug_assertions) {
        std::thread::spawn(run_startup_maintenance);
    }
    let launch_hidden = std::env::args_os().any(|arg| arg == "--hidden");
    let args = std::env::args()
        .filter(|arg| arg != "--hidden")
        .collect::<Vec<_>>();
    let app = adw::Application::builder()
        .application_id(PREVIEW_APP_ID)
        .flags(gtk::gio::ApplicationFlags::HANDLES_OPEN)
        .build();
    let _hold = app.hold();
    let broker = crate::approval_broker::start_native();
    let bridge = http_bridge::BridgeController::default();
    let bridge_for_restore = bridge.clone();
    std::thread::spawn(move || {
        if let Err(error) = bridge_for_restore.restore() {
            eprintln!("toolport: could not restore the native HTTP endpoint: {error}");
        }
    });
    let quit = gtk::gio::SimpleAction::new("quit", None);
    let app_for_quit = app.clone();
    quit.connect_activate(move |_, _| app_for_quit.quit());
    app.add_action(&quit);
    app.set_accels_for_action("app.quit", &["<Primary>q"]);
    ensure_url_scheme_handlers();

    let tray = tray::start(&app);
    // Hidden launch requires a REAL tray host, not just a spawned SNI item: the
    // item is registered optimistically so the icon appears when a host shows
    // up later, but a hidden window with no icon today is unreachable.
    let hide_first_activation = std::rc::Rc::new(std::cell::Cell::new(
        launch_hidden && tray.is_some() && tray::sni_watcher_present(),
    ));
    let broker_for_activate = broker.clone();
    let bridge_for_activate = bridge.clone();
    let hide_first_for_activate = hide_first_activation.clone();
    let notice_for_activate = startup_notice.clone();
    app.connect_activate(move |app| {
        let present = !hide_first_for_activate.replace(false);
        build_window(
            app,
            theme::ThemeController::new(),
            broker_for_activate.clone(),
            bridge_for_activate.clone(),
            notice_for_activate.clone(),
            present,
        )
    });
    let broker_for_open = broker.clone();
    let bridge_for_open = bridge.clone();
    let notice_for_open = startup_notice.clone();
    app.connect_open(move |app, files, _hint| {
        build_window(
            app,
            theme::ThemeController::new(),
            broker_for_open.clone(),
            bridge_for_open.clone(),
            notice_for_open.clone(),
            true,
        );
        let Some(action) = app.lookup_action("open-share-url") else {
            return;
        };
        for file in files {
            let uri = file.uri();
            action.activate(Some(&uri.to_variant()));
        }
    });
    app.run_with_args(&args);
    if let Some(tray) = tray {
        tray.shutdown().wait();
    }
    bridge.shutdown();
    broker.clear_endpoint();
}

/// Make `toolport://` and `conduit://` links reach this binary even on an
/// unpackaged build (the packaged desktop file already claims them). The
/// shipping shell registers its schemes at runtime on Linux for the same
/// reason; without this a locally built `toolport-gtk` cannot receive share
/// links at all. Best-effort: a failure only means links stay unopenable, as
/// before.
fn ensure_url_scheme_handlers() {
    let unhandled: Vec<&str> = ["toolport", "conduit"]
        .into_iter()
        .filter(|scheme| gtk::gio::AppInfo::default_for_uri_scheme(scheme).is_none())
        .collect();
    if unhandled.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        let applications = gtk::glib::user_data_dir().join("applications");
        if std::fs::create_dir_all(&applications).is_err() {
            return;
        }
        let desktop_path = applications.join("com.tsout.Toolport.NativePreview.desktop");
        let contents = format!(
            "[Desktop Entry]\nType=Application\nName=Toolport (native preview)\nExec={} %u\nNoDisplay=true\nMimeType=x-scheme-handler/toolport;x-scheme-handler/conduit;\n",
            exe.display()
        );
        if std::fs::write(&desktop_path, contents).is_err() {
            return;
        }
        let _ = std::process::Command::new("update-desktop-database")
            .arg(&applications)
            .status();
        for scheme in unhandled {
            let _ = std::process::Command::new("xdg-mime")
                .arg("default")
                .arg("com.tsout.Toolport.NativePreview.desktop")
                .arg(format!("x-scheme-handler/{scheme}"))
                .status();
        }
        eprintln!(
            "toolport: registered {} as the handler for toolport:// links",
            desktop_path.display()
        );
    });
}

fn build_window(
    app: &adw::Application,
    theme: std::rc::Rc<theme::ThemeController>,
    broker: crate::approval_broker::ApprovalBroker,
    bridge: http_bridge::BridgeController,
    startup_notice: std::rc::Rc<
        std::cell::RefCell<Option<crate::registry::RegistryRecoveryNotice>>,
    >,
    present: bool,
) {
    if let Some(window) = app.windows().into_iter().next() {
        if present {
            window.present();
        }
        return;
    }

    let saved_state = load_window_state();
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Toolport")
        .default_width(saved_state.as_ref().map(|s| s.width).unwrap_or(1120))
        .default_height(saved_state.as_ref().map(|s| s.height).unwrap_or(720))
        .maximized(saved_state.as_ref().is_some_and(|s| s.maximized))
        .resizable(true)
        .hide_on_close(true)
        .build();
    window.add_css_class("toolport-native");
    // First close-to-tray must not read as a quit: say once, ever, that the app
    // keeps running. The marker is shared with the shipping shell so a user who
    // already learned this is not told again. Closing is also the natural moment
    // to remember the geometry the user chose.
    {
        let app_for_hint = app.clone();
        window.connect_close_request(move |window| {
            save_window_state(window);
            maybe_show_tray_hint(&app_for_hint);
            gtk::glib::Propagation::Proceed
        });
    }

    let split = adw::NavigationSplitView::new();
    split.add_css_class("toolport-shell");
    split.set_min_sidebar_width(220.0);
    split.set_max_sidebar_width(280.0);

    let (content, server_page, approval_page) = build_content(app, broker.clone());
    let bridge_for_reap = bridge.clone();
    let server_page_for_reap = server_page.clone();
    let client_page = ClientPage::new(app, bridge.clone());
    let activity_page = ActivityPage::new(app);
    let catalog_page = CatalogPage::new();
    let playground_page = PlaygroundPage::new(app);
    let teams_page = TeamsPage::new(app);
    let rules_page = RulesPage::new(app);
    let hooks_page = HooksPage::new(app);
    let permissions_page = PermissionsPage::new(app);
    let settings_page = SettingsPage::new(bridge, broker);
    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .transition_duration(140)
        .build();
    stack.add_named(&content, Some("servers"));
    stack.add_named(&client_page.root, Some("clients"));
    stack.add_named(&activity_page.root, Some("activity"));
    stack.add_named(&catalog_page.root, Some("catalog"));
    stack.add_named(&playground_page.root, Some("playground"));
    stack.add_named(&teams_page.root, Some("teams"));
    stack.add_named(&rules_page.root, Some("rules"));
    stack.add_named(&hooks_page.root, Some("hooks"));
    stack.add_named(&permissions_page.root, Some("permissions"));
    stack.add_named(&settings_page.root, Some("settings"));
    let (sidebar, quarantine_badge) = build_sidebar(
        app,
        &split,
        &stack,
        client_page.clone(),
        activity_page.clone(),
        catalog_page.clone(),
        playground_page.clone(),
        teams_page.clone(),
        rules_page.clone(),
        hooks_page.clone(),
        permissions_page.clone(),
        settings_page.clone(),
    );
    start_quarantine_watch(app, quarantine_badge);
    // The Settings tab must not go stale while open: quarantine, remembered
    // approvals, and routine suggestions all change underneath it (the shipping
    // app polls the same way).
    {
        let settings_for_tick = settings_page.clone();
        gtk::glib::timeout_add_local(std::time::Duration::from_secs(15), move || {
            if settings_for_tick.root.is_mapped() {
                settings_for_tick.refresh_quietly();
            }
            gtk::glib::ControlFlow::Continue
        });
    }
    // Activity ticks while visible, like the shipping dashboard, so calls land
    // as they happen; identical data is dropped before any re-render.
    {
        let activity_for_tick = activity_page.clone();
        gtk::glib::timeout_add_local(std::time::Duration::from_secs(3), move || {
            if activity_for_tick.root.is_mapped() {
                activity_for_tick.refresh_quietly();
            }
            gtk::glib::ControlFlow::Continue
        });
    }
    split.set_sidebar(Some(&adw::NavigationPage::new(&sidebar, "Navigation")));
    split.set_content(Some(&adw::NavigationPage::new(&stack, "Toolport")));

    let narrow = adw::Breakpoint::new(
        adw::BreakpointCondition::parse("max-width: 700px")
            .expect("the native-shell breakpoint is a static valid condition"),
    );
    narrow.add_setter(&split, "collapsed", Some(&true.to_value()));
    window.add_breakpoint(narrow);

    window.set_content(Some(&split));
    theme.attach(&window);
    let state = state::RegistryController::new(move |snapshot| {
        server_page.render(snapshot);
        if let Some(notice) = startup_notice.borrow_mut().take() {
            let detail = notice
                .quarantine_path
                .as_deref()
                .map(|path| format!(" The unreadable copy was preserved at {path}."))
                .unwrap_or_default();
            server_page.show_feedback(
                &format!(
                    "Toolport recovered the registry from a backup: {}.{detail}",
                    notice.reason
                ),
                true,
            );
        }
    });
    state.attach(&window);
    approval_page.attach(&window);
    teams_page.attach_background_sync(&window);
    onboarding::install(app, &window, client_page);
    start_startup_reap(app, bridge_for_reap, server_page_for_reap);
    if present {
        window.present();
    }
}

/// The launch-time stale-gateway reaper (SOU-414/418): once immediately, again
/// after a short delay so a client that race-respawns an old binary between the
/// repoint and the first kill is still cleaned up without another app restart.
/// Only newly discovered clients are announced on the second pass; the reaper
/// itself restores the supervised Shared HTTP endpoint if it had to stop it.
fn start_startup_reap(
    app: &adw::Application,
    bridge: http_bridge::BridgeController,
    server_page: ServerPage,
) {
    let app = app.clone();
    gtk::glib::spawn_future_local(async move {
        let mut announced: Vec<u32> = Vec::new();
        for delay in [0u32, 3] {
            if delay > 0 {
                gtk::glib::timeout_future_seconds(delay).await;
            }
            let bridge = bridge.clone();
            let Ok(outcome) = gtk::gio::spawn_blocking(move || bridge.stop_stale_gateways()).await
            else {
                continue;
            };
            if !outcome.killed.is_empty() {
                eprintln!(
                    "toolport: startup reaper stopped {} superseded gateway process(es)",
                    outcome.killed.len()
                );
            }
            let fresh: Vec<crate::gateway_publish::ClientNeedingRestart> = outcome
                .needs_restart
                .iter()
                .filter(|client| !announced.contains(&client.client_pid))
                .cloned()
                .collect();
            if fresh.is_empty() {
                continue;
            }
            announced.extend(fresh.iter().map(|client| client.client_pid));
            let line = restart_advice_line(&fresh);
            server_page.show_feedback(&line, true);
            // Reaches the user even when the app was launched hidden to the tray.
            let notification = gtk::gio::Notification::new("Toolport was updated");
            notification.set_body(Some(&line));
            app.send_notification(Some("toolport-gateway-restart"), &notification);
        }
    });
}

/// The user-facing line for clients still launching a superseded gateway.
fn restart_advice_line(clients: &[crate::gateway_publish::ClientNeedingRestart]) -> String {
    let mut names: Vec<&str> = Vec::new();
    for client in clients {
        if !names.contains(&client.client.as_str()) {
            names.push(client.client.as_str());
        }
    }
    format!(
        "{} {} still launching an old Toolport gateway. Restart {} to finish the upgrade: {}.",
        names.len(),
        if names.len() == 1 {
            "app is"
        } else {
            "apps are"
        },
        if names.len() == 1 { "it" } else { "them" },
        names.join(", ")
    )
}

fn run_startup_maintenance() {
    if let Some(migrated) = crate::registry::migrate_legacy_data_dir() {
        eprintln!(
            "toolport: migrated data directory to {}",
            migrated.display()
        );
    }
    let managed = crate::registry::load()
        .map(|registry| registry.client_managed_entries)
        .unwrap_or_default();
    let repoint = crate::clients::repoint_stale_gateways(&managed);
    if !repoint.repointed.is_empty() {
        let _ = crate::registry::update(|registry| {
            for (client_id, entry) in &repoint.repointed {
                registry.set_client_managed_entry(client_id, entry.clone());
            }
            Ok(())
        });
    }
    if !repoint.failed.is_empty() {
        eprintln!(
            "toolport: could not update {} stale client configuration(s)",
            repoint.failed.len()
        );
    }
    crate::rules::apply_on_startup();
    crate::hooks::apply_on_startup();
    crate::agent_permissions::apply_on_startup();
    crate::agent_guard::apply_on_startup();
}

fn run_registry_startup_failure(error: String) {
    let app = adw::Application::builder()
        .application_id("com.tsout.Toolport.NativePreview.Recovery")
        .build();
    app.connect_activate(move |app| {
        let path = crate::registry::resolved_path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "the Toolport data directory".to_string());
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Toolport could not start safely")
            .default_width(620)
            .default_height(360)
            .resizable(true)
            .build();
        let page = gtk::Box::new(gtk::Orientation::Vertical, 16);
        page.set_margin_top(28);
        page.set_margin_bottom(28);
        page.set_margin_start(28);
        page.set_margin_end(28);
        page.append(
            &gtk::Label::builder()
                .label("Toolport could not start safely")
                .halign(gtk::Align::Start)
                .wrap(true)
                .css_classes(["title-1"])
                .build(),
        );
        page.append(
            &gtk::Label::builder()
                .label("The registry was not replaced. Close other Toolport processes and try again. If the problem continues, restore a registry backup or move the unreadable registry aside before reopening Toolport.")
                .halign(gtk::Align::Start)
                .xalign(0.0)
                .wrap(true)
                .build(),
        );
        page.append(
            &gtk::Label::builder()
                .label(format!("Registry: {path}\n\nError: {error}"))
                .halign(gtk::Align::Start)
                .xalign(0.0)
                .wrap(true)
                .selectable(true)
                .css_classes(["dim-label"])
                .build(),
        );
        let close = gtk::Button::with_label("Close Toolport");
        close.set_halign(gtk::Align::End);
        close.add_css_class("suggested-action");
        let app = app.clone();
        close.connect_clicked(move |_| app.quit());
        page.append(&close);
        window.set_content(Some(&page));
        window.present();
    });
    app.run();
}

fn build_sidebar(
    app: &adw::Application,
    split: &adw::NavigationSplitView,
    stack: &gtk::Stack,
    client_page: ClientPage,
    activity_page: ActivityPage,
    catalog_page: CatalogPage,
    playground_page: PlaygroundPage,
    teams_page: TeamsPage,
    rules_page: RulesPage,
    hooks_page: HooksPage,
    permissions_page: PermissionsPage,
    settings_page: SettingsPage,
) -> (gtk::Box, gtk::Label) {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("toolport-sidebar");

    let header = adw::HeaderBar::new();
    header.add_css_class("toolport-header");
    let brand = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let mark = gtk::Label::new(Some("T"));
    mark.add_css_class("toolport-mark");
    brand.append(&mark);
    brand.append(
        &gtk::Label::builder()
            .label("Toolport")
            .css_classes(["title"])
            .build(),
    );
    header.set_title_widget(Some(&brand));
    root.append(&header);

    let nav = gtk::Box::new(gtk::Orientation::Vertical, 6);
    nav.set_margin_top(12);
    nav.set_margin_bottom(12);
    nav.set_margin_start(12);
    nav.set_margin_end(12);

    // Blocked-tool count on the Settings row, fed by the quarantine watcher.
    // "?" with a tooltip means the state could not be read - unknown is not
    // the same as zero.
    let quarantine_badge = gtk::Label::builder()
        .visible(false)
        .css_classes(["toolport-badge", "review"])
        .build();

    let mut buttons = Vec::new();
    for (target, label, icon) in [
        ("servers", "Servers", "network-server-symbolic"),
        ("clients", "Clients", "computer-symbolic"),
        ("activity", "Activity", "view-list-symbolic"),
        ("catalog", "Catalog", "system-software-install-symbolic"),
        ("playground", "Playground", "applications-science-symbolic"),
        ("teams", "Teams", "system-users-symbolic"),
        ("rules", "Rules", "security-high-symbolic"),
        ("hooks", "Agent activity", "media-record-symbolic"),
        (
            "permissions",
            "Agent permissions",
            "changes-prevent-symbolic",
        ),
        ("settings", "Settings", "emblem-system-symbolic"),
    ] {
        let button = gtk::Button::new();
        button.set_halign(gtk::Align::Fill);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        row.append(&gtk::Image::from_icon_name(icon));
        row.append(
            &gtk::Label::builder()
                .label(label)
                .halign(gtk::Align::Start)
                .hexpand(true)
                .xalign(0.0)
                .build(),
        );
        if target == "settings" {
            row.append(&quarantine_badge);
        }
        button.set_child(Some(&row));
        button.add_css_class("flat");
        button.add_css_class("toolport-nav-item");
        if target == "servers" {
            button.add_css_class("selected");
        }
        nav.append(&button);
        buttons.push((target.to_string(), button));
    }
    for (target, button) in buttons.clone() {
        let split = split.clone();
        let stack = stack.clone();
        let buttons = buttons.clone();
        let client_page = client_page.clone();
        let activity_page = activity_page.clone();
        let catalog_page = catalog_page.clone();
        let playground_page = playground_page.clone();
        let teams_page = teams_page.clone();
        let rules_page = rules_page.clone();
        let hooks_page = hooks_page.clone();
        let permissions_page = permissions_page.clone();
        let settings_page = settings_page.clone();
        button.connect_clicked(move |_| {
            show_native_page(
                &split,
                &stack,
                &buttons,
                &target,
                &client_page,
                &activity_page,
                &catalog_page,
                &playground_page,
                &teams_page,
                &rules_page,
                &hooks_page,
                &permissions_page,
                &settings_page,
            );
        });
    }
    for (index, (target, _)) in buttons.iter().enumerate() {
        let action_name = format!("show-{target}");
        let action = gtk::gio::SimpleAction::new(&action_name, None);
        let split = split.clone();
        let stack = stack.clone();
        let buttons = buttons.clone();
        let target = target.clone();
        let client_page = client_page.clone();
        let activity_page = activity_page.clone();
        let catalog_page = catalog_page.clone();
        let playground_page = playground_page.clone();
        let teams_page = teams_page.clone();
        let rules_page = rules_page.clone();
        let hooks_page = hooks_page.clone();
        let permissions_page = permissions_page.clone();
        let settings_page = settings_page.clone();
        action.connect_activate(move |_, _| {
            show_native_page(
                &split,
                &stack,
                &buttons,
                &target,
                &client_page,
                &activity_page,
                &catalog_page,
                &playground_page,
                &teams_page,
                &rules_page,
                &hooks_page,
                &permissions_page,
                &settings_page,
            );
        });
        app.add_action(&action);
        if index < 9 {
            app.set_accels_for_action(
                &format!("app.{action_name}"),
                &[&format!("<Primary>{}", index + 1)],
            );
        }
    }

    let nav_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&nav)
        .build();
    root.append(&nav_scroll);

    let theme_status = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    theme_status.add_css_class("toolport-theme-status");
    let dot = gtk::Label::new(Some("●"));
    dot.add_css_class("toolport-accent");
    theme_status.append(&dot);
    theme_status.append(
        &gtk::Label::builder()
            .label("Following Omarchy")
            .css_classes(["caption", "toolport-muted"])
            .build(),
    );
    install_star_prompt(&root);
    root.append(&theme_status);
    (root, quarantine_badge)
}

/// The one-off "star the repo" ask, shown once ever, only to someone actually
/// using Toolport (a server enabled), and only after the window has been on
/// screen a while. Dismissing or starring retires it permanently.
fn install_star_prompt(container: &gtk::Box) {
    fn marker() -> Option<std::path::PathBuf> {
        Some(crate::registry::conduit_dir()?.join(".gtk-star-prompt-done"))
    }
    if marker().is_none_or(|marker| marker.exists()) {
        return;
    }
    let card = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    card.add_css_class("toolport-card");
    card.set_margin_start(12);
    card.set_margin_end(12);
    card.set_visible(false);
    card.append(
        &gtk::Label::builder()
            .label("Enjoying Toolport? A star helps others find it.")
            .halign(gtk::Align::Start)
            .hexpand(true)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["caption", "toolport-muted"])
            .build(),
    );
    let retire = |card: &gtk::Box| {
        if let Some(marker) = marker() {
            let _ = std::fs::write(marker, b"1");
        }
        card.set_visible(false);
    };
    let star = gtk::Button::with_label("Star");
    star.add_css_class("toolport-secondary-action");
    {
        let card = card.clone();
        star.connect_clicked(move |_| {
            let _ = crate::oauth::open_web_url("https://github.com/tsouth89/toolport");
            retire(&card);
        });
    }
    card.append(&star);
    let dismiss = gtk::Button::builder()
        .icon_name("window-close-symbolic")
        .tooltip_text("Dismiss forever")
        .css_classes(["flat"])
        .build();
    {
        let card = card.clone();
        dismiss.connect_clicked(move |_| retire(&card));
    }
    card.append(&dismiss);
    container.append(&card);
    let card_for_timer = card.clone();
    gtk::glib::timeout_add_seconds_local_once(8, move || {
        gtk::glib::spawn_future_local(async move {
            let enabled = gtk::gio::spawn_blocking(|| {
                crate::registry::load()
                    .map(|registry| {
                        let profile = registry.active_profile_id();
                        registry
                            .servers
                            .iter()
                            .filter(|server| registry.is_enabled(&profile, &server.id))
                            .count()
                    })
                    .unwrap_or(0)
            })
            .await
            .unwrap_or(0);
            // The card itself is hidden (thus unmapped); "on screen" means its
            // sidebar parent is mapped, i.e. the window is actually shown.
            let sidebar_on_screen = card_for_timer
                .parent()
                .is_some_and(|parent| parent.is_mapped());
            if enabled >= 1 && sidebar_on_screen {
                card_for_timer.set_visible(true);
            }
        });
    });
}

/// Poll the quarantine store every 15 seconds (matching the shipping app): keep
/// the sidebar badge current, and send one OS notification per newly blocked
/// tool batch. The first poll only establishes the baseline so a restart does
/// not re-announce an already known backlog; the badge still shows it.
fn start_quarantine_watch(app: &adw::Application, badge: gtk::Label) {
    let app = app.clone();
    let seen: std::rc::Rc<std::cell::RefCell<Option<std::collections::HashSet<String>>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let running = std::rc::Rc::new(std::cell::Cell::new(false));
    let tick = move || {
        if running.replace(true) {
            return gtk::glib::ControlFlow::Continue;
        }
        let app = app.clone();
        let badge = badge.clone();
        let seen = seen.clone();
        let running = running.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(crate::integrity::all_quarantined).await;
            running.set(false);
            let list = match result {
                Ok(Ok(list)) => list,
                _ => {
                    badge.set_label("?");
                    badge.set_tooltip_text(Some(
                        "Could not read the quarantine state; unknown is not the same as zero",
                    ));
                    badge.set_visible(true);
                    return;
                }
            };
            badge.set_label(&list.len().to_string());
            badge.set_tooltip_text(Some(
                "Blocked tools awaiting re-approval; review them in Settings",
            ));
            badge.set_visible(!list.is_empty());
            let keys: std::collections::HashSet<String> = list
                .iter()
                .map(crate::integrity::quarantine_entry_key)
                .collect();
            let mut guard = seen.borrow_mut();
            match guard.as_mut() {
                None => *guard = Some(keys),
                Some(previous) => {
                    let newcomers: Vec<&serde_json::Value> = list
                        .iter()
                        .filter(|record| {
                            !previous.contains(&crate::integrity::quarantine_entry_key(record))
                        })
                        .collect();
                    if !newcomers.is_empty() {
                        let (title, body) = crate::integrity::quarantine_notification(&newcomers);
                        let notification = gtk::gio::Notification::new(&title);
                        notification.set_body(Some(&body));
                        notification.set_priority(gtk::gio::NotificationPriority::High);
                        app.send_notification(Some("toolport-quarantine"), &notification);
                    }
                    *previous = keys;
                }
            }
        });
        gtk::glib::ControlFlow::Continue
    };
    tick();
    gtk::glib::timeout_add_local(std::time::Duration::from_secs(15), tick);
}

fn show_native_page(
    split: &adw::NavigationSplitView,
    stack: &gtk::Stack,
    buttons: &[(String, gtk::Button)],
    target: &str,
    client_page: &ClientPage,
    activity_page: &ActivityPage,
    catalog_page: &CatalogPage,
    playground_page: &PlaygroundPage,
    teams_page: &TeamsPage,
    rules_page: &RulesPage,
    hooks_page: &HooksPage,
    permissions_page: &PermissionsPage,
    settings_page: &SettingsPage,
) {
    stack.set_visible_child_name(target);
    split.set_show_content(true);
    for (name, candidate) in buttons {
        if name == target {
            candidate.add_css_class("selected");
        } else {
            candidate.remove_css_class("selected");
        }
    }
    if target == "clients" {
        client_page.refresh();
    } else if target == "activity" {
        activity_page.refresh();
    } else if target == "catalog" {
        catalog_page.refresh();
    } else if target == "playground" {
        playground_page.refresh();
    } else if target == "teams" {
        teams_page.refresh();
    } else if target == "rules" {
        rules_page.refresh();
    } else if target == "hooks" {
        hooks_page.refresh();
    } else if target == "permissions" {
        permissions_page.refresh();
    } else if target == "settings" {
        settings_page.refresh();
    }
}

#[derive(Clone)]
struct ServerPage {
    app: adw::Application,
    server_count: gtk::Label,
    enabled_count: gtk::Label,
    profile_count: gtk::Label,
    profile_dropdown: gtk::DropDown,
    add_profile: gtk::Button,
    delete_profile: gtk::Button,
    section_title: gtk::Label,
    posture: gtk::Label,
    search: gtk::SearchEntry,
    feedback: gtk::Label,
    list: gtk::Box,
    last_snapshot: std::rc::Rc<std::cell::RefCell<Option<state::RegistrySnapshot>>>,
    updating_profile: std::rc::Rc<std::cell::Cell<bool>>,
    /// Per-row health widgets for the rows currently on screen, keyed by server
    /// id, so probe results can land on live labels without a re-render.
    health_rows: std::rc::Rc<std::cell::RefCell<std::collections::HashMap<String, HealthRow>>>,
    /// Last known probe result per server. Survives re-renders so the list can
    /// group needs-attention servers first without rows jumping mid-probe.
    probe_results: std::rc::Rc<
        std::cell::RefCell<std::collections::HashMap<String, crate::server_runtime::ProbeResult>>,
    >,
    /// Invalidates in-flight probes when the list re-renders.
    probe_generation: std::rc::Rc<std::cell::Cell<u64>>,
}

#[derive(Clone)]
struct HealthRow {
    label: gtk::Label,
    authenticate: gtk::Button,
    copy_error: gtk::Button,
}

impl ServerPage {
    fn render(&self, state: state::RegistryState) {
        self.feedback.set_visible(false);

        match state {
            state::RegistryState::Ready(snapshot) => {
                *self.last_snapshot.borrow_mut() = Some(snapshot.clone());
                self.server_count
                    .set_label(&snapshot.servers.len().to_string());
                self.enabled_count
                    .set_label(&snapshot.enabled_count.to_string());
                self.profile_count
                    .set_label(&snapshot.profile_count.to_string());
                self.render_profiles(&snapshot);
                self.render_server_list(&snapshot);
            }
            state::RegistryState::FirstRun => {
                self.clear_server_list();
                *self.last_snapshot.borrow_mut() = None;
                self.reset_summary();
                self.section_title.set_label("Servers");
                self.list.append(&state_card(
                    "network-server-symbolic",
                    "Toolport is ready for setup",
                    "Use Add server for a custom endpoint or open Catalog for a curated starting point. New servers stay disabled until you review them.",
                    false,
                ));
            }
            state::RegistryState::Unavailable => {
                self.clear_server_list();
                *self.last_snapshot.borrow_mut() = None;
                self.server_count.set_label("–");
                self.enabled_count.set_label("–");
                self.profile_count.set_label("–");
                self.section_title.set_label("Servers unavailable");
                self.list.append(&state_card(
                    "dialog-warning-symbolic",
                    "Configuration could not be displayed",
                    "The native preview left the registry untouched. Open the current Toolport app to inspect or recover it.",
                    true,
                ));
            }
        }
    }

    fn clear_server_list(&self) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
    }

    fn render_profiles(&self, snapshot: &state::RegistrySnapshot) {
        self.updating_profile.set(true);
        let names = snapshot
            .profiles
            .iter()
            .map(|profile| profile.name.as_str())
            .collect::<Vec<_>>();
        let model = gtk::StringList::new(&names);
        self.profile_dropdown.set_model(Some(&model));
        let selected = snapshot
            .profiles
            .iter()
            .position(|profile| profile.id == snapshot.active_profile_id)
            .unwrap_or(0) as u32;
        self.profile_dropdown.set_selected(selected);
        self.profile_dropdown
            .set_sensitive(!snapshot.profiles.is_empty());
        self.delete_profile
            .set_sensitive(snapshot.profiles.len() > 1);
        self.updating_profile.set(false);
    }

    fn render_server_list(&self, snapshot: &state::RegistrySnapshot) {
        self.clear_server_list();
        self.health_rows.borrow_mut().clear();
        self.probe_generation
            .set(self.probe_generation.get().wrapping_add(1));
        self.section_title
            .set_label(&format!("Servers in {}", snapshot.active_profile));
        if snapshot.servers.is_empty() {
            self.posture.set_visible(false);
            self.list.append(&state_card(
                "network-server-symbolic",
                "No servers yet",
                "Use Add server to connect your first local command or remote endpoint.",
                false,
            ));
            return;
        }

        let query = self.search.text();
        let mut matches = snapshot
            .servers
            .iter()
            .filter(|server| server_matches_query(server, query.as_str()))
            .collect::<Vec<_>>();
        if matches.is_empty() {
            self.posture.set_visible(false);
            self.list.append(&state_card(
                "edit-find-symbolic",
                "No matching servers",
                "Try a different server name or transport.",
                false,
            ));
            return;
        }
        // Group like the shipping list: servers needing attention first, then
        // unprobed, ready, and disabled last. Ranks come from the last finished
        // probe round so rows do not jump around while probes are in flight.
        {
            let results = self.probe_results.borrow();
            matches.sort_by_key(|server| {
                (
                    server_health_rank(server, results.get(&server.id)),
                    server.name.to_lowercase(),
                )
            });
        }
        for server in matches {
            self.list.append(&server_card(
                server,
                &snapshot.active_profile_id,
                snapshot.active_profile_tool_scope.get(&server.id).cloned(),
                self.clone(),
            ));
        }
        self.start_probes(snapshot);
    }

    /// Probe every enabled server in the background and land each result on its
    /// live row. Results also update the posture line and are remembered for
    /// grouping on the next render.
    fn start_probes(&self, snapshot: &state::RegistrySnapshot) {
        let generation = self.probe_generation.get();
        let to_probe: Vec<String> = snapshot
            .servers
            .iter()
            .filter(|server| server.enabled)
            .map(|server| server.id.clone())
            .collect();
        let total = to_probe.len();
        if total == 0 {
            self.posture.set_visible(false);
            return;
        }
        self.posture.set_visible(true);
        self.posture.set_label(&posture_line(0, 0, 0, total, total));
        // Per-round tallies: only this round's probes feed the posture line, so
        // a server removed mid-round can never inflate the counts.
        let counts = std::rc::Rc::new((
            std::cell::Cell::new(0usize), // ready
            std::cell::Cell::new(0usize), // needs sign-in
            std::cell::Cell::new(0usize), // failing
            std::cell::Cell::new(total),  // pending
        ));
        for server_id in to_probe {
            let page = self.clone();
            let counts = counts.clone();
            gtk::glib::spawn_future_local(async move {
                let id_for_probe = server_id.clone();
                let result = gtk::gio::spawn_blocking(move || {
                    crate::server_runtime::probe_registered(&id_for_probe)
                })
                .await;
                if page.probe_generation.get() != generation {
                    return;
                }
                let probe = match result {
                    Ok(Ok(probe)) => probe,
                    Ok(Err(error)) => crate::server_runtime::ProbeResult {
                        server_id: server_id.clone(),
                        ok: false,
                        tool_count: 0,
                        error: Some(error),
                        auth_required: false,
                    },
                    Err(_) => crate::server_runtime::ProbeResult {
                        server_id: server_id.clone(),
                        ok: false,
                        tool_count: 0,
                        error: Some("the probe stopped unexpectedly".to_string()),
                        auth_required: false,
                    },
                };
                let (ready, auth, errors, pending) = &*counts;
                if probe.ok {
                    ready.set(ready.get() + 1);
                } else if probe.auth_required {
                    auth.set(auth.get() + 1);
                } else {
                    errors.set(errors.get() + 1);
                }
                pending.set(pending.get().saturating_sub(1));
                page.apply_probe(&server_id, probe);
                page.posture.set_label(&posture_line(
                    ready.get(),
                    auth.get(),
                    errors.get(),
                    pending.get(),
                    total,
                ));
            });
        }
    }

    fn apply_probe(&self, server_id: &str, probe: crate::server_runtime::ProbeResult) {
        if let Some(row) = self.health_rows.borrow().get(server_id) {
            let (line, class) = probe_status_line(&probe);
            row.label.set_label(&line);
            row.label.remove_css_class("success");
            row.label.remove_css_class("error");
            row.label.remove_css_class("review");
            row.label.add_css_class(class);
            row.label
                .set_tooltip_text(probe.error.as_deref().filter(|error| !error.is_empty()));
            row.authenticate.set_visible(probe.auth_required);
            row.copy_error
                .set_visible(!probe.ok && probe.error.is_some());
        }
        self.probe_results
            .borrow_mut()
            .insert(server_id.to_string(), probe);
    }

    fn reset_summary(&self) {
        self.server_count.set_label("0");
        self.enabled_count.set_label("0");
        self.profile_count.set_label("1");
    }

    fn show_feedback(&self, message: &str, error: bool) {
        self.feedback.set_label(message);
        if error {
            self.feedback.add_css_class("error");
            self.feedback.remove_css_class("success");
        } else {
            self.feedback.add_css_class("success");
            self.feedback.remove_css_class("error");
        }
        self.feedback.set_visible(true);
    }

    fn restore_after_error(&self, message: &str) {
        let last_snapshot = self.last_snapshot.borrow().clone();
        if let Some(snapshot) = last_snapshot {
            self.render(state::RegistryState::Ready(snapshot));
        }
        self.show_feedback(message, true);
    }
}

#[derive(Clone)]
struct ClientPage {
    app: adw::Application,
    bridge: http_bridge::BridgeController,
    root: gtk::Box,
    list: gtk::Box,
    feedback: gtk::Label,
    installed_count: gtk::Label,
    connected_count: gtk::Label,
    configured_count: gtk::Label,
    import_button: gtk::Button,
    refresh_button: gtk::Button,
    scanning: std::rc::Rc<std::cell::Cell<bool>>,
    profiles: std::rc::Rc<std::cell::RefCell<Vec<state::ProfileView>>>,
}

impl ClientPage {
    fn new(app: &adw::Application, bridge: http_bridge::BridgeController) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("toolport-content");
        let header = adw::HeaderBar::new();
        header.add_css_class("toolport-header");
        header.set_show_back_button(true);
        header.set_title_widget(Some(
            &gtk::Label::builder()
                .label("Clients")
                .css_classes(["title"])
                .build(),
        ));
        let refresh_button = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Scan clients again")
            .build();
        header.pack_end(&refresh_button);
        let import_button = gtk::Button::with_label("Import");
        import_button.set_tooltip_text(Some("Review servers found in client configurations"));
        import_button.add_css_class("toolport-secondary-action");
        header.pack_end(&import_button);
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
                .label("Your AI clients, one Toolport gateway")
                .halign(gtk::Align::Start)
                .wrap(true)
                .css_classes(["title-2"])
                .build(),
        );
        page.append(
            &gtk::Label::builder()
                .label("Toolport reads each supported client's local MCP configuration and shows whether its gateway is connected. This scan never changes client files.")
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
        feedback.set_label("Open Clients to scan this machine.");
        page.append(&feedback);

        let summary = gtk::FlowBox::new();
        summary.add_css_class("toolport-summary");
        summary.set_column_spacing(10);
        summary.set_row_spacing(10);
        summary.set_min_children_per_line(1);
        summary.set_max_children_per_line(3);
        summary.set_homogeneous(true);
        summary.set_selection_mode(gtk::SelectionMode::None);
        let mut values = Vec::new();
        for (value, label) in [("–", "Installed"), ("–", "Connected"), ("–", "Configured")] {
            let (item, value) = summary_item(value, label);
            values.push(value);
            summary.insert(&item, -1);
        }
        page.append(&summary);
        page.append(
            &gtk::Label::builder()
                .label("Detected clients")
                .halign(gtk::Align::Start)
                .css_classes(["heading"])
                .build(),
        );
        let list = gtk::Box::new(gtk::Orientation::Vertical, 12);
        page.append(&list);
        scroller.set_child(Some(&page));
        root.append(&scroller);

        let client_page = Self {
            app: app.clone(),
            bridge,
            root,
            list,
            feedback,
            installed_count: values.remove(0),
            connected_count: values.remove(0),
            configured_count: values.remove(0),
            import_button,
            refresh_button,
            scanning: std::rc::Rc::new(std::cell::Cell::new(false)),
            profiles: std::rc::Rc::new(std::cell::RefCell::new(Vec::new())),
        };
        let page_for_refresh = client_page.clone();
        client_page
            .refresh_button
            .connect_clicked(move |_| page_for_refresh.refresh());
        let page_for_import = client_page.clone();
        client_page
            .import_button
            .connect_clicked(move |_| page_for_import.preview_imports());
        client_page
    }

    fn preview_imports(&self) {
        if self.scanning.replace(true) {
            return;
        }
        self.import_button.set_sensitive(false);
        self.feedback
            .set_label("Looking for servers in client configurations…");
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let result =
                gtk::gio::spawn_blocking(crate::registry_controller::preview_client_imports).await;
            page.scanning.set(false);
            page.import_button.set_sensitive(true);
            match result {
                Ok(Ok(candidates)) if candidates.is_empty() => {
                    page.feedback
                        .set_label("No new client-configured servers are available to import.");
                    page.feedback.remove_css_class("error");
                    page.feedback.add_css_class("success");
                }
                Ok(Ok(candidates)) => page.show_import_review(candidates),
                Ok(Err(error)) => page.show_error(&error),
                Err(_) => page.show_error("the import scan stopped unexpectedly"),
            }
        });
    }

    fn show_import_review(
        &self,
        candidates: Vec<crate::registry_controller::ClientImportCandidate>,
    ) {
        let Some(parent) = self.root.root().and_downcast::<gtk::Window>() else {
            return;
        };
        #[allow(deprecated)]
        let dialog = adw::MessageDialog::new(
            Some(&parent),
            Some("Import servers from clients"),
            Some("Review the local servers Toolport found. Imported servers start disabled so you can inspect credentials and commands before enabling them."),
        );
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("import", "Import selected");
        dialog.set_close_response("cancel");
        dialog.set_default_response(Some("import"));
        dialog.set_response_appearance("import", adw::ResponseAppearance::Suggested);

        let rows = gtk::Box::new(gtk::Orientation::Vertical, 6);
        let mut selections = Vec::new();
        for candidate in candidates {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            row.add_css_class("toolport-setting-row");
            let selected = gtk::CheckButton::builder().active(true).build();
            row.append(&selected);
            let copy = gtk::Box::new(gtk::Orientation::Vertical, 2);
            copy.set_hexpand(true);
            copy.append(
                &gtk::Label::builder()
                    .label(&candidate.name)
                    .halign(gtk::Align::Start)
                    .xalign(0.0)
                    .wrap(true)
                    .css_classes(["heading"])
                    .build(),
            );
            let origin = candidate
                .command
                .clone()
                .or_else(|| {
                    candidate
                        .url
                        .as_deref()
                        .map(crate::registry::redact_url_userinfo)
                })
                .unwrap_or_else(|| "Client-managed connection".to_string());
            copy.append(
                &gtk::Label::builder()
                    .label(format!("{} · {origin}", candidate.transport))
                    .halign(gtk::Align::Start)
                    .xalign(0.0)
                    .wrap(true)
                    .ellipsize(gtk::pango::EllipsizeMode::Middle)
                    .css_classes(["toolport-muted"])
                    .build(),
            );
            row.append(&copy);
            rows.append(&row);
            selections.push((selected, candidate.key));
        }
        let scroller = gtk::ScrolledWindow::builder()
            .child(&rows)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .min_content_height(160)
            .max_content_height(360)
            .build();
        dialog.set_extra_child(Some(&scroller));
        let selections = std::rc::Rc::new(selections);
        let page = self.clone();
        dialog.connect_response(None, move |dialog, response| {
            if response == "import" {
                let selected = selections
                    .iter()
                    .filter(|(check, _)| check.is_active())
                    .map(|(_, key)| key.clone())
                    .collect::<Vec<_>>();
                if selected.is_empty() {
                    page.feedback.set_label("Nothing selected for import.");
                } else {
                    page.run_import(selected);
                }
            }
            dialog.close();
        });
        dialog.present();
    }

    fn run_import(&self, selected: Vec<String>) {
        self.import_button.set_sensitive(false);
        self.feedback.set_label("Importing selected servers…");
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                crate::registry_controller::import_client_servers(selected)
            })
            .await;
            page.import_button.set_sensitive(true);
            match result {
                Ok(Ok((_, added))) => {
                    page.feedback.set_label(&format!(
                        "Imported {added} server{}. They remain disabled until reviewed.",
                        if added == 1 { "" } else { "s" }
                    ));
                    page.feedback.remove_css_class("error");
                    page.feedback.add_css_class("success");
                    page.refresh();
                }
                Ok(Err(error)) => page.show_error(&error),
                Err(_) => page.show_error("the import stopped unexpectedly"),
            }
        });
    }

    fn refresh(&self) {
        if self.scanning.replace(true) {
            return;
        }
        self.refresh_button.set_sensitive(false);
        self.import_button.set_sensitive(false);
        self.feedback
            .set_label("Scanning local client configurations…");
        self.feedback.remove_css_class("error");
        self.feedback.set_visible(true);
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(state::detect_client_views).await;
            page.scanning.set(false);
            page.refresh_button.set_sensitive(true);
            page.import_button.set_sensitive(true);
            match result {
                Ok(Ok(snapshot)) => page.render(snapshot),
                Ok(Err(error)) => page.show_error(&error),
                Err(_) => page.show_error("the client scan stopped unexpectedly"),
            }
        });
    }

    fn render(&self, snapshot: state::ClientSnapshot) {
        *self.profiles.borrow_mut() = snapshot.profiles;
        let clients = snapshot.clients;
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        let installed = clients
            .iter()
            .filter(|client| client.app_present || client.config_exists)
            .collect::<Vec<_>>();
        let connected = clients
            .iter()
            .filter(|client| client.gateway_state == state::ClientGatewayState::Connected)
            .count();
        let configured = clients.iter().filter(|client| client.config_exists).count();
        self.installed_count.set_label(&installed.len().to_string());
        self.connected_count.set_label(&connected.to_string());
        self.configured_count.set_label(&configured.to_string());
        self.feedback
            .set_label(&format!("Scanned {} supported clients", clients.len()));
        self.feedback.remove_css_class("error");
        self.feedback.add_css_class("success");

        let absent = clients
            .iter()
            .filter(|client| !client.app_present && !client.config_exists)
            .collect::<Vec<_>>();
        if installed.is_empty() {
            self.list.append(&state_card(
                "computer-symbolic",
                "No supported clients detected",
                "Install or open a supported AI client, then scan again.",
                false,
            ));
        }
        for client in &installed {
            self.list.append(&client_card(client, self.clone()));
        }
        // Supported-but-absent clients: collapsed so they don't crowd the real
        // list, but visible so the user knows what Toolport could manage.
        if !absent.is_empty() {
            let expander = gtk::Expander::new(Some(&format!(
                "Not installed · {} supported {}",
                absent.len(),
                if absent.len() == 1 {
                    "client"
                } else {
                    "clients"
                }
            )));
            expander.add_css_class("toolport-card");
            let names = gtk::Label::builder()
                .label(
                    absent
                        .iter()
                        .map(|client| client.name.as_str())
                        .collect::<Vec<_>>()
                        .join(" · "),
                )
                .halign(gtk::Align::Start)
                .xalign(0.0)
                .wrap(true)
                .margin_top(8)
                .css_classes(["toolport-muted"])
                .build();
            expander.set_child(Some(&names));
            self.list.append(&expander);
        }
    }

    fn show_error(&self, error: &str) {
        self.feedback
            .set_label(&format!("Could not scan clients: {error}"));
        self.feedback.remove_css_class("success");
        self.feedback.add_css_class("error");
    }
}

fn client_card(client: &state::ClientView, page: ClientPage) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    card.add_css_class("toolport-card");
    let icon = gtk::Image::from_icon_name("computer-symbolic");
    icon.add_css_class("toolport-card-icon");
    card.append(&icon);
    let copy = gtk::Box::new(gtk::Orientation::Vertical, 3);
    copy.set_hexpand(true);
    copy.append(
        &gtk::Label::builder()
            .label(&client.name)
            .halign(gtk::Align::Start)
            .css_classes(["heading"])
            .build(),
    );
    let detail = if client.config_error {
        "Configuration could not be read safely".to_string()
    } else if client.uses_connectors {
        "Uses account connectors".to_string()
    } else {
        let mut detail = format!(
            "{} local MCP {}",
            client.server_count,
            if client.server_count == 1 {
                "server"
            } else {
                "servers"
            }
        );
        if client.movable_server_count > 0 {
            detail.push_str(&format!(" · {} to import", client.movable_server_count));
        }
        if client.gateway_state == state::ClientGatewayState::Connected {
            detail.push_str(if client.shared_http {
                " · Shared HTTP"
            } else {
                " · stdio"
            });
            detail.push_str(
                &client
                    .scope_name
                    .as_deref()
                    .map(|scope| format!(" · only {scope}"))
                    .unwrap_or_else(|| " · follows active profile".to_string()),
            );
        }
        detail
    };
    copy.append(
        &gtk::Label::builder()
            .label(detail)
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .wrap(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["toolport-muted"])
            .build(),
    );
    card.append(&copy);
    let (status, class) = match client.gateway_state {
        state::ClientGatewayState::Connected => ("Connected", "success"),
        state::ClientGatewayState::Customized => ("Customized", "review"),
        state::ClientGatewayState::Disconnected => ("Not connected", "disabled"),
    };
    let badge = gtk::Label::new(Some(status));
    badge.add_css_class("toolport-badge");
    badge.add_css_class(class);
    badge.set_tooltip_text(Some(&format!("Client id: {}", client.id)));
    card.append(&badge);
    if !client.uses_connectors {
        if client.movable_server_count > 0 && !client.config_error {
            let migrate =
                gtk::Button::with_label(&format!("Move in {}", client.movable_server_count));
            migrate.add_css_class("toolport-secondary-action");
            migrate.set_tooltip_text(Some(&format!(
                "Import the {} {} this client manages directly, then rewrite its config to use only the Toolport gateway",
                client.movable_server_count,
                if client.movable_server_count == 1 {
                    "server"
                } else {
                    "servers"
                }
            )));
            let client_for_migrate = client.clone();
            let page_for_migrate = page.clone();
            migrate.connect_clicked(move |button| {
                confirm_client_migrate(
                    &client_for_migrate,
                    button.clone(),
                    page_for_migrate.clone(),
                );
            });
            card.append(&migrate);
        }
        match client.gateway_state {
            state::ClientGatewayState::Disconnected => {
                let connect = gtk::Button::with_label("Connect");
                connect.add_css_class("suggested-action");
                let client_for_connect = client.clone();
                let page_for_connect = page.clone();
                connect.connect_clicked(move |button| {
                    run_client_mutation(
                        &client_for_connect,
                        true,
                        false,
                        false,
                        None,
                        button,
                        page_for_connect.clone(),
                    );
                });
                card.append(&connect);
                card.append(&shared_http_menu(client.clone(), false, page));
            }
            state::ClientGatewayState::Customized => {
                let reset = gtk::Button::with_label("Reset");
                reset.add_css_class("toolport-secondary-action");
                let client_for_reset = client.clone();
                let page_for_reset = page.clone();
                reset.connect_clicked(move |button| {
                    confirm_client_reset(&client_for_reset, button.clone(), page_for_reset.clone());
                });
                card.append(&reset);
                card.append(&shared_http_menu(client.clone(), true, page));
            }
            state::ClientGatewayState::Connected => {
                if page.profiles.borrow().len() > 1 {
                    card.append(&client_scope_menu(client.clone(), page.clone()));
                }
                card.append(&client_discovery_menu(client.clone(), page.clone()));
                let disconnect = gtk::Button::with_label("Disconnect");
                disconnect.add_css_class("toolport-secondary-action");
                let client = client.clone();
                disconnect.connect_clicked(move |button| {
                    confirm_client_disconnect(&client, button.clone(), page.clone());
                });
                card.append(&disconnect);
            }
        }
    }
    card
}

/// The feedback line after a one-shot migration. States what moved, what was
/// newly imported, and that a backup exists - the user is about to restart the
/// client and needs to know the old config is recoverable.
fn migrate_feedback(client_name: &str, imported: usize, moved: usize, backup: bool) -> String {
    let mut message = format!(
        "Moved {moved} {} into Toolport ({imported} newly imported). {client_name} now uses only the Toolport gateway.",
        if moved == 1 { "server" } else { "servers" }
    );
    if backup {
        message.push_str(" The previous config was backed up.");
    }
    message.push_str(" Restart the client to pick this up.");
    message
}

fn confirm_client_migrate(client: &state::ClientView, button: gtk::Button, page: ClientPage) {
    let Some(parent) = page.root.root().and_downcast::<gtk::Window>() else {
        return;
    };
    let count = client.movable_server_count;
    let mut body = format!(
        "Toolport imports the {count} {} this client manages directly (new ones stay disabled until you review them), backs the config up, and rewrites it to contain only the Toolport gateway. Plugin-managed servers are left untouched. Secret values are never read from the client; add them under Credentials after the move.",
        if count == 1 { "server" } else { "servers" }
    );
    let force = client.gateway_state == state::ClientGatewayState::Customized;
    if force {
        body.push_str(
            "\n\nThis client's Toolport entry has a custom configuration; migrating replaces it with the default gateway entry.",
        );
    }
    #[allow(deprecated)]
    let dialog = adw::MessageDialog::new(
        Some(&parent),
        Some(&format!("Move {}'s servers into Toolport?", client.name)),
        Some(&body),
    );
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("migrate", "Move into gateway");
    dialog.set_close_response("cancel");
    dialog.set_default_response(Some("cancel"));
    dialog.set_response_appearance("migrate", adw::ResponseAppearance::Suggested);
    let client = client.clone();
    dialog.connect_response(None, move |dialog, response| {
        if response == "migrate" {
            button.set_sensitive(false);
            page.feedback.set_label("Moving servers into Toolport…");
            page.feedback.remove_css_class("error");
            page.feedback.set_visible(true);
            let client_id = client.id.clone();
            let client_name = client.name.clone();
            let scope = client.scope_id.clone();
            let shared_http = client.shared_http;
            let bridge = page.bridge.clone();
            let page = page.clone();
            let button = button.clone();
            gtk::glib::spawn_future_local(async move {
                let result = gtk::gio::spawn_blocking(move || {
                    // A connected Shared HTTP client stays Shared HTTP; everyone
                    // else migrates to the stdio gateway, matching the shipping
                    // dialog's transport choice.
                    let url = if shared_http {
                        let status = bridge.start(None)?;
                        let port = status
                            .port
                            .ok_or("The HTTP endpoint started without a port")?;
                        Some(format!("http://127.0.0.1:{port}/mcp"))
                    } else {
                        None
                    };
                    crate::registry_controller::migrate_client(
                        &client_id,
                        scope.as_deref(),
                        force,
                        url.as_deref(),
                    )
                })
                .await;
                button.set_sensitive(true);
                match result {
                    Ok(Ok(outcome)) => {
                        page.refresh();
                        page.feedback.set_label(&migrate_feedback(
                            &client_name,
                            outcome.imported,
                            outcome.moved.len(),
                            outcome.result.outcome.backup.is_some(),
                        ));
                    }
                    Ok(Err(error)) => page.show_error(&format!("{client_name}: {error}")),
                    Err(_) => page.show_error(&format!("{client_name}: the migration stopped")),
                }
            });
        }
        dialog.close();
    });
    dialog.present();
}

fn client_discovery_menu(client: state::ClientView, page: ClientPage) -> gtk::MenuButton {
    let label = match client.discovery_mode.as_deref() {
        Some("full") => "Full catalog",
        Some("lazy") => "Lazy discovery",
        Some("grouped") => "Grouped discovery",
        _ => "Inherit discovery",
    };
    let menu = gtk::MenuButton::builder()
        .icon_name("system-search-symbolic")
        .tooltip_text(format!("{label} for {}", client.name))
        .build();
    menu.add_css_class("flat");
    let popover = gtk::Popover::new();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 4);
    content.set_margin_top(6);
    content.set_margin_bottom(6);
    content.set_margin_start(6);
    content.set_margin_end(6);
    // Advisory only, mirroring the shipping picker: local-model desktop apps do
    // better browsing one server at a time; everyone else handles lazy's
    // search-then-call chain and saves the most tokens.
    let recommended = if matches!(client.id.as_str(), "lm-studio" | "jan" | "anythingllm") {
        (
            "grouped",
            "Recommended: local models do better browsing one server at a time",
        )
    } else {
        (
            "lazy",
            "Recommended: this client handles the search-then-call step and saves the most tokens",
        )
    };
    for (label, mode, hint) in [
        (
            "Inherit global discovery",
            None,
            "Follow the global discovery setting",
        ),
        (
            "Full catalog",
            Some("full"),
            "Advertises every tool up front. Most tokens, no discovery step.",
        ),
        (
            "Lazy search",
            Some("lazy"),
            "Advertises a few meta-tools; the client searches, then calls. Fewest tokens.",
        ),
        (
            "Grouped search",
            Some("grouped"),
            "One help tool per server; the client expands a server before calling it.",
        ),
    ] {
        let button = gtk::Button::with_label(
            if mode == Some(recommended.0) {
                format!("{label} (recommended)")
            } else {
                label.to_string()
            }
            .as_str(),
        );
        button.add_css_class("flat");
        button.set_tooltip_text(Some(if mode == Some(recommended.0) {
            recommended.1
        } else {
            hint
        }));
        let client_id = client.id.clone();
        let client_name = client.name.clone();
        let page = page.clone();
        let menu = menu.clone();
        button.connect_clicked(move |_| {
            menu.popdown();
            page.feedback.set_label("Updating client discovery…");
            let client_id = client_id.clone();
            let client_name = client_name.clone();
            let page = page.clone();
            gtk::glib::spawn_future_local(async move {
                let result = gtk::gio::spawn_blocking(move || {
                    crate::registry_controller::set_client_discovery(&client_id, mode)
                })
                .await;
                match result {
                    Ok(Ok(_)) => {
                        page.refresh();
                        page.feedback
                            .set_label(&format!("Updated discovery for {client_name}."));
                    }
                    Ok(Err(error)) => page.show_error(&error),
                    Err(_) => page.show_error("the discovery update stopped unexpectedly"),
                }
            });
        });
        content.append(&button);
    }
    popover.set_child(Some(&content));
    menu.set_popover(Some(&popover));
    menu
}

fn shared_http_menu(client: state::ClientView, force: bool, page: ClientPage) -> gtk::MenuButton {
    let menu = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .tooltip_text("Connection options")
        .build();
    menu.add_css_class("flat");
    let popover = gtk::Popover::new();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 4);
    content.set_margin_top(6);
    content.set_margin_bottom(6);
    content.set_margin_start(6);
    content.set_margin_end(6);
    let shared = gtk::Button::with_label(if force {
        "Reset with Shared HTTP"
    } else {
        "Connect with Shared HTTP"
    });
    shared.add_css_class("flat");
    let menu_for_click = menu.clone();
    shared.connect_clicked(move |button| {
        menu_for_click.popdown();
        if force {
            confirm_client_reset_shared(&client, button.clone(), page.clone());
        } else {
            run_client_mutation(&client, true, true, false, None, button, page.clone());
        }
    });
    content.append(&shared);
    popover.set_child(Some(&content));
    menu.set_popover(Some(&popover));
    menu
}

fn client_scope_menu(client: state::ClientView, page: ClientPage) -> gtk::MenuButton {
    let label = client
        .scope_name
        .as_deref()
        .map(|scope| format!("Only {scope}"))
        .unwrap_or_else(|| "Active profile".to_string());
    let menu = gtk::MenuButton::builder()
        .label(label)
        .tooltip_text("Choose which profile this client can use")
        .build();
    menu.add_css_class("toolport-secondary-action");
    let popover = gtk::Popover::new();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 4);
    content.set_margin_top(6);
    content.set_margin_bottom(6);
    content.set_margin_start(6);
    content.set_margin_end(6);

    let active = gtk::Button::with_label("Follow active profile");
    active.add_css_class("flat");
    let client_for_active = client.clone();
    let page_for_active = page.clone();
    let menu_for_active = menu.clone();
    active.connect_clicked(move |button| {
        menu_for_active.popdown();
        run_client_mutation(
            &client_for_active,
            true,
            client_for_active.shared_http,
            false,
            None,
            button,
            page_for_active.clone(),
        );
    });
    content.append(&active);
    for profile in page.profiles.borrow().iter().cloned() {
        let button = gtk::Button::with_label(&format!("Only {}", profile.name));
        button.add_css_class("flat");
        let client = client.clone();
        let page = page.clone();
        let menu = menu.clone();
        button.connect_clicked(move |button| {
            menu.popdown();
            run_client_mutation(
                &client,
                true,
                client.shared_http,
                false,
                Some(profile.id.clone()),
                button,
                page.clone(),
            );
        });
        content.append(&button);
    }
    popover.set_child(Some(&content));
    menu.set_popover(Some(&popover));
    menu
}

fn confirm_client_reset(client: &state::ClientView, button: gtk::Button, page: ClientPage) {
    let Some(parent) = page.app.active_window() else {
        return;
    };
    #[allow(deprecated)]
    let dialog = adw::MessageDialog::new(
        Some(&parent),
        Some(&format!("Reset {}'s Toolport entry?", client.name)),
        Some("This replaces only the customized Toolport gateway entry. Other client settings and MCP servers are preserved."),
    );
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("reset", "Reset and connect");
    dialog.set_close_response("cancel");
    dialog.set_default_response(Some("cancel"));
    dialog.set_response_appearance("reset", adw::ResponseAppearance::Suggested);
    let client = client.clone();
    dialog.connect_response(None, move |dialog, response| {
        if response == "reset" {
            run_client_mutation(&client, true, false, true, None, &button, page.clone());
        }
        dialog.close();
    });
    dialog.present();
}

fn confirm_client_reset_shared(client: &state::ClientView, button: gtk::Button, page: ClientPage) {
    let Some(parent) = page.app.active_window() else {
        return;
    };
    #[allow(deprecated)]
    let dialog = adw::MessageDialog::new(
        Some(&parent),
        Some(&format!("Reset {} for Shared HTTP?", client.name)),
        Some("This replaces only the customized Toolport gateway entry and starts Toolport's authenticated local HTTP endpoint."),
    );
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("reset", "Reset and connect");
    dialog.set_close_response("cancel");
    dialog.set_default_response(Some("cancel"));
    dialog.set_response_appearance("reset", adw::ResponseAppearance::Suggested);
    let client = client.clone();
    dialog.connect_response(None, move |dialog, response| {
        if response == "reset" {
            run_client_mutation(&client, true, true, true, None, &button, page.clone());
        }
        dialog.close();
    });
    dialog.present();
}

fn confirm_client_disconnect(client: &state::ClientView, button: gtk::Button, page: ClientPage) {
    let Some(parent) = page.app.active_window() else {
        return;
    };
    #[allow(deprecated)]
    let dialog = adw::MessageDialog::new(
        Some(&parent),
        Some(&format!("Disconnect {}?", client.name)),
        Some("Toolport removes only its managed gateway entry. The client's other settings and MCP servers are preserved."),
    );
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("disconnect", "Disconnect");
    dialog.set_close_response("cancel");
    dialog.set_default_response(Some("cancel"));
    dialog.set_response_appearance("disconnect", adw::ResponseAppearance::Destructive);
    let client = client.clone();
    dialog.connect_response(None, move |dialog, response| {
        if response == "disconnect" {
            run_client_mutation(&client, false, false, false, None, &button, page.clone());
        }
        dialog.close();
    });
    dialog.present();
}

fn run_client_mutation(
    client: &state::ClientView,
    connect: bool,
    shared_http: bool,
    force: bool,
    profile: Option<String>,
    button: &gtk::Button,
    page: ClientPage,
) {
    button.set_sensitive(false);
    page.feedback.set_label(if connect {
        "Connecting client…"
    } else {
        "Disconnecting client…"
    });
    page.feedback.remove_css_class("error");
    page.feedback.set_visible(true);
    let client_id = client.id.clone();
    let client_name = client.name.clone();
    let bridge = page.bridge.clone();
    let button = button.clone();
    gtk::glib::spawn_future_local(async move {
        let result = gtk::gio::spawn_blocking(move || {
            if connect {
                if shared_http {
                    let status = bridge.start(None)?;
                    let port = status
                        .port
                        .ok_or("The HTTP endpoint started without a port")?;
                    crate::registry_controller::connect_client_shared_http(
                        &client_id,
                        profile.as_deref(),
                        force,
                        &format!("http://127.0.0.1:{port}/mcp"),
                    )
                } else {
                    crate::registry_controller::connect_client_stdio(
                        &client_id,
                        profile.as_deref(),
                        force,
                    )
                }
            } else {
                crate::registry_controller::disconnect_client(&client_id)
            }
        })
        .await;
        button.set_sensitive(true);
        match result {
            Ok(Ok(_)) => {
                page.refresh();
                // The config write is not live until the client restarts; saying
                // only "Connected" would misstate what the running client does.
                page.feedback.set_label(&if connect {
                    format!(
                        "Connected {client_name}. Not live yet: restart {client_name} so it picks up the Toolport gateway."
                    )
                } else {
                    format!(
                        "Disconnected {client_name}. It stays connected to the running gateway until it restarts."
                    )
                });
            }
            Ok(Err(error)) => page.show_error(&format!("{client_name}: {error}")),
            Err(_) => page.show_error(&format!("{client_name}: the operation stopped")),
        }
    });
}

#[derive(Clone)]
struct ActivityPage {
    app: adw::Application,
    root: gtk::Box,
    list: gtk::Box,
    security_list: gtk::Box,
    stats_list: gtk::Box,
    identity_list: gtk::Box,
    search_list: gtk::Box,
    inspect_list: gtk::Box,
    feedback: gtk::Label,
    call_count: gtk::Label,
    success_rate: gtk::Label,
    average_latency: gtk::Label,
    tokens_saved: gtk::Label,
    refresh_button: gtk::Button,
    clear_button: gtk::Button,
    loading: std::rc::Rc<std::cell::Cell<bool>>,
    /// The last successfully loaded snapshot. Kept so a failed refresh shows a
    /// stale notice over real rows instead of replacing them with "all clear",
    /// and so filter changes re-render without another disk read.
    last_snapshot: std::rc::Rc<std::cell::RefCell<Option<state::ActivitySnapshot>>>,
    filter_server: gtk::DropDown,
    errors_only: gtk::ToggleButton,
    filter_count: gtk::Label,
    identity_search: gtk::SearchEntry,
    updating_filters: std::rc::Rc<std::cell::Cell<bool>>,
    savings_banner: gtk::Box,
    savings_value: gtk::Label,
    savings_dollars: gtk::Label,
    savings_model: gtk::DropDown,
    savings_detail: gtk::Label,
    /// Dismissed security-finding identities (`integrity::security_key`),
    /// insertion-ordered and persisted so a dismissal survives restarts.
    security_dismissed: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
}

impl ActivityPage {
    fn new(app: &adw::Application) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("toolport-content");
        let header = adw::HeaderBar::new();
        header.add_css_class("toolport-header");
        header.set_show_back_button(true);
        header.set_title_widget(Some(
            &gtk::Label::builder()
                .label("Activity")
                .css_classes(["title"])
                .build(),
        ));
        let clear_button = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text("Clear retained activity")
            .build();
        clear_button.add_css_class("flat");
        let refresh_button = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Refresh activity")
            .build();
        let export_button = gtk::MenuButton::builder()
            .icon_name("document-save-symbolic")
            .tooltip_text("Export activity")
            .build();
        let export_menu = gtk::Popover::new();
        let export_actions = gtk::Box::new(gtk::Orientation::Vertical, 4);
        export_actions.set_margin_top(6);
        export_actions.set_margin_bottom(6);
        export_actions.set_margin_start(6);
        export_actions.set_margin_end(6);
        let export_json = gtk::Button::with_label("Export JSON");
        export_json.add_css_class("flat");
        export_actions.append(&export_json);
        let export_csv = gtk::Button::with_label("Export CSV");
        export_csv.add_css_class("flat");
        export_actions.append(&export_csv);
        export_menu.set_child(Some(&export_actions));
        export_button.set_popover(Some(&export_menu));
        header.pack_end(&clear_button);
        header.pack_end(&export_button);
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
                .label("Every routed tool call, visible locally")
                .halign(gtk::Align::Start)
                .wrap(true)
                .css_classes(["title-2"])
                .build(),
        );
        page.append(
            &gtk::Label::builder()
                .label("Toolport records outcomes and timing, never tool arguments or result data. The most recent 100 calls are shown below.")
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
        feedback.set_label("Open Activity to load retained calls.");
        page.append(&feedback);

        let summary = gtk::FlowBox::new();
        summary.add_css_class("toolport-summary");
        summary.set_column_spacing(10);
        summary.set_row_spacing(10);
        summary.set_min_children_per_line(1);
        summary.set_max_children_per_line(4);
        summary.set_homogeneous(true);
        summary.set_selection_mode(gtk::SelectionMode::None);
        let mut values = Vec::new();
        for (value, label) in [
            ("–", "Retained calls"),
            ("–", "Success rate"),
            ("–", "Average latency"),
            ("–", "Tokens saved"),
        ] {
            let (item, value) = summary_item(value, label);
            values.push(value);
            summary.insert(&item, -1);
        }
        page.append(&summary);

        let savings_banner = gtk::Box::new(gtk::Orientation::Vertical, 6);
        savings_banner.add_css_class("toolport-card");
        savings_banner.set_visible(false);
        savings_banner.append(
            &gtk::Label::builder()
                .label("Tool definitions lazy discovery keeps out of context")
                .halign(gtk::Align::Start)
                .css_classes(["heading"])
                .build(),
        );
        let savings_row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let savings_value = gtk::Label::builder()
            .halign(gtk::Align::Start)
            .css_classes(["title-2"])
            .build();
        savings_row.append(&savings_value);
        let savings_dollars = gtk::Label::builder()
            .halign(gtk::Align::Start)
            .valign(gtk::Align::End)
            .css_classes(["toolport-muted"])
            .build();
        savings_row.append(&savings_dollars);
        let savings_model = gtk::DropDown::from_strings(
            &SAVINGS_MODELS
                .iter()
                .map(|(label, _)| *label)
                .collect::<Vec<_>>(),
        );
        savings_model.set_selected(1); // Claude Sonnet, the shipping default.
        savings_model.add_css_class("toolport-input");
        savings_model.set_valign(gtk::Align::Center);
        savings_row.append(&savings_model);
        let savings_share = gtk::Button::with_label("Share");
        savings_share.add_css_class("toolport-secondary-action");
        savings_share.set_valign(gtk::Align::Center);
        savings_share.set_tooltip_text(Some("Copy a shareable savings line to the clipboard"));
        savings_row.append(&savings_share);
        savings_banner.append(&savings_row);
        let savings_detail = gtk::Label::builder()
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-muted", "caption"])
            .build();
        savings_banner.append(&savings_detail);
        page.append(&savings_banner);

        page.append(
            &gtk::Label::builder()
                .label("Per-server breakdown")
                .halign(gtk::Align::Start)
                .css_classes(["heading"])
                .build(),
        );
        let stats_list = gtk::Box::new(gtk::Orientation::Vertical, 10);
        page.append(&stats_list);
        page.append(
            &gtk::Label::builder()
                .label("Recent calls")
                .halign(gtk::Align::Start)
                .css_classes(["heading"])
                .build(),
        );
        let filter_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let filter_server = gtk::DropDown::from_strings(&["All servers"]);
        filter_server.add_css_class("toolport-input");
        filter_server.set_tooltip_text(Some("Show calls from one server"));
        filter_row.append(&filter_server);
        let errors_only = gtk::ToggleButton::with_label("Errors only");
        errors_only.add_css_class("toolport-secondary-action");
        filter_row.append(&errors_only);
        let filter_count = gtk::Label::builder()
            .halign(gtk::Align::End)
            .hexpand(true)
            .css_classes(["toolport-muted", "caption"])
            .build();
        filter_row.append(&filter_count);
        page.append(&filter_row);
        let list = gtk::Box::new(gtk::Orientation::Vertical, 10);
        page.append(&list);
        page.append(
            &gtk::Label::builder()
                .label("Security events")
                .halign(gtk::Align::Start)
                .css_classes(["heading"])
                .build(),
        );
        let security_list = gtk::Box::new(gtk::Orientation::Vertical, 10);
        page.append(&security_list);
        page.append(
            &gtk::Label::builder()
                .label("Tool identities")
                .halign(gtk::Align::Start)
                .css_classes(["heading"])
                .build(),
        );
        let identity_search = gtk::SearchEntry::builder()
            .placeholder_text("Filter tools or servers")
            .css_classes(["toolport-search"])
            .build();
        page.append(&identity_search);
        let identity_list = gtk::Box::new(gtk::Orientation::Vertical, 10);
        page.append(&identity_list);
        page.append(
            &gtk::Label::builder()
                .label("Discovery traces")
                .halign(gtk::Align::Start)
                .css_classes(["heading"])
                .build(),
        );
        let search_list = gtk::Box::new(gtk::Orientation::Vertical, 10);
        page.append(&search_list);
        page.append(
            &gtk::Label::builder()
                .label("Live inspector")
                .halign(gtk::Align::Start)
                .css_classes(["heading"])
                .build(),
        );
        let inspect_list = gtk::Box::new(gtk::Orientation::Vertical, 10);
        page.append(&inspect_list);
        scroller.set_child(Some(&page));
        root.append(&scroller);

        let activity_page = Self {
            app: app.clone(),
            root,
            list,
            stats_list,
            security_list,
            identity_list,
            search_list,
            inspect_list,
            feedback,
            call_count: values.remove(0),
            success_rate: values.remove(0),
            average_latency: values.remove(0),
            tokens_saved: values.remove(0),
            refresh_button,
            clear_button,
            loading: std::rc::Rc::new(std::cell::Cell::new(false)),
            last_snapshot: std::rc::Rc::new(std::cell::RefCell::new(None)),
            filter_server,
            errors_only,
            filter_count,
            identity_search,
            updating_filters: std::rc::Rc::new(std::cell::Cell::new(false)),
            savings_banner,
            savings_value,
            savings_dollars,
            savings_model,
            savings_detail,
            security_dismissed: std::rc::Rc::new(
                std::cell::RefCell::new(load_security_dismissed()),
            ),
        };
        let page_for_model = activity_page.clone();
        activity_page
            .savings_model
            .connect_selected_notify(move |_| page_for_model.render_savings());
        let page_for_share = activity_page.clone();
        savings_share.connect_clicked(move |_| {
            let tokens = page_for_share
                .last_snapshot
                .borrow()
                .as_ref()
                .map(|snapshot| snapshot.tokens_saved)
                .unwrap_or(0);
            if let Some(display) = gtk::gdk::Display::default() {
                display.clipboard().set_text(&savings_share_line(tokens));
                page_for_share
                    .feedback
                    .set_label("Savings copied, paste them anywhere.");
                page_for_share.feedback.remove_css_class("error");
                page_for_share.feedback.add_css_class("success");
            }
        });
        let page_for_refresh = activity_page.clone();
        activity_page
            .refresh_button
            .connect_clicked(move |_| page_for_refresh.refresh());
        let page_for_filter = activity_page.clone();
        activity_page
            .filter_server
            .connect_selected_notify(move |_| {
                if !page_for_filter.updating_filters.get() {
                    page_for_filter.render_recent();
                }
            });
        let page_for_errors = activity_page.clone();
        activity_page.errors_only.connect_toggled(move |_| {
            if !page_for_errors.updating_filters.get() {
                page_for_errors.render_recent();
            }
        });
        let page_for_identity_search = activity_page.clone();
        activity_page
            .identity_search
            .connect_search_changed(move |_| page_for_identity_search.render_identities());
        let page_for_clear = activity_page.clone();
        activity_page
            .clear_button
            .connect_clicked(move |_| page_for_clear.confirm_clear());
        let page_for_json = activity_page.clone();
        export_json.connect_clicked(move |_| page_for_json.export("json"));
        let page_for_csv = activity_page.clone();
        export_csv.connect_clicked(move |_| page_for_csv.export("csv"));
        activity_page
    }

    fn export(&self, format: &'static str) {
        let Some(parent) = self.app.active_window() else {
            return;
        };
        let dialog = gtk::FileDialog::builder()
            .title("Export activity")
            .modal(true)
            .accept_label("Export")
            .initial_name(format!("toolport-activity.{format}"))
            .build();
        let page = self.clone();
        dialog.save(Some(&parent), gtk::gio::Cancellable::NONE, move |result| {
            let Ok(file) = result else {
                return;
            };
            let Some(path) = file.path() else {
                page.show_error("the selected destination does not have a local path");
                return;
            };
            page.feedback.set_label("Exporting retained activity…");
            let page = page.clone();
            gtk::glib::spawn_future_local(async move {
                let result = gtk::gio::spawn_blocking(move || {
                    crate::diagnostics_controller::export_audit(&path, format)
                })
                .await;
                match result {
                    Ok(Ok(())) => {
                        page.feedback.set_label(&format!(
                            "Exported retained activity as {}.",
                            format.to_uppercase()
                        ));
                        page.feedback.remove_css_class("error");
                        page.feedback.add_css_class("success");
                    }
                    Ok(Err(error)) => page.show_error(&error),
                    Err(_) => page.show_error("the export stopped unexpectedly"),
                }
            });
        });
    }

    fn refresh(&self) {
        self.refresh_with_feedback(true)
    }

    /// The 3-second background cadence: no "Loading" flash, and identical data
    /// is dropped without a re-render so open expanders stay open.
    fn refresh_quietly(&self) {
        self.refresh_with_feedback(false)
    }

    fn refresh_with_feedback(&self, announce: bool) {
        if self.loading.replace(true) {
            return;
        }
        self.refresh_button.set_sensitive(false);
        if announce {
            self.feedback.set_label("Loading retained activity…");
            self.feedback.remove_css_class("error");
            self.feedback.set_visible(true);
        }
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(state::load_activity_snapshot).await;
            page.loading.set(false);
            page.refresh_button.set_sensitive(true);
            match result {
                Ok(Ok(snapshot)) => {
                    if !announce && page.last_snapshot.borrow().as_ref() == Some(&snapshot) {
                        return;
                    }
                    page.render(snapshot)
                }
                Ok(Err(error)) => page.show_error(&error),
                Err(_) => page.show_error("the activity read stopped unexpectedly"),
            }
        });
    }

    fn render(&self, snapshot: state::ActivitySnapshot) {
        while let Some(child) = self.stats_list.first_child() {
            self.stats_list.remove(&child);
        }
        while let Some(child) = self.search_list.first_child() {
            self.search_list.remove(&child);
        }
        while let Some(child) = self.inspect_list.first_child() {
            self.inspect_list.remove(&child);
        }
        self.call_count.set_label(&snapshot.call_count.to_string());
        let success_rate = if snapshot.call_count == 0 {
            "–".to_string()
        } else {
            format!(
                "{}%",
                (snapshot.call_count - snapshot.error_count) * 100 / snapshot.call_count
            )
        };
        self.success_rate.set_label(&success_rate);
        self.average_latency.set_label(
            &snapshot
                .average_duration_ms
                .map(|duration| format!("{duration} ms"))
                .unwrap_or_else(|| "–".to_string()),
        );
        self.tokens_saved.set_label(&if snapshot.tokens_saved > 0 {
            state::format_token_count(snapshot.tokens_saved)
        } else {
            "–".to_string()
        });
        self.tokens_saved.set_tooltip_text(Some(
            "Tool-definition tokens lazy discovery has kept out of your agent's context",
        ));
        self.feedback.set_label(if snapshot.call_count == 0 {
            "No tool calls have been retained yet."
        } else {
            "Activity is stored only on this machine."
        });
        self.feedback.remove_css_class("error");
        self.feedback.add_css_class("success");
        self.clear_button.set_sensitive(
            snapshot.call_count > 0
                || !snapshot.search_traces.is_empty()
                || !snapshot.inspect_calls.is_empty(),
        );

        // Rebuild the server filter's choices, keeping the current selection when
        // that server still exists in the data.
        {
            self.updating_filters.set(true);
            let previous = self.selected_filter_server();
            let mut servers: Vec<&str> = Vec::new();
            for call in &snapshot.recent {
                if !servers.contains(&call.server.as_str()) {
                    servers.push(call.server.as_str());
                }
            }
            let mut options = vec!["All servers"];
            options.extend(servers.iter().copied());
            self.filter_server
                .set_model(Some(&gtk::StringList::new(&options)));
            let selected = previous
                .as_deref()
                .and_then(|previous| servers.iter().position(|server| *server == previous))
                .map(|index| index as u32 + 1)
                .unwrap_or(0);
            self.filter_server.set_selected(selected);
            self.updating_filters.set(false);
        }

        *self.last_snapshot.borrow_mut() = Some(snapshot);
        self.render_recent();
        self.render_identities();
        self.render_savings();
        self.render_security();
        let snapshot = self
            .last_snapshot
            .borrow()
            .clone()
            .expect("the snapshot was stored above");

        if snapshot.server_stats.is_empty() {
            self.stats_list.append(&empty_activity_label(
                "Per-server statistics appear once calls are retained.",
            ));
        } else {
            for stat in &snapshot.server_stats {
                self.stats_list.append(&server_stat_row(stat));
            }
        }
        if snapshot.search_traces.is_empty() {
            self.search_list.append(&empty_activity_label(
                "No lazy-discovery searches retained.",
            ));
        } else {
            for trace in snapshot.search_traces {
                self.search_list.append(&search_trace_card(&trace));
            }
        }
        if snapshot.inspect_calls.is_empty() {
            self.inspect_list.append(&empty_activity_label(
                "Live inspection is off or has not captured a call.",
            ));
        } else {
            for capture in snapshot.inspect_calls {
                self.inspect_list.append(&inspect_card(&capture));
            }
        }
    }

    fn render_security(&self) {
        let borrowed = self.last_snapshot.borrow();
        let Some(snapshot) = borrowed.as_ref() else {
            return;
        };
        while let Some(child) = self.security_list.first_child() {
            self.security_list.remove(&child);
        }
        if snapshot.security_events.is_empty() {
            self.security_list.append(&empty_activity_label(
                "No definition drift or injection events retained.",
            ));
            return;
        }
        let live: Vec<serde_json::Value> = {
            let dismissed = self.security_dismissed.borrow();
            crate::integrity::dedupe_security(&snapshot.security_events)
                .into_iter()
                .filter(|event| !dismissed.contains(&crate::integrity::security_key(event)))
                .collect()
        };
        if live.is_empty() {
            // The calm "you're protected" state: a protection the user never
            // sees builds no trust.
            self.security_list.append(&empty_activity_label(
                "Protection active. Toolport is watching tool definitions and results. Nothing needs your attention right now.",
            ));
            return;
        }
        // Loud lane: what genuinely needs a human. A first sighting of a new
        // tool is churn and goes quiet, even when annotated destructive.
        let (loud, quiet): (Vec<serde_json::Value>, Vec<serde_json::Value>) =
            live.into_iter().partition(|event| {
                crate::integrity::event_severity(event) == "high"
                    && !crate::integrity::security_event_is_new_tool(event)
            });
        for (event, count) in crate::integrity::collapse_security_by_identity(&loud)
            .into_iter()
            .take(10)
        {
            self.security_list
                .append(&security_notice_card(&event, count, self.clone()));
        }
        if !quiet.is_empty() {
            let collapsed = crate::integrity::collapse_security_by_identity(&quiet);
            let expander = gtk::Expander::new(Some(&format!(
                "Quiet drift history · {} benign {}",
                collapsed.len(),
                if collapsed.len() == 1 {
                    "change"
                } else {
                    "changes"
                }
            )));
            expander.add_css_class("toolport-card");
            let list = gtk::Box::new(gtk::Orientation::Vertical, 8);
            list.set_margin_top(8);
            let dismiss_all = gtk::Button::with_label("Dismiss all");
            dismiss_all.add_css_class("flat");
            dismiss_all.set_halign(gtk::Align::Start);
            let keys: Vec<String> = quiet.iter().map(crate::integrity::security_key).collect();
            let page_for_dismiss_all = self.clone();
            dismiss_all.connect_clicked(move |_| {
                page_for_dismiss_all.dismiss_security(keys.clone());
            });
            list.append(&dismiss_all);
            for (event, count) in collapsed {
                list.append(&security_notice_card(&event, count, self.clone()));
            }
            expander.set_child(Some(&list));
            self.security_list.append(&expander);
        }
    }

    /// Remember dismissed finding identities (deduped, capped, persisted) and
    /// re-render the security section.
    fn dismiss_security(&self, keys: Vec<String>) {
        {
            let mut dismissed = self.security_dismissed.borrow_mut();
            for key in keys {
                if !dismissed.contains(&key) {
                    dismissed.push(key);
                }
            }
            const MAX_DISMISSED: usize = 500;
            let overflow = dismissed.len().saturating_sub(MAX_DISMISSED);
            if overflow > 0 {
                dismissed.drain(..overflow);
            }
            save_security_dismissed(&dismissed);
        }
        self.render_security();
    }

    fn render_savings(&self) {
        let borrowed = self.last_snapshot.borrow();
        let Some(snapshot) = borrowed.as_ref() else {
            return;
        };
        if snapshot.tokens_saved == 0 {
            self.savings_banner.set_visible(false);
            return;
        }
        self.savings_banner.set_visible(true);
        self.savings_value.set_label(&format!(
            "≈ {}",
            state::format_token_count(snapshot.tokens_saved)
        ));
        self.savings_dollars.set_label(&savings_dollar_line(
            snapshot.tokens_saved,
            self.savings_model.selected() as usize,
        ));
        self.savings_detail.set_label(&savings_detail_line(
            snapshot.savings_list_loads,
            snapshot.savings_peak_catalog,
            savings_since_date(snapshot.savings_since_ts),
        ));
    }

    /// The server the filter dropdown currently points at, or `None` for all.
    fn selected_filter_server(&self) -> Option<String> {
        let selected = self.filter_server.selected();
        if selected == 0 {
            return None;
        }
        self.filter_server
            .model()
            .and_then(|model| model.item(selected))
            .and_downcast::<gtk::StringObject>()
            .map(|item| item.string().to_string())
    }

    fn render_recent(&self) {
        let borrowed = self.last_snapshot.borrow();
        let Some(snapshot) = borrowed.as_ref() else {
            return;
        };
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        if snapshot.recent.is_empty() {
            self.filter_count.set_label("");
            self.list.append(&state_card(
                "view-list-symbolic",
                "No activity yet",
                "Calls routed through Toolport will appear here with their outcome and timing.",
                false,
            ));
            return;
        }
        let server = self.selected_filter_server();
        let errors_only = self.errors_only.is_active();
        let filtered = filter_calls(&snapshot.recent, server.as_deref(), errors_only);
        self.filter_count
            .set_label(&if filtered.len() == snapshot.recent.len() {
                format!("{} calls", filtered.len())
            } else {
                format!("{} of {} calls", filtered.len(), snapshot.recent.len())
            });
        if filtered.is_empty() {
            self.list.append(&state_card(
                "edit-find-symbolic",
                "No matching calls",
                "No retained call matches the current filter.",
                false,
            ));
            return;
        }
        for activity in filtered {
            self.list.append(&activity_card(activity));
        }
    }

    fn render_identities(&self) {
        let borrowed = self.last_snapshot.borrow();
        let Some(snapshot) = borrowed.as_ref() else {
            return;
        };
        while let Some(child) = self.identity_list.first_child() {
            self.identity_list.remove(&child);
        }
        if snapshot.tool_identities.is_empty() {
            self.identity_list.append(&empty_activity_label(
                "No tool baselines pinned yet. Identities appear after a client lists tools through the gateway.",
            ));
            return;
        }
        let query = self.identity_search.text().to_lowercase();
        let filtered = filter_tool_identities(&snapshot.tool_identities, &query);
        if filtered.is_empty() {
            self.identity_list
                .append(&empty_activity_label("No pinned tool matches the filter."));
            return;
        }
        let groups = state::group_tool_identities(&filtered);
        if !query.is_empty() {
            self.identity_list.append(&empty_activity_label(&format!(
                "{} {} in {} {}",
                filtered.len(),
                if filtered.len() == 1 { "tool" } else { "tools" },
                groups.len(),
                if groups.len() == 1 {
                    "server"
                } else {
                    "servers"
                },
            )));
        }
        for (server, identities) in groups {
            // A search forces matches open so they are visible without a click.
            self.identity_list
                .append(&tool_identity_group(&server, identities, !query.is_empty()));
        }
    }

    fn show_error(&self, error: &str) {
        // A failed refresh over real rows must never read as "all clear": keep
        // the last loaded data on screen and say it is stale.
        if self.last_snapshot.borrow().is_some() {
            self.feedback.set_label(&format!(
                "Could not refresh; showing the last loaded activity. Retry with the refresh button. ({error})"
            ));
        } else {
            self.feedback
                .set_label(&format!("Could not load activity: {error}"));
        }
        self.feedback.remove_css_class("success");
        self.feedback.add_css_class("error");
    }

    fn confirm_clear(&self) {
        let Some(parent) = self.app.active_window() else {
            return;
        };
        #[allow(deprecated)]
        let dialog = adw::MessageDialog::new(
            Some(&parent),
            Some("Clear retained activity?"),
            Some("This permanently removes the local call audit, discovery traces, live inspector captures, and savings tally. Security drift history remains available for review."),
        );
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("clear", "Clear activity");
        dialog.set_close_response("cancel");
        dialog.set_default_response(Some("cancel"));
        dialog.set_response_appearance("clear", adw::ResponseAppearance::Destructive);
        let page = self.clone();
        dialog.connect_response(None, move |dialog, response| {
            if response == "clear" {
                page.clear();
            }
            dialog.close();
        });
        dialog.present();
    }

    fn clear(&self) {
        self.clear_button.set_sensitive(false);
        self.feedback.set_label("Clearing retained activity…");
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let result =
                gtk::gio::spawn_blocking(crate::observability_controller::clear_activity_logs)
                    .await;
            match result {
                Ok(Ok(())) => page.refresh(),
                Ok(Err(error)) => page.show_error(&format!("could not clear activity: {error}")),
                Err(_) => page.show_error("the clear operation stopped unexpectedly"),
            }
        });
    }
}

fn empty_activity_label(message: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(message)
        .halign(gtk::Align::Start)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["toolport-muted"])
        .build()
}

fn security_dismissed_path() -> Option<std::path::PathBuf> {
    Some(crate::registry::conduit_dir()?.join("gtk-security-dismissed.json"))
}

fn load_security_dismissed() -> Vec<String> {
    security_dismissed_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or_default()
}

fn save_security_dismissed(keys: &[String]) {
    let Some(path) = security_dismissed_path() else {
        return;
    };
    if let Ok(contents) = serde_json::to_string(keys) {
        let _ = std::fs::write(path, contents);
    }
}

/// One loud (or quiet-lane) security finding: badge, subject, recurrence count,
/// evidence excerpt, and a dismissal that is durable by finding identity.
fn security_notice_card(event: &serde_json::Value, count: usize, page: ActivityPage) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 4);
    card.add_css_class("toolport-card");
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let kind = event
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("security_event")
        .replace('_', " ");
    let badge = gtk::Label::new(Some(&kind));
    badge.add_css_class("toolport-badge");
    badge.add_css_class(if crate::integrity::event_severity(event) == "high" {
        "error"
    } else {
        "review"
    });
    row.append(&badge);
    let subject = event
        .get("tool")
        .or_else(|| event.get("server"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("integrity baseline");
    let mut title = subject.to_string();
    if let Some(signatures) = event
        .get("signatures")
        .and_then(serde_json::Value::as_array)
    {
        let names: Vec<&str> = signatures
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect();
        if !names.is_empty() {
            title.push_str(&format!(" ({})", names.join(", ")));
        }
    }
    row.append(
        &gtk::Label::builder()
            .label(title)
            .halign(gtk::Align::Start)
            .hexpand(true)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["heading", "monospace"])
            .build(),
    );
    if count > 1 {
        let recurrence = gtk::Label::new(Some(&format!("×{count}")));
        recurrence.add_css_class("toolport-badge");
        recurrence.add_css_class("review");
        recurrence.set_tooltip_text(Some(&format!("Recurred in {count} separate time windows")));
        row.append(&recurrence);
    }
    let timestamp = event
        .get("ts")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    row.append(
        &gtk::Label::builder()
            .label(format!(
                "{}{}",
                if count > 1 { "last " } else { "" },
                relative_activity_time(timestamp)
            ))
            .css_classes(["toolport-muted", "caption"])
            .build(),
    );
    let dismiss = gtk::Button::builder()
        .icon_name("window-close-symbolic")
        .tooltip_text("Dismiss this notice; it stays dismissed even if the same finding recurs")
        .css_classes(["flat"])
        .build();
    let key = crate::integrity::security_key(event);
    dismiss.connect_clicked(move |_| page.dismiss_security(vec![key.clone()]));
    row.append(&dismiss);
    card.append(&row);
    if let Some(evidence) = event
        .get("evidence")
        .and_then(serde_json::Value::as_str)
        .filter(|evidence| !evidence.is_empty())
    {
        card.append(
            &gtk::Label::builder()
                .label(format!("matched: {evidence}"))
                .halign(gtk::Align::Start)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["toolport-muted", "caption", "monospace"])
                .build(),
        );
    }
    card
}

/// The collapsed header line for one discovery trace.
fn trace_summary_line(trace: &serde_json::Value) -> String {
    let string = |key: &str| trace.get(key).and_then(serde_json::Value::as_str);
    let number = |key: &str| {
        trace
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    };
    let query = string("query").unwrap_or("(empty)");
    let returned = number("returned");
    let total = number("total");
    let fallbacks = trace
        .get("fallbacks")
        .and_then(serde_json::Value::as_u64)
        .or_else(|| {
            trace
                .get("ranking")
                .and_then(serde_json::Value::as_array)
                .map(|ranking| {
                    ranking
                        .iter()
                        .filter(|row| {
                            row.get("fallback")
                                .and_then(serde_json::Value::as_bool)
                                .unwrap_or(false)
                        })
                        .count() as u64
                })
        })
        .unwrap_or(0);
    let result = if fallbacks > 0 {
        format!("{total} direct + {fallbacks} fallback")
    } else if returned > 0 {
        format!("{returned} of {total}")
    } else {
        "no match".to_string()
    };
    let mut parts = vec![format!("\u{201c}{query}\u{201d}"), result];
    if let Some(client) = string("client").filter(|client| !client.is_empty()) {
        parts.push(client.to_string());
    }
    if string("mode") == Some("semantic") {
        parts.push("semantic".to_string());
    }
    if trace
        .get("escalated")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        parts.push("loop-broken".to_string());
    }
    parts.join(" · ")
}

/// One line per ranked candidate, or the flat name list when no ranking was
/// recorded; empty means no tools matched.
fn trace_ranking_lines(trace: &serde_json::Value) -> Vec<String> {
    let top = trace.get("top").and_then(serde_json::Value::as_str);
    if let Some(ranking) = trace.get("ranking").and_then(serde_json::Value::as_array) {
        if !ranking.is_empty() {
            return ranking
                .iter()
                .map(|row| {
                    let rank = row
                        .get("rank")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0);
                    let name = row
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("?");
                    let matched: Vec<&str> = row
                        .get("matched")
                        .and_then(serde_json::Value::as_array)
                        .map(|matched| {
                            matched
                                .iter()
                                .filter_map(serde_json::Value::as_str)
                                .collect()
                        })
                        .unwrap_or_default();
                    let why = if row
                        .get("pinned")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false)
                    {
                        "pinned prerequisite".to_string()
                    } else if row
                        .get("fallback")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false)
                    {
                        "fallback candidate".to_string()
                    } else if !matched.is_empty() {
                        format!("matched {}", matched.join(", "))
                    } else if trace.get("mode").and_then(serde_json::Value::as_str)
                        == Some("semantic")
                    {
                        "semantic match".to_string()
                    } else {
                        "-".to_string()
                    };
                    let marker = if Some(name) == top { " (top)" } else { "" };
                    format!("#{rank} {name}{marker} · {why}")
                })
                .collect();
        }
    }
    trace
        .get("names")
        .and_then(serde_json::Value::as_array)
        .map(|names| {
            names
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(|name| {
                    if Some(name) == top {
                        format!("{name} (top)")
                    } else {
                        name.to_string()
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The returned-vs-flat token math for one trace.
fn trace_token_line(trace: &serde_json::Value) -> String {
    let number = |key: &str| {
        trace
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    };
    let returned = number("returnedTokens");
    let flat = number("flatTokens");
    let saved = number("savedTokens");
    let mut line = format!(
        "Put ≈{} tokens of tool schemas into context, vs ≈{} to load the whole catalog",
        state::format_token_count(returned),
        state::format_token_count(flat)
    );
    if flat > 0 {
        let mut percent = saved.saturating_mul(100) / flat;
        if percent == 0 && saved > 0 {
            percent = 1;
        }
        line.push_str(&format!(" ({percent}% less this turn)."));
    } else {
        line.push('.');
    }
    line
}

fn search_trace_card(trace: &serde_json::Value) -> gtk::Expander {
    let expander = gtk::Expander::new(Some(&trace_summary_line(trace)));
    expander.add_css_class("toolport-card");
    let trace_for_expand = trace.clone();
    let pending = std::rc::Rc::new(std::cell::Cell::new(true));
    expander.connect_expanded_notify(move |expander| {
        if !expander.is_expanded() || !pending.replace(false) {
            return;
        }
        let list = gtk::Box::new(gtk::Orientation::Vertical, 4);
        list.set_margin_top(8);
        let lines = trace_ranking_lines(&trace_for_expand);
        if lines.is_empty() {
            list.append(&empty_activity_label("No tools matched this query."));
        }
        for line in lines {
            list.append(
                &gtk::Label::builder()
                    .label(line)
                    .halign(gtk::Align::Start)
                    .xalign(0.0)
                    .wrap(true)
                    .css_classes(["toolport-muted", "caption", "monospace"])
                    .build(),
            );
        }
        list.append(
            &gtk::Label::builder()
                .label(trace_token_line(&trace_for_expand))
                .halign(gtk::Align::Start)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["toolport-muted", "caption"])
                .build(),
        );
        expander.set_child(Some(&list));
    });
    expander
}

fn inspect_card(capture: &serde_json::Value) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
    card.add_css_class("toolport-card");
    let server = capture
        .get("server")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("Unknown server");
    let tool = capture
        .get("tool")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("Unknown tool");
    let ok = capture
        .get("ok")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let duration = capture
        .get("durationMs")
        .and_then(serde_json::Value::as_u64)
        .map(|duration| format!(" · {duration} ms"))
        .unwrap_or_default();
    card.append(
        &gtk::Label::builder()
            .label(format!("{server} / {tool}"))
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["heading"])
            .build(),
    );
    card.append(
        &gtk::Label::builder()
            .label(format!(
                "{}{} · captured locally while live inspection was enabled",
                if ok { "Succeeded" } else { "Failed" },
                duration
            ))
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-muted"])
            .build(),
    );
    let body = serde_json::to_string_pretty(&serde_json::json!({
        "request": capture.get("request").cloned().unwrap_or(serde_json::Value::Null),
        "response": capture.get("response").cloned().unwrap_or(serde_json::Value::Null),
    }))
    .unwrap_or_else(|_| "Could not format capture".to_string());
    let view = gtk::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(8)
        .bottom_margin(8)
        .left_margin(8)
        .right_margin(8)
        .build();
    view.buffer().set_text(&body);
    let scroll = gtk::ScrolledWindow::builder()
        .child(&view)
        .min_content_height(120)
        .max_content_height(280)
        .build();
    card.append(
        &gtk::Expander::builder()
            .label("Inspect request and response")
            .child(&scroll)
            .build(),
    );
    card
}

/// Models for the savings dollar estimate: input-token list prices ($/1M),
/// matching the shipping banner and the public calculator at toolport.app.
const SAVINGS_MODELS: &[(&str, f64)] = &[
    ("Claude Opus", 5.0),
    ("Claude Sonnet", 3.0),
    ("Claude Haiku", 1.0),
    ("GPT-5.6 Sol", 5.0),
    ("GPT-5.6 Terra", 2.5),
    ("GPT-5.6 Luna", 1.0),
    ("Gemini 3.1 Pro", 2.0),
    ("Gemini 3.5 Flash", 1.5),
    ("Gemini 3.1 Flash-Lite", 0.25),
];

fn savings_dollar_line(tokens_saved: u64, model_index: usize) -> String {
    let (label, price) = SAVINGS_MODELS
        .get(model_index)
        .copied()
        .unwrap_or(("Claude Sonnet", 3.0));
    let dollars = tokens_saved as f64 / 1_000_000.0 * price;
    format!("≈ ${dollars:.2} at {label} input prices")
}

fn savings_detail_line(list_loads: u64, peak_catalog: u64, since: Option<String>) -> String {
    let mut parts = vec![format!(
        "across {list_loads} tool-list {}",
        if list_loads == 1 { "load" } else { "loads" }
    )];
    if peak_catalog > 4 {
        parts.push(format!(
            "biggest catalog collapsed {peak_catalog} tools to a handful"
        ));
    }
    if let Some(since) = since {
        parts.push(format!("since {since}"));
    }
    parts.join(" · ")
}

fn savings_share_line(tokens_saved: u64) -> String {
    format!(
        "Toolport keeps ~{} tokens of MCP tool definitions out of my agent's context so far. \
         One local gateway for all my MCP servers: toolport.app",
        state::format_token_count(tokens_saved)
    )
}

/// "Mar 4"-style date for the savings detail line, or `None` for epoch 0.
fn savings_since_date(since_ts_ms: u64) -> Option<String> {
    if since_ts_ms == 0 {
        return None;
    }
    gtk::glib::DateTime::from_unix_local(since_ts_ms as i64 / 1000)
        .ok()?
        .format("%b %e")
        .ok()
        .map(|formatted| formatted.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// PII pseudonymization badge for one call, mirroring the shipping rules:
/// nothing when the pass didn't run or replaced zero values; a warning when the
/// pass ran but failed OPEN, because the honest reading of that row is "some of
/// this reached the model unredacted".
fn pii_badge(
    replaced: Option<u64>,
    incomplete: bool,
) -> Option<(String, &'static str, &'static str)> {
    let replaced = replaced?;
    if incomplete {
        return Some((
            format!("{replaced} pseudonymized, incomplete"),
            "review",
            "The pseudonymization pass did not fully apply - some values reached the model in the clear (the session map was full, or the result exceeded the scan cap).",
        ));
    }
    if replaced == 0 {
        return None;
    }
    Some((
        format!("{replaced} pseudonymized"),
        "success",
        "Values in this result were replaced with pseudonyms before the model saw them. The values themselves are never logged.",
    ))
}

fn activity_card(activity: &state::ActivityView) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    card.add_css_class("toolport-card");
    let icon = gtk::Image::from_icon_name(if activity.ok {
        "emblem-ok-symbolic"
    } else {
        "dialog-error-symbolic"
    });
    icon.add_css_class("toolport-card-icon");
    icon.set_valign(gtk::Align::Center);
    card.append(&icon);

    let copy = gtk::Box::new(gtk::Orientation::Vertical, 3);
    copy.set_hexpand(true);
    copy.append(
        &gtk::Label::builder()
            .label(format!("{} / {}", activity.server, activity.tool))
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["heading"])
            .build(),
    );
    let mut detail = Vec::new();
    if let Some(client) = activity.client.as_deref() {
        detail.push(client.to_string());
    }
    detail.push(relative_activity_time(activity.timestamp_ms));
    if let Some(duration) = activity.duration_ms {
        detail.push(format!("{duration} ms"));
    }
    copy.append(
        &gtk::Label::builder()
            .label(detail.join(" · "))
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-muted"])
            .build(),
    );
    if let Some(error) = activity.error.as_deref() {
        copy.append(
            &gtk::Label::builder()
                .label(error)
                .halign(gtk::Align::Start)
                .xalign(0.0)
                .single_line_mode(true)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(["caption", "toolport-activity-error"])
                .build(),
        );
    }
    card.append(&copy);
    if let Some((label, class, tooltip)) = pii_badge(activity.pii_replaced, activity.pii_incomplete)
    {
        let badge = gtk::Label::new(Some(&label));
        badge.add_css_class("toolport-badge");
        badge.add_css_class(class);
        badge.set_tooltip_text(Some(tooltip));
        card.append(&badge);
    }
    let (status, class) = if activity.held {
        ("Held", "review")
    } else if activity.ok {
        ("Succeeded", "success")
    } else {
        ("Failed", "disabled")
    };
    let badge = gtk::Label::new(Some(status));
    badge.add_css_class("toolport-badge");
    badge.add_css_class(class);
    badge.set_valign(gtk::Align::Center);
    card.append(&badge);
    card
}

/// One aggregation row's metrics, shared by the server line and its tool lines:
/// calls, errors, and latency where the log carried durations.
fn stat_metrics_line(stat: &serde_json::Value) -> String {
    let number = |key: &str| stat.get(key).and_then(serde_json::Value::as_u64);
    let calls = number("calls").unwrap_or(0);
    let errors = number("errors").unwrap_or(0);
    let mut parts = vec![
        format!("{calls} {}", if calls == 1 { "call" } else { "calls" }),
        format!("{errors} {}", if errors == 1 { "error" } else { "errors" }),
    ];
    if let Some(avg) = number("avgMs") {
        parts.push(format!("avg {avg} ms"));
    }
    if let Some(p95) = number("p95Ms") {
        parts.push(format!("p95 {p95} ms"));
    }
    parts.join(" · ")
}

/// One server's row in the per-server breakdown. Expanding it reveals the
/// per-tool breakdown, built on first expansion like the identity groups.
fn server_stat_row(stat: &serde_json::Value) -> gtk::Expander {
    let server = stat
        .get("server")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("Unknown server");
    let expander = gtk::Expander::new(Some(&format!("{server} · {}", stat_metrics_line(stat))));
    expander.add_css_class("toolport-card");
    let tools = stat
        .get("tools")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let pending = std::rc::Rc::new(std::cell::RefCell::new(Some(tools)));
    expander.connect_expanded_notify(move |expander| {
        if !expander.is_expanded() {
            return;
        }
        let Some(tools) = pending.borrow_mut().take() else {
            return;
        };
        let list = gtk::Box::new(gtk::Orientation::Vertical, 4);
        list.set_margin_top(8);
        if tools.is_empty() {
            list.append(&empty_activity_label("No per-tool rows retained."));
        }
        for tool in tools {
            let name = tool
                .get("tool")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("Unknown tool");
            list.append(
                &gtk::Label::builder()
                    .label(format!("{name} · {}", stat_metrics_line(&tool)))
                    .halign(gtk::Align::Start)
                    .xalign(0.0)
                    .wrap(true)
                    .css_classes(["toolport-muted", "caption"])
                    .build(),
            );
        }
        expander.set_child(Some(&list));
    });
    expander
}

/// The fingerprint as the provenance list shows it: version prefix stripped,
/// truncated to 12 characters, "-" when nothing is pinned.
fn short_fingerprint(fingerprint: &str) -> String {
    let stripped = match fingerprint.split_once(':') {
        Some((version, rest))
            if version.len() > 1
                && version.starts_with('v')
                && version[1..].chars().all(|c| c.is_ascii_digit()) =>
        {
            rest
        }
        _ => fingerprint,
    };
    let short: String = stripped.chars().take(12).collect();
    if short.is_empty() {
        "-".to_string()
    } else {
        short
    }
}

/// One server's pinned tools, collapsed by default so many servers read as a few
/// lines. The rows are built on first expansion: a real install pins thousands of
/// tools, and building every row up front would make the Activity page unusable.
/// Which of the recent calls the current filter keeps.
fn filter_calls<'a>(
    calls: &'a [state::ActivityView],
    server: Option<&str>,
    errors_only: bool,
) -> Vec<&'a state::ActivityView> {
    calls
        .iter()
        .filter(|call| server.is_none_or(|server| call.server == server))
        .filter(|call| !errors_only || !call.ok)
        .collect()
}

/// Pinned tools matching a lowercase search query, by alias, server, or
/// upstream name. An empty query keeps everything.
fn filter_tool_identities(
    identities: &[crate::integrity::ToolIdentity],
    query: &str,
) -> Vec<crate::integrity::ToolIdentity> {
    if query.is_empty() {
        return identities.to_vec();
    }
    identities
        .iter()
        .filter(|identity| {
            identity.alias.to_lowercase().contains(query)
                || identity.server_name.to_lowercase().contains(query)
                || identity.upstream.to_lowercase().contains(query)
        })
        .cloned()
        .collect()
}

fn tool_identity_group(
    server: &str,
    identities: Vec<crate::integrity::ToolIdentity>,
    expanded: bool,
) -> gtk::Expander {
    let count = identities.len();
    let quarantined = identities
        .iter()
        .filter(|identity| identity.quarantined)
        .count();
    let mut label = format!(
        "{server} · {count} {}",
        if count == 1 { "tool" } else { "tools" }
    );
    if quarantined > 0 {
        label.push_str(&format!(" · {quarantined} quarantined"));
    }
    let expander = gtk::Expander::new(Some(&label));
    expander.add_css_class("toolport-card");
    let pending = std::rc::Rc::new(std::cell::RefCell::new(Some(identities)));
    expander.connect_expanded_notify(move |expander| {
        if !expander.is_expanded() {
            return;
        }
        let Some(identities) = pending.borrow_mut().take() else {
            return;
        };
        let list = gtk::Box::new(gtk::Orientation::Vertical, 8);
        list.set_margin_top(8);
        for identity in identities {
            list.append(&tool_identity_row(&identity));
        }
        expander.set_child(Some(&list));
    });
    if expanded {
        expander.set_expanded(true);
    }
    expander
}

fn tool_identity_row(identity: &crate::integrity::ToolIdentity) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 3);
    let title = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let alias = gtk::Label::builder()
        .label(&identity.alias)
        .halign(gtk::Align::Start)
        .hexpand(true)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["heading", "monospace"])
        .build();
    title.append(&alias);
    if identity.quarantined {
        let badge = gtk::Label::new(Some("Quarantined"));
        badge.add_css_class("toolport-badge");
        badge.add_css_class("review");
        title.append(&badge);
    }
    row.append(&title);
    let mut detail = vec![
        if identity.upstream.is_empty() {
            "unattributed alias".to_string()
        } else {
            format!("upstream {}", identity.upstream)
        },
        short_fingerprint(&identity.fingerprint),
    ];
    if identity.last_changed > 0 {
        detail.push(format!(
            "changed {}",
            relative_activity_time(identity.last_changed)
        ));
    }
    if !identity.profiles.is_empty() {
        detail.push(format!("profiles {}", identity.profiles.join(", ")));
    }
    row.append(
        &gtk::Label::builder()
            .label(detail.join(" · "))
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-muted", "caption"])
            .build(),
    );
    row
}

fn relative_activity_time(timestamp_ms: u64) -> String {
    if timestamp_ms == 0 {
        return "Unknown time".to_string();
    }
    let age_ms = epoch_ms().saturating_sub(timestamp_ms);
    let seconds = age_ms / 1000;
    if seconds < 60 {
        "Just now".to_string()
    } else if seconds < 3_600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h ago", seconds / 3_600)
    } else {
        format!("{}d ago", seconds / 86_400)
    }
}

#[derive(Clone)]
struct RulesPage {
    app: adw::Application,
    root: gtk::Box,
    feedback: gtk::Label,
    set_list: gtk::Box,
    client_list: gtk::Box,
    project_list: gtk::Box,
    set_count: gtk::Label,
    enabled_count: gtk::Label,
    project_count: gtk::Label,
    refresh_button: gtk::Button,
    reapply_button: gtk::Button,
    loading: std::rc::Rc<std::cell::Cell<bool>>,
}

impl RulesPage {
    fn new(app: &adw::Application) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("toolport-content");
        let header = adw::HeaderBar::new();
        header.add_css_class("toolport-header");
        header.set_show_back_button(true);
        header.set_title_widget(Some(
            &gtk::Label::builder()
                .label("Rules")
                .css_classes(["title"])
                .build(),
        ));
        let add = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text("Create rule set")
            .css_classes(["suggested-action"])
            .build();
        let add_project = gtk::Button::builder()
            .icon_name("folder-new-symbolic")
            .tooltip_text("Add project folder")
            .build();
        let import = gtk::Button::builder()
            .icon_name("document-open-symbolic")
            .tooltip_text("Start a rule set from an existing file")
            .build();
        let refresh_button = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Refresh rule status")
            .build();
        let reapply_button = gtk::Button::builder()
            .icon_name("emblem-synchronizing-symbolic")
            .tooltip_text(
                "Re-apply the active set to every switched-on client, including a file edited on disk",
            )
            .sensitive(false)
            .build();
        header.pack_end(&add);
        header.pack_end(&add_project);
        header.pack_end(&import);
        header.pack_end(&reapply_button);
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
                .label("One set of instructions across your AI clients")
                .halign(gtk::Align::Start)
                .wrap(true)
                .css_classes(["title-2"])
                .build(),
        );
        page.append(
            &gtk::Label::builder()
                .label("Choose an active rule set, then opt in each client. Toolport previews the exact file before the first write and removes only content it owns.")
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
        feedback.set_label("Open Rules to scan installed clients.");
        page.append(&feedback);

        let summary = gtk::FlowBox::new();
        summary.add_css_class("toolport-summary");
        summary.set_column_spacing(10);
        summary.set_row_spacing(10);
        summary.set_min_children_per_line(1);
        summary.set_max_children_per_line(3);
        summary.set_homogeneous(true);
        summary.set_selection_mode(gtk::SelectionMode::None);
        let mut values = Vec::new();
        for (value, label) in [
            ("–", "Rule sets"),
            ("–", "Clients enabled"),
            ("–", "Projects"),
        ] {
            let (item, value) = summary_item(value, label);
            values.push(value);
            summary.insert(&item, -1);
        }
        page.append(&summary);
        page.append(
            &gtk::Label::builder()
                .label("Rule sets")
                .halign(gtk::Align::Start)
                .css_classes(["heading"])
                .build(),
        );
        let set_list = gtk::Box::new(gtk::Orientation::Vertical, 10);
        page.append(&set_list);
        page.append(
            &gtk::Label::builder()
                .label("Client coverage")
                .halign(gtk::Align::Start)
                .css_classes(["heading"])
                .build(),
        );
        let client_list = gtk::Box::new(gtk::Orientation::Vertical, 10);
        page.append(&client_list);
        page.append(
            &gtk::Label::builder()
                .label("Project rules")
                .halign(gtk::Align::Start)
                .css_classes(["heading"])
                .build(),
        );
        let project_list = gtk::Box::new(gtk::Orientation::Vertical, 10);
        page.append(&project_list);
        scroller.set_child(Some(&page));
        root.append(&scroller);

        let rules_page = Self {
            app: app.clone(),
            root,
            feedback,
            set_list,
            client_list,
            project_list,
            set_count: values.remove(0),
            enabled_count: values.remove(0),
            project_count: values.remove(0),
            refresh_button,
            reapply_button,
            loading: std::rc::Rc::new(std::cell::Cell::new(false)),
        };
        let page_for_add = rules_page.clone();
        add.connect_clicked(move |_| open_rule_set_editor(None, page_for_add.clone()));
        let page_for_project = rules_page.clone();
        add_project.connect_clicked(move |_| choose_rules_project_folder(page_for_project.clone()));
        let page_for_import = rules_page.clone();
        import.connect_clicked(move |_| open_rules_import_picker(page_for_import.clone()));
        let page_for_refresh = rules_page.clone();
        rules_page
            .refresh_button
            .connect_clicked(move |_| page_for_refresh.refresh());
        let page_for_reapply = rules_page.clone();
        rules_page
            .reapply_button
            .connect_clicked(move |_| confirm_reapply_rules(page_for_reapply.clone()));
        rules_page
    }

    fn refresh(&self) {
        if self.loading.replace(true) {
            return;
        }
        self.refresh_button.set_sensitive(false);
        self.feedback.set_label("Scanning rule files…");
        self.feedback.remove_css_class("error");
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(crate::rules::view).await;
            page.loading.set(false);
            page.refresh_button.set_sensitive(true);
            match result {
                Ok(Ok(view)) => page.render(view),
                Ok(Err(error)) => page.show_error(&error),
                Err(_) => page.show_error("the rules scan stopped unexpectedly"),
            }
        });
    }

    fn render(&self, view: crate::rules::RulesView) {
        while let Some(child) = self.set_list.first_child() {
            self.set_list.remove(&child);
        }
        while let Some(child) = self.client_list.first_child() {
            self.client_list.remove(&child);
        }
        while let Some(child) = self.project_list.first_child() {
            self.project_list.remove(&child);
        }
        self.set_count.set_label(&view.sets.len().to_string());
        self.enabled_count.set_label(
            &view
                .clients
                .iter()
                .filter(|client| client.enabled)
                .count()
                .to_string(),
        );
        self.project_count
            .set_label(&view.projects.len().to_string());
        self.feedback
            .set_label(match view.active_set_id.as_deref() {
                Some(_) => "The active set is reconciled with every opted-in client.",
                None => "Choose an active set before enabling a client.",
            });
        self.feedback.remove_css_class("error");
        self.feedback.add_css_class("success");

        if view.sets.is_empty() {
            self.set_list.append(&state_card(
                "security-high-symbolic",
                "No rule sets yet",
                "Create a set for the instructions you want every opted-in AI client to follow.",
                false,
            ));
        } else {
            for set in &view.sets {
                self.set_list.append(&rule_set_card(
                    set,
                    view.active_set_id.as_deref() == Some(set.id.as_str()),
                    self.clone(),
                ));
            }
        }
        let active_set = view
            .active_set_id
            .as_deref()
            .and_then(|id| view.sets.iter().find(|set| set.id == id))
            .cloned();
        self.reapply_button.set_sensitive(active_set.is_some());
        let clients = view
            .clients
            .iter()
            .filter(|client| client.path.is_some())
            .collect::<Vec<_>>();
        if clients.is_empty() {
            self.client_list.append(&state_card(
                "computer-symbolic",
                "No supported clients installed",
                "Installed clients with a rules location will appear here.",
                false,
            ));
        } else {
            for client in clients {
                self.client_list.append(&rules_client_card(
                    client,
                    active_set.as_ref(),
                    self.clone(),
                ));
            }
        }
        // Clients Toolport cannot write global rules for are said out loud, not
        // silently dropped: the user can still paste the set in by hand.
        for client in view.clients.iter().filter(|client| client.path.is_none()) {
            self.client_list.append(
                &gtk::Label::builder()
                    .label(format!(
                        "No rules file Toolport can write for {}. Paste your rules in by hand.",
                        client.name
                    ))
                    .halign(gtk::Align::Start)
                    .xalign(0.0)
                    .wrap(true)
                    .css_classes(["toolport-muted", "caption"])
                    .build(),
            );
        }
        if view.projects.is_empty() {
            self.project_list.append(&state_card(
                "folder-symbolic",
                "No project folders registered",
                "Add a folder, choose a rule set and exact files, then review and apply explicitly.",
                false,
            ));
        } else {
            for project in &view.projects {
                self.project_list
                    .append(&rules_project_card(project, &view.sets, self.clone()));
            }
        }
    }

    fn show_error(&self, error: &str) {
        self.feedback.set_label(&format!("Rules error: {error}"));
        self.feedback.remove_css_class("success");
        self.feedback.add_css_class("error");
    }

    fn finish_mutation(&self, result: Result<crate::rules::RulesView, String>, success: &str) {
        match result {
            Ok(view) => {
                self.render(view);
                self.feedback.set_label(success);
            }
            Err(error) => self.show_error(&error),
        }
    }
}

fn rule_set_card(set: &crate::registry::RuleSet, active: bool, page: RulesPage) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    card.add_css_class("toolport-card");
    let icon = gtk::Image::from_icon_name("security-high-symbolic");
    icon.add_css_class("toolport-card-icon");
    card.append(&icon);
    let copy = gtk::Box::new(gtk::Orientation::Vertical, 3);
    copy.set_hexpand(true);
    copy.append(
        &gtk::Label::builder()
            .label(&set.name)
            .halign(gtk::Align::Start)
            .css_classes(["heading"])
            .build(),
    );
    copy.append(
        &gtk::Label::builder()
            .label(format!(
                "{} lines · revision {}",
                set.content.lines().count(),
                set.revision
            ))
            .halign(gtk::Align::Start)
            .css_classes(["toolport-muted"])
            .build(),
    );
    card.append(&copy);
    let select = gtk::Switch::builder()
        .active(active)
        .valign(gtk::Align::Center)
        .tooltip_text(if active {
            "Clear active rule set"
        } else {
            "Make this the active rule set"
        })
        .build();
    let set_id = set.id.clone();
    let set_name = set.name.clone();
    let page_for_select = page.clone();
    select.connect_state_set(move |switch, enabled| {
        switch.set_sensitive(false);
        let set_id = set_id.clone();
        let set_name = set_name.clone();
        let page = page_for_select.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                crate::rules::set_active(enabled.then_some(set_id.as_str()))
            })
            .await;
            match result {
                Ok(result) => {
                    let success = if enabled {
                        format!("Activated {set_name}")
                    } else {
                        "Cleared the active rule set".to_string()
                    };
                    page.finish_mutation(result, &success);
                }
                Err(_) => page.show_error("the active-set change stopped unexpectedly"),
            }
        });
        gtk::glib::Propagation::Stop
    });
    card.append(&select);
    let edit = gtk::Button::builder()
        .icon_name("document-edit-symbolic")
        .tooltip_text(format!("Edit {}", set.name))
        .css_classes(["flat"])
        .build();
    let set = set.clone();
    edit.connect_clicked(move |_| open_rule_set_editor(Some(set.clone()), page.clone()));
    card.append(&edit);
    card
}

fn rules_client_card(
    client: &crate::rules::ClientStatus,
    active_set: Option<&crate::registry::RuleSet>,
    page: RulesPage,
) -> gtk::Box {
    let has_active_set = active_set.is_some();
    let card = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    card.add_css_class("toolport-card");
    let icon = gtk::Image::from_icon_name("computer-symbolic");
    icon.add_css_class("toolport-card-icon");
    card.append(&icon);
    let copy = gtk::Box::new(gtk::Orientation::Vertical, 3);
    copy.set_hexpand(true);
    copy.append(
        &gtk::Label::builder()
            .label(&client.name)
            .halign(gtk::Align::Start)
            .css_classes(["heading"])
            .build(),
    );
    let (state_label, badge_class) = rule_apply_state(client.state);
    copy.append(
        &gtk::Label::builder()
            .label(if client.enabled {
                state_label
            } else {
                "Not enabled"
            })
            .halign(gtk::Align::Start)
            .css_classes(["toolport-muted"])
            .build(),
    );
    card.append(&copy);
    if client.enabled {
        let badge = gtk::Label::new(Some(state_label));
        badge.add_css_class("toolport-badge");
        badge.add_css_class(badge_class);
        card.append(&badge);
    }
    let preview = gtk::Button::builder()
        .icon_name("document-properties-symbolic")
        .tooltip_text(format!(
            "Preview the exact rules file for {}, without changing anything",
            client.name
        ))
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    {
        let client_id = client.id.clone();
        let client_name = client.name.clone();
        let page = page.clone();
        preview.connect_clicked(move |_| {
            preview_rules_client(&client_id, &client_name, None, page.clone());
        });
    }
    card.append(&preview);
    if client.enabled
        && client.state == crate::instructions::ApplyState::Drifted
        && client.on_disk.is_some()
    {
        let review = gtk::Button::with_label("Review");
        review.add_css_class("flat");
        review.set_valign(gtk::Align::Center);
        review.set_tooltip_text(Some(
            "See the edited file, pull it into the set, or rewrite it from the set",
        ));
        let client_for_review = client.clone();
        let active_for_review = active_set.cloned();
        let page_for_review = page.clone();
        review.connect_clicked(move |_| {
            open_rules_drift_review(
                client_for_review.clone(),
                active_for_review.clone(),
                page_for_review.clone(),
            )
        });
        card.append(&review);
    }
    let toggle = gtk::Switch::builder()
        .active(client.enabled)
        .sensitive(has_active_set || client.enabled)
        .valign(gtk::Align::Center)
        .tooltip_text(if client.enabled {
            "Remove Toolport's rules from this client"
        } else {
            "Preview and enable rules for this client"
        })
        .build();
    let client_id = client.id.clone();
    let client_name = client.name.clone();
    let page_for_toggle = page.clone();
    toggle.connect_state_set(move |toggle, enabled| {
        toggle.set_sensitive(false);
        if enabled {
            preview_rules_client(
                &client_id,
                &client_name,
                Some(toggle.clone()),
                page_for_toggle.clone(),
            );
        } else {
            let client_id = client_id.clone();
            let client_name = client_name.clone();
            let page = page_for_toggle.clone();
            gtk::glib::spawn_future_local(async move {
                let result = gtk::gio::spawn_blocking(move || {
                    crate::rules::set_client_enabled(&client_id, false)
                })
                .await;
                match result {
                    Ok(result) => {
                        page.finish_mutation(result, &format!("Disabled rules for {client_name}"))
                    }
                    Err(_) => page.show_error("the client rule update stopped unexpectedly"),
                }
            });
        }
        gtk::glib::Propagation::Stop
    });
    card.append(&toggle);
    card
}

fn choose_rules_project_folder(page: RulesPage) {
    let Some(parent) = page.app.active_window() else {
        return;
    };
    let dialog = gtk::FileDialog::builder()
        .title("Add project folder")
        .modal(true)
        .accept_label("Add project")
        .build();
    dialog.select_folder(Some(&parent), gtk::gio::Cancellable::NONE, move |result| {
        let Ok(folder) = result else {
            return;
        };
        let Some(path) = folder.path() else {
            page.show_error("the selected folder does not have a local path");
            return;
        };
        let path = path.to_string_lossy().to_string();
        let page = page.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || crate::rules::project_add(&path)).await;
            match result {
                Ok(result) => page.finish_mutation(result, "Added project folder"),
                Err(_) => page.show_error("the project registration stopped unexpectedly"),
            }
        });
    });
}

fn open_rules_import_picker(page: RulesPage) {
    page.feedback.set_label("Looking for existing rule files…");
    gtk::glib::spawn_future_local(async move {
        let result = gtk::gio::spawn_blocking(crate::rules::import_candidates).await;
        match result {
            Ok(candidates) => show_rules_import_candidates(candidates, page),
            Err(_) => page.show_error("the rule-file scan stopped unexpectedly"),
        }
    });
}

fn show_rules_import_candidates(candidates: Vec<crate::rules::ImportCandidate>, page: RulesPage) {
    let Some(parent) = page.app.active_window() else {
        return;
    };
    let window = adw::Window::builder()
        .application(&page.app)
        .transient_for(&parent)
        .modal(true)
        .title("Start from an existing file")
        .default_width(660)
        .default_height(620)
        .build();
    window.add_css_class("toolport-editor");
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = adw::HeaderBar::new();
    let cancel = gtk::Button::with_label("Cancel");
    cancel.add_css_class("toolport-secondary-action");
    header.pack_start(&cancel);
    root.append(&header);
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.add_css_class("toolport-editor-body");
    body.append(&editor_intro(
        "document-open-symbolic",
        "Import only after review",
        "Choose a local rules file. Toolport removes its own marker blocks, then opens the remaining text in an unsaved editor.",
    ));
    if candidates.is_empty() {
        body.append(&state_card(
            "document-open-symbolic",
            "No existing rule files found",
            "Supported installed clients do not currently have a non-empty rules file to import.",
            false,
        ));
    } else {
        for candidate in candidates {
            let card = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            card.add_css_class("toolport-card");
            let icon = gtk::Image::from_icon_name("text-x-generic-symbolic");
            icon.add_css_class("toolport-card-icon");
            card.append(&icon);
            let copy = gtk::Box::new(gtk::Orientation::Vertical, 3);
            copy.set_hexpand(true);
            copy.append(
                &gtk::Label::builder()
                    .label(&candidate.client_name)
                    .halign(gtk::Align::Start)
                    .css_classes(["heading"])
                    .build(),
            );
            copy.append(
                &gtk::Label::builder()
                    .label(format!("{} · {} bytes", candidate.path, candidate.bytes))
                    .halign(gtk::Align::Start)
                    .xalign(0.0)
                    .ellipsize(gtk::pango::EllipsizeMode::Middle)
                    .tooltip_text(&candidate.path)
                    .css_classes(["toolport-muted"])
                    .build(),
            );
            card.append(&copy);
            let choose = gtk::Button::with_label("Review");
            choose.add_css_class("suggested-action");
            let path = candidate.path;
            let client_name = candidate.client_name;
            let window_for_choose = window.clone();
            let page_for_choose = page.clone();
            choose.connect_clicked(move |button| {
                button.set_sensitive(false);
                let path = path.clone();
                let client_name = client_name.clone();
                let window = window_for_choose.clone();
                let page = page_for_choose.clone();
                let button = button.clone();
                gtk::glib::spawn_future_local(async move {
                    let result = gtk::gio::spawn_blocking(move || {
                        crate::rules::import_file(&path, Some(&client_name))
                    })
                    .await;
                    match result {
                        Ok(Ok(imported)) => {
                            window.close();
                            open_rule_set_editor_draft(
                                None,
                                Some((imported.name, imported.content)),
                                page,
                            );
                        }
                        Ok(Err(error)) => {
                            button.set_sensitive(true);
                            page.show_error(&error);
                        }
                        Err(_) => {
                            button.set_sensitive(true);
                            page.show_error("the rule-file import stopped unexpectedly");
                        }
                    }
                });
            });
            card.append(&choose);
            body.append(&card);
        }
    }
    scroller.set_child(Some(&body));
    root.append(&scroller);
    window.set_content(Some(&root));
    let window_for_cancel = window.clone();
    cancel.connect_clicked(move |_| window_for_cancel.close());
    window.present();
}

fn rules_project_card(
    project: &crate::rules::ProjectStatus,
    sets: &[crate::registry::RuleSet],
    page: RulesPage,
) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 12);
    card.add_css_class("toolport-card");
    let heading = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let icon = gtk::Image::from_icon_name("folder-symbolic");
    icon.add_css_class("toolport-card-icon");
    heading.append(&icon);
    let copy = gtk::Box::new(gtk::Orientation::Vertical, 3);
    copy.set_hexpand(true);
    copy.append(
        &gtk::Label::builder()
            .label(&project.name)
            .halign(gtk::Align::Start)
            .css_classes(["heading"])
            .build(),
    );
    copy.append(
        &gtk::Label::builder()
            .label(&project.path)
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .tooltip_text(&project.path)
            .css_classes(["toolport-muted"])
            .build(),
    );
    heading.append(&copy);
    let remove = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text(format!("Remove {}", project.name))
        .css_classes(["flat"])
        .build();
    remove.add_css_class("destructive-action");
    let project_id = project.id.clone();
    let project_name = project.name.clone();
    let page_for_remove = page.clone();
    remove.connect_clicked(move |_| {
        confirm_remove_rules_project(&project_id, &project_name, page_for_remove.clone())
    });
    heading.append(&remove);
    card.append(&heading);

    let mut set_names = vec!["No rule set".to_string()];
    set_names.extend(sets.iter().map(|set| set.name.clone()));
    let set_name_refs = set_names.iter().map(String::as_str).collect::<Vec<_>>();
    let set_picker = gtk::DropDown::from_strings(&set_name_refs);
    set_picker.add_css_class("toolport-input");
    let selected = project
        .set_id
        .as_deref()
        .and_then(|id| sets.iter().position(|set| set.id == id))
        .map(|index| index as u32 + 1)
        .unwrap_or(0);
    set_picker.set_selected(selected);
    let picker_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    picker_row.append(
        &gtk::Label::builder()
            .label("Rule set")
            .halign(gtk::Align::Start)
            .hexpand(true)
            .css_classes(["toolport-muted"])
            .build(),
    );
    picker_row.append(&set_picker);
    card.append(&picker_row);
    let project_id = project.id.clone();
    let sets_for_picker = sets.to_vec();
    let page_for_picker = page.clone();
    set_picker.connect_selected_notify(move |picker| {
        picker.set_sensitive(false);
        let set_id = picker
            .selected()
            .checked_sub(1)
            .and_then(|index| sets_for_picker.get(index as usize))
            .map(|set| set.id.clone());
        let project_id = project_id.clone();
        let page = page_for_picker.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                crate::rules::project_set_set(&project_id, set_id.as_deref())
            })
            .await;
            match result {
                Ok(result) => page.finish_mutation(result, "Updated project rule set"),
                Err(_) => page.show_error("the project rule-set update stopped unexpectedly"),
            }
        });
    });

    let files = gtk::Box::new(gtk::Orientation::Vertical, 0);
    files.add_css_class("toolport-project-files");
    for file in &project.files {
        files.append(&rules_project_file_row(project, file, page.clone()));
    }
    card.append(&files);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    let apply = gtk::Button::with_label("Review and apply");
    apply.add_css_class("suggested-action");
    apply.set_sensitive(project.set_id.is_some() && project.files.iter().any(|file| file.enabled));
    let project = project.clone();
    apply.connect_clicked(move |_| confirm_apply_rules_project(&project, page.clone()));
    actions.append(&apply);
    card.append(&actions);
    card
}

fn rules_project_file_row(
    project: &crate::rules::ProjectStatus,
    file: &crate::rules::ProjectFileStatus,
    page: RulesPage,
) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    row.add_css_class("toolport-project-file-row");
    let copy = gtk::Box::new(gtk::Orientation::Vertical, 2);
    copy.set_hexpand(true);
    copy.append(
        &gtk::Label::builder()
            .label(&file.rel_path)
            .halign(gtk::Align::Start)
            .css_classes(["heading"])
            .build(),
    );
    let clients = if file.clients.is_empty() {
        "No detected clients".to_string()
    } else {
        format!("Read by {}", file.clients.join(", "))
    };
    copy.append(
        &gtk::Label::builder()
            .label(clients)
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-muted"])
            .build(),
    );
    row.append(&copy);
    if file.enabled {
        let (label, class) = rule_apply_state(file.state);
        let badge = gtk::Label::new(Some(label));
        badge.add_css_class("toolport-badge");
        badge.add_css_class(class);
        badge.set_valign(gtk::Align::Center);
        row.append(&badge);
    }
    let preview = gtk::Button::builder()
        .icon_name("document-properties-symbolic")
        .tooltip_text(format!("Preview {}", file.rel_path))
        .css_classes(["flat"])
        .sensitive(project.set_id.is_some())
        .build();
    let project_id = project.id.clone();
    let file_key = file.key.clone();
    let page_for_preview = page.clone();
    preview.connect_clicked(move |button| {
        preview_rules_project_file(
            &project_id,
            &file_key,
            button.clone(),
            page_for_preview.clone(),
        )
    });
    row.append(&preview);
    let toggle = gtk::Switch::builder()
        .active(file.enabled)
        .valign(gtk::Align::Center)
        .sensitive(project.set_id.is_some() || file.enabled)
        .tooltip_text(if file.enabled {
            "Remove Toolport's rules from this project file"
        } else {
            "Include this file on the next explicit apply"
        })
        .build();
    let project_id = project.id.clone();
    let file_key = file.key.clone();
    let file_name = file.rel_path.clone();
    toggle.connect_state_set(move |toggle, enabled| {
        toggle.set_sensitive(false);
        let project_id = project_id.clone();
        let file_key = file_key.clone();
        let file_name = file_name.clone();
        let page = page.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                crate::rules::project_set_file_enabled(&project_id, &file_key, enabled)
            })
            .await;
            match result {
                Ok(result) => page.finish_mutation(
                    result,
                    &format!(
                        "{} {file_name}",
                        if enabled { "Selected" } else { "Disabled" }
                    ),
                ),
                Err(_) => page.show_error("the project file update stopped unexpectedly"),
            }
        });
        gtk::glib::Propagation::Stop
    });
    row.append(&toggle);
    row
}

fn preview_rules_project_file(
    project_id: &str,
    file_key: &str,
    button: gtk::Button,
    page: RulesPage,
) {
    button.set_sensitive(false);
    let project_id = project_id.to_string();
    let file_key = file_key.to_string();
    gtk::glib::spawn_future_local(async move {
        let result =
            gtk::gio::spawn_blocking(move || crate::rules::project_preview(&project_id, &file_key))
                .await;
        button.set_sensitive(true);
        match result {
            Ok(Ok(Some(preview))) => open_rules_project_preview(preview, page),
            Ok(Ok(None)) => page.show_error("pick a rule set for this project first"),
            Ok(Err(error)) => page.show_error(&error),
            Err(_) => page.show_error("the project preview stopped unexpectedly"),
        }
    });
}

fn open_rules_project_preview(preview: crate::rules::RulesPreview, page: RulesPage) {
    let Some(parent) = page.app.active_window() else {
        return;
    };
    let window = adw::Window::builder()
        .application(&page.app)
        .transient_for(&parent)
        .modal(true)
        .title("Project rule preview")
        .default_width(700)
        .default_height(620)
        .build();
    window.add_css_class("toolport-editor");
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = adw::HeaderBar::new();
    let done = gtk::Button::with_label("Done");
    done.add_css_class("toolport-secondary-action");
    header.pack_end(&done);
    root.append(&header);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.add_css_class("toolport-editor-body");
    body.append(&editor_intro(
        "document-properties-symbolic",
        "Exact project file after apply",
        &preview.path,
    ));
    let text = gtk::TextView::new();
    text.set_editable(false);
    text.set_cursor_visible(false);
    text.set_monospace(true);
    text.set_wrap_mode(gtk::WrapMode::WordChar);
    text.set_top_margin(10);
    text.set_bottom_margin(10);
    text.set_left_margin(10);
    text.set_right_margin(10);
    text.buffer().set_text(&preview.after);
    let scroller = gtk::ScrolledWindow::builder()
        .child(&text)
        .vexpand(true)
        .min_content_height(360)
        .build();
    scroller.add_css_class("toolport-text-area");
    body.append(&scroller);
    root.append(&body);
    window.set_content(Some(&root));
    let window_for_done = window.clone();
    done.connect_clicked(move |_| window_for_done.close());
    window.present();
}

fn confirm_reapply_rules(page: RulesPage) {
    let Some(parent) = page.app.active_window() else {
        return;
    };
    #[allow(deprecated)]
    let dialog = adw::MessageDialog::new(
        Some(&parent),
        Some("Re-apply rules to every switched-on client?"),
        Some(
            "Toolport rewrites each opted-in client's rules block from the active set. \
             A block you edited outside Toolport is overwritten too.",
        ),
    );
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("reapply", "Re-apply");
    dialog.set_close_response("cancel");
    dialog.set_default_response(Some("cancel"));
    dialog.set_response_appearance("reapply", adw::ResponseAppearance::Destructive);
    dialog.connect_response(None, move |dialog, response| {
        if response == "reapply" {
            let page = page.clone();
            gtk::glib::spawn_future_local(async move {
                let result = gtk::gio::spawn_blocking(crate::rules::apply_overwriting_drift).await;
                match result {
                    Ok(result) => page.finish_mutation(
                        result,
                        "Re-applied the active set to every switched-on client",
                    ),
                    Err(_) => page.show_error("the re-apply stopped unexpectedly"),
                }
            });
        }
        dialog.close();
    });
    dialog.present();
}

fn open_rules_drift_review(
    client: crate::rules::ClientStatus,
    active_set: Option<crate::registry::RuleSet>,
    page: RulesPage,
) {
    let Some(parent) = page.app.active_window() else {
        return;
    };
    let Some(on_disk) = client.on_disk.clone() else {
        return;
    };
    let window = adw::Window::builder()
        .application(&page.app)
        .transient_for(&parent)
        .modal(true)
        .title("Review edited rules")
        .default_width(700)
        .default_height(620)
        .build();
    window.add_css_class("toolport-editor");
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = adw::HeaderBar::new();
    let done = gtk::Button::with_label("Done");
    done.add_css_class("toolport-secondary-action");
    header.pack_end(&done);
    root.append(&header);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.add_css_class("toolport-editor-body");
    body.append(&editor_intro(
        "dialog-warning-symbolic",
        &format!("{} was edited outside Toolport", client.name),
        client
            .path
            .as_deref()
            .unwrap_or("This is the rules block as it is on disk right now."),
    ));
    let text = gtk::TextView::new();
    text.set_editable(false);
    text.set_cursor_visible(false);
    text.set_monospace(true);
    text.set_wrap_mode(gtk::WrapMode::WordChar);
    text.set_top_margin(10);
    text.set_bottom_margin(10);
    text.set_left_margin(10);
    text.set_right_margin(10);
    // With an active set to compare against, show what the on-disk edit changed
    // rather than making the user eyeball two blobs; without one, the raw block
    // is the only honest rendering.
    match active_set.as_ref() {
        Some(set) => {
            let rendered = line_diff(&set.content, &on_disk)
                .into_iter()
                .map(|(marker, line)| format!("{marker} {line}"))
                .collect::<Vec<_>>()
                .join("\n");
            text.buffer().set_text(&rendered);
        }
        None => text.buffer().set_text(&on_disk),
    }
    let scroller = gtk::ScrolledWindow::builder()
        .child(&text)
        .vexpand(true)
        .min_content_height(320)
        .build();
    scroller.add_css_class("toolport-text-area");
    body.append(&scroller);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let pull = gtk::Button::with_label("Pull into set");
    pull.add_css_class("suggested-action");
    pull.set_sensitive(active_set.is_some());
    pull.set_tooltip_text(Some(if active_set.is_some() {
        "Open the active set in the editor with this file's text; nothing is written until you save"
    } else {
        "Choose an active rule set first"
    }));
    actions.append(&pull);
    let overwrite = gtk::Button::with_label("Overwrite this file");
    overwrite.add_css_class("destructive-action");
    overwrite.set_tooltip_text(Some(
        "Rewrite only this client's file from the set as saved; other clients are left as they are",
    ));
    overwrite.set_hexpand(true);
    overwrite.set_halign(gtk::Align::End);
    actions.append(&overwrite);
    body.append(&actions);
    root.append(&body);
    window.set_content(Some(&root));

    let window_for_pull = window.clone();
    let page_for_pull = page.clone();
    pull.connect_clicked(move |_| {
        let Some(mut set) = active_set.clone() else {
            return;
        };
        set.content = on_disk.clone();
        window_for_pull.close();
        let page = page_for_pull.clone();
        gtk::glib::idle_add_local_once(move || open_rule_set_editor(Some(set), page));
    });
    let window_for_overwrite = window.clone();
    let client_id = client.id.clone();
    let client_name = client.name.clone();
    overwrite.connect_clicked(move |_| {
        window_for_overwrite.close();
        let client_id = client_id.clone();
        let client_name = client_name.clone();
        let page = page.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                crate::rules::apply_overwriting_client(&client_id)
            })
            .await;
            match result {
                Ok(result) => page.finish_mutation(
                    result,
                    &format!("Rewrote {client_name}'s rules file from the active set"),
                ),
                Err(_) => page.show_error("the overwrite stopped unexpectedly"),
            }
        });
    });
    let window_for_done = window.clone();
    done.connect_clicked(move |_| window_for_done.close());
    window.present();
}

fn confirm_apply_rules_project(project: &crate::rules::ProjectStatus, page: RulesPage) {
    let Some(parent) = page.app.active_window() else {
        return;
    };
    let enabled = project.files.iter().filter(|file| file.enabled).count();
    #[allow(deprecated)]
    let dialog = adw::MessageDialog::new(
        Some(&parent),
        Some(&format!("Apply rules to {}?", project.name)),
        Some(&format!(
            "Toolport will write its managed block to {enabled} selected project {}. Existing content outside Toolport's block is preserved.",
            if enabled == 1 { "file" } else { "files" }
        )),
    );
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("apply", "Apply rules");
    dialog.set_close_response("cancel");
    dialog.set_default_response(Some("cancel"));
    dialog.set_response_appearance("apply", adw::ResponseAppearance::Suggested);
    let project_id = project.id.clone();
    let project_name = project.name.clone();
    dialog.connect_response(None, move |dialog, response| {
        if response == "apply" {
            let project_id = project_id.clone();
            let project_name = project_name.clone();
            let page = page.clone();
            gtk::glib::spawn_future_local(async move {
                let result =
                    gtk::gio::spawn_blocking(move || crate::rules::project_apply(&project_id))
                        .await;
                match result {
                    Ok(result) => {
                        page.finish_mutation(result, &format!("Applied rules to {project_name}"))
                    }
                    Err(_) => page.show_error("the project apply stopped unexpectedly"),
                }
            });
        }
        dialog.close();
    });
    dialog.present();
}

fn confirm_remove_rules_project(project_id: &str, project_name: &str, page: RulesPage) {
    let Some(parent) = page.app.active_window() else {
        return;
    };
    #[allow(deprecated)]
    let dialog = adw::MessageDialog::new(
        Some(&parent),
        Some(&format!("Remove {project_name}?")),
        Some("Toolport removes only its managed rule blocks from this project, then unregisters the folder."),
    );
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("remove", "Remove project");
    dialog.set_close_response("cancel");
    dialog.set_default_response(Some("cancel"));
    dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
    let project_id = project_id.to_string();
    dialog.connect_response(None, move |dialog, response| {
        if response == "remove" {
            let project_id = project_id.clone();
            let page = page.clone();
            gtk::glib::spawn_future_local(async move {
                let result =
                    gtk::gio::spawn_blocking(move || crate::rules::project_remove(&project_id))
                        .await;
                match result {
                    Ok(result) => page.finish_mutation(result, "Removed project folder"),
                    Err(_) => page.show_error("the project removal stopped unexpectedly"),
                }
            });
        }
        dialog.close();
    });
    dialog.present();
}

fn rule_apply_state(state: crate::instructions::ApplyState) -> (&'static str, &'static str) {
    match state {
        crate::instructions::ApplyState::Applied => ("Applied", "success"),
        crate::instructions::ApplyState::Drifted => ("Edited outside Toolport", "review"),
        crate::instructions::ApplyState::Stale => ("Needs update", "review"),
        crate::instructions::ApplyState::Unsupported => ("Unsupported", "disabled"),
        crate::instructions::ApplyState::BlockedOverride => ("Blocked by override", "review"),
        crate::instructions::ApplyState::TooLong => ("Too long", "review"),
        crate::instructions::ApplyState::Error => ("Could not apply", "disabled"),
    }
}

fn preview_rules_client(
    client_id: &str,
    client_name: &str,
    toggle: Option<gtk::Switch>,
    page: RulesPage,
) {
    page.feedback
        .set_label(&format!("Preparing {client_name} preview…"));
    let client_id = client_id.to_string();
    let client_name = client_name.to_string();
    gtk::glib::spawn_future_local(async move {
        let client_id_for_preview = client_id.clone();
        let result =
            gtk::gio::spawn_blocking(move || crate::rules::preview(&client_id_for_preview, None))
                .await;
        let restore = |toggle: &Option<gtk::Switch>| {
            if let Some(toggle) = toggle {
                toggle.set_sensitive(true);
            }
        };
        match result {
            Ok(Ok(Some(preview))) => {
                open_rules_client_preview(preview, client_id, client_name, toggle, page)
            }
            Ok(Ok(None)) => {
                restore(&toggle);
                page.show_error("this client has no rule file Toolport can manage");
            }
            Ok(Err(error)) => {
                restore(&toggle);
                page.show_error(&error);
            }
            Err(_) => {
                restore(&toggle);
                page.show_error("the client preview stopped unexpectedly");
            }
        }
    });
}

/// Line diff (LCS) from `old` to `new`: '-' lines were removed, '+' lines were
/// added, ' ' lines are unchanged. Inputs are rule files, so quadratic LCS is
/// fine.
fn line_diff(old: &str, new: &str) -> Vec<(char, String)> {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let mut lcs = vec![vec![0usize; new_lines.len() + 1]; old_lines.len() + 1];
    for i in (0..old_lines.len()).rev() {
        for j in (0..new_lines.len()).rev() {
            lcs[i][j] = if old_lines[i] == new_lines[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0usize, 0usize);
    let mut diff = Vec::new();
    while i < old_lines.len() && j < new_lines.len() {
        if old_lines[i] == new_lines[j] {
            diff.push((' ', old_lines[i].to_string()));
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            diff.push(('-', old_lines[i].to_string()));
            i += 1;
        } else {
            diff.push(('+', new_lines[j].to_string()));
            j += 1;
        }
    }
    for line in &old_lines[i..] {
        diff.push(('-', line.to_string()));
    }
    for line in &new_lines[j..] {
        diff.push(('+', line.to_string()));
    }
    diff
}

/// Why an apply would be refused for this client, or `None` when it can write.
fn refused_write_reason(state: crate::instructions::ApplyState) -> Option<&'static str> {
    match state {
        crate::instructions::ApplyState::BlockedOverride => {
            Some("an override blocks Toolport's managed rules for this client")
        }
        crate::instructions::ApplyState::TooLong => {
            Some("the combined content exceeds this client's length limit")
        }
        crate::instructions::ApplyState::Error => {
            Some("the client's rules file could not be read or written")
        }
        _ => None,
    }
}

fn open_rules_client_preview(
    preview: crate::rules::RulesPreview,
    client_id: String,
    client_name: String,
    toggle: Option<gtk::Switch>,
    page: RulesPage,
) {
    let Some(parent) = page.app.active_window() else {
        if let Some(toggle) = &toggle {
            toggle.set_sensitive(true);
        }
        return;
    };
    let window = adw::Window::builder()
        .application(&page.app)
        .transient_for(&parent)
        .modal(true)
        .title(format!("Preview rules for {client_name}"))
        .default_width(700)
        .default_height(620)
        .build();
    window.add_css_class("toolport-editor");
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = adw::HeaderBar::new();
    let cancel = gtk::Button::with_label(if toggle.is_some() { "Cancel" } else { "Done" });
    cancel.add_css_class("toolport-secondary-action");
    let apply = gtk::Button::with_label("Apply to client");
    apply.add_css_class("suggested-action");
    apply.set_visible(toggle.is_some());
    header.pack_start(&cancel);
    header.pack_end(&apply);
    root.append(&header);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.add_css_class("toolport-editor-body");
    body.append(&editor_intro(
        "document-properties-symbolic",
        "Review the exact file",
        &format!(
            "Toolport will update {} using its {} strategy. Other client settings are untouched.",
            preview.path, preview.strategy
        ),
    ));
    if let Some(reason) = refused_write_reason(preview.state) {
        apply.set_sensitive(false);
        body.append(
            &gtk::Label::builder()
                .label(format!("This write will be refused: {reason}."))
                .halign(gtk::Align::Start)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["toolport-feedback", "error"])
                .build(),
        );
    }
    let text = gtk::TextView::new();
    text.set_editable(false);
    text.set_cursor_visible(false);
    text.set_monospace(true);
    text.set_wrap_mode(gtk::WrapMode::WordChar);
    text.set_top_margin(10);
    text.set_bottom_margin(10);
    text.set_left_margin(10);
    text.set_right_margin(10);
    text.buffer().set_text(&preview.after);
    let preview_scroller = gtk::ScrolledWindow::builder()
        .child(&text)
        .vexpand(true)
        .min_content_height(320)
        .build();
    preview_scroller.add_css_class("toolport-text-area");
    body.append(&preview_scroller);
    let clamp = adw::Clamp::builder()
        .maximum_size(760)
        .tightening_threshold(560)
        .child(&body)
        .build();
    root.append(&clamp);
    window.set_content(Some(&root));

    let toggle_for_close = toggle.clone();
    window.connect_close_request(move |_| {
        if let Some(toggle) = &toggle_for_close {
            toggle.set_sensitive(true);
        }
        gtk::glib::Propagation::Proceed
    });

    let window_for_cancel = window.clone();
    let toggle_for_cancel = toggle.clone();
    cancel.connect_clicked(move |_| {
        if let Some(toggle) = &toggle_for_cancel {
            toggle.set_sensitive(true);
        }
        window_for_cancel.close();
    });
    let window_for_apply = window.clone();
    apply.connect_clicked(move |button| {
        button.set_sensitive(false);
        let client_id = client_id.clone();
        let client_name = client_name.clone();
        let page = page.clone();
        let window = window_for_apply.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                crate::rules::set_client_enabled(&client_id, true)
            })
            .await;
            match result {
                Ok(result) => {
                    page.finish_mutation(result, &format!("Enabled rules for {client_name}"));
                    window.close();
                }
                Err(_) => page.show_error("the client rule update stopped unexpectedly"),
            }
        });
    });
    window.present();
}

fn open_rule_set_editor(set: Option<crate::registry::RuleSet>, page: RulesPage) {
    open_rule_set_editor_draft(set, None, page);
}

fn open_rule_set_editor_draft(
    set: Option<crate::registry::RuleSet>,
    draft: Option<(String, String)>,
    page: RulesPage,
) {
    let Some(parent) = page.app.active_window() else {
        return;
    };
    let editing = set.is_some();
    let set_id = set.as_ref().map(|set| set.id.clone());
    let set_name = set.as_ref().map(|set| set.name.clone());
    let editor = adw::Window::builder()
        .application(&page.app)
        .transient_for(&parent)
        .modal(true)
        .title(if editing {
            "Edit rule set"
        } else {
            "New rule set"
        })
        .default_width(700)
        .default_height(700)
        .build();
    editor.add_css_class("toolport-editor");
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = adw::HeaderBar::new();
    let cancel = gtk::Button::with_label("Cancel");
    cancel.add_css_class("toolport-secondary-action");
    let save = gtk::Button::with_label("Save");
    save.add_css_class("suggested-action");
    header.pack_start(&cancel);
    if let (Some(set_id), Some(set_name)) = (set_id.clone(), set_name) {
        let delete = gtk::Button::with_label("Delete");
        delete.add_css_class("destructive-action");
        let page_for_delete = page.clone();
        let editor_for_delete = editor.clone();
        delete.connect_clicked(move |_| {
            confirm_delete_rule_set(
                &set_id,
                &set_name,
                page_for_delete.clone(),
                editor_for_delete.clone(),
            )
        });
        header.pack_start(&delete);
    }
    header.pack_end(&save);
    root.append(&header);
    let form = gtk::Box::new(gtk::Orientation::Vertical, 14);
    form.add_css_class("toolport-editor-body");
    form.append(&editor_intro(
        "security-high-symbolic",
        if editing {
            "Edit rule set"
        } else {
            "Create a rule set"
        },
        "Saving reconciles this content with every opted-in client when the set is active.",
    ));
    let feedback = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .wrap(true)
        .visible(false)
        .css_classes(["toolport-feedback", "error"])
        .build();
    form.append(&feedback);
    let name = gtk::Entry::builder()
        .text(
            set.as_ref()
                .map(|set| set.name.as_str())
                .or_else(|| draft.as_ref().map(|(name, _)| name.as_str()))
                .unwrap_or(""),
        )
        .placeholder_text("Work")
        .css_classes(["toolport-input"])
        .build();
    form.append(&editor_field("Name", &name));
    let content = gtk::TextView::new();
    content.set_monospace(true);
    content.set_wrap_mode(gtk::WrapMode::WordChar);
    content.set_top_margin(10);
    content.set_bottom_margin(10);
    content.set_left_margin(10);
    content.set_right_margin(10);
    content.buffer().set_text(
        set.as_ref()
            .map(|set| set.content.as_str())
            .or_else(|| draft.as_ref().map(|(_, content)| content.as_str()))
            .unwrap_or(""),
    );
    let content_scroller = gtk::ScrolledWindow::builder()
        .child(&content)
        .vexpand(true)
        .min_content_height(360)
        .build();
    content_scroller.add_css_class("toolport-text-area");
    form.append(&editor_field("Instructions", &content_scroller));
    let clamp = adw::Clamp::builder()
        .maximum_size(760)
        .tightening_threshold(560)
        .child(&form)
        .build();
    root.append(&clamp);
    editor.set_content(Some(&root));
    let editor_for_cancel = editor.clone();
    cancel.connect_clicked(move |_| editor_for_cancel.close());
    let editor_for_save = editor.clone();
    save.connect_clicked(move |button| {
        let name = name.text().to_string();
        let content = content
            .buffer()
            .text(
                &content.buffer().start_iter(),
                &content.buffer().end_iter(),
                false,
            )
            .to_string();
        if name.trim().is_empty() || content.trim().is_empty() {
            feedback.set_label("Enter both a name and instructions.");
            feedback.set_visible(true);
            return;
        }
        button.set_sensitive(false);
        let set_id = set_id.clone();
        let page = page.clone();
        let editor = editor_for_save.clone();
        let feedback = feedback.clone();
        let button = button.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                crate::rules::save_set(set_id.as_deref(), &name, &content)
            })
            .await;
            match result {
                Ok(Ok(view)) => {
                    page.render(view);
                    page.feedback.set_label("Saved rule set");
                    editor.close();
                }
                Ok(Err(error)) => {
                    feedback.set_label(&error);
                    feedback.set_visible(true);
                    button.set_sensitive(true);
                }
                Err(_) => {
                    feedback.set_label("The rule-set save stopped unexpectedly.");
                    feedback.set_visible(true);
                    button.set_sensitive(true);
                }
            }
        });
    });
    editor.present();
}

fn confirm_delete_rule_set(set_id: &str, set_name: &str, page: RulesPage, editor: adw::Window) {
    #[allow(deprecated)]
    let dialog = adw::MessageDialog::new(
        Some(&editor),
        Some(&format!("Delete {set_name}?")),
        Some("If this set is active, Toolport also removes its managed rule blocks from opted-in clients. User-owned content is preserved."),
    );
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("delete", "Delete rule set");
    dialog.set_close_response("cancel");
    dialog.set_default_response(Some("cancel"));
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    let set_id = set_id.to_string();
    dialog.connect_response(None, move |dialog, response| {
        if response != "delete" {
            dialog.close();
            return;
        }
        let set_id = set_id.clone();
        let page = page.clone();
        let editor = editor.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || crate::rules::delete_set(&set_id)).await;
            match result {
                Ok(result) => {
                    page.finish_mutation(result, "Deleted rule set");
                    editor.close();
                }
                Err(_) => page.show_error("the rule-set delete stopped unexpectedly"),
            }
        });
        dialog.close();
    });
    dialog.present();
}

#[derive(Clone)]
struct ApprovalPage {
    root: gtk::Box,
    count: gtk::Label,
    list: gtk::Box,
    broker: crate::approval_broker::ApprovalBroker,
    timer: std::rc::Rc<std::cell::RefCell<Option<gtk::glib::SourceId>>>,
    rendered: std::rc::Rc<std::cell::RefCell<Vec<(String, u64)>>>,
    deadlines: std::rc::Rc<std::cell::RefCell<Vec<(u64, gtk::Label)>>>,
    app: adw::Application,
    notified: std::rc::Rc<std::cell::RefCell<std::collections::HashSet<String>>>,
}

impl ApprovalPage {
    fn new(app: &adw::Application, broker: crate::approval_broker::ApprovalBroker) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
        root.add_css_class("toolport-approvals");
        root.set_visible(false);

        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        heading.append(
            &gtk::Label::builder()
                .label("Pending approvals")
                .halign(gtk::Align::Start)
                .hexpand(true)
                .css_classes(["heading"])
                .build(),
        );
        let count = gtk::Label::new(None);
        count.add_css_class("toolport-badge");
        count.add_css_class("warning");
        heading.append(&count);
        root.append(&heading);

        let list = gtk::Box::new(gtk::Orientation::Vertical, 10);
        root.append(&list);
        Self {
            root,
            count,
            list,
            broker,
            timer: std::rc::Rc::new(std::cell::RefCell::new(None)),
            rendered: std::rc::Rc::new(std::cell::RefCell::new(Vec::new())),
            deadlines: std::rc::Rc::new(std::cell::RefCell::new(Vec::new())),
            app: app.clone(),
            notified: std::rc::Rc::new(std::cell::RefCell::new(std::collections::HashSet::new())),
        }
    }

    fn attach(&self, window: &adw::ApplicationWindow) {
        self.refresh();
        let page = self.clone();
        let timer = gtk::glib::timeout_add_local(std::time::Duration::from_secs(1), move || {
            page.refresh();
            gtk::glib::ControlFlow::Continue
        });
        *self.timer.borrow_mut() = Some(timer);

        let timer = self.timer.clone();
        window.connect_destroy(move |_| {
            if let Some(timer) = timer.borrow_mut().take() {
                timer.remove();
            }
        });
    }

    fn refresh(&self) {
        let mut pending = self.broker.list();
        pending.sort_by_key(|view| view.deadline_ms);
        let signature = pending
            .iter()
            .map(|view| (view.id.clone(), view.deadline_ms))
            .collect::<Vec<_>>();
        self.reconcile_notifications(&pending);
        self.root.set_visible(!pending.is_empty());
        self.count.set_label(&pending.len().to_string());
        if *self.rendered.borrow() == signature {
            self.update_deadlines();
            return;
        }
        *self.rendered.borrow_mut() = signature;
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        self.deadlines.borrow_mut().clear();
        for view in pending {
            let (card, deadline) = approval_card(view.clone(), self.clone());
            self.deadlines
                .borrow_mut()
                .push((view.deadline_ms, deadline));
            self.list.append(&card);
        }
        self.update_deadlines();
    }

    fn update_deadlines(&self) {
        let now = epoch_ms();
        for (deadline, label) in self.deadlines.borrow().iter() {
            label.set_label(&format!("{}s", deadline.saturating_sub(now) / 1000));
        }
    }

    fn reconcile_notifications(&self, pending: &[crate::approval_broker::PendingView]) {
        tray::set_pending(pending.len());
        let current = pending
            .iter()
            .map(|view| view.id.as_str())
            .collect::<std::collections::HashSet<_>>();
        let mut notified = self.notified.borrow_mut();
        for id in notified
            .iter()
            .filter(|id| !current.contains(id.as_str()))
            .cloned()
            .collect::<Vec<_>>()
        {
            self.app.withdraw_notification(&format!("approval-{id}"));
            notified.remove(&id);
        }
        for view in pending {
            if notified.insert(view.id.clone()) {
                let (title, body) = approval_notification(view);
                let notification = gtk::gio::Notification::new(&title);
                notification.set_body(Some(&body));
                // The urgent priority is the Wayland-era stand-in for the
                // shipping app's taskbar-attention flash: a held call must not
                // vanish as a transient banner.
                notification.set_priority(gtk::gio::NotificationPriority::Urgent);
                self.app
                    .send_notification(Some(&format!("approval-{}", view.id)), &notification);
            }
        }
    }

    fn decide(&self, id: &str, approved: bool) {
        if let Err(error) = self.broker.decide(id, approved) {
            eprintln!("toolport-gtk: could not resolve approval: {error}");
        }
        self.refresh();
    }

    fn approve_with_scope(&self, id: &str, scope: &str) {
        match self.broker.decide(id, true) {
            Ok(view) => {
                if let Some(fingerprint) = view.tool_fingerprint.as_deref() {
                    let key = crate::approval::fingerprint_allow_key(
                        &view.server,
                        &view.tool,
                        fingerprint,
                    );
                    self.broker.add_session_allow(key.clone());
                    if scope == "always" {
                        if let Err(error) = crate::registry::update(|registry| {
                            registry.allow_tool(key);
                            Ok(())
                        }) {
                            eprintln!(
                                "toolport-gtk: approved the call but could not save its allow rule: {error}"
                            );
                        }
                    }
                }
            }
            Err(error) => eprintln!("toolport-gtk: could not resolve approval: {error}"),
        }
        self.refresh();
    }
}

fn approval_card(
    view: crate::approval_broker::PendingView,
    page: ApprovalPage,
) -> (gtk::Box, gtk::Label) {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 10);
    card.add_css_class("toolport-approval-card");

    let title_row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let title = gtk::Label::builder()
        .label(format!("{} / {}", view.server, view.tool))
        .halign(gtk::Align::Start)
        .hexpand(true)
        .wrap(true)
        .css_classes(["heading"])
        .build();
    title_row.append(&title);
    let deadline = gtk::Label::builder()
        .css_classes(["toolport-approval-deadline"])
        .build();
    title_row.append(&deadline);
    card.append(&title_row);

    let requester = view.client.as_deref().unwrap_or("An AI client");
    card.append(
        &gtk::Label::builder()
            .label(format!(
                "{requester} requested this action · {}",
                approval_reason(view.reason)
            ))
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-muted"])
            .build(),
    );

    if let Some(url) = &view.url_elicitation {
        card.append(
            &gtk::Label::builder()
                .label(format!(
                    "Browser destination: {}\n{}",
                    url.origin, url.message
                ))
                .halign(gtk::Align::Start)
                .xalign(0.0)
                .wrap(true)
                .selectable(true)
                .build(),
        );
    }

    if let Some(release) = &view.pii_release {
        let values = release
            .values
            .iter()
            .map(|value| {
                let origins = if value.origins.is_empty() {
                    "no recorded source".to_string()
                } else {
                    format!("from {}", value.origins.join(", "))
                };
                format!("{} → {} ({origins})", value.token, value.value)
            })
            .collect::<Vec<_>>()
            .join("\n");
        card.append(
            &gtk::Label::builder()
                .label(format!("Would send to {}:\n{values}", release.server))
                .halign(gtk::Align::Start)
                .xalign(0.0)
                .wrap(true)
                .selectable(true)
                .css_classes(["toolport-sensitive-review"])
                .build(),
        );
    }

    let arguments = serde_json::to_string_pretty(&view.arguments)
        .unwrap_or_else(|_| "Arguments could not be displayed".into());
    let details = gtk::Expander::builder().label("Review arguments").build();
    details.set_child(Some(
        &gtk::Label::builder()
            .label(arguments)
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .selectable(true)
            .css_classes(["toolport-arguments"])
            .build(),
    ));
    card.append(&details);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    if let Some(url) = view.url_elicitation.as_ref() {
        // The URL comes from the (untrusted) MCP server. Only a validated web
        // URL gets a button at all; anything else is shown refused so the user
        // knows the server asked for something Toolport will not launch.
        match crate::oauth::validate_web_url(&url.url) {
            Ok(_) => {
                let open = gtk::Button::with_label("Open browser");
                let target = url.url.clone();
                open.connect_clicked(move |_| {
                    let _ = crate::oauth::open_web_url(&target);
                });
                actions.append(&open);
            }
            Err(error) => {
                actions.append(
                    &gtk::Label::builder()
                        .label(format!("Browser link refused: {error}"))
                        .halign(gtk::Align::Start)
                        .xalign(0.0)
                        .wrap(true)
                        .css_classes(["toolport-feedback", "error"])
                        .build(),
                );
            }
        }
    }
    let deny = gtk::Button::with_label(if view.url_elicitation.is_some() {
        "Cancel"
    } else {
        "Deny"
    });
    deny.add_css_class("destructive-action");
    let deny_id = view.id.clone();
    let deny_page = page.clone();
    deny.connect_clicked(move |_| deny_page.decide(&deny_id, false));
    actions.append(&deny);

    if view.tool_fingerprint.is_some()
        && view.url_elicitation.is_none()
        && view.pii_release.is_none()
    {
        let remember = gtk::MenuButton::builder()
            .label("Remember approval")
            .tooltip_text("Approve this definition without asking again")
            .build();
        remember.add_css_class("toolport-secondary-action");
        let popover = gtk::Popover::new();
        let choices = gtk::Box::new(gtk::Orientation::Vertical, 4);
        choices.set_margin_top(6);
        choices.set_margin_bottom(6);
        choices.set_margin_start(6);
        choices.set_margin_end(6);
        for (label, scope) in [
            ("For this session", "session"),
            ("Always for this definition", "always"),
        ] {
            let choice = gtk::Button::with_label(label);
            choice.add_css_class("flat");
            let id = view.id.clone();
            let page = page.clone();
            let remember = remember.clone();
            choice.connect_clicked(move |_| {
                remember.popdown();
                page.approve_with_scope(&id, scope);
            });
            choices.append(&choice);
        }
        popover.set_child(Some(&choices));
        remember.set_popover(Some(&popover));
        actions.append(&remember);
    }

    let approve = gtk::Button::with_label(if view.url_elicitation.is_some() {
        "Continue"
    } else {
        "Approve once"
    });
    approve.add_css_class("suggested-action");
    let approve_id = view.id;
    approve.connect_clicked(move |_| page.decide(&approve_id, true));
    actions.append(&approve);
    card.append(&actions);
    (card, deadline)
}

fn epoch_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn approval_reason(reason: crate::approval::ApprovalReason) -> &'static str {
    match reason {
        crate::approval::ApprovalReason::Destructive => "destructive tool",
        crate::approval::ApprovalReason::UntrustedSource => "untrusted source",
        crate::approval::ApprovalReason::DestructiveAndUntrusted => {
            "destructive tool from an untrusted source"
        }
        crate::approval::ApprovalReason::PersistentCodeWrite => "persistent routine write",
        crate::approval::ApprovalReason::PiiCrossServer => "cross-server data release",
    }
}

fn build_content(
    app: &adw::Application,
    broker: crate::approval_broker::ApprovalBroker,
) -> (gtk::Box, ServerPage, ApprovalPage) {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("toolport-content");

    let header = adw::HeaderBar::new();
    header.add_css_class("toolport-header");
    header.set_show_back_button(true);
    header.set_title_widget(Some(
        &gtk::Label::builder()
            .label("Servers")
            .css_classes(["title"])
            .build(),
    ));
    let mode = gtk::Label::builder()
        .label("Native preview")
        .css_classes(["toolport-mode-badge"])
        .build();
    header.pack_end(&mode);
    let add_server = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text("Add server")
        .css_classes(["suggested-action"])
        .build();
    header.pack_end(&add_server);
    let menu = gtk::gio::Menu::new();
    menu.append(Some("Import setup…"), Some("app.import-setup"));
    menu.append(Some("Import pasted setup…"), Some("app.paste-setup"));
    menu.append(Some("Export setup…"), Some("app.export-setup"));
    menu.append(Some("Share setup…"), Some("app.share-setup"));
    menu.append(Some("Run setup assistant…"), Some("app.show-onboarding"));
    menu.append(Some("Quit Toolport"), Some("app.quit"));
    let menu_button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&menu)
        .tooltip_text("Toolport menu")
        .build();
    header.pack_end(&menu_button);
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

    let intro = gtk::Label::builder()
        .label("One local gateway for every AI client")
        .halign(gtk::Align::Start)
        .wrap(true)
        .css_classes(["title-2"])
        .build();
    page.append(&intro);

    let description = gtk::Label::builder()
        .label("Your MCP servers, available everywhere. This native preview follows the active Omarchy palette and behaves like a regular Hyprland window.")
        .halign(gtk::Align::Start)
        .wrap(true)
        .xalign(0.0)
        .css_classes(["toolport-muted"])
        .build();
    page.append(&description);

    let feedback = gtk::Label::builder()
        .halign(gtk::Align::Fill)
        .xalign(0.0)
        .wrap(true)
        .visible(false)
        .css_classes(["toolport-feedback"])
        .build();
    page.append(&feedback);

    let approval_page = ApprovalPage::new(app, broker);
    page.append(&approval_page.root);

    let summary = gtk::FlowBox::new();
    summary.add_css_class("toolport-summary");
    summary.set_column_spacing(10);
    summary.set_row_spacing(10);
    summary.set_min_children_per_line(1);
    summary.set_max_children_per_line(3);
    summary.set_homogeneous(true);
    summary.set_selection_mode(gtk::SelectionMode::None);
    let mut values = Vec::new();
    for (value, label) in [("0", "Servers"), ("0", "Enabled"), ("1", "Profiles")] {
        let (item, value) = summary_item(value, label);
        values.push(value);
        summary.insert(&item, -1);
    }
    page.append(&summary);

    let profile_controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    profile_controls.append(
        &gtk::Label::builder()
            .label("Active profile")
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .hexpand(true)
            .css_classes(["heading"])
            .build(),
    );
    let profile_dropdown = gtk::DropDown::new(
        Some(gtk::StringList::new(&["Default"])),
        gtk::Expression::NONE,
    );
    profile_dropdown.set_tooltip_text(Some("Choose which server profile is active"));
    profile_controls.append(&profile_dropdown);
    let add_profile = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text("Create profile")
        .build();
    profile_controls.append(&add_profile);
    let delete_profile = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text("Delete active profile")
        .css_classes(["flat"])
        .build();
    profile_controls.append(&delete_profile);
    let profile_actions = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .tooltip_text("Profile actions")
        .build();
    profile_actions.add_css_class("flat");
    let profile_popover = gtk::Popover::new();
    let profile_action_list = gtk::Box::new(gtk::Orientation::Vertical, 4);
    profile_action_list.set_margin_top(6);
    profile_action_list.set_margin_bottom(6);
    profile_action_list.set_margin_start(6);
    profile_action_list.set_margin_end(6);
    let enable_all = gtk::Button::with_label("Enable all reviewed servers");
    enable_all.add_css_class("flat");
    profile_action_list.append(&enable_all);
    let disable_all = gtk::Button::with_label("Disable all servers");
    disable_all.add_css_class("flat");
    profile_action_list.append(&disable_all);
    profile_popover.set_child(Some(&profile_action_list));
    profile_actions.set_popover(Some(&profile_popover));
    profile_controls.append(&profile_actions);
    page.append(&profile_controls);

    let section_title = gtk::Label::builder()
        .label("Servers")
        .halign(gtk::Align::Start)
        .css_classes(["heading"])
        .build();
    page.append(&section_title);

    let posture = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .xalign(0.0)
        .wrap(true)
        .visible(false)
        .css_classes(["toolport-muted"])
        .build();
    page.append(&posture);

    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search servers")
        .hexpand(true)
        .css_classes(["toolport-search"])
        .build();
    page.append(&search);

    let list = gtk::Box::new(gtk::Orientation::Vertical, 12);
    page.append(&list);

    scroller.set_child(Some(&page));
    root.append(&scroller);
    let server_page = (
        root,
        ServerPage {
            app: app.clone(),
            server_count: values.remove(0),
            enabled_count: values.remove(0),
            profile_count: values.remove(0),
            profile_dropdown: profile_dropdown.clone(),
            add_profile: add_profile.clone(),
            delete_profile: delete_profile.clone(),
            section_title,
            posture,
            search: search.clone(),
            feedback,
            list,
            last_snapshot: std::rc::Rc::new(std::cell::RefCell::new(None)),
            updating_profile: std::rc::Rc::new(std::cell::Cell::new(false)),
            health_rows: std::rc::Rc::new(
                std::cell::RefCell::new(std::collections::HashMap::new()),
            ),
            probe_results: std::rc::Rc::new(std::cell::RefCell::new(
                std::collections::HashMap::new(),
            )),
            probe_generation: std::rc::Rc::new(std::cell::Cell::new(0)),
        },
    );
    let page_for_add = server_page.1.clone();
    add_server.connect_clicked(move |_| open_server_editor(None, page_for_add.clone()));
    let add_action = gtk::gio::SimpleAction::new("add-server", None);
    let page_for_action = server_page.1.clone();
    add_action.connect_activate(move |_, _| open_server_editor(None, page_for_action.clone()));
    app.add_action(&add_action);
    app.set_accels_for_action("app.add-server", &["<Primary>n"]);
    let page_for_search = server_page.1.clone();
    search.connect_search_changed(move |_| {
        if let Some(snapshot) = page_for_search.last_snapshot.borrow().clone() {
            page_for_search.render_server_list(&snapshot);
        }
    });
    let search_action = gtk::gio::SimpleAction::new("search-servers", None);
    let search_for_action = search.clone();
    search_action.connect_activate(move |_, _| {
        search_for_action.grab_focus();
    });
    app.add_action(&search_action);
    app.set_accels_for_action("app.search-servers", &["<Primary>f"]);
    let import_action = gtk::gio::SimpleAction::new("import-setup", None);
    let page_for_import = server_page.1.clone();
    import_action.connect_activate(move |_, _| choose_setup_import(page_for_import.clone()));
    app.add_action(&import_action);
    let export_action = gtk::gio::SimpleAction::new("export-setup", None);
    let page_for_export = server_page.1.clone();
    export_action.connect_activate(move |_, _| choose_setup_export(page_for_export.clone()));
    app.add_action(&export_action);
    let paste_action = gtk::gio::SimpleAction::new("paste-setup", None);
    let page_for_paste = server_page.1.clone();
    paste_action.connect_activate(move |_, _| open_paste_import(page_for_paste.clone()));
    app.add_action(&paste_action);
    let share_action = gtk::gio::SimpleAction::new("share-setup", None);
    let page_for_outgoing_share = server_page.1.clone();
    share_action.connect_activate(move |_, _| share_setup(page_for_outgoing_share.clone()));
    app.add_action(&share_action);
    let open_share_action =
        gtk::gio::SimpleAction::new("open-share-url", Some(gtk::glib::VariantTy::STRING));
    let page_for_share = server_page.1.clone();
    open_share_action.connect_activate(move |_, parameter| {
        let Some(url) = parameter.and_then(|value| value.str()) else {
            page_for_share.show_feedback("The shared setup link was invalid.", true);
            return;
        };
        open_shared_setup(url, page_for_share.clone());
    });
    app.add_action(&open_share_action);
    let page_for_profile = server_page.1.clone();
    profile_dropdown.connect_selected_notify(move |dropdown| {
        if page_for_profile.updating_profile.get() {
            return;
        }
        let Some(snapshot) = page_for_profile.last_snapshot.borrow().clone() else {
            return;
        };
        let Some(profile) = snapshot.profiles.get(dropdown.selected() as usize) else {
            return;
        };
        if profile.id != snapshot.active_profile_id {
            let id = profile.id.clone();
            run_profile_mutation(
                page_for_profile.clone(),
                "Switched active profile",
                move || crate::registry_controller::set_active_profile(&id),
            );
        }
    });
    let page_for_add_profile = server_page.1.clone();
    add_profile.connect_clicked(move |_| open_profile_editor(page_for_add_profile.clone()));
    let page_for_delete_profile = server_page.1.clone();
    delete_profile
        .connect_clicked(move |_| confirm_delete_profile(page_for_delete_profile.clone()));
    let page_for_enable_all = server_page.1.clone();
    enable_all.connect_clicked(move |_| {
        let Some(snapshot) = page_for_enable_all.last_snapshot.borrow().clone() else {
            return;
        };
        let profile_id = snapshot.active_profile_id;
        run_profile_mutation(
            page_for_enable_all.clone(),
            "Enabled reviewed servers",
            move || crate::registry_controller::set_all_enabled(&profile_id, true),
        );
    });
    let page_for_disable_all = server_page.1.clone();
    disable_all.connect_clicked(move |_| {
        let Some(snapshot) = page_for_disable_all.last_snapshot.borrow().clone() else {
            return;
        };
        let profile_id = snapshot.active_profile_id;
        run_profile_mutation(
            page_for_disable_all.clone(),
            "Disabled all servers",
            move || crate::registry_controller::set_all_enabled(&profile_id, false),
        );
    });
    (server_page.0, server_page.1, approval_page)
}

fn open_shared_setup(url: &str, page: ServerPage) {
    let Some(id) = crate::sharing_controller::parse_share_url(url) else {
        page.show_feedback("The shared setup link was invalid.", true);
        return;
    };
    page.show_feedback("Fetching shared setup for review…", false);
    gtk::glib::spawn_future_local(async move {
        let result = gtk::gio::spawn_blocking(move || {
            let json = crate::sharing_controller::fetch_shared_setup(&id)?;
            let preview = crate::sharing_controller::preview_import(&json)?;
            Ok::<_, String>((json, preview))
        })
        .await;
        match result {
            Ok(Ok((json, preview))) => show_setup_import_review(json, preview, page),
            Ok(Err(error)) => {
                page.show_feedback(&format!("Could not open shared setup: {error}"), true)
            }
            Err(_) => page.show_feedback("The shared setup fetch stopped unexpectedly.", true),
        }
    });
}

/// The share/export dialog: name and describe the setup, choose which servers
/// it includes, then copy a link, copy the JSON, or save it to a file. All
/// three paths export the same secret-free document.
fn share_setup(page: ServerPage) {
    let Some(parent) = page.app.active_window() else {
        return;
    };
    let servers: Vec<(String, String)> = page
        .last_snapshot
        .borrow()
        .as_ref()
        .map(|snapshot| {
            snapshot
                .servers
                .iter()
                .map(|server| (server.id.clone(), server.name.clone()))
                .collect()
        })
        .unwrap_or_default();
    let window = adw::Window::builder()
        .application(&page.app)
        .transient_for(&parent)
        .modal(true)
        .title("Share this setup")
        .default_width(640)
        .default_height(640)
        .build();
    window.add_css_class("toolport-editor");
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = adw::HeaderBar::new();
    let done = gtk::Button::with_label("Done");
    done.add_css_class("toolport-secondary-action");
    header.pack_end(&done);
    root.append(&header);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.add_css_class("toolport-editor-body");
    body.append(&editor_intro(
        "send-to-symbolic",
        "Share a secret-free setup",
        "Keychain values, inline credentials, and the local gateway are always excluded. Share links expire after 90 days.",
    ));
    let feedback = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .xalign(0.0)
        .wrap(true)
        .visible(false)
        .css_classes(["toolport-feedback"])
        .build();
    body.append(&feedback);
    let name = gtk::Entry::builder()
        .placeholder_text("Setup name (optional)")
        .css_classes(["toolport-input"])
        .build();
    body.append(&editor_field("Name", &name));
    let description = gtk::Entry::builder()
        .placeholder_text("What this setup is for (optional)")
        .css_classes(["toolport-input"])
        .build();
    body.append(&editor_field("Description", &description));
    body.append(&section_heading(
        "Servers to include",
        "Everything is included unless you uncheck it.",
    ));
    let select_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let select_all = gtk::Button::with_label("All");
    select_all.add_css_class("flat");
    let select_none = gtk::Button::with_label("None");
    select_none.add_css_class("flat");
    select_row.append(&select_all);
    select_row.append(&select_none);
    body.append(&select_row);
    let list = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let mut checks: Vec<(String, gtk::CheckButton)> = Vec::new();
    for (id, server_name) in &servers {
        let check = gtk::CheckButton::with_label(server_name);
        check.set_active(true);
        list.append(&check);
        checks.push((id.clone(), check));
    }
    let checks = std::rc::Rc::new(checks);
    {
        let checks = checks.clone();
        select_all.connect_clicked(move |_| {
            for (_, check) in checks.iter() {
                check.set_active(true);
            }
        });
    }
    {
        let checks = checks.clone();
        select_none.connect_clicked(move |_| {
            for (_, check) in checks.iter() {
                check.set_active(false);
            }
        });
    }
    let list_scroller = gtk::ScrolledWindow::builder()
        .child(&list)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_height(140)
        .max_content_height(260)
        .propagate_natural_height(true)
        .build();
    body.append(&list_scroller);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    let copy_json = gtk::Button::with_label("Copy JSON");
    copy_json.add_css_class("toolport-secondary-action");
    let save_file = gtk::Button::with_label("Save to file…");
    save_file.add_css_class("toolport-secondary-action");
    let copy_link = gtk::Button::with_label("Copy share link");
    copy_link.add_css_class("suggested-action");
    actions.append(&copy_json);
    actions.append(&save_file);
    actions.append(&copy_link);
    body.append(&actions);
    let clamp = adw::Clamp::builder()
        .maximum_size(720)
        .tightening_threshold(520)
        .child(&body)
        .build();
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&clamp)
        .build();
    root.append(&scroller);
    window.set_content(Some(&root));

    let collect = {
        let name = name.clone();
        let description = description.clone();
        let checks = checks.clone();
        move || {
            let name = name.text().trim().to_string();
            let description = description.text().trim().to_string();
            let selected: Vec<String> = checks
                .iter()
                .filter(|(_, check)| check.is_active())
                .map(|(id, _)| id.clone())
                .collect();
            (
                (!name.is_empty()).then_some(name),
                (!description.is_empty()).then_some(description),
                selected,
            )
        }
    };
    let export = move |name: Option<String>, description: Option<String>, selected: Vec<String>| {
        crate::sharing_controller::export_json(
            name.as_deref(),
            description.as_deref(),
            Some(&selected),
        )
    };
    let show = |feedback: &gtk::Label, message: &str, error: bool| {
        feedback.set_visible(true);
        feedback.set_label(message);
        if error {
            feedback.remove_css_class("success");
            feedback.add_css_class("error");
        } else {
            feedback.remove_css_class("error");
            feedback.add_css_class("success");
        }
    };

    {
        let collect = collect.clone();
        let export = export.clone();
        let feedback = feedback.clone();
        copy_link.connect_clicked(move |button| {
            let (name, description, selected) = collect();
            if selected.is_empty() {
                show(&feedback, "Select at least one server to share.", true);
                return;
            }
            button.set_sensitive(false);
            show(&feedback, "Creating a secret-free share link…", false);
            let export = export.clone();
            let feedback = feedback.clone();
            let button = button.clone();
            gtk::glib::spawn_future_local(async move {
                let result = gtk::gio::spawn_blocking(move || {
                    let json = export(name, description, selected)?;
                    crate::sharing_controller::share_setup(&json)
                })
                .await;
                button.set_sensitive(true);
                match result {
                    Ok(Ok(link)) => {
                        if let Some(display) = gtk::gdk::Display::default() {
                            display.clipboard().set_text(&link);
                            show(&feedback, "Copied the secret-free share link.", false);
                        } else {
                            show(&feedback, "Could not access the desktop clipboard.", true);
                        }
                    }
                    Ok(Err(error)) => show(
                        &feedback,
                        &format!("Could not create the share link: {error}"),
                        true,
                    ),
                    Err(_) => show(&feedback, "The sharing task stopped unexpectedly.", true),
                }
            });
        });
    }
    {
        let collect = collect.clone();
        let export = export.clone();
        let feedback = feedback.clone();
        copy_json.connect_clicked(move |_| {
            let (name, description, selected) = collect();
            if selected.is_empty() {
                show(&feedback, "Select at least one server to export.", true);
                return;
            }
            match export(name, description, selected) {
                Ok(json) => {
                    if let Some(display) = gtk::gdk::Display::default() {
                        display.clipboard().set_text(&json);
                        show(&feedback, "Copied the setup JSON to the clipboard.", false);
                    }
                }
                Err(error) => show(&feedback, &format!("Export failed: {error}"), true),
            }
        });
    }
    {
        let window_for_save = window.clone();
        let feedback = feedback.clone();
        save_file.connect_clicked(move |_| {
            let (name, description, selected) = collect();
            if selected.is_empty() {
                show(&feedback, "Select at least one server to export.", true);
                return;
            }
            let dialog = gtk::FileDialog::builder()
                .title("Export Toolport setup")
                .initial_name("toolport-setup.json")
                .accept_label("Export")
                .modal(true)
                .build();
            let export = export.clone();
            let feedback = feedback.clone();
            dialog.save(
                Some(&window_for_save),
                gtk::gio::Cancellable::NONE,
                move |result| {
                    let Ok(file) = result else {
                        return;
                    };
                    let Some(path) = file.path() else {
                        show(
                            &feedback,
                            "The selected destination is not a local file.",
                            true,
                        );
                        return;
                    };
                    let outcome = export(name.clone(), description.clone(), selected.clone())
                        .and_then(|json| crate::sharing_controller::write_setup_file(&path, &json));
                    match outcome {
                        Ok(()) => show(
                            &feedback,
                            "Exported the setup without keychain values or inline credentials.",
                            false,
                        ),
                        Err(error) => show(&feedback, &format!("Export failed: {error}"), true),
                    }
                },
            );
        });
    }
    let window_for_done = window.clone();
    done.connect_clicked(move |_| window_for_done.close());
    window.present();
}

/// Import a setup from pasted JSON, through the same review dialog as files
/// and share links.
fn open_paste_import(page: ServerPage) {
    let Some(parent) = page.app.active_window() else {
        return;
    };
    let window = adw::Window::builder()
        .application(&page.app)
        .transient_for(&parent)
        .modal(true)
        .title("Import pasted setup")
        .default_width(640)
        .default_height(480)
        .build();
    window.add_css_class("toolport-editor");
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = adw::HeaderBar::new();
    let cancel = gtk::Button::with_label("Cancel");
    cancel.add_css_class("toolport-secondary-action");
    let review = gtk::Button::with_label("Review and import");
    review.add_css_class("suggested-action");
    header.pack_start(&cancel);
    header.pack_end(&review);
    root.append(&header);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.add_css_class("toolport-editor-body");
    body.append(&editor_intro(
        "edit-paste-symbolic",
        "Paste a Toolport setup",
        "Paste the JSON someone exported. Nothing is imported until you review it.",
    ));
    let feedback = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .xalign(0.0)
        .wrap(true)
        .visible(false)
        .css_classes(["toolport-feedback", "error"])
        .build();
    body.append(&feedback);
    let editor = gtk::TextView::new();
    editor.set_monospace(true);
    editor.set_wrap_mode(gtk::WrapMode::WordChar);
    let editor_scroll = gtk::ScrolledWindow::builder()
        .min_content_height(220)
        .vexpand(true)
        .child(&editor)
        .build();
    editor_scroll.add_css_class("toolport-text-area");
    body.append(&editor_scroll);
    root.append(&body);
    window.set_content(Some(&root));
    let window_for_cancel = window.clone();
    cancel.connect_clicked(move |_| window_for_cancel.close());
    let window_for_review = window.clone();
    review.connect_clicked(move |button| {
        let buffer = editor.buffer();
        let json = buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), true)
            .to_string();
        const MAX_SETUP_BYTES: usize = 4 * 1024 * 1024;
        if json.len() > MAX_SETUP_BYTES {
            feedback.set_visible(true);
            feedback.set_label("That is too large to be a Toolport setup.");
            return;
        }
        button.set_sensitive(false);
        let json_for_preview = json.clone();
        let page = page.clone();
        let window = window_for_review.clone();
        let feedback = feedback.clone();
        let button = button.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                crate::sharing_controller::preview_import(&json_for_preview)
            })
            .await;
            button.set_sensitive(true);
            match result {
                Ok(Ok(preview)) => {
                    window.close();
                    show_setup_import_review(json, preview, page);
                }
                Ok(Err(error)) => {
                    feedback.set_visible(true);
                    feedback.set_label(&error);
                }
                Err(_) => {
                    feedback.set_visible(true);
                    feedback.set_label("The import review stopped unexpectedly.");
                }
            }
        });
    });
    window.present();
}

fn choose_setup_export(page: ServerPage) {
    let Some(parent) = page.app.active_window() else {
        return;
    };
    let dialog = gtk::FileDialog::builder()
        .title("Export Toolport setup")
        .initial_name("toolport-setup.json")
        .accept_label("Export")
        .modal(true)
        .build();
    dialog.save(Some(&parent), gtk::gio::Cancellable::NONE, move |result| {
        let Ok(file) = result else {
            return;
        };
        let Some(path) = file.path() else {
            page.show_feedback("The selected destination is not a local file.", true);
            return;
        };
        page.show_feedback("Exporting a secret-free setup…", false);
        let page = page.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                let json = crate::sharing_controller::export_json(None, None, None)?;
                crate::sharing_controller::write_setup_file(&path, &json)
            })
            .await;
            match result {
                Ok(Ok(())) => page.show_feedback(
                    "Exported the setup without keychain values or inline credentials.",
                    false,
                ),
                Ok(Err(error)) => page.show_feedback(&format!("Export failed: {error}"), true),
                Err(_) => page.show_feedback("The export stopped unexpectedly.", true),
            }
        });
    });
}

fn choose_setup_import(page: ServerPage) {
    let Some(parent) = page.app.active_window() else {
        return;
    };
    let dialog = gtk::FileDialog::builder()
        .title("Import Toolport setup")
        .accept_label("Review")
        .modal(true)
        .build();
    dialog.open(Some(&parent), gtk::gio::Cancellable::NONE, move |result| {
        let Ok(file) = result else {
            return;
        };
        let Some(path) = file.path() else {
            page.show_feedback("The selected setup is not a local file.", true);
            return;
        };
        page.show_feedback("Reading setup for review…", false);
        let page = page.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                let json = crate::sharing_controller::read_setup_file(&path)?;
                let preview = crate::sharing_controller::preview_import(&json)?;
                Ok::<_, String>((json, preview))
            })
            .await;
            match result {
                Ok(Ok((json, preview))) => show_setup_import_review(json, preview, page),
                Ok(Err(error)) => page.show_feedback(&format!("Import failed: {error}"), true),
                Err(_) => page.show_feedback("The import review stopped unexpectedly.", true),
            }
        });
    });
}

fn show_setup_import_review(
    json: String,
    preview: Vec<crate::sharing_controller::SetupImportItem>,
    page: ServerPage,
) {
    let Some(parent) = page.app.active_window() else {
        return;
    };
    let new_count = preview.iter().filter(|item| item.is_new).count();
    #[allow(deprecated)]
    let dialog = adw::MessageDialog::new(
        Some(&parent),
        Some("Import this Toolport setup?"),
        Some("Review every launch target below. Existing server names are skipped, and imported servers remain disabled until you enable them."),
    );
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("import", &format!("Import {new_count}"));
    dialog.set_response_enabled("import", new_count > 0);
    dialog.set_close_response("cancel");
    dialog.set_default_response(Some("cancel"));
    dialog.set_response_appearance("import", adw::ResponseAppearance::Suggested);
    let rows = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let mut selections: Vec<(String, gtk::CheckButton)> = Vec::new();
    for item in preview {
        let row = gtk::Box::new(gtk::Orientation::Vertical, 3);
        row.add_css_class("toolport-setting-row");
        let title_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let check = gtk::CheckButton::builder()
            .active(item.is_new)
            .sensitive(item.is_new)
            .build();
        title_row.append(&check);
        title_row.append(
            &gtk::Label::builder()
                .label(if item.is_new {
                    item.name.clone()
                } else {
                    format!("{} · already present", item.name)
                })
                .halign(gtk::Align::Start)
                .hexpand(true)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["heading"])
                .build(),
        );
        row.append(&title_row);
        row.append(
            &gtk::Label::builder()
                .label(setup_import_target(&item))
                .halign(gtk::Align::Start)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["toolport-muted"])
                .build(),
        );
        // The two facts that change the risk calculus, stated on the row that
        // carries them - not buried in the dialog preamble.
        for warning in crate::sharing_controller::import_item_warnings(&item) {
            row.append(
                &gtk::Label::builder()
                    .label(warning)
                    .halign(gtk::Align::Start)
                    .xalign(0.0)
                    .wrap(true)
                    .css_classes(["toolport-feedback", "error", "caption"])
                    .build(),
            );
        }
        if item.is_new {
            selections.push((item.name.clone(), check));
        }
        rows.append(&row);
    }
    let scroller = gtk::ScrolledWindow::builder()
        .child(&rows)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_height(180)
        .max_content_height(420)
        .build();
    dialog.set_extra_child(Some(&scroller));
    let selections = std::rc::Rc::new(selections);
    dialog.connect_response(None, move |dialog, response| {
        if response == "import" {
            let selected: Vec<String> = selections
                .iter()
                .filter(|(_, check)| check.is_active())
                .map(|(name, _)| name.clone())
                .collect();
            if selected.is_empty() {
                page.show_feedback("Nothing selected; nothing was imported.", true);
                dialog.close();
                return;
            }
            page.show_feedback("Importing reviewed servers…", false);
            let json = json.clone();
            let page = page.clone();
            gtk::glib::spawn_future_local(async move {
                let result = gtk::gio::spawn_blocking(move || {
                    crate::sharing_controller::import_json_selected(&json, &selected)
                })
                .await;
                match result {
                    Ok(Ok((_, added))) => page.show_feedback(
                        &format!(
                            "Imported {added} server{}. Review credentials before enabling them.",
                            if added == 1 { "" } else { "s" }
                        ),
                        false,
                    ),
                    Ok(Err(error)) => page.show_feedback(&format!("Import failed: {error}"), true),
                    Err(_) => page.show_feedback("The import stopped unexpectedly.", true),
                }
            });
        }
        dialog.close();
    });
    dialog.present();
}

fn setup_import_target(item: &crate::sharing_controller::SetupImportItem) -> String {
    if let Some(command) = item.command.as_deref() {
        let mask = crate::registry::secret_arg_mask(&item.args);
        let args = item
            .args
            .iter()
            .zip(mask)
            .map(|(argument, secret)| {
                if secret {
                    "<redacted>".to_string()
                } else {
                    argument.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        return format!(
            "{} · {}{}",
            item.transport,
            command,
            (!args.is_empty())
                .then(|| format!(" {args}"))
                .unwrap_or_default()
        );
    }
    format!(
        "{} · {}",
        item.transport,
        item.url
            .as_deref()
            .map(crate::registry::redact_url_userinfo)
            .unwrap_or_else(|| "No launch target".to_string())
    )
}

fn run_profile_mutation(
    page: ServerPage,
    success: &'static str,
    operation: impl FnOnce() -> Result<crate::registry::Registry, String> + Send + 'static,
) {
    page.profile_dropdown.set_sensitive(false);
    page.add_profile.set_sensitive(false);
    page.delete_profile.set_sensitive(false);
    gtk::glib::spawn_future_local(async move {
        let result = gtk::gio::spawn_blocking(operation).await;
        match result {
            Ok(Ok(registry)) => {
                page.render(state::RegistryState::Ready(
                    state::RegistrySnapshot::from_registry(registry),
                ));
                page.add_profile.set_sensitive(true);
                page.show_feedback(success, false);
            }
            Ok(Err(error)) => page.restore_after_error(&format!("Profile error: {error}")),
            Err(_) => page.restore_after_error("Profile update stopped unexpectedly"),
        }
    });
}

fn open_profile_editor(page: ServerPage) {
    let window = adw::Window::builder()
        .application(&page.app)
        .title("Create profile")
        .default_width(440)
        .default_height(220)
        .modal(true)
        .build();
    if let Some(parent) = page.app.active_window() {
        window.set_transient_for(Some(&parent));
    }
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&gtk::Label::new(Some("Create profile"))));
    let create = gtk::Button::with_label("Create");
    create.add_css_class("suggested-action");
    header.pack_end(&create);
    root.append(&header);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 10);
    content.add_css_class("toolport-dialog-content");
    content.append(&section_heading(
        "Profile name",
        "A new profile starts with every server disabled.",
    ));
    let name = gtk::Entry::builder()
        .placeholder_text("Work")
        .css_classes(["toolport-input"])
        .build();
    content.append(&name);
    root.append(&content);
    window.set_content(Some(&root));
    let window_for_create = window.clone();
    create.connect_clicked(move |_| {
        let value = name.text().to_string();
        if value.trim().is_empty() {
            name.grab_focus();
            return;
        }
        window_for_create.close();
        run_profile_mutation(page.clone(), "Created profile", move || {
            crate::registry_controller::create_profile(&value)
        });
    });
    window.present();
}

fn confirm_delete_profile(page: ServerPage) {
    let Some(snapshot) = page.last_snapshot.borrow().clone() else {
        return;
    };
    let Some(profile) = snapshot
        .profiles
        .iter()
        .find(|profile| profile.id == snapshot.active_profile_id)
    else {
        return;
    };
    #[allow(deprecated)]
    let dialog = adw::MessageDialog::new(
        page.app.active_window().as_ref(),
        Some(&format!("Delete {}?", profile.name)),
        Some("The profile is removed, but its servers and other profiles stay intact."),
    );
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("delete", "Delete profile");
    dialog.set_close_response("cancel");
    dialog.set_default_response(Some("cancel"));
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    let id = profile.id.clone();
    dialog.connect_response(None, move |dialog, response| {
        if response == "delete" {
            let id = id.clone();
            run_profile_mutation(page.clone(), "Deleted profile", move || {
                crate::registry_controller::delete_profile(&id)
            });
        }
        dialog.close();
    });
    dialog.present();
}

fn server_matches_query(server: &state::ServerView, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty()
        || server.name.to_lowercase().contains(&query)
        || server.transport.to_lowercase().contains(&query)
}

fn summary_item(value: &str, label: &str) -> (gtk::Box, gtk::Label) {
    let item = gtk::Box::new(gtk::Orientation::Vertical, 2);
    item.set_size_request(150, -1);
    item.add_css_class("toolport-summary-item");
    let value = gtk::Label::builder()
        .label(value)
        .halign(gtk::Align::Start)
        .css_classes(["title-3"])
        .build();
    item.append(&value);
    item.append(
        &gtk::Label::builder()
            .label(label)
            .halign(gtk::Align::Start)
            .css_classes(["caption", "toolport-muted"])
            .build(),
    );
    (item, value)
}

/// Window geometry persisted across launches. Only size and maximized state:
/// position and stacking belong to the compositor (the tiling contract), and a
/// tiling compositor is free to ignore the size hint too.
#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct WindowState {
    width: i32,
    height: i32,
    maximized: bool,
}

fn window_state_path() -> Option<std::path::PathBuf> {
    Some(crate::registry::conduit_dir()?.join("gtk-window-state.json"))
}

fn parse_window_state(contents: &str) -> Option<WindowState> {
    let state: WindowState = serde_json::from_str(contents).ok()?;
    // A corrupt or hand-edited file must not produce an invisible window.
    (state.width >= 200 && state.height >= 200).then_some(state)
}

fn load_window_state() -> Option<WindowState> {
    parse_window_state(&std::fs::read_to_string(window_state_path()?).ok()?)
}

fn save_window_state(window: &adw::ApplicationWindow) {
    let Some(path) = window_state_path() else {
        return;
    };
    let state = WindowState {
        width: window.width(),
        height: window.height(),
        maximized: window.is_maximized(),
    };
    if state.width < 200 || state.height < 200 {
        // Not realized yet (hidden launch) - keep the previous saved state.
        return;
    }
    if let Ok(contents) = serde_json::to_string(&state) {
        let _ = std::fs::write(path, contents);
    }
}

/// One-time notification the first time the window hides to the tray, matching
/// the shipping shell's `.tray-hint-shown` marker (and sharing it, so the hint
/// is shown at most once across both shells).
fn maybe_show_tray_hint(app: &adw::Application) {
    let Some(dir) = crate::registry::conduit_dir() else {
        return;
    };
    let marker = dir.join(".tray-hint-shown");
    if marker.exists() {
        return;
    }
    let _ = std::fs::write(&marker, b"1");
    let notification = gtk::gio::Notification::new("Toolport is still running");
    notification.set_body(Some(
        "It stays in your tray so it can hold tool calls for your approval. \
         Quit it any time from the tray icon.",
    ));
    app.send_notification(Some("toolport-tray-hint"), &notification);
}

/// Title and body of a held-call notification, mirroring the shipping broker's
/// wording: a URL-elicitation request must say a browser interaction was asked
/// for, not disguise itself as an ordinary tool call.
fn approval_notification(view: &crate::approval_broker::PendingView) -> (String, String) {
    if let Some(elicitation) = &view.url_elicitation {
        return (
            "Toolport: browser action required".to_string(),
            format!(
                "{} requested an external browser interaction. Review it in Toolport.",
                elicitation.origin
            ),
        );
    }
    let requester = view.client.as_deref().unwrap_or("An AI client");
    (
        "Toolport: approval required".to_string(),
        format!(
            "{requester} wants to run {} / {} - approve or deny it in Toolport.",
            view.server, view.tool
        ),
    )
}

/// One row's health line and badge class from a finished probe.
fn probe_status_line(probe: &crate::server_runtime::ProbeResult) -> (String, &'static str) {
    if probe.ok {
        (
            format!(
                "Ready · {} {}",
                probe.tool_count,
                if probe.tool_count == 1 {
                    "tool"
                } else {
                    "tools"
                }
            ),
            "success",
        )
    } else if probe.auth_required {
        ("Needs sign-in".to_string(), "review")
    } else {
        ("Error".to_string(), "error")
    }
}

/// The one-line posture summary above the server list.
fn posture_line(ready: usize, auth: usize, errors: usize, pending: usize, total: usize) -> String {
    if pending > 0 {
        return format!(
            "Checking {pending} of {total} enabled {}…",
            if total == 1 { "server" } else { "servers" }
        );
    }
    if auth == 0 && errors == 0 {
        return format!(
            "All {total} enabled {} ready.",
            if total == 1 {
                "server is"
            } else {
                "servers are"
            }
        );
    }
    let mut parts = vec![format!("{ready} ready")];
    if auth > 0 {
        parts.push(format!(
            "{auth} {} sign-in",
            if auth == 1 { "needs" } else { "need" }
        ));
    }
    if errors > 0 {
        parts.push(format!("{errors} failing"));
    }
    parts.join(" · ")
}

/// Grouping rank for the server list: needs-attention first, then unprobed,
/// ready, and disabled last. Review-gated team servers count as attention too.
fn server_health_rank(
    server: &state::ServerView,
    probe: Option<&crate::server_runtime::ProbeResult>,
) -> u8 {
    if server.requires_review {
        return 0;
    }
    if !server.enabled {
        return 3;
    }
    match probe {
        Some(probe) if !probe.ok => 0,
        None => 1,
        Some(_) => 2,
    }
}

fn server_card(
    server: &state::ServerView,
    profile_id: &str,
    tool_scope: Option<Vec<String>>,
    page: ServerPage,
) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    card.add_css_class("toolport-card");
    card.set_margin_top(1);
    card.set_margin_bottom(1);

    let icon = gtk::Image::from_icon_name(match server.transport.as_str() {
        "Remote HTTP" | "Remote SSE" => "network-server-symbolic",
        "Local stdio" => "utilities-terminal-symbolic",
        _ => "applications-system-symbolic",
    });
    icon.add_css_class("toolport-card-icon");
    card.append(&icon);

    let text = gtk::Box::new(gtk::Orientation::Vertical, 3);
    text.set_hexpand(true);
    text.append(
        &gtk::Label::builder()
            .label(&server.name)
            .halign(gtk::Align::Start)
            .css_classes(["heading"])
            .build(),
    );
    text.append(
        &gtk::Label::builder()
            .label(&server.transport)
            .halign(gtk::Align::Start)
            .css_classes(["toolport-muted"])
            .build(),
    );
    let health = gtk::Label::builder()
        .label("Checking…")
        .halign(gtk::Align::Start)
        .xalign(0.0)
        .visible(server.enabled && !server.requires_review)
        .css_classes(["toolport-muted", "caption"])
        .build();
    text.append(&health);
    card.append(&text);

    let authenticate = gtk::Button::with_label("Authenticate");
    authenticate.add_css_class("suggested-action");
    authenticate.set_valign(gtk::Align::Center);
    authenticate.set_visible(false);
    authenticate.set_tooltip_text(Some(
        "This server responded but needs credentials before its tools are available",
    ));
    {
        let server_for_auth = server.clone();
        let page_for_auth = page.clone();
        authenticate.connect_clicked(move |_| {
            if matches!(server_for_auth.transport_id.as_str(), "http" | "sse") {
                open_authentication_editor(server_for_auth.clone(), page_for_auth.clone());
            } else {
                open_credentials_editor(server_for_auth.clone(), page_for_auth.clone());
            }
        });
    }
    card.append(&authenticate);
    let copy_error = gtk::Button::builder()
        .icon_name("edit-copy-symbolic")
        .tooltip_text("Copy the full probe error")
        .valign(gtk::Align::Center)
        .visible(false)
        .css_classes(["flat"])
        .build();
    {
        // The full error rides on the health label's tooltip; copying reads it
        // from there so the two can never disagree.
        let health_for_copy = health.clone();
        copy_error.connect_clicked(move |button| {
            let Some(error) = health_for_copy.tooltip_text() else {
                return;
            };
            if let Some(display) = gtk::gdk::Display::default() {
                display.clipboard().set_text(error.as_str());
                button.set_tooltip_text(Some("Copied"));
            }
        });
    }
    card.append(&copy_error);
    if server.enabled && !server.requires_review {
        page.health_rows.borrow_mut().insert(
            server.id.clone(),
            HealthRow {
                label: health.clone(),
                authenticate: authenticate.clone(),
                copy_error: copy_error.clone(),
            },
        );
        // Show the last known result immediately; the in-flight round replaces it.
        if let Some(previous) = page.probe_results.borrow().get(&server.id) {
            let (line, class) = probe_status_line(previous);
            health.set_label(&line);
            health.add_css_class(class);
            authenticate.set_visible(previous.auth_required);
            copy_error.set_visible(!previous.ok && previous.error.is_some());
        }
    }

    if server.requires_review {
        let badge = gtk::Label::new(Some("Review in Teams"));
        badge.add_css_class("toolport-badge");
        badge.add_css_class("review");
        badge.set_tooltip_text(Some(
            "This team server must be reviewed before its command or private address can run",
        ));
        card.append(&badge);
    } else {
        let status = gtk::Label::new(Some(if server.enabled {
            "Enabled"
        } else {
            "Disabled"
        }));
        status.add_css_class("toolport-server-state");
        let toggle = gtk::Switch::builder()
            .active(server.enabled)
            .valign(gtk::Align::Center)
            .tooltip_text(format!(
                "{} {}",
                if server.enabled { "Disable" } else { "Enable" },
                server.name
            ))
            .build();
        let server_id = server.id.clone();
        let server_name = server.name.clone();
        let profile_id = profile_id.to_string();
        let status_for_toggle = status.clone();
        let page_for_toggle = page.clone();
        toggle.connect_state_set(move |toggle, enabled| {
            toggle.set_sensitive(false);
            status_for_toggle.set_label("Updating");

            let server_id = server_id.clone();
            let server_name = server_name.clone();
            let profile_id = profile_id.clone();
            let page = page_for_toggle.clone();
            gtk::glib::spawn_future_local(async move {
                let update = gtk::gio::spawn_blocking(move || {
                    crate::registry_controller::set_server_enabled(
                        &profile_id,
                        &server_id,
                        enabled,
                        false,
                    )
                })
                .await;
                match update {
                    Ok(Ok(registry)) => {
                        page.render(state::RegistryState::Ready(
                            state::RegistrySnapshot::from_registry(registry),
                        ));
                        page.show_feedback(
                            &format!(
                                "{} {server_name}",
                                if enabled { "Enabled" } else { "Disabled" }
                            ),
                            false,
                        );
                    }
                    Ok(Err(error)) => {
                        page.restore_after_error(&format!(
                            "Could not update {server_name}: {error}"
                        ));
                    }
                    Err(_) => {
                        page.restore_after_error(&format!(
                            "Could not update {server_name}: the operation stopped"
                        ));
                    }
                }
            });
            gtk::glib::Propagation::Stop
        });
        card.append(&status);
        card.append(&toggle);
    }
    let actions = gtk::Box::new(gtk::Orientation::Vertical, 4);
    actions.set_margin_top(6);
    actions.set_margin_bottom(6);
    actions.set_margin_start(6);
    actions.set_margin_end(6);
    let test = action_menu_button("Test connection", "network-transmit-receive-symbolic");
    let server_for_test = server.clone();
    let page_for_test = page.clone();
    test.connect_clicked(move |button| {
        test_server_connection(&server_for_test, button, page_for_test.clone());
    });
    actions.append(&test);

    let scope = action_menu_button("Profile tool scope", "view-list-symbolic");
    let server_for_scope = server.clone();
    let profile_for_scope = profile_id.to_string();
    let page_for_scope = page.clone();
    scope.connect_clicked(move |_| {
        open_profile_tool_scope(
            server_for_scope.clone(),
            profile_for_scope.clone(),
            tool_scope.clone(),
            page_for_scope.clone(),
        )
    });
    actions.append(&scope);

    let edit = action_menu_button("Edit", "document-edit-symbolic");
    edit.set_sensitive(matches!(
        server.transport_id.as_str(),
        "stdio" | "http" | "sse"
    ));
    let server_for_edit = server.clone();
    let page_for_edit = page.clone();
    edit.connect_clicked(move |_| {
        open_server_editor(Some(server_for_edit.clone()), page_for_edit.clone())
    });
    actions.append(&edit);

    let duplicate = action_menu_button("Duplicate", "edit-copy-symbolic");
    duplicate.set_tooltip_text(Some(
        "Add another account: a new entry with the same connection, named separately",
    ));
    duplicate.set_sensitive(matches!(
        server.transport_id.as_str(),
        "stdio" | "http" | "sse"
    ));
    let server_for_duplicate = server.clone();
    let page_for_duplicate = page.clone();
    duplicate.connect_clicked(move |_| {
        open_duplicate_server_editor(&server_for_duplicate, page_for_duplicate.clone())
    });
    actions.append(&duplicate);

    if matches!(server.transport_id.as_str(), "http" | "sse") {
        let authentication = action_menu_button("Authentication", "system-lock-screen-symbolic");
        let server_for_authentication = server.clone();
        let page_for_authentication = page.clone();
        authentication.connect_clicked(move |_| {
            open_authentication_editor(
                server_for_authentication.clone(),
                page_for_authentication.clone(),
            )
        });
        actions.append(&authentication);
    }

    let credentials = action_menu_button("Credentials", "dialog-password-symbolic");
    let server_for_credentials = server.clone();
    let page_for_credentials = page.clone();
    credentials.connect_clicked(move |_| {
        open_credentials_editor(server_for_credentials.clone(), page_for_credentials.clone())
    });
    actions.append(&credentials);

    let remove = action_menu_button("Remove", "user-trash-symbolic");
    remove.add_css_class("destructive-action");
    let server_id = server.id.clone();
    let server_name = server.name.clone();
    remove.connect_clicked(move |_| {
        confirm_remove_server(&server_id, &server_name, page.clone());
    });
    actions.append(&remove);
    let popover = gtk::Popover::new();
    popover.add_css_class("toolport-action-menu");
    popover.set_child(Some(&actions));
    let menu = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .tooltip_text(format!("Actions for {}", server.name))
        .popover(&popover)
        .css_classes(["flat"])
        .build();
    card.append(&menu);
    card
}

fn open_profile_tool_scope(
    server: state::ServerView,
    profile_id: String,
    current_scope: Option<Vec<String>>,
    page: ServerPage,
) {
    let Some(parent) = page.app.active_window() else {
        return;
    };
    let window = adw::Window::builder()
        .application(&page.app)
        .transient_for(&parent)
        .modal(true)
        .title(format!("Tool scope for {}", server.name))
        .default_width(520)
        .default_height(560)
        .build();
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
    content.append(&editor_intro(
        "view-list-symbolic",
        "Tools available in this profile",
        "Uncheck tools this profile should hide. Selecting every tool restores the server's default full scope.",
    ));
    let feedback = gtk::Label::builder()
        .label("Loading server tools…")
        .halign(gtk::Align::Start)
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
                    page.render(state::RegistryState::Ready(
                        state::RegistrySnapshot::from_registry(registry),
                    ));
                    page.show_feedback("Updated the active profile's tool scope.", false);
                    window.close();
                }
                Ok(Err(error)) => {
                    page.show_feedback(&format!("Could not update tool scope: {error}"), true)
                }
                Err(_) => page.show_feedback("The tool-scope update stopped unexpectedly.", true),
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
                feedback.set_label("Changes apply only to the active server profile.");
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

/// The one-line explanation of a live auth probe, shown above the credential
/// fields so the user picks the path the server actually supports.
fn auth_probe_summary(info: &crate::vendors::AuthInfo) -> String {
    let mut summary = match info.kind.as_str() {
        "none" => "This server responded without authentication.".to_string(),
        "oauth" => "This server supports browser sign-in.".to_string(),
        "token" => "This server expects a bearer token.".to_string(),
        _ => "Toolport could not determine what authentication this server wants.".to_string(),
    };
    if let Some(vendor) = info.vendor.as_deref() {
        summary.push_str(&format!(" Detected {vendor}."));
    }
    if let Some(instructions) = info.instructions.as_deref() {
        summary.push(' ');
        summary.push_str(instructions);
    }
    if let Some(token_url) = info.token_url.as_deref() {
        summary.push_str(&format!(" Get a token at {token_url}"));
    }
    summary
}

fn open_authentication_editor(server: state::ServerView, page: ServerPage) {
    let Some(parent) = page.app.active_window() else {
        return;
    };
    let editor = adw::Window::builder()
        .application(&page.app)
        .transient_for(&parent)
        .modal(true)
        .title(format!("Authentication for {}", server.name))
        .default_width(560)
        .default_height(440)
        .build();
    editor.add_css_class("toolport-editor");

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = adw::HeaderBar::new();
    let done = gtk::Button::with_label("Done");
    done.add_css_class("toolport-secondary-action");
    header.pack_end(&done);
    root.append(&header);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
    content.add_css_class("toolport-editor-body");
    content.append(&editor_intro(
        "system-lock-screen-symbolic",
        "Remote authentication",
        "Paste a bearer token for this server. Toolport stores it in the system keychain and never displays it again.",
    ));
    let feedback = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["toolport-feedback"])
        .build();
    feedback.set_label("Checking the system keychain…");
    content.append(&feedback);

    // What the server actually wants, probed live so the user does not have to
    // guess between a token paste and a browser sign-in.
    let guidance = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .xalign(0.0)
        .wrap(true)
        .visible(false)
        .css_classes(["toolport-muted"])
        .build();
    content.append(&guidance);
    if let Some(url) = server.url.clone() {
        let guidance = guidance.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || crate::vendors::probe_auth(&url)).await;
            if let Ok(info) = result {
                guidance.set_label(&auth_probe_summary(&info));
                guidance.set_visible(true);
            }
        });
    }

    let token_section = gtk::Box::new(gtk::Orientation::Vertical, 10);
    token_section.add_css_class("toolport-form-section");
    token_section.append(&section_heading(
        "Bearer token",
        "Saving a manual token replaces any previous browser sign-in for this server.",
    ));
    let token = gtk::PasswordEntry::builder()
        .placeholder_text("Paste a new token")
        .show_peek_icon(true)
        .hexpand(true)
        .css_classes(["toolport-input"])
        .build();
    token_section.append(&token);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    let sign_in = gtk::Button::with_label("Sign in with browser");
    sign_in.add_css_class("toolport-secondary-action");
    let remove = gtk::Button::with_label("Remove token");
    remove.add_css_class("destructive-action");
    remove.set_sensitive(false);
    let save = gtk::Button::with_label("Store token");
    save.add_css_class("suggested-action");
    actions.append(&sign_in);
    actions.append(&remove);
    actions.append(&save);
    token_section.append(&actions);
    content.append(&token_section);

    let client_section = gtk::Box::new(gtk::Orientation::Vertical, 10);
    client_section.add_css_class("toolport-form-section");
    client_section.append(&section_heading(
        "Client credentials",
        "For unattended OAuth servers. The client secret is stored only in the system keychain.",
    ));
    let client_id = gtk::Entry::builder()
        .placeholder_text("OAuth client id")
        .text(
            server
                .client_credentials
                .as_ref()
                .map(|credentials| credentials.client_id.as_str())
                .unwrap_or_default(),
        )
        .css_classes(["toolport-input"])
        .build();
    let client_secret = gtk::PasswordEntry::builder()
        .placeholder_text(if server.client_credentials.is_some() {
            "Leave blank to keep the stored secret"
        } else {
            "OAuth client secret"
        })
        .show_peek_icon(true)
        .css_classes(["toolport-input"])
        .build();
    let methods = gtk::StringList::new(&["client_secret_basic", "client_secret_post"]);
    let method = gtk::DropDown::new(Some(methods), None::<gtk::Expression>);
    method.set_selected(
        (server
            .client_credentials
            .as_ref()
            .and_then(|credentials| credentials.token_endpoint_auth_method.as_deref())
            == Some("client_secret_post")) as u32,
    );
    let scope = gtk::Entry::builder()
        .placeholder_text("Scopes (optional)")
        .text(
            server
                .client_credentials
                .as_ref()
                .and_then(|credentials| credentials.scope.as_deref())
                .unwrap_or_default(),
        )
        .css_classes(["toolport-input"])
        .build();
    client_section.append(&editor_field("Client id", &client_id));
    client_section.append(&editor_field("Client secret", &client_secret));
    // Whether a secret is actually vaulted is a keychain question, not a
    // registry one: a lost/locked keychain must not read as "stored", and
    // unknown must not read as absent (SBS-722).
    let secret_status = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .xalign(0.0)
        .wrap(true)
        .visible(false)
        .css_classes(["toolport-muted", "caption"])
        .build();
    client_section.append(&secret_status);
    let secret_retry = gtk::Button::with_label("Retry the secret check");
    secret_retry.add_css_class("flat");
    secret_retry.set_halign(gtk::Align::Start);
    secret_retry.set_visible(false);
    client_section.append(&secret_retry);
    {
        let check = {
            let server_id = server.id.clone();
            let secret_status = secret_status.clone();
            let secret_retry = secret_retry.clone();
            let client_secret = client_secret.clone();
            std::rc::Rc::new(move || {
                let server_id = server_id.clone();
                let secret_status = secret_status.clone();
                let secret_retry = secret_retry.clone();
                let client_secret = client_secret.clone();
                gtk::glib::spawn_future_local(async move {
                    let result = gtk::gio::spawn_blocking(move || {
                        crate::registry_controller::has_client_secret(&server_id)
                    })
                    .await;
                    secret_status.set_visible(true);
                    match result {
                        Ok(Ok(true)) => {
                            secret_status.set_label(
                                "A client secret is stored in the keychain. It cannot be shown again; leave the field blank to keep it.",
                            );
                            secret_retry.set_visible(false);
                            client_secret.set_placeholder_text(Some(
                                "Leave blank to keep the stored secret",
                            ));
                        }
                        Ok(Ok(false)) => {
                            secret_status.set_label("No client secret is stored yet.");
                            secret_retry.set_visible(false);
                            client_secret.set_placeholder_text(Some("OAuth client secret"));
                        }
                        _ => {
                            secret_status.set_label(
                                "Couldn't check the stored secret. Unknown is not the same as absent.",
                            );
                            secret_retry.set_visible(true);
                        }
                    }
                });
            })
        };
        check();
        let check_for_retry = check.clone();
        secret_retry.connect_clicked(move |_| check_for_retry());
    }
    client_section.append(&editor_field("Token endpoint auth method", &method));
    client_section.append(&editor_field("Scope", &scope));
    let client_actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    client_actions.set_halign(gtk::Align::End);
    let remove_client = gtk::Button::with_label("Remove client credentials");
    remove_client.add_css_class("destructive-action");
    remove_client.set_sensitive(server.client_credentials.is_some());
    let save_client = gtk::Button::with_label("Save client credentials");
    save_client.add_css_class("suggested-action");
    client_actions.append(&remove_client);
    client_actions.append(&save_client);
    client_section.append(&client_actions);
    content.append(&client_section);

    let clamp = adw::Clamp::builder()
        .maximum_size(680)
        .tightening_threshold(520)
        .child(&content)
        .build();
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&clamp)
        .build();
    root.append(&scroller);
    editor.set_content(Some(&root));

    let editor_for_done = editor.clone();
    done.connect_clicked(move |_| editor_for_done.close());

    let server_id = server.id.clone();
    let feedback_for_status = feedback.clone();
    let remove_for_status = remove.clone();
    gtk::glib::spawn_future_local(async move {
        let result = gtk::gio::spawn_blocking(move || {
            crate::registry_controller::has_auth_token(&server_id)
        })
        .await;
        match result {
            Ok(Ok(has_token)) => {
                feedback_for_status.set_label(if has_token {
                    "A bearer token is stored in the system keychain."
                } else {
                    "No bearer token is stored for this server."
                });
                feedback_for_status.remove_css_class("error");
                feedback_for_status.add_css_class("success");
                remove_for_status.set_sensitive(has_token);
            }
            Ok(Err(error)) => {
                feedback_for_status
                    .set_label(&format!("Could not read authentication status: {error}"));
                feedback_for_status.add_css_class("error");
            }
            Err(_) => {
                feedback_for_status.set_label("The keychain status check stopped unexpectedly.");
                feedback_for_status.add_css_class("error");
            }
        }
    });

    let server_id = server.id.clone();
    let server_name = server.name.clone();
    let page_for_client = page.clone();
    let feedback_for_client = feedback.clone();
    let remove_for_client = remove_client.clone();
    save_client.connect_clicked(move |button| {
        button.set_sensitive(false);
        let server_id = server_id.clone();
        let server_name = server_name.clone();
        let client_id = client_id.text().to_string();
        let secret = client_secret.text().to_string();
        let method = if method.selected() == 1 {
            "client_secret_post"
        } else {
            "client_secret_basic"
        }
        .to_string();
        let scope = scope.text().to_string();
        let page = page_for_client.clone();
        let feedback = feedback_for_client.clone();
        let remove = remove_for_client.clone();
        let button = button.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                crate::registry_controller::set_client_credentials(
                    &server_id,
                    &client_id,
                    Some(secret),
                    Some(&method),
                    Some(&scope),
                )
            })
            .await;
            button.set_sensitive(true);
            match result {
                Ok(Ok(registry)) => {
                    page.render(state::RegistryState::Ready(
                        state::RegistrySnapshot::from_registry(registry),
                    ));
                    feedback.set_label(&format!(
                        "Client credentials for {server_name} are configured."
                    ));
                    feedback.remove_css_class("error");
                    feedback.add_css_class("success");
                    remove.set_sensitive(true);
                }
                Ok(Err(error)) => {
                    feedback.set_label(&format!("Could not save client credentials: {error}"));
                    feedback.add_css_class("error");
                }
                Err(_) => {
                    feedback.set_label("The client-credentials update stopped unexpectedly.");
                    feedback.add_css_class("error");
                }
            }
        });
    });

    let server_id = server.id.clone();
    let server_name = server.name.clone();
    let page_for_client_remove = page.clone();
    let feedback_for_client_remove = feedback.clone();
    remove_client.connect_clicked(move |button| {
        button.set_sensitive(false);
        let server_id = server_id.clone();
        let server_name = server_name.clone();
        let page = page_for_client_remove.clone();
        let feedback = feedback_for_client_remove.clone();
        let button = button.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                crate::registry_controller::clear_client_credentials(&server_id)
            })
            .await;
            match result {
                Ok(Ok(registry)) => {
                    page.render(state::RegistryState::Ready(
                        state::RegistrySnapshot::from_registry(registry),
                    ));
                    feedback.set_label(&format!("Removed client credentials for {server_name}."));
                    feedback.remove_css_class("error");
                    feedback.add_css_class("success");
                    button.set_sensitive(false);
                }
                Ok(Err(error)) => {
                    button.set_sensitive(true);
                    feedback.set_label(&format!("Could not remove client credentials: {error}"));
                    feedback.add_css_class("error");
                }
                Err(_) => {
                    button.set_sensitive(true);
                    feedback.set_label("The client-credentials removal stopped unexpectedly.");
                    feedback.add_css_class("error");
                }
            }
        });
    });

    let server_id = server.id.clone();
    let server_name = server.name.clone();
    let server_url = server.url.clone().unwrap_or_default();
    let feedback_for_sign_in = feedback.clone();
    let remove_for_sign_in = remove.clone();
    let page_for_sign_in = page.clone();
    sign_in.connect_clicked(move |button| {
        if server_url.is_empty() {
            feedback_for_sign_in.set_label("This server does not have a remote URL to sign in to.");
            feedback_for_sign_in.add_css_class("error");
            return;
        }
        button.set_sensitive(false);
        feedback_for_sign_in
            .set_label("Opening your browser. Finish sign-in there, then return to Toolport…");
        feedback_for_sign_in.remove_css_class("error");
        let server_id = server_id.clone();
        let server_name = server_name.clone();
        let server_url = server_url.clone();
        let feedback = feedback_for_sign_in.clone();
        let remove = remove_for_sign_in.clone();
        let page = page_for_sign_in.clone();
        let button = button.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                crate::oauth_controller::authenticate(&server_id, &server_url)
            })
            .await;
            button.set_sensitive(true);
            match result {
                Ok(Ok(())) => {
                    feedback.set_label(
                        "Browser sign-in completed and the token is stored in the system keychain.",
                    );
                    feedback.remove_css_class("error");
                    feedback.add_css_class("success");
                    remove.set_sensitive(true);
                    page.show_feedback(&format!("Authenticated {server_name}"), false);
                }
                Ok(Err(error)) => {
                    feedback.set_label(&error);
                    feedback.remove_css_class("success");
                    feedback.add_css_class("error");
                }
                Err(_) => {
                    feedback.set_label("Browser sign-in stopped unexpectedly.");
                    feedback.add_css_class("error");
                }
            }
        });
    });

    let server_id = server.id.clone();
    let server_name = server.name.clone();
    let feedback_for_save = feedback.clone();
    let remove_for_save = remove.clone();
    let page_for_save = page.clone();
    save.connect_clicked(move |button| {
        let value = token.text().to_string();
        if value.is_empty() {
            feedback_for_save.set_label("Paste a token before storing it.");
            feedback_for_save.add_css_class("error");
            return;
        }
        let remove_was_sensitive = remove_for_save.is_sensitive();
        button.set_sensitive(false);
        remove_for_save.set_sensitive(false);
        feedback_for_save.set_label("Storing token in the system keychain…");
        feedback_for_save.remove_css_class("error");
        let server_id = server_id.clone();
        let server_name = server_name.clone();
        let feedback = feedback_for_save.clone();
        let remove = remove_for_save.clone();
        let page = page_for_save.clone();
        let button = button.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                crate::registry_controller::set_auth_token(&server_id, &value)
            })
            .await;
            button.set_sensitive(true);
            match result {
                Ok(Ok(())) => {
                    feedback.set_label("Bearer token stored in the system keychain.");
                    feedback.remove_css_class("error");
                    feedback.add_css_class("success");
                    remove.set_sensitive(true);
                    page.show_feedback(&format!("Updated authentication for {server_name}"), false);
                }
                Ok(Err(error)) => {
                    feedback.set_label(&error);
                    feedback.remove_css_class("success");
                    feedback.add_css_class("error");
                    remove.set_sensitive(remove_was_sensitive);
                }
                Err(_) => {
                    feedback.set_label("The token update stopped unexpectedly.");
                    feedback.add_css_class("error");
                    remove.set_sensitive(remove_was_sensitive);
                }
            }
        });
    });

    let server_id = server.id;
    let server_name = server.name;
    let feedback_for_remove = feedback;
    let page_for_remove = page;
    remove.connect_clicked(move |button| {
        button.set_sensitive(false);
        feedback_for_remove.set_label("Removing token from the system keychain…");
        feedback_for_remove.remove_css_class("error");
        let server_id = server_id.clone();
        let server_name = server_name.clone();
        let feedback = feedback_for_remove.clone();
        let page = page_for_remove.clone();
        let button = button.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                crate::registry_controller::clear_auth_token(&server_id)
            })
            .await;
            match result {
                Ok(Ok(())) => {
                    feedback.set_label("No bearer token is stored for this server.");
                    feedback.remove_css_class("error");
                    feedback.add_css_class("success");
                    page.show_feedback(&format!("Removed authentication for {server_name}"), false);
                }
                Ok(Err(error)) => {
                    button.set_sensitive(true);
                    feedback.set_label(&error);
                    feedback.remove_css_class("success");
                    feedback.add_css_class("error");
                }
                Err(_) => {
                    button.set_sensitive(true);
                    feedback.set_label("The token removal stopped unexpectedly.");
                    feedback.add_css_class("error");
                }
            }
        });
    });
    editor.present();
}

fn test_server_connection(server: &state::ServerView, button: &gtk::Button, page: ServerPage) {
    button.set_sensitive(false);
    page.show_feedback(&format!("Testing {}…", server.name), false);
    let server_id = server.id.clone();
    let server_name = server.name.clone();
    let button = button.clone();
    gtk::glib::spawn_future_local(async move {
        let result =
            gtk::gio::spawn_blocking(move || crate::server_runtime::probe_registered(&server_id))
                .await;
        button.set_sensitive(true);
        match result {
            Ok(Ok(probe)) if probe.ok => page.show_feedback(
                &format!(
                    "{server_name} connected and exposed {} {}",
                    probe.tool_count,
                    if probe.tool_count == 1 {
                        "tool"
                    } else {
                        "tools"
                    }
                ),
                false,
            ),
            Ok(Ok(probe)) if probe.auth_required => page.show_feedback(
                &format!("{server_name} needs credentials before it can connect"),
                true,
            ),
            Ok(Ok(probe)) => page.show_feedback(
                &format!(
                    "Could not connect to {server_name}: {}",
                    probe.error.as_deref().unwrap_or("unknown connection error")
                ),
                true,
            ),
            Ok(Err(error)) => {
                page.show_feedback(&format!("Could not test {server_name}: {error}"), true)
            }
            Err(_) => page.show_feedback(
                &format!("Could not test {server_name}: the operation stopped"),
                true,
            ),
        }
    });
}

fn action_menu_button(label: &str, icon: &str) -> gtk::Button {
    let button = gtk::Button::new();
    button.set_halign(gtk::Align::Fill);
    button.add_css_class("toolport-action-item");
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 9);
    row.append(&gtk::Image::from_icon_name(icon));
    row.append(
        &gtk::Label::builder()
            .label(label)
            .halign(gtk::Align::Start)
            .hexpand(true)
            .build(),
    );
    button.set_child(Some(&row));
    button
}

fn open_credentials_editor(server: state::ServerView, page: ServerPage) {
    let Some(parent) = page.app.active_window() else {
        return;
    };
    let editor = adw::Window::builder()
        .application(&page.app)
        .transient_for(&parent)
        .modal(true)
        .title(format!("Credentials for {}", server.name))
        .default_width(620)
        .default_height(640)
        .build();
    editor.add_css_class("toolport-editor");

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = adw::HeaderBar::new();
    let done = gtk::Button::with_label("Done");
    done.add_css_class("suggested-action");
    header.pack_end(&done);
    root.append(&header);

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
    content.add_css_class("toolport-editor-body");
    let intro = editor_intro(
        "dialog-password-symbolic",
        "Server credentials",
        "Secret values stay in the system keychain and are never read back into Toolport.",
    );
    content.append(&intro);
    let feedback = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .wrap(true)
        .visible(false)
        .css_classes(["toolport-feedback", "error"])
        .build();
    content.append(&feedback);

    let stored = gtk::Box::new(gtk::Orientation::Vertical, 10);
    stored.add_css_class("toolport-form-section");
    stored.append(&section_heading(
        "Stored credentials",
        "Enter a new value only when you want to replace one.",
    ));
    if server.secret_keys.is_empty() {
        stored.append(
            &gtk::Label::builder()
                .label("No credential keys are declared for this server.")
                .halign(gtk::Align::Start)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["toolport-muted"])
                .build(),
        );
    }
    for key in &server.secret_keys {
        let row = gtk::Box::new(gtk::Orientation::Vertical, 10);
        row.add_css_class("toolport-credential-row");
        let key_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        key_row.append(
            &gtk::Label::builder()
                .label(key)
                .halign(gtk::Align::Start)
                .hexpand(true)
                .css_classes(["heading"])
                .build(),
        );
        let remove = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text(format!("Remove {key}"))
            .css_classes(["flat", "toolport-destructive-icon"])
            .build();
        key_row.append(&remove);
        row.append(&key_row);
        let replace_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let value = gtk::PasswordEntry::builder()
            .placeholder_text("Enter a replacement value")
            .show_peek_icon(true)
            .hexpand(true)
            .css_classes(["toolport-input"])
            .build();
        replace_row.append(&value);
        let replace = gtk::Button::with_label("Replace");
        replace.add_css_class("toolport-secondary-action");
        replace_row.append(&replace);
        row.append(&replace_row);
        stored.append(&row);

        let server_id = server.id.clone();
        let key_for_replace = key.clone();
        let feedback_for_replace = feedback.clone();
        let page_for_replace = page.clone();
        let editor_for_replace = editor.clone();
        replace.connect_clicked(move |button| {
            let secret = value.text().to_string();
            if secret.is_empty() {
                feedback_for_replace.set_label("Enter a replacement value first.");
                feedback_for_replace.set_visible(true);
                return;
            }
            button.set_sensitive(false);
            let server_id = server_id.clone();
            let key = key_for_replace.clone();
            let page = page_for_replace.clone();
            let editor = editor_for_replace.clone();
            let feedback = feedback_for_replace.clone();
            let button = button.clone();
            gtk::glib::spawn_future_local(async move {
                let result = gtk::gio::spawn_blocking(move || {
                    crate::registry_controller::set_server_secret(&server_id, &key, &secret)
                })
                .await;
                finish_credential_update(result, &page, &editor, &feedback, &button, "Updated");
            });
        });

        let server_id = server.id.clone();
        let key_for_remove = key.clone();
        let feedback_for_remove = feedback.clone();
        let page_for_remove = page.clone();
        let editor_for_remove = editor.clone();
        remove.connect_clicked(move |button| {
            confirm_remove_credential(
                &server_id,
                &key_for_remove,
                page_for_remove.clone(),
                editor_for_remove.clone(),
                feedback_for_remove.clone(),
                button.clone(),
            );
        });
    }
    content.append(&stored);

    let add_section = gtk::Box::new(gtk::Orientation::Vertical, 10);
    add_section.add_css_class("toolport-form-section");
    add_section.append(&section_heading(
        "Add credential",
        "Use the environment variable name expected by this server.",
    ));
    let add_row = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let new_key = gtk::Entry::builder()
        .placeholder_text("VARIABLE_NAME")
        .css_classes(["toolport-input"])
        .build();
    let new_value = gtk::PasswordEntry::builder()
        .placeholder_text("Secret value")
        .show_peek_icon(true)
        .hexpand(true)
        .css_classes(["toolport-input"])
        .build();
    let add = gtk::Button::with_label("Store");
    add.add_css_class("suggested-action");
    add_row.append(&new_key);
    add_row.append(&new_value);
    let add_actions = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    add_actions.set_halign(gtk::Align::End);
    add_actions.append(&add);
    add_row.append(&add_actions);
    add_section.append(&add_row);
    content.append(&add_section);

    let server_id = server.id.clone();
    let feedback_for_add = feedback.clone();
    let page_for_add = page.clone();
    let editor_for_add = editor.clone();
    add.connect_clicked(move |button| {
        let key = new_key.text().to_string();
        let secret = new_value.text().to_string();
        if key.trim().is_empty() || secret.is_empty() {
            feedback_for_add.set_label("Enter both a variable name and a secret value.");
            feedback_for_add.set_visible(true);
            return;
        }
        button.set_sensitive(false);
        let server_id = server_id.clone();
        let page = page_for_add.clone();
        let editor = editor_for_add.clone();
        let feedback = feedback_for_add.clone();
        let button = button.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                crate::registry_controller::set_server_secret(&server_id, &key, &secret)
            })
            .await;
            finish_credential_update(result, &page, &editor, &feedback, &button, "Stored");
        });
    });

    let clamp = adw::Clamp::builder()
        .maximum_size(680)
        .tightening_threshold(520)
        .child(&content)
        .build();
    scroller.set_child(Some(&clamp));
    root.append(&scroller);
    editor.set_content(Some(&root));
    let editor_for_done = editor.clone();
    done.connect_clicked(move |_| editor_for_done.close());
    editor.present();
}

fn confirm_remove_credential(
    server_id: &str,
    key: &str,
    page: ServerPage,
    editor: adw::Window,
    feedback: gtk::Label,
    button: gtk::Button,
) {
    #[allow(deprecated)]
    let dialog = adw::MessageDialog::new(
        Some(&editor),
        Some(&format!("Remove {key}?")),
        Some("The stored value will be removed from the system keychain."),
    );
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("remove", "Remove");
    dialog.set_close_response("cancel");
    dialog.set_default_response(Some("cancel"));
    dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
    let server_id = server_id.to_string();
    let key = key.to_string();
    dialog.connect_response(None, move |dialog, response| {
        if response != "remove" {
            dialog.close();
            return;
        }
        button.set_sensitive(false);
        let server_id = server_id.clone();
        let key = key.clone();
        let page = page.clone();
        let editor = editor.clone();
        let feedback = feedback.clone();
        let button = button.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                crate::registry_controller::delete_server_secret(&server_id, &key)
            })
            .await;
            finish_credential_update(result, &page, &editor, &feedback, &button, "Removed");
        });
        dialog.close();
    });
    dialog.present();
}

fn finish_credential_update(
    result: Result<
        Result<crate::registry::Registry, String>,
        Box<dyn std::any::Any + Send + 'static>,
    >,
    page: &ServerPage,
    editor: &adw::Window,
    feedback: &gtk::Label,
    button: &gtk::Button,
    action: &str,
) {
    match result {
        Ok(Ok(registry)) => {
            page.render(state::RegistryState::Ready(
                state::RegistrySnapshot::from_registry(registry),
            ));
            page.show_feedback(&format!("{action} credential"), false);
            editor.close();
        }
        Ok(Err(error)) => {
            feedback.set_label(&format!("Could not update credential: {error}"));
            feedback.set_visible(true);
            button.set_sensitive(true);
        }
        Err(_) => {
            feedback.set_label("Could not update credential: the operation stopped");
            feedback.set_visible(true);
            button.set_sensitive(true);
        }
    }
}

fn open_server_editor(server: Option<state::ServerView>, page: ServerPage) {
    let server_id = server.as_ref().map(|server| server.id.clone());
    open_server_editor_prefilled(server, server_id, page)
}

/// Duplicate-for-another-account: the Add editor prefilled from an existing
/// server under the next free "Name (N)", saved as a separate entry.
fn open_duplicate_server_editor(server: &state::ServerView, page: ServerPage) {
    let existing: Vec<String> = page
        .last_snapshot
        .borrow()
        .as_ref()
        .map(|snapshot| {
            snapshot
                .servers
                .iter()
                .map(|server| server.name.clone())
                .collect()
        })
        .unwrap_or_default();
    let mut copy = server.clone();
    copy.name = duplicate_server_name(&server.name, &existing);
    open_server_editor_prefilled(Some(copy), None, page)
}

/// The next free "Name (N)" among existing names, case-insensitively; an
/// existing "(N)" suffix on the source is treated as the base, not doubled.
fn duplicate_server_name(name: &str, existing: &[String]) -> String {
    let base = match name.rfind(" (") {
        Some(start)
            if name.ends_with(')')
                && name[start + 2..name.len() - 1]
                    .chars()
                    .all(|c| c.is_ascii_digit())
                && !name[start + 2..name.len() - 1].is_empty() =>
        {
            &name[..start]
        }
        _ => name,
    };
    let taken: Vec<String> = existing.iter().map(|name| name.to_lowercase()).collect();
    let mut index = 2usize;
    loop {
        let candidate = format!("{base} ({index})");
        if !taken.contains(&candidate.to_lowercase()) {
            return candidate;
        }
        index += 1;
    }
}

fn open_server_editor_prefilled(
    server: Option<state::ServerView>,
    server_id: Option<String>,
    page: ServerPage,
) {
    let Some(parent) = page.app.active_window() else {
        return;
    };
    let editing = server_id.is_some();
    let editor = adw::Window::builder()
        .application(&page.app)
        .transient_for(&parent)
        .modal(true)
        .title(if editing { "Edit server" } else { "Add server" })
        .default_width(640)
        .default_height(700)
        .build();
    editor.add_css_class("toolport-editor");

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = adw::HeaderBar::new();
    let cancel = gtk::Button::with_label("Cancel");
    cancel.add_css_class("toolport-secondary-action");
    let test = gtk::Button::with_label("Test connection");
    test.add_css_class("toolport-secondary-action");
    let save = gtk::Button::with_label(if editing { "Save" } else { "Add" });
    save.add_css_class("suggested-action");
    header.pack_start(&cancel);
    header.pack_start(&test);
    header.pack_end(&save);
    root.append(&header);

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();
    let form = gtk::Box::new(gtk::Orientation::Vertical, 16);
    form.add_css_class("toolport-editor-body");
    form.append(&editor_intro(
        if editing {
            "document-edit-symbolic"
        } else {
            "list-add-symbolic"
        },
        if editing {
            "Edit server"
        } else {
            "Add a server"
        },
        "Connect a local command or a remote MCP endpoint to every Toolport client.",
    ));

    let feedback = gtk::Label::builder()
        .halign(gtk::Align::Start)
        .wrap(true)
        .visible(false)
        .css_classes(["toolport-feedback", "error"])
        .build();
    form.append(&feedback);

    // Pasted env values wait here until the add commits; the entry itself never
    // carries secret values, they go straight to the keychain on save.
    let snippet_env: std::rc::Rc<std::cell::RefCell<Vec<(String, Option<String>)>>> =
        std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));

    let name = gtk::Entry::builder()
        .text(
            server
                .as_ref()
                .map(|server| server.name.as_str())
                .unwrap_or(""),
        )
        .hexpand(true)
        .placeholder_text("My MCP server")
        .css_classes(["toolport-input"])
        .build();
    let identity = gtk::Box::new(gtk::Orientation::Vertical, 10);
    identity.add_css_class("toolport-form-section");
    identity.append(&section_heading(
        "Identity",
        "Choose a clear name that will make sense in every AI client.",
    ));
    identity.append(&editor_field("Server name", &name));
    if !editing {
        // Soft warning only: multiple accounts under one name are legitimate,
        // the user just needs to know a second entry is what they will get.
        let duplicate_warning = gtk::Label::builder()
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .wrap(true)
            .visible(false)
            .css_classes(["toolport-muted", "caption"])
            .build();
        identity.append(&duplicate_warning);
        let existing: Vec<String> = page
            .last_snapshot
            .borrow()
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .servers
                    .iter()
                    .map(|server| server.name.to_lowercase())
                    .collect()
            })
            .unwrap_or_default();
        name.connect_changed(move |name| {
            let typed = name.text().trim().to_lowercase();
            let duplicate = !typed.is_empty() && existing.contains(&typed);
            if duplicate {
                duplicate_warning.set_label(&format!(
                    "Another server is already named \"{}\". That's fine for multiple accounts; it'll be saved as a separate entry.",
                    name.text().trim()
                ));
            }
            duplicate_warning.set_visible(duplicate);
        });
    }
    form.append(&identity);

    let transport = gtk::DropDown::from_strings(&["Local stdio", "Remote HTTP", "Remote SSE"]);
    transport.add_css_class("toolport-input");
    transport.set_selected(
        match server.as_ref().map(|server| server.transport_id.as_str()) {
            Some("http") => 1,
            Some("sse") => 2,
            _ => 0,
        },
    );
    let connection = gtk::Box::new(gtk::Orientation::Vertical, 12);
    connection.add_css_class("toolport-form-section");
    connection.append(&section_heading(
        "Connection",
        "Toolport will validate these fields before saving.",
    ));
    connection.append(&editor_field("Transport", &transport));

    let command = gtk::Entry::builder()
        .text(
            server
                .as_ref()
                .and_then(|server| server.command.as_deref())
                .unwrap_or(""),
        )
        .placeholder_text("npx")
        .hexpand(true)
        .css_classes(["toolport-input"])
        .build();
    let command_row = editor_field("Command", &command);
    connection.append(&command_row);

    let args = gtk::TextView::new();
    args.set_wrap_mode(gtk::WrapMode::None);
    args.set_monospace(true);
    args.set_top_margin(8);
    args.set_bottom_margin(8);
    args.set_left_margin(8);
    args.set_right_margin(8);
    args.buffer().set_text(
        &server
            .as_ref()
            .map(|server| server.args.join("\n"))
            .unwrap_or_default(),
    );
    let args_scroller = gtk::ScrolledWindow::builder()
        .child(&args)
        .min_content_height(96)
        .build();
    args_scroller.add_css_class("toolport-text-area");
    let args_row = editor_field("Arguments, one per line", &args_scroller);
    connection.append(&args_row);

    let cwd = gtk::Entry::builder()
        .text(
            server
                .as_ref()
                .and_then(|server| server.cwd.as_deref())
                .unwrap_or(""),
        )
        .placeholder_text("Optional working directory")
        .hexpand(true)
        .css_classes(["toolport-input"])
        .build();
    let cwd_row = editor_field("Working directory", &cwd);
    connection.append(&cwd_row);

    let url = gtk::Entry::builder()
        .text(
            server
                .as_ref()
                .and_then(|server| server.url.as_deref())
                .unwrap_or(""),
        )
        .placeholder_text("https://example.com/mcp")
        .hexpand(true)
        .css_classes(["toolport-input"])
        .build();
    let url_row = editor_field("Server URL", &url);
    connection.append(&url_row);
    form.append(&connection);

    let credentials = gtk::Label::builder()
        .label(if editing {
            "Existing credentials, tool settings, and newer configuration fields are preserved. Use the server card's Credentials action to add or replace secret values."
        } else {
            "Save the server first, then use its Credentials action to store secret values in the system keychain."
        })
        .halign(gtk::Align::Start)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["toolport-editor-note", "caption"])
        .build();
    form.append(&credentials);

    if !editing {
        let paste = gtk::Box::new(gtk::Orientation::Vertical, 10);
        paste.add_css_class("toolport-form-section");
        paste.append(&section_heading(
            "Start from a pasted config",
            "Paste a JSON, TOML, or YAML MCP snippet, or a claude mcp add command, and Toolport fills the fields.",
        ));
        let snippet = gtk::TextView::new();
        snippet.set_monospace(true);
        snippet.set_wrap_mode(gtk::WrapMode::WordChar);
        snippet.set_top_margin(8);
        snippet.set_bottom_margin(8);
        snippet.set_left_margin(8);
        snippet.set_right_margin(8);
        let snippet_scroller = gtk::ScrolledWindow::builder()
            .child(&snippet)
            .min_content_height(88)
            .build();
        snippet_scroller.add_css_class("toolport-text-area");
        paste.append(&snippet_scroller);
        let fill = gtk::Button::with_label("Fill from snippet");
        fill.add_css_class("toolport-secondary-action");
        fill.set_halign(gtk::Align::Start);
        paste.append(&fill);
        form.insert_child_after(&paste, Some(&feedback));

        let feedback_for_fill = feedback.clone();
        let name_for_fill = name.clone();
        let transport_for_fill = transport.clone();
        let command_for_fill = command.clone();
        let args_for_fill = args.clone();
        let url_for_fill = url.clone();
        let cwd_for_fill = cwd.clone();
        let env_for_fill = snippet_env.clone();
        fill.connect_clicked(move |_| {
            let buffer = snippet.buffer();
            let text = buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), false)
                .to_string();
            const MAX_SNIPPET_BYTES: usize = 256 * 1024;
            let parsed = if text.len() > MAX_SNIPPET_BYTES {
                Err(format!(
                    "Snippet is {} KB; limit is {} KB. Paste a single server config, not an entire file.",
                    text.len() / 1024,
                    MAX_SNIPPET_BYTES / 1024,
                ))
            } else {
                crate::clients::parse_snippet(&text)
            };
            feedback_for_fill.set_visible(true);
            match parsed {
                Ok(servers) => {
                    let Some(first) = servers.first() else {
                        feedback_for_fill.set_label("No servers found in the pasted config.");
                        feedback_for_fill.remove_css_class("success");
                        feedback_for_fill.add_css_class("error");
                        return;
                    };
                    name_for_fill.set_text(&first.name);
                    transport_for_fill.set_selected(match first.transport.as_str() {
                        "http" => 1,
                        "sse" => 2,
                        _ => 0,
                    });
                    command_for_fill.set_text(first.command.as_deref().unwrap_or(""));
                    args_for_fill.buffer().set_text(&first.args.join("\n"));
                    url_for_fill.set_text(first.url.as_deref().unwrap_or(""));
                    cwd_for_fill.set_text("");
                    *env_for_fill.borrow_mut() = first
                        .env
                        .iter()
                        .map(|env| (env.key.clone(), env.value.clone()))
                        .collect();
                    feedback_for_fill.set_label(&snippet_fill_feedback(
                        &first.name,
                        servers.len(),
                        first.env.len(),
                    ));
                    feedback_for_fill.remove_css_class("error");
                    feedback_for_fill.add_css_class("success");
                }
                Err(error) => {
                    feedback_for_fill.set_label(&format!("Could not parse the snippet: {error}"));
                    feedback_for_fill.remove_css_class("success");
                    feedback_for_fill.add_css_class("error");
                }
            }
        });
    }

    update_editor_transport(
        transport.selected(),
        &command_row,
        &args_row,
        &cwd_row,
        &url_row,
    );
    let command_row_for_transport = command_row.clone();
    let args_row_for_transport = args_row.clone();
    let cwd_row_for_transport = cwd_row.clone();
    let url_row_for_transport = url_row.clone();
    transport.connect_selected_notify(move |transport| {
        update_editor_transport(
            transport.selected(),
            &command_row_for_transport,
            &args_row_for_transport,
            &cwd_row_for_transport,
            &url_row_for_transport,
        )
    });

    let clamp = adw::Clamp::builder()
        .maximum_size(720)
        .tightening_threshold(520)
        .child(&form)
        .build();
    scroller.set_child(Some(&clamp));
    root.append(&scroller);
    editor.set_content(Some(&root));

    let editor_for_cancel = editor.clone();
    cancel.connect_clicked(move |_| editor_for_cancel.close());

    let server_id_for_test = server_id.clone();
    let feedback_for_test = feedback.clone();
    let name_for_test = name.clone();
    let transport_for_test = transport.clone();
    let command_for_test = command.clone();
    let args_for_test = args.clone();
    let url_for_test = url.clone();
    let cwd_for_test = cwd.clone();
    test.connect_clicked(move |button| {
        button.set_sensitive(false);
        feedback_for_test.set_label("Testing connection…");
        feedback_for_test.remove_css_class("error");
        feedback_for_test.add_css_class("success");
        feedback_for_test.set_visible(true);
        let fields = collect_server_fields(
            &name_for_test,
            &transport_for_test,
            &command_for_test,
            &args_for_test,
            &url_for_test,
            &cwd_for_test,
        );
        let server_id = server_id_for_test.clone();
        let feedback = feedback_for_test.clone();
        let button = button.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                let entry = crate::registry_controller::server_entry_for_probe(
                    server_id.as_deref(),
                    fields,
                )?;
                Ok::<_, String>(crate::server_runtime::probe_one_bounded(&entry))
            })
            .await;
            button.set_sensitive(true);
            match result {
                Ok(Ok(probe)) if probe.ok => {
                    feedback.set_label(&format!(
                        "Connected successfully and found {} {}.",
                        probe.tool_count,
                        if probe.tool_count == 1 {
                            "tool"
                        } else {
                            "tools"
                        }
                    ));
                    feedback.remove_css_class("error");
                    feedback.add_css_class("success");
                }
                Ok(Ok(probe)) if probe.auth_required => {
                    feedback.set_label("The server responded but needs credentials.");
                    feedback.remove_css_class("success");
                    feedback.add_css_class("error");
                }
                Ok(Ok(probe)) => {
                    feedback.set_label(&format!(
                        "Connection failed: {}",
                        probe.error.as_deref().unwrap_or("unknown connection error")
                    ));
                    feedback.remove_css_class("success");
                    feedback.add_css_class("error");
                }
                Ok(Err(error)) => {
                    feedback.set_label(&format!("Could not test: {error}"));
                    feedback.remove_css_class("success");
                    feedback.add_css_class("error");
                }
                Err(_) => {
                    feedback.set_label("Could not test: the operation stopped");
                    feedback.remove_css_class("success");
                    feedback.add_css_class("error");
                }
            }
        });
    });

    let editor_for_save = editor.clone();
    save.connect_clicked(move |save| {
        save.set_sensitive(false);
        feedback.set_visible(false);
        let server_id = server_id.clone();
        let fields = collect_server_fields(&name, &transport, &command, &args, &url, &cwd);
        let display_name = fields.name.trim().to_string();
        let env = snippet_env.borrow().clone();
        let page = page.clone();
        let save = save.clone();
        let feedback = feedback.clone();
        let editor = editor_for_save.clone();
        gtk::glib::spawn_future_local(async move {
            let update = gtk::gio::spawn_blocking(move || match server_id {
                Some(server_id) => {
                    let registry =
                        crate::registry_controller::update_server_fields(&server_id, fields)?;
                    Ok((registry, Vec::new(), Vec::new()))
                }
                None if env.is_empty() => {
                    let registry = crate::registry_controller::add_server(fields)?;
                    Ok((registry, Vec::new(), Vec::new()))
                }
                None => {
                    let outcome = crate::registry_controller::add_snippet_server(fields, env)?;
                    Ok::<_, String>((
                        outcome.registry,
                        outcome.declared_without_value,
                        outcome.failed,
                    ))
                }
            })
            .await;
            match update {
                Ok(Ok((registry, declared_without_value, failed_env))) => {
                    page.render(state::RegistryState::Ready(
                        state::RegistrySnapshot::from_registry(registry),
                    ));
                    page.show_feedback(
                        &server_saved_feedback(
                            editing,
                            &display_name,
                            &declared_without_value,
                            &failed_env,
                        ),
                        !failed_env.is_empty(),
                    );
                    editor.close();
                }
                Ok(Err(error)) => {
                    feedback.set_label(&format!("Could not save: {error}"));
                    feedback.set_visible(true);
                    save.set_sensitive(true);
                }
                Err(_) => {
                    feedback.set_label("Could not save: the operation stopped");
                    feedback.set_visible(true);
                    save.set_sensitive(true);
                }
            }
        });
    });
    editor.present();
}

fn editor_field(label: &str, child: &impl IsA<gtk::Widget>) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 6);
    row.append(
        &gtk::Label::builder()
            .label(label)
            .halign(gtk::Align::Start)
            .css_classes(["toolport-field-label"])
            .build(),
    );
    row.append(child);
    row
}

/// What the editor reports after filling the form from a pasted snippet.
fn snippet_fill_feedback(name: &str, server_count: usize, env_count: usize) -> String {
    let mut message = if server_count > 1 {
        format!("Found {server_count} servers; filled \"{name}\". Add the rest separately.")
    } else {
        format!("Parsed \"{name}\" from the snippet.")
    };
    if env_count > 0 {
        message.push_str(&format!(
            " {env_count} credential {} will be stored in the system keychain when you add the server.",
            if env_count == 1 { "value" } else { "values" }
        ));
    }
    message
}

/// The page feedback after a server save, including what happened to pasted env
/// values. A failed key is worth an error tone: the server exists but a
/// credential the user pasted did not make it into the keychain.
fn server_saved_feedback(
    editing: bool,
    display_name: &str,
    declared_without_value: &[String],
    failed_env: &[String],
) -> String {
    let mut message = format!(
        "{} {display_name}",
        if editing { "Updated" } else { "Added" }
    );
    if !declared_without_value.is_empty() {
        message.push_str(&format!(
            ". Add a value for {} in Credentials",
            declared_without_value.join(", ")
        ));
    }
    if !failed_env.is_empty() {
        message.push_str(&format!(
            ". Could not store {}; use Credentials to add {}",
            failed_env.join(", "),
            if failed_env.len() == 1 { "it" } else { "them" }
        ));
    }
    message
}

fn collect_server_fields(
    name: &gtk::Entry,
    transport: &gtk::DropDown,
    command: &gtk::Entry,
    args: &gtk::TextView,
    url: &gtk::Entry,
    cwd: &gtk::Entry,
) -> crate::registry_controller::ServerFields {
    crate::registry_controller::ServerFields {
        name: name.text().to_string(),
        transport: match transport.selected() {
            1 => "http",
            2 => "sse",
            _ => "stdio",
        }
        .into(),
        command: Some(command.text().to_string()),
        args: args
            .buffer()
            .text(
                &args.buffer().start_iter(),
                &args.buffer().end_iter(),
                false,
            )
            .lines()
            .map(str::trim)
            .filter(|argument| !argument.is_empty())
            .map(ToOwned::to_owned)
            .collect(),
        url: Some(url.text().to_string()),
        cwd: Some(cwd.text().to_string()),
    }
}

fn editor_intro(icon: &str, title: &str, subtitle: &str) -> gtk::Box {
    let intro = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    intro.add_css_class("toolport-editor-intro");
    let icon = gtk::Image::from_icon_name(icon);
    icon.add_css_class("toolport-editor-icon");
    intro.append(&icon);
    let copy = gtk::Box::new(gtk::Orientation::Vertical, 3);
    copy.set_hexpand(true);
    copy.append(
        &gtk::Label::builder()
            .label(title)
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["title-2"])
            .build(),
    );
    copy.append(
        &gtk::Label::builder()
            .label(subtitle)
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-muted"])
            .build(),
    );
    intro.append(&copy);
    intro
}

fn section_heading(title: &str, subtitle: &str) -> gtk::Box {
    let heading = gtk::Box::new(gtk::Orientation::Vertical, 2);
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
            .css_classes(["toolport-muted", "caption"])
            .build(),
    );
    heading
}

fn update_editor_transport(
    selected: u32,
    command: &gtk::Box,
    args: &gtk::Box,
    cwd: &gtk::Box,
    url: &gtk::Box,
) {
    let stdio = selected == 0;
    command.set_visible(stdio);
    args.set_visible(stdio);
    cwd.set_visible(stdio);
    url.set_visible(!stdio);
}

fn confirm_remove_server(server_id: &str, server_name: &str, page: ServerPage) {
    let Some(parent) = page.app.active_window() else {
        return;
    };
    #[allow(deprecated)]
    let dialog = adw::MessageDialog::new(
        Some(&parent),
        Some(&format!("Remove {server_name}?")),
        Some("This removes the server from every profile. This cannot be undone."),
    );
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("remove", "Remove");
    dialog.set_close_response("cancel");
    dialog.set_default_response(Some("cancel"));
    dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
    let server_id = server_id.to_string();
    let server_name = server_name.to_string();
    dialog.connect_response(None, move |dialog, response| {
        if response != "remove" {
            dialog.close();
            return;
        }
        let page = page.clone();
        let server_id = server_id.clone();
        let server_name = server_name.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                crate::registry_controller::remove_server(&server_id)
            })
            .await;
            match result {
                Ok(Ok(registry)) => {
                    page.render(state::RegistryState::Ready(
                        state::RegistrySnapshot::from_registry(registry),
                    ));
                    page.show_feedback(&format!("Removed {server_name}"), false);
                }
                Ok(Err(error)) => {
                    page.restore_after_error(&format!("Could not remove {server_name}: {error}"));
                }
                Err(_) => page.restore_after_error(&format!(
                    "Could not remove {server_name}: the operation stopped"
                )),
            }
        });
        dialog.close();
    });
    dialog.present();
}

fn state_card(icon_name: &str, title: &str, body: &str, error: bool) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
    card.add_css_class("toolport-state-card");
    if error {
        card.add_css_class("error");
    }
    let icon = gtk::Image::from_icon_name(icon_name);
    icon.set_pixel_size(32);
    icon.add_css_class("toolport-state-icon");
    card.append(&icon);
    card.append(
        &gtk::Label::builder()
            .label(title)
            .wrap(true)
            .justify(gtk::Justification::Center)
            .css_classes(["heading"])
            .build(),
    );
    card.append(
        &gtk::Label::builder()
            .label(body)
            .wrap(true)
            .justify(gtk::Justification::Center)
            .max_width_chars(52)
            .css_classes(["toolport-muted"])
            .build(),
    );
    card
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(server: &str, ok: bool) -> state::ActivityView {
        state::ActivityView {
            timestamp_ms: 0,
            server: server.to_string(),
            tool: "tool".to_string(),
            client: None,
            ok,
            held: false,
            duration_ms: None,
            error: None,
            pii_replaced: None,
            pii_incomplete: false,
        }
    }

    #[test]
    fn call_filters_compose_server_and_errors_only() {
        let calls = [
            call("github", true),
            call("github", false),
            call("jira", false),
        ];
        assert_eq!(filter_calls(&calls, None, false).len(), 3);
        assert_eq!(filter_calls(&calls, Some("github"), false).len(), 2);
        assert_eq!(filter_calls(&calls, Some("github"), true).len(), 1);
        assert_eq!(filter_calls(&calls, None, true).len(), 2);
        assert_eq!(filter_calls(&calls, Some("missing"), false).len(), 0);
    }

    #[test]
    fn identity_search_matches_alias_server_and_upstream() {
        let identity = |alias: &str, server: &str, upstream: &str| crate::integrity::ToolIdentity {
            alias: alias.to_string(),
            server_id: String::new(),
            server_name: server.to_string(),
            profiles: Vec::new(),
            upstream: upstream.to_string(),
            fingerprint: String::new(),
            first_seen: 0,
            last_changed: 0,
            quarantined: false,
        };
        let identities = [
            identity("github__create_issue", "GitHub", "create_issue"),
            identity("jira__search", "Jira", "search"),
        ];
        assert_eq!(filter_tool_identities(&identities, "").len(), 2);
        assert_eq!(filter_tool_identities(&identities, "github").len(), 1);
        assert_eq!(filter_tool_identities(&identities, "search").len(), 1);
        assert_eq!(filter_tool_identities(&identities, "jira").len(), 1);
        assert_eq!(filter_tool_identities(&identities, "nothing").len(), 0);
    }

    #[test]
    fn line_diffs_mark_removals_additions_and_context() {
        let diff = line_diff("a\nb\nc", "a\nx\nc\nd");
        assert_eq!(
            diff,
            vec![
                (' ', "a".to_string()),
                ('-', "b".to_string()),
                ('+', "x".to_string()),
                (' ', "c".to_string()),
                ('+', "d".to_string()),
            ]
        );
        assert!(line_diff("same", "same")
            .iter()
            .all(|(marker, _)| *marker == ' '));
    }

    #[test]
    fn refused_writes_name_their_reason_and_ordinary_states_stay_quiet() {
        use crate::instructions::ApplyState;
        assert!(refused_write_reason(ApplyState::BlockedOverride).is_some());
        assert!(refused_write_reason(ApplyState::TooLong).is_some());
        assert!(refused_write_reason(ApplyState::Error).is_some());
        assert_eq!(refused_write_reason(ApplyState::Applied), None);
        assert_eq!(refused_write_reason(ApplyState::Stale), None);
        assert_eq!(refused_write_reason(ApplyState::Drifted), None);
    }

    #[test]
    fn duplicate_names_find_the_next_free_slot_and_never_double_suffix() {
        let existing = vec!["GitHub".to_string(), "GitHub (2)".to_string()];
        assert_eq!(duplicate_server_name("GitHub", &existing), "GitHub (3)");
        assert_eq!(duplicate_server_name("GitHub (2)", &existing), "GitHub (3)");
        assert_eq!(duplicate_server_name("Jira", &existing), "Jira (2)");
        assert_eq!(
            duplicate_server_name("odd (name", &existing),
            "odd (name (2)"
        );
    }

    #[test]
    fn savings_lines_price_detail_and_share_read_like_the_shipping_banner() {
        assert_eq!(
            savings_dollar_line(2_000_000, 1),
            "≈ $6.00 at Claude Sonnet input prices"
        );
        assert_eq!(
            savings_dollar_line(2_000_000, 999),
            "≈ $6.00 at Claude Sonnet input prices"
        );
        assert_eq!(
            savings_detail_line(12, 80, Some("Mar 4".to_string())),
            "across 12 tool-list loads · biggest catalog collapsed 80 tools to a handful · since Mar 4"
        );
        assert_eq!(savings_detail_line(1, 3, None), "across 1 tool-list load");
        assert_eq!(
            savings_share_line(41_100),
            "Toolport keeps ~41.1k tokens of MCP tool definitions out of my agent's context \
             so far. One local gateway for all my MCP servers: toolport.app"
        );
    }

    #[test]
    fn trace_rows_summarize_expand_and_do_the_token_math() {
        let trace = serde_json::json!({
            "query": "issues",
            "returned": 2,
            "total": 40,
            "client": "claude",
            "mode": "semantic",
            "top": "github__search_issues",
            "ranking": [
                {"rank": 1, "name": "github__search_issues", "matched": ["issues"]},
                {"rank": 2, "name": "jira__search", "pinned": true},
                {"rank": 3, "name": "linear__list", "fallback": true},
            ],
            "returnedTokens": 900,
            "flatTokens": 42000,
            "savedTokens": 41100,
        });
        assert_eq!(
            trace_summary_line(&trace),
            "\u{201c}issues\u{201d} · 40 direct + 1 fallback · claude · semantic"
        );
        assert_eq!(
            trace_ranking_lines(&trace),
            vec![
                "#1 github__search_issues (top) · matched issues",
                "#2 jira__search · pinned prerequisite",
                "#3 linear__list · fallback candidate",
            ]
        );
        assert_eq!(
            trace_token_line(&trace),
            "Put ≈900 tokens of tool schemas into context, vs ≈42.0k to load the whole \
             catalog (97% less this turn)."
        );
        let miss = serde_json::json!({ "query": "nothing", "returned": 0, "total": 12 });
        assert_eq!(
            trace_summary_line(&miss),
            "\u{201c}nothing\u{201d} · no match"
        );
        assert!(trace_ranking_lines(&miss).is_empty());
    }

    #[test]
    fn pii_badges_stay_silent_until_the_pass_did_something_and_warn_on_fail_open() {
        assert_eq!(pii_badge(None, false), None);
        assert_eq!(pii_badge(Some(0), false), None);
        let (label, class, _) = pii_badge(Some(3), false).unwrap();
        assert_eq!((label.as_str(), class), ("3 pseudonymized", "success"));
        let (label, class, _) = pii_badge(Some(2), true).unwrap();
        assert_eq!(
            (label.as_str(), class),
            ("2 pseudonymized, incomplete", "review")
        );
        // Fail-open with zero replacements still warns: values reached the model.
        let (label, class, _) = pii_badge(Some(0), true).unwrap();
        assert_eq!(
            (label.as_str(), class),
            ("0 pseudonymized, incomplete", "review")
        );
    }

    #[test]
    fn window_state_round_trips_and_rejects_degenerate_sizes() {
        let state = WindowState {
            width: 1280,
            height: 800,
            maximized: true,
        };
        let encoded = serde_json::to_string(&state).unwrap();
        assert_eq!(parse_window_state(&encoded), Some(state));
        assert_eq!(
            parse_window_state("{\"width\":10,\"height\":800,\"maximized\":false}"),
            None
        );
        assert_eq!(parse_window_state("not json"), None);
    }

    #[test]
    fn approval_notifications_name_browser_handoffs_for_what_they_are() {
        let mut view = crate::approval_broker::PendingView {
            id: "1".into(),
            client: Some("claude".into()),
            server: "github".into(),
            tool: "create_issue".into(),
            tool_fingerprint: None,
            reason: crate::approval::ApprovalReason::Destructive,
            arguments: serde_json::json!({}),
            url_elicitation: None,
            pii_release: None,
            deadline_ms: 0,
        };
        assert_eq!(
            approval_notification(&view),
            (
                "Toolport: approval required".to_string(),
                "claude wants to run github / create_issue - approve or deny it in Toolport."
                    .to_string()
            )
        );
        view.url_elicitation = Some(crate::approval::UrlElicitationRequest {
            url: "https://example.com/auth".into(),
            origin: "github".into(),
            message: "sign in".into(),
        });
        assert_eq!(
            approval_notification(&view),
            (
                "Toolport: browser action required".to_string(),
                "github requested an external browser interaction. Review it in Toolport."
                    .to_string()
            )
        );
    }

    #[test]
    fn restart_advice_names_each_app_once() {
        let client = |name: &str, pid: u32| crate::gateway_publish::ClientNeedingRestart {
            client: name.to_string(),
            client_pid: pid,
            gateway: "toolport-gateway-1.16.0".to_string(),
        };
        assert_eq!(
            restart_advice_line(&[
                client("claude", 1),
                client("cursor", 2),
                client("claude", 3)
            ]),
            "2 apps are still launching an old Toolport gateway. Restart them to finish the \
             upgrade: claude, cursor."
        );
        assert_eq!(
            restart_advice_line(&[client("zed", 9)]),
            "1 app is still launching an old Toolport gateway. Restart it to finish the \
             upgrade: zed."
        );
    }

    #[test]
    fn posture_lines_cover_checking_healthy_and_mixed_states() {
        assert_eq!(
            posture_line(0, 0, 0, 3, 3),
            "Checking 3 of 3 enabled servers…"
        );
        assert_eq!(
            posture_line(4, 0, 0, 0, 4),
            "All 4 enabled servers are ready."
        );
        assert_eq!(
            posture_line(1, 0, 0, 0, 1),
            "All 1 enabled server is ready."
        );
        assert_eq!(
            posture_line(2, 1, 1, 0, 4),
            "2 ready · 1 needs sign-in · 1 failing"
        );
    }

    #[test]
    fn probe_lines_distinguish_ready_auth_and_failure() {
        let probe = |ok: bool, tools: usize, auth: bool| crate::server_runtime::ProbeResult {
            server_id: "s".into(),
            ok,
            tool_count: tools,
            error: None,
            auth_required: auth,
        };
        assert_eq!(
            probe_status_line(&probe(true, 1, false)),
            ("Ready · 1 tool".to_string(), "success")
        );
        assert_eq!(
            probe_status_line(&probe(false, 0, true)),
            ("Needs sign-in".to_string(), "review")
        );
        assert_eq!(
            probe_status_line(&probe(false, 0, false)),
            ("Error".to_string(), "error")
        );
    }

    #[test]
    fn attention_outranks_ready_and_disabled_sinks() {
        let mut server = server("github", "Remote HTTP");
        server.enabled = true;
        let failing = crate::server_runtime::ProbeResult {
            server_id: "s".into(),
            ok: false,
            tool_count: 0,
            error: Some("boom".into()),
            auth_required: false,
        };
        let ready = crate::server_runtime::ProbeResult {
            server_id: "s".into(),
            ok: true,
            tool_count: 3,
            error: None,
            auth_required: false,
        };
        assert_eq!(server_health_rank(&server, Some(&failing)), 0);
        assert_eq!(server_health_rank(&server, None), 1);
        assert_eq!(server_health_rank(&server, Some(&ready)), 2);
        server.enabled = false;
        assert_eq!(server_health_rank(&server, Some(&ready)), 3);
        server.requires_review = true;
        assert_eq!(server_health_rank(&server, None), 0);
    }

    #[test]
    fn migrate_feedback_reports_moved_imported_and_the_backup() {
        assert_eq!(
            migrate_feedback("Claude Desktop", 2, 3, true),
            "Moved 3 servers into Toolport (2 newly imported). Claude Desktop now uses only \
             the Toolport gateway. The previous config was backed up. Restart the client to \
             pick this up."
        );
        assert_eq!(
            migrate_feedback("Zed", 0, 1, false),
            "Moved 1 server into Toolport (0 newly imported). Zed now uses only the Toolport \
             gateway. Restart the client to pick this up."
        );
    }

    #[test]
    fn auth_probe_summaries_name_the_path_the_server_supports() {
        let info = |kind: &str| crate::vendors::AuthInfo {
            kind: kind.to_string(),
            vendor: None,
            token_url: None,
            instructions: None,
        };
        assert_eq!(
            auth_probe_summary(&info("token")),
            "This server expects a bearer token."
        );
        assert_eq!(
            auth_probe_summary(&info("mystery")),
            "Toolport could not determine what authentication this server wants."
        );
        let vendor = crate::vendors::AuthInfo {
            kind: "token".to_string(),
            vendor: Some("Stripe".to_string()),
            token_url: Some("https://example.com/keys".to_string()),
            instructions: Some("Use a restricted key.".to_string()),
        };
        assert_eq!(
            auth_probe_summary(&vendor),
            "This server expects a bearer token. Detected Stripe. Use a restricted key. \
             Get a token at https://example.com/keys"
        );
    }

    #[test]
    fn stat_lines_only_promise_latency_the_log_actually_carried() {
        let with_latency = serde_json::json!({
            "calls": 12, "errors": 1, "avgMs": 40, "p95Ms": 90
        });
        assert_eq!(
            stat_metrics_line(&with_latency),
            "12 calls · 1 error · avg 40 ms · p95 90 ms"
        );
        let without_latency = serde_json::json!({ "calls": 1, "errors": 0 });
        assert_eq!(stat_metrics_line(&without_latency), "1 call · 0 errors");
    }

    #[test]
    fn fingerprints_lose_their_version_prefix_but_not_their_meaning() {
        assert_eq!(short_fingerprint("v1:abcdef0123456789"), "abcdef012345");
        assert_eq!(short_fingerprint("abcdef0123456789"), "abcdef012345");
        // A colon that is not a version prefix is content, not a marker.
        assert_eq!(short_fingerprint("x1:abc"), "x1:abc");
        assert_eq!(short_fingerprint(""), "-");
        assert_eq!(short_fingerprint("v1:"), "-");
    }

    #[test]
    fn identity_groups_preserve_newest_first_order_and_name_unattributed_rows() {
        let identity =
            |alias: &str, server_name: &str, server_id: &str| crate::integrity::ToolIdentity {
                alias: alias.to_string(),
                server_id: server_id.to_string(),
                server_name: server_name.to_string(),
                profiles: Vec::new(),
                upstream: String::new(),
                fingerprint: String::new(),
                first_seen: 0,
                last_changed: 0,
                quarantined: false,
            };
        let groups = state::group_tool_identities(&[
            identity("b__x", "Beta", "b"),
            identity("a__y", "Alpha", "a"),
            identity("b__z", "Beta", "b"),
            identity("orphan", "", ""),
        ]);
        let names: Vec<&str> = groups.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["Beta", "Alpha", "Unattributed"]);
        assert_eq!(groups[0].1.len(), 2);
        assert_eq!(groups[0].1[0].alias, "b__x");
        assert_eq!(groups[0].1[1].alias, "b__z");
    }

    #[test]
    fn snippet_fill_reports_extra_servers_and_pending_credentials() {
        assert_eq!(
            snippet_fill_feedback("github", 1, 0),
            "Parsed \"github\" from the snippet."
        );
        assert_eq!(
            snippet_fill_feedback("github", 3, 1),
            "Found 3 servers; filled \"github\". Add the rest separately. \
             1 credential value will be stored in the system keychain when you add the server."
        );
    }

    #[test]
    fn a_save_with_unvaulted_keys_tells_the_user_where_to_finish() {
        assert_eq!(
            server_saved_feedback(true, "github", &[], &[]),
            "Updated github"
        );
        assert_eq!(
            server_saved_feedback(
                false,
                "github",
                &["GITHUB_TOKEN".to_string()],
                &["BAD=KEY".to_string()]
            ),
            "Added github. Add a value for GITHUB_TOKEN in Credentials. \
             Could not store BAD=KEY; use Credentials to add it"
        );
    }

    fn server(name: &str, transport: &str) -> state::ServerView {
        state::ServerView {
            id: "server".into(),
            name: name.into(),
            transport: transport.into(),
            transport_id: "stdio".into(),
            command: None,
            args: Vec::new(),
            url: None,
            cwd: None,
            secret_keys: Vec::new(),
            client_credentials: None,
            enabled: true,
            requires_review: false,
        }
    }

    #[test]
    fn server_search_is_case_insensitive_and_matches_transport() {
        let linear = server("Linear Workspace", "Remote HTTP");

        assert!(server_matches_query(&linear, "linear"));
        assert!(server_matches_query(&linear, " HTTP "));
        assert!(server_matches_query(&linear, ""));
        assert!(!server_matches_query(&linear, "local stdio"));
    }
}
