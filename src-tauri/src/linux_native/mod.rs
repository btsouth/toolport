//! Additive GTK4 desktop shell for Linux.
//!
//! This module is feature-gated so the existing Tauri application and every
//! non-Linux build remain unchanged while the native shell is developed.

mod branding;
mod catalog;
mod health;
mod http_bridge;
mod i18n;
mod io;
mod notify;
mod onboarding;
mod package_updates;
mod pairing;
mod settings;
mod setup;
mod single_instance;
mod state;
mod teams;
mod theme;
mod tools;
mod tray;

use self::i18n as l10n;
use adw::prelude::*;
use catalog::CatalogPage;
use gtk::glib::prelude::ToValue;
use settings::SettingsPage;
use teams::TeamsPage;
use tools::ServerToolsPanel;

/// The desktop identity. The GTK shell replaces the Tauri one on Linux rather
/// than sitting beside it (they share `~/.config/Toolport` and only one can hold
/// the approval broker's owner lock), so it carries the plain name.
const APP_ID: &str = "com.tsout.Toolport";
/// The identity the preview shipped under. Kept only so [`migrate_preview_identity`]
/// can clean up what it left in the user's home directory.
const LEGACY_PREVIEW_APP_ID: &str = "com.tsout.Toolport.NativePreview";

pub fn run() {
    let _ = crate::i18n::initialize();
    #[cfg(target_env = "gnu")]
    // SAFETY: this process-wide glibc setting is applied before GTK workers start.
    unsafe {
        libc::mallopt(libc::M_ARENA_MAX, 2);
    }
    let raw_args = std::env::args().collect::<Vec<_>>();
    if let Some(text) = single_instance::information(&raw_args) {
        println!("{text}");
        return;
    }
    let launch_hidden = std::env::args_os().any(|arg| arg == "--hidden");
    let args = std::env::args()
        .filter(|arg| arg != "--hidden")
        .collect::<Vec<_>>();
    // Register before starting the tray, broker, bridge or startup maintenance.
    // An upgraded launch retires the old build before taking over its identity.
    let app = match single_instance::register(APP_ID) {
        Ok(app) => app,
        Err(error) => {
            single_instance::show_failure(&error);
            return;
        }
    };
    if app.is_remote() {
        app.run_with_args(&args);
        return;
    }
    let _hold = app.hold();
    let _language_monitor = l10n::install_language_watch(tray::refresh_language).ok();
    let quit = gtk::gio::SimpleAction::new("quit", None);
    let app_for_quit = app.clone();
    quit.connect_activate(move |_, _| app_for_quit.quit());
    app.add_action(&quit);
    app.set_accels_for_action("app.quit", &["<Primary>q"]);

    let startup = std::rc::Rc::new(std::cell::RefCell::new(
        None::<Result<NativeStartup, String>>,
    ));
    let requested = std::rc::Rc::new(std::cell::Cell::new(false));
    let present = std::rc::Rc::new(std::cell::Cell::new(!launch_hidden));
    let urls = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
    let tray = std::rc::Rc::new(std::cell::RefCell::new(None));
    let activation = std::rc::Rc::new(NativeActivation {
        state: startup.clone(),
        requested: requested.clone(),
        present: present.clone(),
        urls: urls.clone(),
        launch_hidden,
        fallback_scheduled: std::cell::Cell::new(false),
    });
    let activation_for_signal = activation.clone();
    app.connect_activate(move |app| activation_for_signal.activate(app, tray::HOST_GRACE));
    let present_for_open = present.clone();
    app.connect_open(move |app, files, _hint| {
        present_for_open.set(true);
        let pair_link = files
            .iter()
            .any(|file| crate::teams::parse_pair_link(file.uri().as_str()).is_some());
        urls.borrow_mut()
            .extend(files.iter().map(|file| file.uri().to_string()));
        let app = app.clone();
        gtk::glib::spawn_future_local(async move {
            if pair_link {
                let _ = gtk::gio::spawn_blocking(onboarding::mark_complete).await;
            }
            app.activate();
        });
    });
    let app_for_startup = app.clone();
    let activation_for_startup = activation.clone();
    let tray_for_startup = tray.clone();
    gtk::glib::spawn_future_local(async move {
        let app_for_events: gtk::glib::SendWeakRef<adw::Application> =
            app_for_startup.downgrade().into();
        let result = gtk::gio::spawn_blocking(move || load_native_startup(app_for_events))
            .await
            .unwrap_or_else(|_| Err(crate::i18n::tr("The startup worker stopped unexpectedly")));
        if result.is_ok() {
            *tray_for_startup.borrow_mut() = tray::start(&app_for_startup);
            if launch_hidden && tray_for_startup.borrow().is_none() {
                present.set(true);
            }
        }
        activation_for_startup.ready(&app_for_startup, result);
    });
    app.connect_shutdown(|_| {
        crate::oauth_controller::cancel_all_attempts();
        crate::telemetry::shutdown();
    });
    app.run_with_args(&args);
    finish_allow_saves();
    if let Some(tray) = tray.borrow_mut().take() {
        tray.shutdown().wait();
    }
    if let Some(Ok(startup)) = startup.borrow_mut().take() {
        for writer in startup.window.writers {
            writer.finish();
        }
        startup.bridge.shutdown();
        startup.broker.clear_endpoint();
    };
}

/// Queue activation while startup runs, then replay it once the result arrives.
/// The same activation handler also handles a first launch arriving after readiness.
struct NativeActivation {
    state: std::rc::Rc<std::cell::RefCell<Option<Result<NativeStartup, String>>>>,
    requested: std::rc::Rc<std::cell::Cell<bool>>,
    present: std::rc::Rc<std::cell::Cell<bool>>,
    urls: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
    launch_hidden: bool,
    fallback_scheduled: std::cell::Cell<bool>,
}

impl NativeActivation {
    fn ready(&self, app: &adw::Application, result: Result<NativeStartup, String>) {
        *self.state.borrow_mut() = Some(result);
        if self.requested.get() {
            // Replaying the first activation must preserve a hidden launch.
            self.requested.set(false);
            app.activate();
        }
    }

    fn activate(&self, app: &adw::Application, grace: std::time::Duration) {
        // A second activation (including a tray click) must reveal even a hidden launch.
        if self.requested.replace(true) {
            self.present.set(true);
        }
        let mut state = self.state.borrow_mut();
        let Some(result) = state.as_mut() else {
            return;
        };
        match result {
            Ok(startup) => {
                build_window(
                    app,
                    theme::ThemeController::new(),
                    startup.broker.clone(),
                    startup.bridge.clone(),
                    &mut startup.window,
                    self.present.get(),
                );
                schedule_hidden_fallback(
                    app,
                    self.launch_hidden,
                    self.present.get(),
                    &self.fallback_scheduled,
                    grace,
                );
                for uri in self.urls.borrow_mut().drain(..) {
                    if let Some(action) = app.lookup_action("open-share-url") {
                        action.activate(Some(&uri.to_variant()));
                    }
                }
            }
            Err(error) => show_registry_startup_failure(app, error),
        }
    }
}

/// Called after the window exists, regardless of startup/activation ordering.
fn schedule_hidden_fallback(
    app: &adw::Application,
    hidden: bool,
    present: bool,
    scheduled: &std::cell::Cell<bool>,
    grace: std::time::Duration,
) {
    if hidden && !present && !scheduled.replace(true) {
        tray::present_unless_host_appears(app, grace);
    }
}

struct WindowStartup {
    paired: bool,
    saved_state: Option<WindowState>,
    removed: Vec<String>,
    access_notice: bool,
    recovery: Option<crate::registry::RegistryRecoveryNotice>,
    writers: Vec<WindowWriter>,
}

/// Writes are ordered while GTK runs and drained after the event loop exits.
struct WindowWriter {
    sender: std::sync::mpsc::Sender<Option<WindowState>>,
    thread: std::thread::JoinHandle<()>,
}

impl WindowWriter {
    fn finish(self) {
        let _ = self.sender.send(None);
        let _ = self.thread.join();
    }
}

struct NativeStartup {
    window: WindowStartup,
    broker: crate::approval_broker::ApprovalBroker,
    bridge: http_bridge::BridgeController,
}

/// Everything that can wait on the registry, disk, broker or endpoint runs on a worker.
fn load_native_startup(
    app: gtk::glib::SendWeakRef<adw::Application>,
) -> Result<NativeStartup, String> {
    let registry = crate::registry::load()?;
    if !registry.live_inspect {
        crate::inspect::clear();
    }
    let window = WindowStartup {
        paired: registry.team.is_some(),
        saved_state: load_window_state(),
        removed: registry.removed_features_notice(),
        access_notice: registry.access_upgrade_notice_pending(),
        recovery: crate::registry::take_recovery_notice(),
        writers: Vec::new(),
    };
    let broker = crate::approval_broker::start_native_with_events(move || {
        let app = app.clone();
        gtk::glib::idle_add_once(move || {
            if let Some(app) = app.upgrade() {
                if let Some(action) = app.lookup_action("approval-queue-changed") {
                    action.activate(None);
                }
            }
        });
    });
    let bridge = http_bridge::BridgeController::default();
    let restore = bridge.clone();
    std::thread::spawn(move || {
        if let Err(error) = restore.restore() {
            eprintln!("toolport: could not restore the native HTTP endpoint: {error}");
        }
    });
    migrate_preview_identity();
    crate::autostart::repair_linux(settings::NATIVE_AUTOSTART_NAME);
    ensure_url_scheme_handlers();
    if !cfg!(debug_assertions) {
        std::thread::spawn(run_startup_maintenance);
    }
    Ok(NativeStartup {
        window,
        broker,
        bridge,
    })
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
        let desktop_path = applications.join(format!("{}.desktop", APP_ID));
        let contents = format!("[Desktop Entry]\nType=Application\nName=Toolport\nExec={} %u\nIcon=toolport\nNoDisplay=true\nMimeType=x-scheme-handler/toolport;x-scheme-handler/conduit;\n", exe.display());
        if std::fs::write(&desktop_path, contents).is_err() {
            return;
        }
        let _ = std::process::Command::new("update-desktop-database")
            .arg(&applications)
            .status();
        for scheme in unhandled {
            let _ = std::process::Command::new("xdg-mime")
                .arg("default")
                .arg(format!("{}.desktop", APP_ID))
                .arg(format!("x-scheme-handler/{scheme}"))
                .status();
        }
        eprintln!(
            "toolport: registered {} as the handler for toolport:// links",
            desktop_path.display()
        );
    });
}

/// Clean up what the preview identity left behind.
///
/// The preview shipped as `com.tsout.Toolport.NativePreview` with its own
/// autostart entry, deliberately separate so it could not repoint the shipping
/// shell's login launch. Now that the GTK shell IS the shipping shell on Linux
/// the two merge, and anything left under the old identity would either launch a
/// binary the package no longer installs or register a second URL handler for
/// the same schemes. Runs once per start; every step is best effort, because a
/// failed cleanup must never stop the app from opening.
fn migrate_preview_identity() {
    let old_autostart = crate::autostart::linux_autostart_file(settings::LEGACY_AUTOSTART_NAME);
    let applications = gtk::glib::user_data_dir().join("applications");
    let stale_handler = applications.join(format!("{}.desktop", LEGACY_PREVIEW_APP_ID));
    let retired = retire_preview_files(old_autostart.as_deref(), &stale_handler, || {
        let _ = crate::autostart::enable_linux(settings::NATIVE_AUTOSTART_NAME);
    });
    if retired.handler {
        let _ = std::process::Command::new("update-desktop-database")
            .arg(&applications)
            .status();
    }
}

/// What [`retire_preview_files`] actually removed.
#[derive(Debug, Default, PartialEq, Eq)]
struct RetiredPreview {
    autostart: bool,
    handler: bool,
}

/// The file work behind [`migrate_preview_identity`], with its paths injected.
///
/// Split out because this deletes files in the user's home directory, and a test
/// that exercised it through `dirs::home_dir()` would delete the developer's.
fn retire_preview_files(
    old_autostart: Option<&std::path::Path>,
    stale_handler: &std::path::Path,
    carry_autostart: impl FnOnce(),
) -> RetiredPreview {
    let mut retired = RetiredPreview::default();
    // The file existing IS the "launch at login" setting, so carry it across
    // before removing rather than silently turning it off.
    if let Some(old) = old_autostart {
        if old.is_file() {
            carry_autostart();
            retired.autostart = std::fs::remove_file(old).is_ok();
        }
    }
    // Leaving the handler registered means two entries claim toolport://, and
    // xdg may pick the stale one.
    if stale_handler.is_file() {
        retired.handler = std::fs::remove_file(stale_handler).is_ok();
    }
    retired
}

fn build_window(
    app: &adw::Application,
    theme: std::rc::Rc<theme::ThemeController>,
    broker: crate::approval_broker::ApprovalBroker,
    bridge: http_bridge::BridgeController,
    startup: &mut WindowStartup,
    present: bool,
) {
    if let Some(window) = app.windows().into_iter().next() {
        if present {
            window.present();
        }
        return;
    }

    let saved_state = &startup.saved_state;
    let paired = startup.paired;
    let startup_notice = std::rc::Rc::new(std::cell::RefCell::new(startup.recovery.take()));
    let window = {
        let widget = adw::ApplicationWindow::builder()
            .application(app)
            .title(crate::i18n::tr("Toolport"))
            .icon_name("toolport")
            .default_width(saved_state.as_ref().map(|s| s.width).unwrap_or(1120))
            .default_height(saved_state.as_ref().map(|s| s.height).unwrap_or(720))
            .resizable(true)
            .hide_on_close(true)
            .build();
        l10n::bind_window_title(&widget, "Toolport");
        widget
    };
    window.add_css_class("toolport-native");
    // First close-to-tray must not read as a quit: say once, ever, that the app
    // keeps running. The marker is shared with the shipping shell so a user who
    // already learned this is not told again. Closing is also the natural moment
    // to remember the geometry the user chose.
    {
        let app_for_hint: gtk::glib::SendWeakRef<adw::Application> = app.downgrade().into();
        let (saved, receiver) = std::sync::mpsc::channel::<Option<WindowState>>();
        let thread = std::thread::spawn(move || {
            while let Ok(Some(state)) = receiver.recv() {
                save_window_state(state);
                if mark_tray_hint_shown() {
                    let app = app_for_hint.clone();
                    gtk::glib::idle_add_once(move || {
                        if let Some(app) = app.upgrade() {
                            show_tray_hint(&app);
                        }
                    });
                }
            }
        });
        startup.writers.push(WindowWriter {
            sender: saved.clone(),
            thread,
        });
        window.connect_close_request(move |window| {
            let _ = saved.send(Some(WindowState {
                width: window.width(),
                height: window.height(),
            }));
            gtk::glib::Propagation::Proceed
        });
    }

    let split = adw::NavigationSplitView::new();
    split.add_css_class("toolport-shell");
    split.set_min_sidebar_width(220.0);
    split.set_max_sidebar_width(280.0);

    let (content, server_page, approval_page) = build_content(app, broker.clone());
    let share_requests = server_page.share_requests.clone();
    io::on_window_closed(&window, move || share_requests.stop());
    let bridge_for_reap = bridge.clone();
    let server_page_for_reap = server_page.clone();
    let client_page = ClientPage::new(app, server_page.clone());
    let activity_page = ActivityPage::new(app);
    let catalog_page = CatalogPage::new(server_page.clone());
    let teams_page = TeamsPage::new(app, server_page.clone());
    let settings_page = SettingsPage::new(bridge, broker);
    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .transition_duration(140)
        .hhomogeneous(false)
        .vhomogeneous(false)
        .build();
    stack.add_named(&content, Some("servers"));
    stack.add_named(&client_page.root, Some("clients"));
    stack.add_named(&activity_page.root, Some("activity"));
    stack.add_named(&catalog_page.root, Some("catalog"));
    stack.add_named(&teams_page.root, Some("teams"));
    stack.add_named(&settings_page.root, Some("settings"));
    let (sidebar, quarantine_badge, team_button) = build_sidebar(
        app,
        &split,
        &stack,
        server_page.clone(),
        client_page.clone(),
        activity_page.clone(),
        catalog_page.clone(),
        teams_page.clone(),
        settings_page.clone(),
        paired,
    );
    {
        let settings = settings_page.clone();
        io::while_mapped(&settings_page.root, 15, move || settings.refresh_quietly());
        activity_page.attach(&window);
    }
    split.set_sidebar(Some(&adw::NavigationPage::new(&sidebar, "Navigation")));
    let content_with_notice = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let removed = &startup.removed;
    if !removed.is_empty() {
        let notice = gtk::Box::new(gtk::Orientation::Vertical, 8);
        notice.add_css_class("toolport-setting-row");
        notice.append(
            &gtk::Label::builder()
                .label(crate::registry::removed_features_message(removed))
                .wrap(true)
                .xalign(0.0)
                .build(),
        );
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let exports = l10n::button("Open exports folder");
        exports.connect_clicked(|_| {
            if let Err(error) = crate::diagnostics_controller::open_exports_dir() {
                eprintln!("toolport: {error}");
            }
        });
        actions.append(&exports);
        let need = l10n::button("I need this");
        let issue = crate::registry::removed_features_issue_url(removed);
        need.connect_clicked(move |_| {
            let _ = crate::oauth::open_web_url(&issue);
        });
        actions.append(&need);
        let go_back = l10n::button("Go back to 1.24");
        go_back.connect_clicked(|_| {
            let _ = crate::oauth::open_web_url(crate::registry::GO_BACK_TO_1X_URL);
        });
        actions.append(&go_back);
        let dismiss = l10n::button("Dismiss");
        let notice_for_dismiss = notice.clone();
        dismiss.connect_clicked(move |button| {
            button.set_sensitive(false);
            let button = button.clone();
            let notice = notice_for_dismiss.clone();
            gtk::glib::spawn_future_local(async move {
                let result = gtk::gio::spawn_blocking(|| {
                    crate::registry::update(|reg| {
                        reg.dismiss_removed_features_notice();
                        Ok(())
                    })
                })
                .await;
                match result {
                    Ok(Ok(_)) => notice.set_visible(false),
                    _ => {
                        button.set_sensitive(true);
                        eprintln!("toolport: could not dismiss the notice");
                    }
                }
            });
        });
        actions.append(&dismiss);
        notice.append(&actions);
        content_with_notice.append(&notice);
    }
    if startup.access_notice {
        let notice = gtk::Box::new(gtk::Orientation::Vertical, 8);
        notice.add_css_class("toolport-setting-row");
        notice.append(&{ let widget = gtk::Label::builder().label(crate::i18n::tr("Stop old Toolport gateways, then restart apps to use client access controls.")).tooltip_text(
            crate::i18n::tr("Old Toolport gateways may still be running from before the upgrade. Stop old gateways, then restart any apps still using them so they use the new client access controls.")
        ).wrap(true).xalign(0.0).build(); l10n::bind_label(&widget, "Stop old Toolport gateways, then restart apps to use client access controls."); widget });
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let stop = l10n::button("Stop old gateways");
        let settings = settings_page.clone();
        let stack_for_stop = stack.clone();
        stop.connect_clicked(move |_| {
            stack_for_stop.set_visible_child_name("settings");
            settings.select_page(4);
            settings.stop_stale.emit_clicked();
        });
        actions.append(&stop);
        let dismiss = l10n::button("Dismiss");
        let notice_for_dismiss = notice.clone();
        dismiss.connect_clicked(move |button| {
            button.set_sensitive(false);
            let button = button.clone();
            let notice = notice_for_dismiss.clone();
            gtk::glib::spawn_future_local(async move {
                let result = gtk::gio::spawn_blocking(|| {
                    crate::registry::update(|reg| {
                        reg.dismiss_access_upgrade_notice();
                        Ok(())
                    })
                })
                .await;
                match result {
                    Ok(Ok(_)) => notice.set_visible(false),
                    _ => {
                        button.set_sensitive(true);
                        eprintln!("toolport: could not dismiss the upgrade notice");
                    }
                }
            });
        });
        actions.append(&dismiss);
        notice.append(&actions);
        content_with_notice.append(&notice);
    }
    stack.set_vexpand(true);
    content_with_notice.append(&stack);
    split.set_content(Some(&adw::NavigationPage::new(
        &content_with_notice,
        "Toolport",
    )));

    let review_quarantine = gtk::gio::SimpleAction::new("review-quarantine", None);
    let app_for_quarantine = app.clone();
    let window_for_quarantine = window.clone();
    let settings_for_quarantine = settings_page.clone();
    review_quarantine.connect_activate(move |_, _| {
        if let Some(action) = app_for_quarantine.lookup_action("show-settings") {
            action.activate(None);
        }
        settings_for_quarantine.select_page(2);
        window_for_quarantine.present();
    });
    app.add_action(&review_quarantine);

    let review_security = gtk::gio::SimpleAction::new("review-security", None);
    let app_for_security = app.clone();
    let window_for_security = window.clone();
    let activity_for_security = activity_page.clone();
    review_security.connect_activate(move |_, _| {
        if let Some(action) = app_for_security.lookup_action("show-activity") {
            action.activate(None);
        }
        activity_for_security.reveal_security();
        window_for_security.present();
    });
    app.add_action(&review_security);

    let show_approvals = gtk::gio::SimpleAction::new("show-approvals", None);
    let window_for_approvals = window.clone();
    show_approvals.connect_activate(move |_, _| window_for_approvals.present());
    app.add_action(&show_approvals);

    let alerts = gtk::Overlay::new();
    alerts.set_child(Some(&split));
    let approval_host = adw::Clamp::builder()
        .maximum_size(720)
        .tightening_threshold(560)
        .child(&approval_page.root)
        .build();
    approval_host.set_valign(gtk::Align::Start);
    approval_host.set_margin_top(64);
    approval_host.set_margin_start(12);
    approval_host.set_margin_end(12);
    alerts.add_overlay(&approval_host);
    let security_event_alert = SecurityEventAlert::new();
    let quarantine_alert = QuarantineAlert::new();
    let security_alerts = gtk::Box::new(gtk::Orientation::Vertical, 8);
    security_alerts.append(&security_event_alert.root);
    security_alerts.append(&quarantine_alert.root);
    let quarantine_host = adw::Clamp::builder()
        .maximum_size(620)
        .tightening_threshold(480)
        .child(&security_alerts)
        .build();
    quarantine_host.set_valign(gtk::Align::End);
    quarantine_host.set_margin_start(12);
    quarantine_host.set_margin_end(12);
    quarantine_host.set_margin_bottom(16);
    alerts.add_overlay(&quarantine_host);
    start_quarantine_watch(app, &window, quarantine_badge, quarantine_alert);
    start_security_event_watch(app, &window, security_event_alert);

    let narrow = adw::Breakpoint::new(
        adw::BreakpointCondition::parse("max-width: 620px")
            .expect("the native-shell breakpoint is a static valid condition"),
    );
    narrow.add_setter(&split, "collapsed", Some(&true.to_value()));
    narrow.add_setter(&split, "show-content", Some(&true.to_value()));
    window.add_breakpoint(narrow);

    window.set_content(Some(&alerts));
    io::track_hidden_memory(&window);
    theme.attach(&window);
    let state = state::RegistryController::new(move |snapshot| {
        // Team joins the sidebar the moment the registry says this install is
        // paired, without a relaunch.
        if let state::RegistryState::Ready(ready) = &snapshot {
            team_button.set_visible(true);
            let label = if !ready.paired {
                crate::i18n::msg("Sign in to sync")
            } else if ready.personal_sync {
                "Sync"
            } else {
                "Team"
            };
            let count = ready.servers.iter().filter(|s| s.requires_review).count();
            set_nav_label(
                &team_button,
                &if count > 0 {
                    format!("{} ({count})", crate::i18n::lookup(label))
                } else {
                    crate::i18n::lookup(label)
                },
            );
        }
        server_page.render(snapshot);
        if let Some(notice) = startup_notice.borrow_mut().take() {
            let detail = notice
                .quarantine_path
                .as_deref()
                .map(|path| {
                    crate::i18n::trf(
                        " The unreadable copy was preserved at {value0}.",
                        &[("value0", path)],
                    )
                })
                .unwrap_or_default();
            server_page.show_feedback(
                &crate::i18n::trf(
                    "Toolport recovered the registry from a backup: {value0}.{value1}",
                    &[
                        ("value0", &notice.reason.to_string()),
                        ("value1", &detail.to_string()),
                    ],
                ),
                true,
            );
        }
    });
    state.attach(&window);
    approval_page.attach(&window);
    teams_page.attach_background_sync(&window);
    onboarding::install(app, &window);
    if std::env::var_os("TOOLPORT_DEBUG_MEASURE").is_none() {
        start_startup_reap(app, bridge_for_reap, server_page_for_reap);
    }
    if std::env::var_os("TOOLPORT_DEBUG_MEASURE").is_some() {
        let window_for_measure = window.clone();
        let app_for_measure = app.clone();
        gtk::glib::timeout_add_seconds_local_once(2, move || {
            fn dump(widget: &gtk::Widget, depth: usize) {
                let (min, natural, _, _) = widget.measure(gtk::Orientation::Horizontal, -1);
                if min > 200 {
                    let text = widget
                        .downcast_ref::<gtk::Label>()
                        .map(|label| label.text().chars().take(40).collect::<String>())
                        .or_else(|| {
                            widget
                                .downcast_ref::<gtk::Button>()
                                .and_then(|b| b.label())
                                .map(|l| l.chars().take(40).collect())
                        })
                        .unwrap_or_default();
                    eprintln!(
                        "{:indent$}{} min={min} nat={natural} visible={} css={:?} text={text:?}",
                        "",
                        widget.type_(),
                        widget.is_visible(),
                        widget.css_classes(),
                        indent = depth * 2
                    );
                }
                let mut child = widget.first_child();
                while let Some(current) = child {
                    dump(&current, depth + 1);
                    child = current.next_sibling();
                }
            }
            dump(window_for_measure.upcast_ref::<gtk::Widget>(), 0);
            app_for_measure.quit();
        });
    }
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
            let notification =
                gtk::gio::Notification::new(&crate::i18n::tr("Toolport was updated"));
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
    crate::i18n::trf(
        if names.len() == 1 {
            if names.len() == 1 {
                crate::i18n::msg("{value0} app is still launching an old Toolport gateway. Restart it to finish the upgrade: {value3}.")
            } else {
                crate::i18n::msg("{value0} app is still launching an old Toolport gateway. Restart them to finish the upgrade: {value3}.")
            }
        } else {
            if names.len() == 1 {
                crate::i18n::msg("{value0} apps are still launching an old Toolport gateway. Restart it to finish the upgrade: {value3}.")
            } else {
                crate::i18n::msg("{value0} apps are still launching an old Toolport gateway. Restart them to finish the upgrade: {value3}.")
            }
        },
        &[
            ("value0", &format!("{}", names.len())),
            ("value3", &names.join(", ").to_string()),
        ],
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
    crate::hooks::cleanup_on_startup();
    // One-time removal of the retired agent guard hook entries (see `guard_cleanup`).
    crate::guard_cleanup::run_once();
}

fn show_registry_startup_failure(app: &adw::Application, error: &str) {
    if let Some(window) = app.active_window() {
        window.present();
        return;
    }
    let path = crate::registry::resolved_path()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| crate::i18n::tr("the Toolport data directory").to_string());
    let window = {
        let widget = adw::ApplicationWindow::builder()
            .application(app)
            .title(crate::i18n::tr("Toolport could not start safely"))
            .default_width(620)
            .default_height(360)
            .resizable(true)
            .build();
        l10n::bind_window_title(&widget, "Toolport could not start safely");
        widget
    };
    let page = gtk::Box::new(gtk::Orientation::Vertical, 16);
    page.set_margin_top(20);
    page.set_margin_bottom(20);
    page.set_margin_start(20);
    page.set_margin_end(20);
    page.append(&{
        let widget = gtk::Label::builder()
            .label(crate::i18n::tr("Toolport could not start safely"))
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["title-1"])
            .build();
        l10n::bind_label(&widget, "Toolport could not start safely");
        widget
    });
    page.append(
            &{ let widget = gtk::Label::builder()
                .label(crate::i18n::tr("The registry was not replaced. Close other Toolport processes and try again. If the problem continues, restore a registry backup or move the unreadable registry aside before reopening Toolport."))
                .halign(gtk::Align::Fill)
                .xalign(0.0)
                .wrap(true)
                .build(); l10n::bind_label(&widget, "The registry was not replaced. Close other Toolport processes and try again. If the problem continues, restore a registry backup or move the unreadable registry aside before reopening Toolport."); widget },
        );
    page.append(&{
        let widget = gtk::Label::builder()
            .label("")
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .selectable(true)
            .css_classes(["dim-label"])
            .build();
        l10n::bind_formatted_label(
            &widget,
            "Registry: {value0}\n\nError: {value1}",
            &[("value0", &path.to_string()), ("value1", error)],
        );
        widget
    });
    let app_for_close = app.clone();
    window.connect_close_request(move |_| {
        app_for_close.quit();
        gtk::glib::Propagation::Proceed
    });
    let app_for_destroy = app.clone();
    window.connect_destroy(move |_| app_for_destroy.quit());
    let close = l10n::button("Close Toolport");
    close.set_halign(gtk::Align::End);
    close.add_css_class("suggested-action");
    let app = app.clone();
    close.connect_clicked(move |_| app.quit());
    page.append(&close);
    window.set_content(Some(&page));
    window.present();
}

/// The 2.0 top-level navigation, in sidebar order. Team joins only on a paired
/// install (see [`is_sidebar_row`]). Catalog leaves the sidebar
/// here; their pages and `show-*` actions stay so onboarding, the tray and
/// pairing can still reach them until a later PR removes or relocates them.
const NAV_SECTIONS: &[(&str, &str, &str)] = &[
    (
        "servers",
        crate::i18n::msg("Servers"),
        "network-server-symbolic",
    ),
    ("clients", crate::i18n::msg("Clients"), "computer-symbolic"),
    (
        "activity",
        crate::i18n::msg("Activity"),
        "view-list-symbolic",
    ),
    (
        "settings",
        crate::i18n::msg("Settings"),
        "emblem-system-symbolic",
    ),
    (
        "teams",
        crate::i18n::msg("Sign in to sync"),
        "system-users-symbolic",
    ),
    (
        "catalog",
        crate::i18n::msg("Catalog"),
        "system-software-install-symbolic",
    ),
];

/// Number-key order for the fixed sidebar rows. Team is click-only, matching the
/// React shell's four shortcuts.
const NAV_SHORTCUTS: &[&str] = &["servers", "clients", "activity", "settings"];

/// Whether `target` is drawn as a sidebar row. Team is hidden on an unpaired
/// install; the dropped top-level views stay reachable by action only.
fn is_sidebar_row(target: &str, _paired: bool) -> bool {
    match target {
        "teams" => true,
        "catalog" => false,
        _ => true,
    }
}

// Keep the native navigation widgets and their shared page state together.
#[allow(clippy::too_many_arguments)]
fn build_sidebar(
    app: &adw::Application,
    split: &adw::NavigationSplitView,
    stack: &gtk::Stack,
    server_page: ServerPage,
    client_page: ClientPage,
    activity_page: ActivityPage,
    catalog_page: CatalogPage,
    teams_page: TeamsPage,
    settings_page: SettingsPage,
    paired: bool,
) -> (gtk::Box, gtk::Label, gtk::Button) {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("toolport-sidebar");

    let header = adw::HeaderBar::new();
    header.add_css_class("toolport-header");
    let brand = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let mark = branding::toolport_mark();
    brand.append(&mark);
    brand.append(&{
        let widget = gtk::Label::builder()
            .label(crate::i18n::tr("Toolport"))
            .css_classes(["title"])
            .build();
        l10n::bind_label(&widget, "Toolport");
        widget
    });
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
        .valign(gtk::Align::Center)
        .halign(gtk::Align::Start)
        .build();

    let tools_action = gtk::gio::SimpleAction::new("show-server-tools", None);
    let page_for_tools = server_page.clone();
    tools_action.connect_activate(move |_, _| {
        let page = page_for_tools.clone();
        gtk::glib::spawn_future_local(async move {
            // Onboarding may have just added the first server. Read the current
            // registry rather than the Servers page's pre-onboarding snapshot.
            let result = gtk::gio::spawn_blocking(|| {
                crate::registry::load().map(state::RegistrySnapshot::from_registry)
            })
            .await;
            match result {
                Ok(Ok(snapshot)) => {
                    let server = snapshot
                        .servers
                        .iter()
                        .find(|server| server.enabled)
                        .or_else(|| snapshot.servers.first())
                        .cloned();
                    page.render(state::RegistryState::Ready(snapshot));
                    if let Some(server) = server {
                        open_server_details(&server, &page, true);
                    } else {
                        page.show_confirmation(&crate::i18n::tr("Add a server to try its tools."));
                    }
                }
                Ok(Err(error)) => page.restore_after_error(&error),
                Err(_) => page.restore_after_error(&crate::i18n::tr(
                    "Could not load server tools: the operation stopped",
                )),
            }
        });
    });
    app.add_action(&tools_action);
    let mut buttons = Vec::new();
    let mut team_button = None;
    for &(target, label, icon) in NAV_SECTIONS {
        let button = gtk::Button::new();
        button.set_halign(gtk::Align::Fill);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        row.append(&gtk::Image::from_icon_name(icon));
        row.append(&{
            let widget = l10n::label(label);
            widget.set_halign(gtk::Align::Start);
            widget.set_hexpand(true);
            widget.set_xalign(0.0);
            widget
        });
        if target == "settings" {
            row.append(&quarantine_badge);
            button.add_css_class("toolport-settings-parent");
        }
        if let Some(index) = NAV_SHORTCUTS.iter().position(|name| *name == target) {
            l10n::bind_tooltip_with(&button, move || {
                format!("{} (Ctrl+{})", crate::i18n::lookup(label), index + 1)
            });
        }
        button.set_child(Some(&row));
        button.add_css_class("flat");
        button.add_css_class("toolport-nav-item");
        if target == "servers" {
            button.add_css_class("selected");
        }
        // Hidden pages keep their button, page and action so `show-catalog`,
        // `show-teams` still resolve from onboarding,
        // pairing and the tray; only the sidebar row is dropped.
        if is_sidebar_row(target, paired) {
            nav.append(&button);
        } else if target == "teams" {
            // Team is appended even when unpaired so pairing can reveal the row
            // in place instead of waiting for the next launch.
            button.set_visible(false);
            nav.append(&button);
        }
        if target == "settings" {
            let children = gtk::Box::new(gtk::Orientation::Vertical, 2);
            children.set_margin_start(20);
            children.set_visible(false);
            nav.append(&children);
            let mut subbuttons = Vec::new();
            for (index, (label, icon)) in settings::SETTINGS_PAGES.iter().enumerate() {
                let child = gtk::Button::new();
                child.add_css_class("flat");
                child.add_css_class("toolport-nav-item");
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
                row.append(&gtk::Image::from_icon_name(icon));
                let section_label = l10n::label(label);
                section_label.set_xalign(0.0);
                section_label.set_hexpand(true);
                row.append(&section_label);
                if index == 2 {
                    let badge = gtk::Label::builder()
                        .visible(false)
                        .css_classes(["toolport-badge", "review"])
                        .build();
                    row.append(&badge);
                    let broker = settings_page.approval_broker();
                    let badge = badge.downgrade();
                    if let Some(action) = app
                        .lookup_action("approval-queue-changed")
                        .and_then(|action| action.downcast::<gtk::gio::SimpleAction>().ok())
                    {
                        action.connect_activate(move |_, _| {
                            let Some(badge) = badge.upgrade() else {
                                return;
                            };
                            let count = broker.list().len();
                            badge.set_label(&count.to_string());
                            badge.set_tooltip_text(Some(&crate::i18n::trf(
                                "{value0} pending approvals",
                                &[("value0", &format!("{}", count))],
                            )));
                            badge.set_visible(count > 0);
                        });
                    }
                }
                child.set_child(Some(&row));
                let selected = child.clone();
                settings_page.connect_page_selected(index, move |visible| {
                    if visible {
                        selected.add_css_class("selected");
                    } else {
                        selected.remove_css_class("selected");
                    }
                });
                children.append(&child);
                subbuttons.push(child);
            }
            for (index, child) in subbuttons.iter().enumerate() {
                let page = settings_page.clone();
                let split = split.clone();
                child.connect_clicked(move |_| {
                    page.select_page(index);
                    split.set_show_content(true);
                });
            }
            subbuttons[0].add_css_class("selected");
            stack.connect_visible_child_name_notify(move |stack| {
                children.set_visible(stack.visible_child_name().as_deref() == Some("settings"));
            });
        }
        if target == "teams" {
            team_button = Some(button.clone());
        }
        buttons.push((target.to_string(), button));
    }
    for (target, button) in buttons.clone() {
        let split = split.clone();
        let stack = stack.clone();
        let buttons = buttons.clone();
        let client_page = client_page.clone();
        let server_page = server_page.clone();
        let activity_page = activity_page.clone();
        let catalog_page = catalog_page.clone();
        let teams_page = teams_page.clone();
        let settings_page = settings_page.clone();
        button.connect_clicked(move |_| {
            show_native_page(
                &split,
                &stack,
                &buttons,
                &target,
                &server_page,
                &client_page,
                &activity_page,
                &catalog_page,
                &teams_page,
                &settings_page,
            );
        });
    }
    for (target, _) in buttons.iter() {
        let action_name = format!("show-{target}");
        // Number keys follow the visible sidebar rows only, so a dropped page
        // cannot be reached by a stale chord. Team is click-only.
        let shortcut_number = NAV_SHORTCUTS
            .iter()
            .position(|name| *name == target.as_str())
            .map(|index| index + 1);
        let action = gtk::gio::SimpleAction::new(&action_name, None);
        let split = split.clone();
        let stack = stack.clone();
        let buttons = buttons.clone();
        let target = target.clone();
        let client_page = client_page.clone();
        let server_page = server_page.clone();
        let activity_page = activity_page.clone();
        let catalog_page = catalog_page.clone();
        let teams_page = teams_page.clone();
        let settings_page = settings_page.clone();
        action.connect_activate(move |_, _| {
            show_native_page(
                &split,
                &stack,
                &buttons,
                &target,
                &server_page,
                &client_page,
                &activity_page,
                &catalog_page,
                &teams_page,
                &settings_page,
            );
        });
        app.add_action(&action);
        if let Some(number) = shortcut_number {
            app.set_accels_for_action(
                &format!("app.{}", action_name),
                &[&format!("<Primary>{number}")],
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
    install_star_prompt(&root);

    (
        root,
        quarantine_badge,
        team_button.expect("the Team row is always in NAV_SECTIONS"),
    )
}

/// The one-off "star the repo" ask, shown once ever, only to someone actually
/// using Toolport (three servers and calls on two UTC dates), and only after the window has been on
/// screen a while. Showing it retires it permanently.
fn install_star_prompt(container: &gtk::Box) {
    fn marker() -> Option<std::path::PathBuf> {
        Some(crate::registry::conduit_dir()?.join(".gtk-star-prompt-done"))
    }
    if marker().is_none_or(|marker| marker.exists()) {
        return;
    }
    // Vertical, with copy short enough to sit on one line: the sidebar is ~220px
    // wide, so a horizontal row of label plus two buttons squeezes the label into
    // a five-line column.
    let card = gtk::Box::new(gtk::Orientation::Vertical, 6);
    card.add_css_class("toolport-card");
    card.set_margin_start(12);
    card.set_margin_end(12);
    card.set_margin_bottom(6);
    card.set_visible(false);
    card.append(&{
        let widget = gtk::Label::builder()
            .label(crate::i18n::tr("Enjoying Toolport?"))
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["caption", "toolport-muted"])
            .build();
        l10n::bind_label(&widget, "Enjoying Toolport?");
        widget
    });
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let retire = |card: &gtk::Box| {
        if let Some(marker) = marker() {
            let _ = std::fs::write(marker, b"1");
        }
        card.set_visible(false);
    };
    let star = l10n::button("Star on GitHub");
    star.add_css_class("toolport-secondary-action");
    star.set_hexpand(true);
    {
        let card = card.clone();
        star.connect_clicked(move |_| {
            let _ = crate::oauth::open_web_url("https://github.com/btsouth/toolport");
            retire(&card);
        });
    }
    actions.append(&star);
    let dismiss = gtk::Button::builder()
        .icon_name("window-close-symbolic")
        .tooltip_text(crate::i18n::tr("Dismiss forever"))
        .css_classes(["flat"])
        .build();
    {
        let card = card.clone();
        dismiss.connect_clicked(move |_| retire(&card));
    }
    actions.append(&dismiss);
    card.append(&actions);
    container.append(&card);
    let card_for_timer = card.clone();
    gtk::glib::timeout_add_seconds_local_once(8, move || {
        gtk::glib::spawn_future_local(async move {
            let eligible = gtk::gio::spawn_blocking(|| {
                crate::registry::load()
                    .map(|registry| {
                        registry
                            .servers
                            .iter()
                            .filter(|server| {
                                !crate::clients::is_gateway_server(server)
                                    && registry.enabled_here(&server.id)
                            })
                            .count()
                    })
                    .map(|enabled| {
                        star_value_reached(enabled, &crate::audit::read_all().unwrap_or_default())
                    })
                    .unwrap_or(false)
            })
            .await
            .unwrap_or(false);
            // The card itself is hidden (thus unmapped); "on screen" means its
            // sidebar parent is mapped, i.e. the window is actually shown.
            let sidebar_on_screen = card_for_timer
                .parent()
                .is_some_and(|parent| parent.is_mapped());
            let window = card_for_timer.root().and_downcast::<gtk::Window>();
            let unobstructed = window.as_ref().is_some_and(|window| window.is_active())
                && !gtk::Window::list_toplevels().iter().any(|widget| {
                    widget
                        .downcast_ref::<gtk::Window>()
                        .is_some_and(|window| window.is_modal() && window.is_visible())
                });
            if star_should_show(eligible, sidebar_on_screen, unobstructed)
                && marker().is_some_and(|marker| spend_star_prompt(&marker))
            {
                card_for_timer.set_visible(true);
            }
        });
    });
}

fn spend_star_prompt(marker: &std::path::Path) -> bool {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(marker)
        .is_ok()
}

fn star_value_reached(enabled: usize, entries: &[serde_json::Value]) -> bool {
    let days: std::collections::HashSet<_> = entries
        .iter()
        .filter(|entry| {
            crate::audit::tool_call_ok(entry) == Some(true)
                && entry.get("held").and_then(serde_json::Value::as_bool) != Some(true)
        })
        .filter_map(|entry| entry.get("ts").and_then(serde_json::Value::as_u64))
        .filter(|ts| *ts > 0)
        .map(|ts| ts / 86_400_000)
        .collect();
    enabled >= 3 && days.len() >= 2
}
fn star_should_show(eligible: bool, mapped: bool, unobstructed: bool) -> bool {
    eligible && mapped && unobstructed
}

#[derive(Clone)]
struct SecurityEventAlert {
    root: gtk::Box,
    title: gtk::Label,
    detail: gtk::Label,
    timer: std::rc::Rc<std::cell::RefCell<Option<gtk::glib::SourceId>>>,
}

impl SecurityEventAlert {
    fn new() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        root.add_css_class("toolport-global-alert");
        root.add_css_class("security-event");
        root.set_visible(false);
        root.append(&gtk::Image::from_icon_name("dialog-warning-symbolic"));

        let copy = gtk::Box::new(gtk::Orientation::Vertical, 2);
        copy.set_hexpand(true);
        let title = gtk::Label::builder()
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .css_classes(["heading"])
            .build();
        copy.append(&title);
        let detail = gtk::Label::builder()
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-muted", "caption"])
            .build();
        copy.append(&detail);
        root.append(&copy);

        let review = l10n::button("Review");
        review.add_css_class("toolport-secondary-action");
        review.set_action_name(Some("app.review-security"));
        root.append(&review);
        let close = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text(crate::i18n::tr("Dismiss this alert"))
            .css_classes(["flat", "circular"])
            .build();
        root.append(&close);

        let alert = Self {
            root,
            title,
            detail,
            timer: std::rc::Rc::new(std::cell::RefCell::new(None)),
        };
        for button in [review, close] {
            let alert = alert.clone();
            button.connect_clicked(move |_| alert.hide());
        }
        alert
    }

    fn show(&self, events: &[serde_json::Value]) {
        let (title, detail) = security_alert_copy(events);
        self.title.set_label(&title);
        self.detail.set_label(&detail);
        self.root.set_visible(true);
        if let Some(timer) = self.timer.borrow_mut().take() {
            timer.remove();
        }
        let root = self.root.clone();
        let timer = self.timer.clone();
        let source =
            gtk::glib::timeout_add_local_once(std::time::Duration::from_secs(6), move || {
                root.set_visible(false);
                timer.borrow_mut().take();
            });
        *self.timer.borrow_mut() = Some(source);
    }

    fn hide(&self) {
        if let Some(timer) = self.timer.borrow_mut().take() {
            timer.remove();
        }
        self.root.set_visible(false);
    }
}

#[derive(Clone)]
struct QuarantineAlert {
    root: gtk::Box,
    title: gtk::Label,
    detail: gtk::Label,
    current: std::rc::Rc<std::cell::RefCell<String>>,
    dismissed: std::rc::Rc<std::cell::RefCell<String>>,
}

impl QuarantineAlert {
    fn new() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        root.add_css_class("toolport-global-alert");
        root.add_css_class("security");
        root.set_visible(false);

        root.append(&gtk::Image::from_icon_name("security-high-symbolic"));
        let copy = gtk::Box::new(gtk::Orientation::Vertical, 2);
        copy.set_hexpand(true);
        let title = gtk::Label::builder()
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .css_classes(["heading"])
            .build();
        copy.append(&title);
        let detail = gtk::Label::builder()
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-muted", "caption"])
            .build();
        copy.append(&detail);
        root.append(&copy);

        let review = l10n::button("Review");
        review.add_css_class("suggested-action");
        review.set_action_name(Some("app.review-quarantine"));
        root.append(&review);
        let close = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text(crate::i18n::tr("Dismiss this alert"))
            .css_classes(["flat", "circular"])
            .build();
        root.append(&close);

        let alert = Self {
            root,
            title,
            detail,
            current: std::rc::Rc::new(std::cell::RefCell::new(String::new())),
            dismissed: std::rc::Rc::new(std::cell::RefCell::new(String::new())),
        };
        for button in [review, close] {
            let alert = alert.clone();
            button.connect_clicked(move |_| alert.dismiss_current());
        }
        alert
    }

    fn update(&self, list: &[serde_json::Value]) {
        if list.is_empty() {
            self.root.set_visible(false);
            self.current.borrow_mut().clear();
            self.dismissed.borrow_mut().clear();
            return;
        }
        let signature = quarantine_signature(list);
        *self.current.borrow_mut() = signature.clone();
        let refs = list.iter().collect::<Vec<_>>();
        let (title, detail) = crate::integrity::quarantine_notification(&refs);
        self.title.set_label(&title);
        self.detail.set_label(&detail);
        self.root.set_visible(*self.dismissed.borrow() != signature);
    }

    fn dismiss_current(&self) {
        *self.dismissed.borrow_mut() = self.current.borrow().clone();
        self.root.set_visible(false);
    }
}

fn quarantine_signature(list: &[serde_json::Value]) -> String {
    let mut keys = list
        .iter()
        .map(crate::integrity::quarantine_entry_key)
        .collect::<Vec<_>>();
    keys.sort();
    keys.join("\n")
}

fn security_event_instance_key(event: &serde_json::Value) -> String {
    format!(
        "{}:{}",
        crate::integrity::security_key(event),
        event
            .get("ts")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    )
}

fn security_event_needs_attention(event: &serde_json::Value) -> bool {
    crate::integrity::event_severity(event) == "high"
        && !crate::integrity::security_event_is_new_tool(event)
}

fn security_event_covered_by_quarantine(
    event: &serde_json::Value,
    quarantined: &[serde_json::Value],
) -> bool {
    match event.get("type").and_then(serde_json::Value::as_str) {
        Some("pins_load_failed") => quarantined.iter().any(|record| {
            record.get("change").and_then(serde_json::Value::as_str) == Some("tamper")
        }),
        Some("tool_drift" | "tool_poison_flag") => {
            let tool = event.get("tool").and_then(serde_json::Value::as_str);
            tool.is_some_and(|tool| {
                quarantined.iter().any(|record| {
                    record.get("tool").and_then(serde_json::Value::as_str) == Some(tool)
                })
            })
        }
        _ => false,
    }
}

fn security_alert_copy(events: &[serde_json::Value]) -> (String, String) {
    let events = security_attention_incidents(events);
    let title = if events.len() == 1 {
        crate::i18n::tr("Toolport: security finding").to_string()
    } else {
        crate::i18n::trf(
            "Toolport: {value0} security findings",
            &[("value0", &format!("{}", events.len()))],
        )
    };
    let first = events
        .first()
        .map(security_event_kind)
        .unwrap_or("A security finding was recorded");
    let detail = if events.len() > 1 {
        let remaining = events.len() - 1;
        crate::i18n::trf(
            if remaining == 1 {
                crate::i18n::msg("{value0}. {value1} more is ready to review.")
            } else {
                crate::i18n::msg("{value0}. {value1} more are ready to review.")
            },
            &[("value0", first), ("value1", &format!("{}", remaining))],
        )
    } else {
        crate::i18n::trf(
            "{value0}. Review the retained details in Activity.",
            &[("value0", first)],
        )
    };
    (title, detail)
}

fn newly_observed_security_events(
    previous: Option<&std::collections::HashSet<String>>,
    events: &[serde_json::Value],
    quarantined: &[serde_json::Value],
) -> (std::collections::HashSet<String>, Vec<serde_json::Value>) {
    let current = events
        .iter()
        .map(security_event_instance_key)
        .collect::<std::collections::HashSet<_>>();
    let Some(previous) = previous else {
        return (current, Vec::new());
    };
    let newcomers = events
        .iter()
        .filter(|event| !previous.contains(&security_event_instance_key(event)))
        .filter(|event| security_event_needs_attention(event))
        .filter(|event| !security_event_covered_by_quarantine(event, quarantined))
        .cloned()
        .collect::<Vec<_>>();
    (current, security_attention_incidents(&newcomers))
}

/// A damaged quarantine store makes duplicate suppression uncertain, but it
/// must not stop the independent security-event feed. The quarantine badge
/// reports its own read failure as unknown.
type SecurityWatchSnapshot = (
    Vec<serde_json::Value>,
    Result<Vec<serde_json::Value>, String>,
);

fn read_security_watch_snapshot() -> Result<SecurityWatchSnapshot, String> {
    let events = crate::integrity::read_recent(25).map_err(|error| {
        crate::i18n::trf(
            "could not read security events: {value0}",
            &[("value0", &format!("{}", error))],
        )
    })?;
    // Keep the error distinct from a known-empty quarantine. The badge displays
    // unknown, while this watcher still announces new security findings.
    let quarantined = crate::integrity::all_quarantined_summaries();
    Ok((events, quarantined))
}

fn security_attention_incidents(events: &[serde_json::Value]) -> Vec<serde_json::Value> {
    events
        .iter()
        .filter(|event| {
            if event.get("type").and_then(serde_json::Value::as_str) != Some("result_injection") {
                return true;
            }
            let server = event.get("server").and_then(serde_json::Value::as_str);
            let tool = event.get("tool").and_then(serde_json::Value::as_str);
            let ts = event
                .get("ts")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0);
            !events.iter().any(|candidate| {
                candidate.get("type").and_then(serde_json::Value::as_str)
                    == Some("result_injection_blocked")
                    && candidate.get("server").and_then(serde_json::Value::as_str) == server
                    && candidate.get("tool").and_then(serde_json::Value::as_str) == tool
                    && (candidate
                        .get("ts")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or(0)
                        - ts)
                        .abs()
                        <= 1_000
            })
        })
        .cloned()
        .collect()
}

fn start_security_event_watch(
    app: &adw::Application,
    window: &adw::ApplicationWindow,
    alert: SecurityEventAlert,
) {
    let app = app.clone();
    let seen: std::rc::Rc<std::cell::RefCell<Option<std::collections::HashSet<String>>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let reader = io::SnapshotReader::new(read_security_watch_snapshot, move |result| {
        let Ok((events, quarantined)) = result else {
            return;
        };
        let mut guard = seen.borrow_mut();
        let (current, newcomers) = newly_observed_security_events(
            guard.as_ref(),
            &events,
            quarantined.as_deref().unwrap_or_default(),
        );
        *guard = Some(current);
        if newcomers.is_empty() {
            return;
        }
        alert.show(&newcomers);
        if app.active_window().is_some_and(|window| window.is_active()) {
            return;
        }
        let (title, detail) = security_alert_copy(&newcomers);
        let notification = gtk::gio::Notification::new(&title);
        notification.set_body(Some(&detail));
        notification.set_priority(gtk::gio::NotificationPriority::High);
        notification.set_default_action("app.review-security");
        app.send_notification(Some("toolport-security-finding"), &notification);
    });
    reader.refresh();
    let reader_for_destroy = reader.clone();
    io::on_window_closed(window, move || reader_for_destroy.stop());
    io::watch_data(
        window,
        5,
        true,
        |name| {
            name == "security.jsonl" || name.starts_with("quarantine") || name == "registry.json"
        },
        move || {
            reader.refresh();
        },
    );
}

/// Watch quarantine store changes, including while hidden: keep
/// the sidebar badge current, and send one OS notification per newly blocked
/// tool batch. The first poll only establishes the baseline so a restart does
/// not re-announce an already known backlog; the badge still shows it.
fn start_quarantine_watch(
    app: &adw::Application,
    window: &adw::ApplicationWindow,
    badge: gtk::Label,
    alert: QuarantineAlert,
) {
    let app = app.clone();
    let seen: std::rc::Rc<std::cell::RefCell<Option<std::collections::HashSet<String>>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));
    let reader =
        io::SnapshotReader::new(crate::integrity::all_quarantined_summaries, move |result| {
            let list = match result {
                Ok(list) => list,
                _ => {
                    badge.set_label("?");
                    badge.set_tooltip_text(Some(&crate::i18n::tr(
                        "Could not read the quarantine state; unknown is not the same as zero",
                    )));
                    badge.set_visible(true);
                    return;
                }
            };
            badge.set_label(&list.len().to_string());
            badge.set_tooltip_text(Some(&crate::i18n::tr(
                "Blocked tools awaiting re-approval; review them in Settings",
            )));
            badge.set_visible(!list.is_empty());
            alert.update(&list);
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
                        notification.set_default_action("app.review-quarantine");
                        app.send_notification(Some("toolport-quarantine"), &notification);
                    }
                    *previous = keys;
                }
            }
        });
    reader.refresh();
    let reader_for_destroy = reader.clone();
    io::on_window_closed(window, move || reader_for_destroy.stop());
    io::watch_data(
        window,
        15,
        true,
        |name| {
            name.starts_with("quarantine")
                || name == "registry.json"
                || name == "root-integrity-scopes.json"
        },
        move || {
            reader.refresh();
        },
    );
}

// Keep the native navigation widgets and their shared page state together.
#[allow(clippy::too_many_arguments)]
fn show_native_page(
    split: &adw::NavigationSplitView,
    stack: &gtk::Stack,
    buttons: &[(String, gtk::Button)],
    target: &str,
    server_page: &ServerPage,
    client_page: &ClientPage,
    activity_page: &ActivityPage,
    catalog_page: &CatalogPage,
    teams_page: &TeamsPage,
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
    if target == "servers" {
        server_page.reprobe_if_stale();
    } else if target == "clients" {
        client_page.refresh();
    } else if target == "activity" {
        activity_page.refresh_quietly();
    } else if target == "catalog" {
        catalog_page.refresh();
    } else if target == "teams" {
        teams_page.refresh();
    } else if target == "settings" {
        settings_page.select_page(0);
        settings_page.refresh();
    }
}

#[derive(Clone)]
struct ServerPage {
    app: adw::Application,
    share_requests: std::rc::Rc<io::RequestGuard>,
    server_count: gtk::Label,
    enabled_count: gtk::Label,
    section_title: gtk::Label,
    posture: gtk::Label,
    search: gtk::SearchEntry,
    feedback: gtk::Label,
    list: gtk::Box,
    last_snapshot: std::rc::Rc<std::cell::RefCell<Option<state::RegistrySnapshot>>>,
    feedback_timer: std::rc::Rc<std::cell::RefCell<Option<gtk::glib::SourceId>>>,
    /// Per-row health widgets for the rows currently on screen, keyed by server
    /// id, so probe results can land on live labels without a re-render.
    health_rows: std::rc::Rc<std::cell::RefCell<std::collections::HashMap<String, Vec<HealthRow>>>>,
    /// Every rendered card with the server it shows, so search can hide and show
    /// rows without rebuilding them.
    rows: std::rc::Rc<std::cell::RefCell<Vec<(state::ServerView, gtk::Box)>>>,
    no_matches: std::rc::Rc<std::cell::RefCell<Option<gtk::Box>>>,
    /// Heading above turned-off servers; hidden when the search leaves none.
    off_heading: std::rc::Rc<std::cell::RefCell<Option<gtk::Box>>>,
    /// Probe results and in-flight probes. Survives re-renders so the list can
    /// group needs-attention servers first, and decides when a server is
    /// launched to check it.
    health: std::rc::Rc<std::cell::RefCell<health::HealthCache>>,
}

#[derive(Clone)]
struct HealthRow {
    label: gtk::Label,
    /// Prefix the probe result is appended to, so the row keeps saying what
    /// transport it is once the status lands.
    transport: String,
    authenticate: gtk::Button,
    copy_error: gtk::Button,
    retry: gtk::Button,
}

impl ServerPage {
    /// Credentials live in the keychain, outside the registry fingerprint, so
    /// a change to them has to ask for a fresh probe of that server.
    fn reprobe_after_auth_change(&self, server_id: &str) {
        self.health.borrow_mut().invalidate(server_id);
        self.probe_servers(health::ProbeReason::ConfigChanged);
    }

    fn reprobe_if_stale(&self) {
        self.probe_servers(health::ProbeReason::Stale);
    }

    fn render(&self, state: state::RegistryState) {
        match state {
            state::RegistryState::Ready(snapshot) => {
                if self.last_snapshot.borrow().as_ref() == Some(&snapshot) {
                    return;
                }
                *self.last_snapshot.borrow_mut() = Some(snapshot.clone());
                self.server_count
                    .set_label(&snapshot.servers.len().to_string());
                self.enabled_count
                    .set_label(&snapshot.enabled_count.to_string());
                self.render_server_list(&snapshot);
            }
            state::RegistryState::FirstRun => {
                self.clear_server_list();
                *self.last_snapshot.borrow_mut() = None;
                self.reset_summary();
                l10n::bind_text(&self.section_title, "Servers");
                self.list.append(&state_card("network-server-symbolic", crate::i18n::msg("Toolport is ready for setup"), crate::i18n::msg("Use Add server for a custom endpoint or open Catalog for a curated starting point. Valid servers turn on. Missing setup inputs stay off."), false));
            }
            state::RegistryState::Unavailable => {
                self.clear_server_list();
                *self.last_snapshot.borrow_mut() = None;
                self.server_count.set_label("–");
                self.enabled_count.set_label("–");
                self.section_title
                    .set_label(&crate::i18n::tr("Servers unavailable"));
                self.list.append(&state_card("dialog-warning-symbolic", crate::i18n::msg("Configuration could not be displayed"), crate::i18n::msg("Toolport left the registry untouched. Open the app again to inspect or recover it."), true));
            }
            state::RegistryState::NewerVersion(message) => {
                self.clear_server_list();
                *self.last_snapshot.borrow_mut() = None;
                self.server_count.set_label("–");
                self.enabled_count.set_label("–");
                self.section_title
                    .set_label(&crate::i18n::tr("Servers unavailable"));
                self.list.append(&state_card_formatted(
                    "software-update-available-symbolic",
                    crate::i18n::msg("Update Toolport"),
                    crate::i18n::msg("{value0} Toolport left the registry untouched."),
                    &[("value0", &message.to_string())],
                    true,
                ));
            }
        }
    }

    fn clear_server_list(&self) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        self.rows.borrow_mut().clear();
        self.no_matches.borrow_mut().take();
        self.off_heading.borrow_mut().take();
    }

    fn render_server_list(&self, snapshot: &state::RegistrySnapshot) {
        if let Some(popover) = open_popover(self.list.upcast_ref()) {
            if popover.widget_name() != "toolport-deferred-refresh" {
                popover.set_widget_name("toolport-deferred-refresh");
                let page = self.clone();
                popover.connect_closed(move |p| {
                    let _ = p;
                    let snapshot = page.last_snapshot.borrow().clone();
                    if let Some(snapshot) = snapshot {
                        page.render_server_list(&snapshot);
                    }
                });
            }
            return;
        }
        let previous = self.rows.borrow().clone();
        let focused = self
            .app
            .active_window()
            .and_then(|w| gtk::prelude::RootExt::focus(&w));
        let focused_row = focused.as_ref().and_then(|focus| {
            previous
                .iter()
                .find(|(_, row)| focus.is_ancestor(row) || focus == row.upcast_ref::<gtk::Widget>())
                .map(|(server, row)| (server.id.clone(), focus_path(focus, row.upcast_ref())))
        });
        // Remove headings only; unchanged cards retain focus and widget identity.
        let mut child = self.list.first_child();
        while let Some(w) = child {
            child = w.next_sibling();
            if !previous
                .iter()
                .any(|(_, row)| row.upcast_ref::<gtk::Widget>() == &w)
            {
                self.list.remove(&w);
            }
        }
        self.no_matches.borrow_mut().take();
        self.off_heading.borrow_mut().take();
        l10n::bind_text(&self.section_title, "Servers");
        if snapshot.servers.is_empty() {
            self.clear_server_list();
            self.posture.set_visible(false);
            self.list.append(&state_card(
                "network-server-symbolic",
                crate::i18n::msg("No servers yet"),
                crate::i18n::msg(
                    "Use Add server to connect your first local command or remote endpoint.",
                ),
                false,
            ));
            return;
        }

        // Turned-off servers get their own group below the running ones, so a
        // disabled server never reads as one that is merely still loading.
        let (on, off) = server_groups(&snapshot.servers);
        let mut rows = Vec::with_capacity(on.len() + off.len());
        for server in on {
            let card = previous
                .iter()
                .find(|(old, card)| {
                    old == server
                        && card.widget_name()
                            == format!("toolport-profile:{}", snapshot.active_profile_id)
                })
                .map(|(_, card)| card.clone())
                .unwrap_or_else(|| server_card(server, &snapshot.active_profile_id, self.clone()));
            if card.parent().is_none() {
                self.list.append(&card);
            }
            if self.list.last_child().as_ref() != Some(card.upcast_ref::<gtk::Widget>()) {
                self.list
                    .reorder_child_after(&card, self.list.last_child().as_ref());
            }
            rows.push((server.clone(), card));
        }
        if !off.is_empty() {
            let heading = client_section_title(crate::i18n::msg("Turned off"), off.len());
            self.list.append(&heading);
            *self.off_heading.borrow_mut() = Some(heading);
        }
        for server in off {
            let card = previous
                .iter()
                .find(|(old, card)| {
                    old == server
                        && card.widget_name()
                            == format!("toolport-profile:{}", snapshot.active_profile_id)
                })
                .map(|(_, card)| card.clone())
                .unwrap_or_else(|| server_card(server, &snapshot.active_profile_id, self.clone()));
            card.add_css_class("toolport-card-off");
            if card.parent().is_none() {
                self.list.append(&card);
            }
            if self.list.last_child().as_ref() != Some(card.upcast_ref::<gtk::Widget>()) {
                self.list
                    .reorder_child_after(&card, self.list.last_child().as_ref());
            }
            rows.push((server.clone(), card));
        }
        for (_, card) in &previous {
            if !rows.iter().any(|(_, current)| current == card) {
                self.list.remove(card);
            }
        }
        if let Some((id, path)) = focused_row {
            if focused.as_ref().is_some_and(|w| w.root().is_none()) {
                if let Some((_, card)) = rows.iter().find(|(s, _)| s.id == id) {
                    if let Some(widget) = follow_focus_path(card.upcast_ref(), &path) {
                        widget.grab_focus();
                    }
                }
            }
        }
        self.health_rows.borrow_mut().retain(|_, rows| {
            rows.retain(|row| row.label.root().is_some());
            !rows.is_empty()
        });
        *self.rows.borrow_mut() = rows;
        let no_matches = state_card(
            "edit-find-symbolic",
            crate::i18n::msg("No matching servers"),
            crate::i18n::msg("Try a different server name or transport."),
            false,
        );
        self.list.append(&no_matches);
        *self.no_matches.borrow_mut() = Some(no_matches);
        self.apply_filter();
        self.probe_servers(health::ProbeReason::ConfigChanged);
    }

    /// Show the rows matching the search. Filtering never probes: a probe
    /// launches the server, and typing must not launch anything.
    fn apply_filter(&self) {
        let query = self.search.text();
        let mut any = false;
        let mut any_off = false;
        for (server, card) in self.rows.borrow().iter() {
            let visible = server_matches_query(server, query.as_str());
            card.set_visible(visible);
            any |= visible;
            any_off |= visible && !server.enabled;
        }
        if let Some(heading) = self.off_heading.borrow().as_ref() {
            heading.set_visible(any_off);
        }
        if let Some(no_matches) = self.no_matches.borrow().as_ref() {
            no_matches.set_visible(!any && !self.rows.borrow().is_empty());
        }
        self.update_posture();
    }

    fn update_posture(&self) {
        let snapshot = self.last_snapshot.borrow();
        let enabled = snapshot
            .iter()
            .flat_map(|snapshot| snapshot.servers.iter())
            .filter(|server| server.enabled)
            .map(|server| server.id.as_str())
            .collect::<Vec<_>>();
        let filtered_out = self
            .no_matches
            .borrow()
            .as_ref()
            .is_some_and(|card| card.is_visible());
        if enabled.is_empty() || filtered_out {
            self.posture.set_visible(false);
            return;
        }
        let (ready, auth, errors, pending) = self.health.borrow().tally(enabled.iter().copied());
        self.posture.set_visible(true);
        self.posture
            .set_label(&posture_line(ready, auth, errors, pending, enabled.len()));
    }

    /// Probe the enabled servers the health cache says need it, in the
    /// background, and land each result on its live row.
    fn probe_servers(&self, reason: health::ProbeReason) {
        let Some(snapshot) = self.last_snapshot.borrow().clone() else {
            return;
        };
        let enabled = snapshot
            .servers
            .iter()
            .filter(|server| server.enabled)
            .map(|server| (server.id.as_str(), server.probe_fingerprint))
            .collect::<Vec<_>>();
        let tickets = self
            .health
            .borrow_mut()
            .plan(&enabled, reason, std::time::Instant::now());
        for ticket in tickets {
            for row in self
                .health_rows
                .borrow()
                .get(&ticket.server_id)
                .into_iter()
                .flatten()
            {
                l10n::bind_formatted_label(
                    &row.label,
                    "{value0} · Checking…",
                    &[("value0", &row.transport.to_string())],
                );
                for class in ["success", "error", "review"] {
                    row.label.remove_css_class(class);
                }
                row.label.set_tooltip_text(None);
                row.authenticate.set_visible(false);
                row.copy_error.set_visible(false);
                row.retry.set_visible(false);
            }
            self.spawn_probe(ticket);
        }
        self.update_posture();
    }

    fn spawn_probe(&self, ticket: health::ProbeTicket) {
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let server_id = ticket.server_id.clone();
            let id_for_probe = server_id.clone();
            let result = gtk::gio::spawn_blocking(move || {
                crate::server_runtime::probe_registered(&id_for_probe)
            })
            .await;
            let probe = match result {
                Ok(Ok(probe)) => probe,
                Ok(Err(error)) => crate::server_runtime::ProbeResult {
                    server_id: server_id.clone(),
                    ok: false,
                    tool_count: 0,
                    error: Some(error),
                    auth_required: false,
                    failure: None,
                    auth_target: None,
                },
                Err(_) => crate::server_runtime::ProbeResult {
                    server_id: server_id.clone(),
                    ok: false,
                    tool_count: 0,
                    error: Some(crate::i18n::tr("the probe stopped unexpectedly").to_string()),
                    auth_required: false,
                    failure: None,
                    auth_target: None,
                },
            };
            let outcome = page.health.borrow_mut().complete(
                &ticket,
                probe.clone(),
                std::time::Instant::now(),
            );
            match outcome {
                health::ProbeOutcome::Apply => page.apply_probe(&server_id, &probe),
                health::ProbeOutcome::Rerun(next) => page.spawn_probe(next),
                health::ProbeOutcome::Discard => {}
            }
            page.update_posture();
        });
    }

    fn apply_probe(&self, server_id: &str, probe: &crate::server_runtime::ProbeResult) {
        for row in self
            .health_rows
            .borrow()
            .get(server_id)
            .into_iter()
            .flatten()
        {
            let (status, class) = probe_status_line(probe);
            row.label.set_label(&format!(
                "{} · {status}",
                state::display_transport(&row.transport)
            ));
            row.label.remove_css_class("success");
            row.label.remove_css_class("error");
            row.label.remove_css_class("review");
            row.label.add_css_class(class);
            row.label
                .set_tooltip_text(probe.error.as_deref().filter(|error| !error.is_empty()));
            row.authenticate
                .set_label(&crate::i18n::lookup(probe_auth_action(probe)));
            row.authenticate.set_visible(probe.auth_required);
            row.copy_error
                .set_visible(!probe.ok && probe.error.is_some());
            row.retry.set_visible(!probe.ok && !probe.auth_required);
        }
    }

    fn reset_summary(&self) {
        self.server_count.set_label("0");
        self.enabled_count.set_label("0");
    }

    fn show_feedback(&self, message: &str, error: bool) {
        self.cancel_feedback_timer();
        self.feedback.remove_css_class("idle");
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

    fn show_confirmation(&self, message: &str) {
        self.show_feedback(message, false);
        let feedback = self.feedback.clone();
        let feedback_timer = self.feedback_timer.clone();
        let timer =
            gtk::glib::timeout_add_local_once(std::time::Duration::from_secs(4), move || {
                feedback.set_visible(true);
                feedback.set_label("");
                feedback.remove_css_class("success");
                feedback.remove_css_class("error");
                feedback.add_css_class("idle");
                feedback_timer.borrow_mut().take();
            });
        *self.feedback_timer.borrow_mut() = Some(timer);
    }

    #[cfg(test)]
    fn hide_feedback(&self) {
        self.cancel_feedback_timer();
        self.feedback.set_visible(true);
        self.feedback.set_label("");
        self.feedback.remove_css_class("success");
        self.feedback.remove_css_class("error");
        self.feedback.add_css_class("idle");
    }

    fn cancel_feedback_timer(&self) {
        if let Some(timer) = self.feedback_timer.borrow_mut().take() {
            timer.remove();
        }
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
    credential_page: ServerPage,
    app: adw::Application,
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
    pending_confirmation: std::rc::Rc<std::cell::RefCell<Option<String>>>,
    feedback_timer: std::rc::Rc<std::cell::RefCell<Option<gtk::glib::SourceId>>>,
}

impl ClientPage {
    fn new(app: &adw::Application, credential_page: ServerPage) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("toolport-content");
        let header = adw::HeaderBar::new();
        header.add_css_class("toolport-header");
        header.set_show_back_button(true);
        header.set_title_widget(Some(&{
            let widget = gtk::Label::builder()
                .label(crate::i18n::tr("Clients"))
                .css_classes(["title"])
                .build();
            l10n::bind_label(&widget, "Clients");
            widget
        }));
        let refresh_button = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text(crate::i18n::tr("Scan clients again"))
            .build();
        header.pack_end(&refresh_button);
        let add_location = l10n::button("Add client location");
        let import_button = l10n::button("Import");
        import_button.set_tooltip_text(Some(&crate::i18n::tr(
            "Review servers found in client configurations",
        )));
        import_button.add_css_class("toolport-secondary-action");
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
        let page_actions = wrapping_actions();
        page_actions.insert(&add_location, -1);
        page_actions.insert(&import_button, -1);
        page.append(&page_actions);
        page.append(&{
            let widget = gtk::Label::builder()
                .label(crate::i18n::tr("Your AI clients, one Toolport gateway"))
                .halign(gtk::Align::Fill)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["title-2"])
                .build();
            l10n::bind_label(&widget, "Your AI clients, one Toolport gateway");
            widget
        });
        page.append(
            &{ let widget = gtk::Label::builder()
                .label(crate::i18n::tr("Toolport reads each client's local MCP configuration. Existing Toolport entries in extra locations are adopted with a backup."))
                .halign(gtk::Align::Fill)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["toolport-muted"])
                .build(); l10n::bind_label(&widget, "Toolport reads each client's local MCP configuration. Existing Toolport entries in extra locations are adopted with a backup."); widget },
        );
        let feedback = gtk::Label::builder()
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-feedback"])
            .build();
        l10n::bind_text(&feedback, "Open Clients to scan this machine.");
        page.append(&feedback);

        let summary = gtk::Grid::new();
        summary.add_css_class("toolport-summary");
        summary.set_column_spacing(10);
        summary.set_column_homogeneous(true);
        let mut values = Vec::new();
        for (column, (value, label)) in
            [("–", "Installed"), ("–", "Connected"), ("–", "Configured")]
                .into_iter()
                .enumerate()
        {
            let (item, value) = summary_item(value, label);
            item.set_size_request(0, -1);
            item.set_hexpand(true);
            values.push(value);
            summary.attach(&item, column as i32, 0, 1, 1);
        }
        page.append(&summary);
        let list = gtk::Box::new(gtk::Orientation::Vertical, 12);
        page.append(&list);
        scroller.set_child(Some(&page));
        root.append(&scroller);

        let client_page = Self {
            credential_page,
            app: app.clone(),
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
            pending_confirmation: std::rc::Rc::new(std::cell::RefCell::new(None)),
            feedback_timer: std::rc::Rc::new(std::cell::RefCell::new(None)),
        };
        let page_for_add = client_page.clone();
        add_location.connect_clicked(move |_| page_for_add.add_location());
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

    fn add_location(&self) {
        let Some(parent) = self.root.root().and_downcast::<gtk::Window>() else {
            return;
        };
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let types = match gtk::gio::spawn_blocking(crate::clients::client_location_types).await
            {
                Ok(types) => types,
                Err(_) => {
                    page.show_error(&crate::i18n::tr("Could not load client types."));
                    return;
                }
            };
            #[allow(deprecated)]
            let dialog = adw::MessageDialog::new(Some(&parent), Some(&crate::i18n::tr("Add client location")), Some(&crate::i18n::tr("Choose this client's config folder. Locations stay on this machine. Connecting is a separate step.")));
            l10n::bind_dialog_heading(&dialog, "Add client location");
            l10n::bind_dialog_body(&dialog, "Choose this client's config folder. Locations stay on this machine. Connecting is a separate step.");
            let fields = gtk::Box::new(gtk::Orientation::Vertical, 8);
            let kind_label = {
                let widget = gtk::Label::builder()
                    .label(crate::i18n::tr("Client type"))
                    .xalign(0.0)
                    .build();
                l10n::bind_label(&widget, "Client type");
                widget
            };
            fields.append(&kind_label);
            let names: Vec<&str> = types.iter().map(|kind| kind.name.as_str()).collect();
            let kind = gtk::DropDown::from_strings(&names);
            kind.set_selected(
                types
                    .iter()
                    .position(|kind| kind.id == "claude-code")
                    .unwrap_or(0) as u32,
            );
            fields.append(&kind);
            fields.append(&{
                let widget = gtk::Label::builder()
                    .label(crate::i18n::tr("Config folder"))
                    .xalign(0.0)
                    .build();
                l10n::bind_label(&widget, "Config folder");
                widget
            });
            let folder = gtk::Entry::builder().placeholder_text("~/.claude2").build();
            fields.append(&folder);
            let hint = {
                let widget = gtk::Label::builder()
                    .label(crate::i18n::tr("For Codex, choose its CODEX_HOME folder."))
                    .wrap(true)
                    .xalign(0.0)
                    .build();
                l10n::bind_label(&widget, "For Codex, choose its CODEX_HOME folder.");
                widget
            };
            fields.append(&hint);
            dialog.set_extra_child(Some(&fields));
            dialog.add_response("cancel", &crate::i18n::tr("Cancel"));
            l10n::bind_dialog_response(&dialog, "cancel", "Cancel");
            dialog.add_response("add", &crate::i18n::tr("Add location"));
            l10n::bind_dialog_response(&dialog, "add", "Add location");
            dialog.set_response_enabled("add", false);
            dialog.set_close_response("cancel");
            let dialog_for_input = dialog.clone();
            folder.connect_changed(move |entry| {
                dialog_for_input.set_response_enabled("add", !entry.text().trim().is_empty())
            });
            dialog.connect_response(None, move |_, response| {
                if response != "add" {
                    return;
                }
                let Some(selected) = types.get(kind.selected() as usize) else {
                    return;
                };
                let client_type = selected.id.clone();
                let folder = folder.text().to_string();
                let page = page.clone();
                gtk::glib::spawn_future_local(async move {
                    let result = gtk::gio::spawn_blocking(move || {
                        crate::clients::add_client_location(&client_type, &folder)
                    })
                    .await;
                    match result {
                        Ok(Ok(_)) => page.refresh_with_confirmation(crate::i18n::tr(
                            "Client location added. Select Connect to configure it.",
                        )),
                        Ok(Err(error)) => page.show_error(&error),
                        Err(_) => {
                            page.show_error(&crate::i18n::tr("Could not add client location."))
                        }
                    }
                });
            });
            dialog.present();
        });
    }

    fn preview_imports(&self) {
        if self.scanning.replace(true) {
            return;
        }
        self.import_button.set_sensitive(false);
        self.show_progress(&crate::i18n::tr(
            "Looking for servers in client configurations…",
        ));
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let result =
                gtk::gio::spawn_blocking(crate::registry_controller::preview_client_imports).await;
            page.scanning.set(false);
            page.import_button.set_sensitive(true);
            match result {
                Ok(Ok(candidates)) if candidates.is_empty() => {
                    page.show_confirmation(&crate::i18n::tr(
                        "No new client-configured servers are available to import.",
                    ));
                }
                Ok(Ok(candidates)) => page.show_import_review(candidates),
                Ok(Err(error)) => page.show_error(&crate::i18n::trf(
                    "Could not inspect client configurations: {value0}",
                    &[("value0", &error.to_string())],
                )),
                Err(_) => {
                    page.show_error(&crate::i18n::tr("The import scan stopped unexpectedly."))
                }
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
        let items = candidates
            .into_iter()
            .map(|candidate| crate::registry_controller::SetupItem {
                key: candidate.key,
                name: candidate.name,
                transport: candidate.transport,
                command: candidate.command,
                args: candidate.args,
                url: candidate.url,
                env_keys: candidate
                    .credentials
                    .iter()
                    .map(|env| env.key.clone())
                    .collect(),
                credentials: candidate.credentials,
                unsupported: candidate.unsupported,
                updates: Vec::new(),
                is_new: true,
            })
            .collect();
        let page = self.clone();
        setup::review(&parent, &crate::i18n::tr("Review servers to import"), items,
            &crate::i18n::tr("Review each command and URL. Values are saved using your keychain choices. Missing inputs stay off."),
            crate::i18n::msg("Import selected servers"), false, move |selected, choices, inputs| {
                let (registry, added) = crate::registry_controller::import_client_servers_inputs(selected, &choices, &inputs)?;
                let missing = registry.servers.iter().filter(|server| !server.enabled && server.source.as_deref().is_some_and(|source| source.starts_with("imported:"))).map(|server| server.name.clone()).collect::<Vec<_>>();
                let message = if missing.is_empty() { crate::i18n::trf("Imported {value0} servers. Check their status under Servers.", &[("value0", &format!("{}", added))]) } else { crate::i18n::trf("Imported {value0} servers. Needs input: {value1}", &[("value0", &format!("{}", added)), ("value1", &missing.join(", ").to_string())]) };
                Ok(message.into())
            }, move || page.refresh(), Some(self.credential_page.clone()));
    }

    fn refresh(&self) {
        if self.scanning.replace(true) {
            return;
        }
        self.refresh_button.set_sensitive(false);
        self.import_button.set_sensitive(false);
        self.show_progress(&crate::i18n::tr("Scanning local client configurations…"));
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(state::detect_client_views).await;
            page.scanning.set(false);
            page.refresh_button.set_sensitive(true);
            page.import_button.set_sensitive(true);
            match result {
                Ok(Ok(snapshot)) => page.render(snapshot),
                Ok(Err(error)) => page.show_error(&crate::i18n::trf(
                    "Could not scan clients: {value0}",
                    &[("value0", &error.to_string())],
                )),
                Err(_) => {
                    page.show_error(&crate::i18n::tr("The client scan stopped unexpectedly."))
                }
            }
        });
    }

    fn render(&self, snapshot: state::ClientSnapshot) {
        *self.profiles.borrow_mut() = snapshot.profiles;
        let sessions = snapshot.sessions;
        let clients = snapshot.clients;
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        let installed = clients
            .iter()
            .filter(|client| client.app_present || client.config_exists)
            .collect::<Vec<_>>();
        let connected_clients = installed
            .iter()
            .filter(|client| client.gateway_state == state::ClientGatewayState::Connected)
            .copied()
            .collect::<Vec<_>>();
        let available_clients = installed
            .iter()
            .filter(|client| client.gateway_state != state::ClientGatewayState::Connected)
            .copied()
            .collect::<Vec<_>>();
        let configured = clients.iter().filter(|client| client.config_exists).count();
        self.installed_count.set_label(&installed.len().to_string());
        self.connected_count
            .set_label(&connected_clients.len().to_string());
        self.configured_count.set_label(&configured.to_string());
        if let Some(confirmation) = self.pending_confirmation.borrow_mut().take() {
            self.show_confirmation(&confirmation);
        } else {
            self.cancel_feedback_timer();
            self.feedback.set_visible(false);
        }

        let absent = clients
            .iter()
            .filter(|client| !client.app_present && !client.config_exists)
            .collect::<Vec<_>>();
        if installed.is_empty() {
            self.list.append(&state_card(
                "computer-symbolic",
                crate::i18n::msg("No supported clients detected"),
                crate::i18n::msg("Install or open a supported AI client, then scan again."),
                false,
            ));
        }
        if !connected_clients.is_empty() {
            self.list.append(&client_section_title(
                crate::i18n::msg("Connected to Toolport"),
                connected_clients.len(),
            ));
        }
        for client in connected_clients {
            self.list.append(&client_card(client, self.clone()));
        }
        if !available_clients.is_empty() {
            self.list.append(&client_section_title(
                crate::i18n::msg("Available to connect"),
                available_clients.len(),
            ));
        }
        for client in available_clients {
            self.list.append(&client_card(client, self.clone()));
        }
        // Supported-but-absent clients: collapsed so they don't crowd the real
        // list, but visible so the user knows what Toolport could manage.
        if !absent.is_empty() {
            let expander = {
                let widget = gtk::Expander::new(None);
                widget.set_label_widget(Some(&l10n::formatted_label(
                    if absent.len() == 1 {
                        crate::i18n::msg("Not installed · {value0} supported client")
                    } else {
                        crate::i18n::msg("Not installed · {value0} supported clients")
                    },
                    &[("value0", &format!("{}", absent.len()))],
                )));
                widget
            };
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
        if snapshot.sessions_error {
            self.list.append(&{
                let widget = gtk::Label::builder()
                    .label(crate::i18n::tr("Client activity could not be read."))
                    .halign(gtk::Align::Start)
                    .wrap(true)
                    .css_classes(["toolport-muted"])
                    .build();
                l10n::bind_label(&widget, "Client activity could not be read.");
                widget
            });
        }
        if !sessions.is_empty() {
            let expander = {
                let widget = gtk::Expander::new(None);
                widget.set_label_widget(Some(&l10n::formatted_label(
                    "Recent client activity · {value0}",
                    &[("value0", &format!("{}", sessions.len()))],
                )));
                widget
            };
            expander.add_css_class("toolport-card");
            let rows = gtk::Box::new(gtk::Orientation::Vertical, 8);
            for client in sessions {
                let row = gtk::Box::new(gtk::Orientation::Vertical, 3);
                let name = crate::clients::display_caller_name(
                    client["clientName"].as_str(),
                    client["clientLabel"].as_str(),
                );
                let tooltip = if name == "Unrecorded client" {
                    unrecorded_client_tooltip()
                } else {
                    crate::i18n::trf(
                        "{value0} recorded sessions. Reports itself as: {value1}",
                        &[
                            (
                                "value0",
                                &format!("{}", client["sessionCount"].as_u64().unwrap_or(1)),
                            ),
                            (
                                "value1",
                                client["clientLabel"]
                                    .as_str()
                                    .map(str::to_owned)
                                    .unwrap_or_else(|| crate::i18n::tr("not provided"))
                                    .as_str(),
                            ),
                        ],
                    )
                };
                row.append(
                    &gtk::Label::builder()
                        .label(localized_caller_name(
                            client["clientName"].as_str(),
                            client["clientLabel"].as_str(),
                        ))
                        .tooltip_text(tooltip)
                        .halign(gtk::Align::Start)
                        .css_classes(["heading"])
                        .build(),
                );
                row.append(&gtk::Label::builder().label(client_activity_detail(&client)).tooltip_text(crate::i18n::tr("Calls today counts retained tool calls since local midnight. Older calls may no longer be retained.")).halign(gtk::Align::Start).wrap(true).css_classes(["toolport-muted"]).build());
                rows.append(&row);
            }
            expander.set_child(Some(&rows));
            self.list.append(&expander);
        }
    }

    fn show_error(&self, error: &str) {
        self.pending_confirmation.borrow_mut().take();
        self.cancel_feedback_timer();
        self.feedback.set_label(error);
        self.feedback.remove_css_class("success");
        self.feedback.add_css_class("error");
        self.feedback.set_visible(true);
    }

    fn show_progress(&self, message: &str) {
        self.cancel_feedback_timer();
        self.feedback.set_label(message);
        self.feedback.remove_css_class("error");
        self.feedback.remove_css_class("success");
        self.feedback.set_visible(true);
    }

    fn show_confirmation(&self, message: &str) {
        self.cancel_feedback_timer();
        self.feedback.set_label(message);
        self.feedback.remove_css_class("error");
        self.feedback.add_css_class("success");
        self.feedback.set_visible(true);
        let feedback = self.feedback.clone();
        let feedback_timer = self.feedback_timer.clone();
        let timer =
            gtk::glib::timeout_add_local_once(std::time::Duration::from_secs(4), move || {
                feedback.set_visible(false);
                feedback.set_label("");
                feedback_timer.borrow_mut().take();
            });
        *self.feedback_timer.borrow_mut() = Some(timer);
    }

    fn refresh_with_confirmation(&self, message: String) {
        *self.pending_confirmation.borrow_mut() = Some(message);
        self.refresh();
    }

    fn cancel_feedback_timer(&self) {
        if let Some(timer) = self.feedback_timer.borrow_mut().take() {
            timer.remove();
        }
    }
}

fn client_activity_detail(client: &serde_json::Value) -> String {
    let mut details = vec![client["lastActiveMs"]
        .as_u64()
        .map(|ts| {
            crate::i18n::trf(
                "Last active {value0}",
                &[("value0", &relative_activity_time(ts).to_string())],
            )
        })
        .unwrap_or_else(|| crate::i18n::tr("Last active time unavailable"))];
    if let Some(calls) = client["callsToday"].as_u64() {
        details.push(crate::i18n::trf(
            if calls == 1 {
                crate::i18n::msg("{value0} call today")
            } else {
                crate::i18n::msg("{value0} calls today")
            },
            &[("value0", &grouped_number(calls).to_string())],
        ));
    }
    if let Some(tools) = client["firstCatalogSize"].as_u64() {
        details.push(crate::i18n::trf(
            "Last saw {value0} tools",
            &[("value0", &grouped_number(tools).to_string())],
        ));
    }
    details.join(" · ")
}

fn grouped_number(n: u64) -> String {
    let digits = n.to_string();
    digits
        .chars()
        .enumerate()
        .fold(String::new(), |mut s, (i, c)| {
            if i > 0 && (digits.len() - i).is_multiple_of(3) {
                s.push(',');
            }
            s.push(c);
            s
        })
}

fn client_section_title(title: &'static str, count: usize) -> gtk::Box {
    let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    heading.set_margin_top(6);
    heading.append(&{
        let widget = gtk::Label::builder()
            .label(title)
            .halign(gtk::Align::Start)
            .hexpand(true)
            .css_classes(["toolport-section-label"])
            .build();
        l10n::bind_label(&widget, title);
        widget
    });
    let count = gtk::Label::new(Some(&count.to_string()));
    count.add_css_class("toolport-badge");
    count.set_valign(gtk::Align::Center);
    count.set_halign(gtk::Align::Start);
    heading.append(&count);
    heading
}

fn toolport_menu_popover() -> gtk::Popover {
    let popover = gtk::Popover::new();
    popover.add_css_class("toolport-menu");
    popover
}

fn toolport_menu_button(label: &str) -> gtk::Button {
    let label = gtk::Label::builder()
        .label(label)
        .halign(gtk::Align::Start)
        .xalign(0.0)
        .build();
    gtk::Button::builder()
        .child(&label)
        .css_classes(["flat", "toolport-menu-item"])
        .build()
}

fn toolport_menu_source_button(source: &'static str) -> gtk::Button {
    let button = toolport_menu_button(source);
    let label = button
        .child()
        .and_downcast::<gtk::Label>()
        .expect("menu caption");
    l10n::bind_label(&label, source);
    button
}
fn toolport_menu_formatted_button(source: &'static str, values: &[(&str, &str)]) -> gtk::Button {
    let button = toolport_menu_button("");
    let label = button
        .child()
        .and_downcast::<gtk::Label>()
        .expect("menu caption");
    l10n::bind_formatted_label(&label, source, values);
    button
}
fn toolport_menu_choice_button(label: &'static str, selected: bool) -> gtk::Button {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    content.append(&{
        let widget = gtk::Label::builder()
            .label(label)
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .hexpand(true)
            .build();
        l10n::bind_label(&widget, label);
        widget
    });
    if selected {
        content.append(
            &gtk::Image::builder()
                .icon_name("object-select-symbolic")
                .css_classes(["toolport-menu-check"])
                .build(),
        );
    }
    gtk::Button::builder()
        .child(&content)
        .css_classes(["flat", "toolport-menu-item"])
        .build()
}

fn wrapping_actions() -> gtk::FlowBox {
    gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .min_children_per_line(1)
        .max_children_per_line(8)
        .column_spacing(8)
        .row_spacing(6)
        .build()
}

fn client_card(client: &state::ClientView, page: ClientPage) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 10);
    card.add_css_class("toolport-card");
    let heading = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    card.append(&heading);
    let icon = branding::client_logo(&client.id);
    icon.set_valign(gtk::Align::Start);
    heading.append(&icon);
    let copy = gtk::Box::new(gtk::Orientation::Vertical, 3);
    copy.set_hexpand(true);
    copy.append(
        &gtk::Label::builder()
            .label(&client.name)
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .tooltip_text(crate::i18n::trf(
                "{value0}\nClient id: {value1}",
                &[
                    ("value0", &client.name.to_string()),
                    ("value1", &client.id.to_string()),
                ],
            ))
            .css_classes(["heading"])
            .build(),
    );
    let detail = if client.config_error {
        crate::i18n::tr("Configuration could not be read safely").to_string()
    } else if client.uses_connectors {
        let mut detail = crate::i18n::tr("Toolport gateway").to_string();
        if client.gateway_state == state::ClientGatewayState::Connected {
            detail.push_str(if client.shared_http {
                " · Shared HTTP"
            } else {
                " · stdio"
            });
            // Access is not repeated here: the card's access button shows it.
        }
        detail
    } else if client.gateway_state == state::ClientGatewayState::Connected {
        // Toolport's own entry is the gateway, not one of the client's servers.
        let mut detail = crate::i18n::tr("Toolport gateway").to_string();
        if client.other_server_count > 0 {
            detail.push_str(&crate::i18n::trf(
                if client.other_server_count == 1 {
                    crate::i18n::msg(" · {value0} other server")
                } else {
                    crate::i18n::msg(" · {value0} other servers")
                },
                &[("value0", &format!("{}", client.other_server_count))],
            ));
        }
        if client.movable_server_count > 0 {
            detail.push_str(&crate::i18n::trf(
                " · {value0} to import",
                &[("value0", &format!("{}", client.movable_server_count))],
            ));
        }
        detail.push_str(if client.shared_http {
            " · Shared HTTP"
        } else {
            " · stdio"
        });
        detail
    } else {
        let mut detail = crate::i18n::trf(
            if client.server_count == 1 {
                crate::i18n::msg("{value0} local MCP server")
            } else {
                crate::i18n::msg("{value0} local MCP servers")
            },
            &[("value0", &format!("{}", client.server_count))],
        );
        if client.movable_server_count > 0 {
            detail.push_str(&crate::i18n::trf(
                " · {value0} to import",
                &[("value0", &format!("{}", client.movable_server_count))],
            ));
        }
        if client.gateway_state == state::ClientGatewayState::Connected {
            detail.push_str(if client.shared_http {
                " · Shared HTTP"
            } else {
                " · stdio"
            });
            // Access is not repeated here: the card's access button shows it.
        }
        detail
    };
    copy.append(
        &gtk::Label::builder()
            .label(detail)
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["toolport-muted"])
            .build(),
    );
    if client.gateway_state == state::ClientGatewayState::Connected {
        let capabilities = crate::clients::discovery_capabilities(&client.id);
        let chosen = client
            .discovery_mode
            .as_deref()
            .or(client.inherited_discovery.as_deref());
        let hint = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-muted"])
            .build();
        if let Some(max) = capabilities.auto_full_max_tools_for(chosen) {
            // Auto lists a small catalog in full for this client, then switches to search.
            let max = max.to_string();
            l10n::bind_label_with(&hint, move || {
                crate::i18n::trf(
                    "Auto: {mode}",
                    &[(
                        "mode",
                        &crate::i18n::trf(
                            "full up to {count} tools, then lazy",
                            &[("count", &max)],
                        ),
                    )],
                )
            });
        } else {
            l10n::bind_label(
                &hint,
                match capabilities.resolve_mode(chosen) {
                    "full" => crate::i18n::msg("Full tool list. Client per-tool permission rules need Full mode."),
                    "grouped" => {
                        crate::i18n::msg("Browse a server, then call tools. Client per-tool permission rules need Full mode.")
                    }
                    _ => crate::i18n::msg("Search, then call tools. Client per-tool permission rules need Full mode."),
                },
            );
        }
        copy.append(&hint);
    }
    heading.append(&copy);
    if client.gateway_state == state::ClientGatewayState::Customized {
        let badge = l10n::label("Customized");
        badge.add_css_class("toolport-badge");
        badge.set_valign(gtk::Align::Center);
        badge.set_halign(gtk::Align::Start);
        badge.add_css_class("review");
        copy.append(&badge);
    }
    let actions = wrapping_actions();
    if client.legacy_bearer_argv {
        let warning = l10n::label("This older HTTP connection exposes a bearer credential in process arguments. Review migration to stdio; it stays unchanged until you confirm.");
        warning.set_wrap(true);
        warning.set_xalign(0.0);
        card.append(&warning);
        let migrate = l10n::button("Review migration to stdio");
        let reviewed_client = client.clone();
        let reviewed_page = page.clone();
        migrate.connect_clicked(move |button| {
            confirm_client_reset(&reviewed_client, button.clone(), reviewed_page.clone());
        });
        actions.insert(&migrate, -1);
    }
    if client.location_removable {
        let remove = l10n::button("Remove location");
        remove.set_sensitive(
            !client.config_error && client.gateway_state == state::ClientGatewayState::Disconnected,
        );
        let id = client.id.clone();
        let page = page.clone();
        remove.connect_clicked(move |_| {
            let Some(parent) = page.root.root().and_downcast::<gtk::Window>() else { return; };
            let dialog = adw::MessageDialog::new(Some(&parent), Some(&crate::i18n::tr("Remove client location?")), Some(&crate::i18n::tr("This forgets the added location. Your client config and backups are kept. Auto-detected Claude folders still appear in Clients.")));
l10n::bind_dialog_heading(&dialog, "Remove client location?");
l10n::bind_dialog_body(&dialog, "This forgets the added location. Your client config and backups are kept. Auto-detected Claude folders still appear in Clients.");
            dialog.add_responses(&[("cancel", "Cancel"), ("remove", "Remove location")]);
l10n::bind_dialog_response(&dialog,"cancel","Cancel");
l10n::bind_dialog_response(&dialog,"remove","Remove location");
            let id = id.clone();
            let page = page.clone();
            dialog.connect_response(Some("remove"), move |_, _| {
                let id = id.clone();
                let page = page.clone();
                gtk::glib::spawn_future_local(async move {
                    match gtk::gio::spawn_blocking(move || crate::clients::remove_client_location(&id)).await {
                        Ok(Ok(())) => page.refresh_with_confirmation(crate::i18n::tr("Client location removed.")),
                        Ok(Err(error)) => page.show_error(&error),
                        Err(_) => page.show_error(&crate::i18n::tr("Could not remove client location.")),
                    }
                });
            });
            dialog.present();
        });
        actions.insert(&remove, -1);
    }
    match client.gateway_state {
        state::ClientGatewayState::Disconnected => {
            let connect = l10n::button("Connect");
            connect.add_css_class("suggested-action");
            let client_for_connect = client.clone();
            let page_for_connect = page.clone();
            connect.connect_clicked(move |_| {
                confirm_client_migrate(&client_for_connect, page_for_connect.clone());
            });
            actions.insert(&connect, -1);
        }
        state::ClientGatewayState::Customized if client_needs_reset_action(client) => {
            let reset = l10n::button("Reset");
            reset.add_css_class("toolport-secondary-action");
            let client_for_reset = client.clone();
            let page_for_reset = page.clone();
            reset.connect_clicked(move |button| {
                confirm_client_reset(&client_for_reset, button.clone(), page_for_reset.clone());
            });
            actions.insert(&reset, -1);
        }
        state::ClientGatewayState::Customized => {}
        state::ClientGatewayState::Connected => {
            actions.insert(&client_scope_menu(client.clone(), page.clone()), -1);
            actions.insert(&connected_client_actions_menu(client.clone(), page), -1);
        }
    }
    if actions.first_child().is_some() {
        card.append(&actions);
    }
    card
}

fn confirm_client_migrate(client: &state::ClientView, page: ClientPage) {
    let Some(parent) = page.root.root().and_downcast::<gtk::Window>() else {
        return;
    };
    setup::connect(
        &parent,
        client.id.clone(),
        client.scope_id.clone(),
        client.gateway_state == state::ClientGatewayState::Customized,
        {
            let page = page.clone();
            move || page.refresh()
        },
        Some(page.credential_page.clone()),
    );
}

fn append_client_discovery_actions(
    content: &gtk::Box,
    client: &state::ClientView,
    page: &ClientPage,
    menu: &gtk::MenuButton,
) {
    content.append(&{
        let widget = gtk::Label::builder()
            .label(crate::i18n::tr("Discovery"))
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .css_classes(["toolport-menu-heading"])
            .build();
        l10n::bind_label(&widget, "Discovery");
        widget
    });
    let auto_hint = match client.inherited_discovery.as_deref() {
        Some(mode) => crate::i18n::trf(
            "Use your global discovery choice ({value0})",
            &[("value0", mode)],
        ),
        None => {
            crate::i18n::tr("Use this client's measured backend default discovery mode").to_string()
        }
    };
    for (label, mode, _hint) in [
        (crate::i18n::msg("Auto"), None, auto_hint.as_str()),
        (
            crate::i18n::msg("Full catalog"),
            Some("full"),
            crate::i18n::msg("Full tool list. Client per-tool permission rules need Full mode."),
        ),
        (
            crate::i18n::msg("Lazy search"),
            Some("lazy"),
            crate::i18n::msg("Search, then call tools. Client per-tool permission rules need Full mode."),
        ),
        (
            crate::i18n::msg("Grouped search"),
            Some("grouped"),
            crate::i18n::msg("Browse a server, then call tools. Client per-tool permission rules need Full mode."),
        ),
    ] {
        let selected = match (client.discovery_mode.as_deref(), mode) {
            (None, None) => true,
            (Some(stored), Some(mode)) => stored.trim().eq_ignore_ascii_case(mode),
            _ => false,
        };
        let button = toolport_menu_choice_button(label, selected);
        let tooltip = match mode {
            Some("full") => crate::i18n::tr("Full tool list. Client per-tool permission rules need Full mode."),
            Some("grouped") => crate::i18n::tr("Browse a server, then call tools. Client per-tool permission rules need Full mode."),
            Some("lazy") => crate::i18n::tr("Search, then call tools. Client per-tool permission rules need Full mode."),
            _ => auto_hint.clone(),
        };
        button.set_tooltip_text(Some(&tooltip));
        let client_id = client.id.clone();
        let client_name = client.name.clone();
        let page = page.clone();
        let menu = menu.clone();
        button.connect_clicked(move |_| {
            menu.popdown();
            page.show_progress(&crate::i18n::tr("Updating client discovery…"));
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
                        page.refresh_with_confirmation(crate::i18n::trf(
                            "Updated discovery for {value0}.",
                            &[("value0", &client_name.to_string())],
                        ));
                    }
                    Ok(Err(error)) => page.show_error(&error),
                    Err(_) => page.show_error(&crate::i18n::tr(
                        "the discovery update stopped unexpectedly",
                    )),
                }
            });
        });
        content.append(&button);
    }
}

fn connected_client_actions_menu(client: state::ClientView, page: ClientPage) -> gtk::MenuButton {
    let menu = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .tooltip_text(crate::i18n::trf(
            "Actions for {value0}",
            &[("value0", &client.name.to_string())],
        ))
        .css_classes(["flat"])
        .build();
    let popover = toolport_menu_popover();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 4);
    content.set_margin_top(6);
    content.set_margin_bottom(6);
    content.set_margin_start(6);
    content.set_margin_end(6);

    if client.movable_server_count > 0 && !client.config_error {
        let count = client.movable_server_count;
        let migrate = toolport_menu_formatted_button(
            if count == 1 {
                crate::i18n::msg("Move {count} server into Toolport")
            } else {
                crate::i18n::msg("Move {count} servers into Toolport")
            },
            &[("count", &count.to_string())],
        );
        let client_for_migrate = client.clone();
        let page_for_migrate = page.clone();
        let menu_for_migrate = menu.clone();
        migrate.connect_clicked(move |_| {
            menu_for_migrate.popdown();
            confirm_client_migrate(&client_for_migrate, page_for_migrate.clone());
        });
        content.append(&migrate);
        content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    }

    append_client_discovery_actions(&content, &client, &page, &menu);
    content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    let disconnect = toolport_menu_source_button(crate::i18n::msg("Disconnect from Toolport"));
    disconnect.add_css_class("destructive-action");
    let menu_for_disconnect = menu.clone();
    disconnect.connect_clicked(move |button| {
        menu_for_disconnect.popdown();
        confirm_client_disconnect(&client, button.clone(), page.clone());
    });
    content.append(&disconnect);
    popover.set_child(Some(&content));
    menu.set_popover(Some(&popover));
    menu
}

fn client_scope_menu(client: state::ClientView, page: ClientPage) -> gtk::MenuButton {
    let label = client
        .scope_name
        .as_deref()
        .map(|scope| scope.to_string())
        .unwrap_or_else(|| crate::i18n::tr("Default access").to_string());
    // "Default access" and "All enabled servers" already say what they are.
    let label = if client
        .scope_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .is_none_or(|id| id == crate::registry::ALL_ENABLED_ACCESS)
    {
        label
    } else {
        crate::i18n::trf("Access: {value0}", &[("value0", &label.to_string())])
    };
    let menu = gtk::MenuButton::builder()
        .always_show_arrow(true)
        .child(
            &gtk::Label::builder()
                .label(&label)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .max_width_chars(24)
                .build(),
        )
        .tooltip_text(crate::i18n::trf(
            "Choose this client's access to enabled servers: {value0}",
            &[("value0", &label.to_string())],
        ))
        .build();
    menu.add_css_class("toolport-secondary-action");
    let popover = toolport_menu_popover();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 4);
    content.set_margin_top(6);
    content.set_margin_bottom(6);
    content.set_margin_start(6);
    content.set_margin_end(6);

    for profile in page.profiles.borrow().iter().cloned() {
        let button = toolport_menu_button(&profile.name);
        let client = client.clone();
        let page = page.clone();
        let menu = menu.clone();
        button.connect_clicked(move |button| {
            menu.popdown();
            run_client_mutation(
                &client,
                true,
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

fn client_needs_reset_action(client: &state::ClientView) -> bool {
    !client.legacy_bearer_argv
}

fn client_reset_scope(client: &state::ClientView) -> Option<String> {
    client.scope_id.clone()
}

fn client_reset_label(client: &state::ClientView) -> &'static str {
    if client.legacy_bearer_argv {
        crate::i18n::msg("Migrate to stdio")
    } else {
        crate::i18n::msg("Reset and connect")
    }
}

fn confirm_client_reset(client: &state::ClientView, button: gtk::Button, page: ClientPage) {
    let Some(parent) = page.app.active_window() else {
        return;
    };
    #[allow(deprecated)]
    let dialog = adw::MessageDialog::new(Some(&parent), Some(&crate::i18n::trf(if client.legacy_bearer_argv { crate::i18n::msg("Migrate {value1}'s Toolport entry?") } else { crate::i18n::msg("Reset {value1}'s Toolport entry?") }, &[("value1", &client.name.to_string())])), Some(&crate::i18n::tr("Toolport backs up the config before replacing its gateway entry with the standard stdio command. Customized commands, arguments and headers will be replaced; review those changes before confirming. Other client settings and MCP servers are preserved. Restart the client afterward.")));
    l10n::bind_formatted_dialog_heading(
        &dialog,
        if client.legacy_bearer_argv {
            crate::i18n::msg("Migrate {value1}'s Toolport entry?")
        } else {
            crate::i18n::msg("Reset {value1}'s Toolport entry?")
        },
        &[("value1", &client.name.to_string())],
    );
    l10n::bind_dialog_body(&dialog, "Toolport backs up the config before replacing its gateway entry with the standard stdio command. Customized commands, arguments and headers will be replaced; review those changes before confirming. Other client settings and MCP servers are preserved. Restart the client afterward.");
    dialog.add_response("cancel", &crate::i18n::tr("Cancel"));
    l10n::bind_dialog_response(&dialog, "cancel", "Cancel");
    dialog.add_response("reset", &crate::i18n::lookup(client_reset_label(client)));
    dialog.set_close_response("cancel");
    dialog.set_default_response(Some("cancel"));
    dialog.set_response_appearance("reset", adw::ResponseAppearance::Suggested);
    let client = client.clone();
    dialog.connect_response(None, move |dialog, response| {
        if response == "reset" {
            run_client_mutation(
                &client,
                true,
                true,
                client_reset_scope(&client),
                &button,
                page.clone(),
            );
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
    let dialog = adw::MessageDialog::new(Some(&parent), Some(&crate::i18n::trf("Disconnect {value0}?", &[("value0", &client.name.to_string())])), Some(&crate::i18n::tr("Toolport removes only its managed gateway entry. The client's other settings and MCP servers are preserved.")));
    l10n::bind_formatted_dialog_heading(
        &dialog,
        "Disconnect {value0}?",
        &[("value0", &client.name.to_string())],
    );
    l10n::bind_dialog_body(&dialog, "Toolport removes only its managed gateway entry. The client's other settings and MCP servers are preserved.");
    dialog.add_response("cancel", &crate::i18n::tr("Cancel"));
    l10n::bind_dialog_response(&dialog, "cancel", "Cancel");
    dialog.add_response("disconnect", &crate::i18n::tr("Disconnect"));
    l10n::bind_dialog_response(&dialog, "disconnect", "Disconnect");
    dialog.set_close_response("cancel");
    dialog.set_default_response(Some("cancel"));
    dialog.set_response_appearance("disconnect", adw::ResponseAppearance::Destructive);
    let client = client.clone();
    dialog.connect_response(None, move |dialog, response| {
        if response == "disconnect" {
            run_client_mutation(&client, false, false, None, &button, page.clone());
        }
        dialog.close();
    });
    dialog.present();
}

fn run_client_mutation(
    client: &state::ClientView,
    connect: bool,
    force: bool,
    profile: Option<String>,
    button: &gtk::Button,
    page: ClientPage,
) {
    button.set_sensitive(false);
    page.show_progress(&crate::i18n::tr(if connect {
        crate::i18n::msg("Connecting client…")
    } else {
        crate::i18n::msg("Disconnecting client…")
    }));
    let client_id = client.id.clone();
    let client_name = client.name.clone();
    let button = button.clone();
    gtk::glib::spawn_future_local(async move {
        let result = gtk::gio::spawn_blocking(move || {
            if connect {
                crate::registry_controller::connect_client_stdio(
                    &client_id,
                    profile.as_deref(),
                    force,
                )
            } else {
                crate::registry_controller::disconnect_client(&client_id)
            }
        })
        .await;
        button.set_sensitive(true);
        match result {
            Ok(Ok(result)) => {
                // The config write is not live until the client restarts; saying
                // only "Connected" would misstate what the running client does.
                let restored = result.outcome.restored.len();
                let message = if connect {
                    crate::i18n::trf(
                        "Connected {value0} to Toolport. Restart {value1} to apply it.",
                        &[
                            ("value0", &client_name.to_string()),
                            ("value1", &client_name.to_string()),
                        ],
                    )
                } else if restored > 0 {
                    crate::i18n::trf(
                        if restored == 1 {
                            crate::i18n::msg("Disconnected {value0} from Toolport and put back {value1} server it had moved. Restart {value3} to apply it.")
                        } else {
                            crate::i18n::msg("Disconnected {value0} from Toolport and put back {value1} servers it had moved. Restart {value3} to apply it.")
                        },
                        &[
                            ("value0", &client_name.to_string()),
                            ("value1", &format!("{}", restored)),
                            ("value3", &client_name.to_string()),
                        ],
                    )
                } else {
                    crate::i18n::trf(
                        "Disconnected {value0} from Toolport. Restart {value1} to apply it.",
                        &[
                            ("value0", &client_name.to_string()),
                            ("value1", &client_name.to_string()),
                        ],
                    )
                };
                page.refresh_with_confirmation(
                    std::iter::once(message)
                        .chain(result.outcome.warnings)
                        .collect::<Vec<_>>()
                        .join(" "),
                );
            }
            Ok(Err(error)) => page.show_error(&format!("{client_name}: {error}")),
            Err(_) => page.show_error(&crate::i18n::trf(
                "{value0}: the operation stopped",
                &[("value0", &client_name.to_string())],
            )),
        }
    });
}

type RetainedRows<T> = std::rc::Rc<std::cell::RefCell<Vec<(T, gtk::Widget)>>>;

fn reconcile_rows<T: Clone + PartialEq>(
    list: &gtk::Box,
    retained: &RetainedRows<T>,
    values: &[T],
    build: impl Fn(&T) -> gtk::Widget,
) {
    let mut rows = retained.borrow_mut();
    if rows.iter().map(|(value, _)| value).eq(values.iter()) {
        return;
    }
    if rows.is_empty() {
        while let Some(child) = list.first_child() {
            list.remove(&child);
        }
    }
    let mut previous = std::mem::take(&mut *rows);
    let mut sibling = None::<gtk::Widget>;
    for value in values {
        let row = if let Some(index) = previous.iter().position(|(old, _)| old == value) {
            previous.remove(index).1
        } else {
            let row = build(value);
            list.append(&row);
            row
        };
        if row.prev_sibling() != sibling.clone() {
            list.reorder_child_after(&row, sibling.as_ref());
        }
        sibling = Some(row.clone());
        rows.push((value.clone(), row));
    }
    for (_, row) in previous {
        list.remove(&row);
    }
}

const RECENT_CALL_PREVIEW_LIMIT: usize = 10;

#[derive(Clone)]
struct ActivityPage {
    app: adw::Application,
    root: gtk::Box,
    scroller: gtk::ScrolledWindow,
    list: gtk::Box,
    security_status: gtk::Button,
    security_status_icon: gtk::Image,
    security_status_title: gtk::Label,
    security_status_detail: gtk::Label,
    security_list: gtk::Box,
    security_expander: gtk::Expander,
    diagnostics_expander: gtk::Expander,
    stats_list: gtk::Box,
    identity_list: gtk::Box,
    search_list: gtk::Box,
    inspect_list: gtk::Box,
    feedback: gtk::Label,
    telemetry_feedback: gtk::Label,
    call_count: gtk::Label,
    success_rate: gtk::Label,
    average_latency: gtk::Label,
    tokens_saved: gtk::Label,
    refresh_button: gtk::Button,
    clear_button: gtk::Button,
    loading: std::rc::Rc<std::cell::Cell<bool>>,
    dirty: std::rc::Rc<std::cell::Cell<bool>>,
    revision: std::rc::Rc<std::cell::RefCell<Option<String>>>,
    revision_error: std::rc::Rc<std::cell::RefCell<Option<String>>>,
    revision_loading: std::rc::Rc<std::cell::Cell<bool>>,
    health_loading: std::rc::Rc<std::cell::Cell<bool>>,
    health_generation: std::rc::Rc<std::cell::Cell<u64>>,
    recent_rows: RetainedRows<state::ActivityView>,
    stat_rows: RetainedRows<serde_json::Value>,
    /// The last successfully loaded snapshot. Kept so a failed refresh shows a
    /// stale notice over real rows instead of replacing them with "all clear",
    /// and so filter changes re-render without another disk read.
    last_snapshot: std::rc::Rc<std::cell::RefCell<Option<state::ActivitySnapshot>>>,
    filter_server: gtk::DropDown,
    filter_server_options: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
    errors_only: gtk::ToggleButton,
    filter_count: gtk::Label,
    show_more_calls: gtk::Button,
    show_all_recent: std::rc::Rc<std::cell::Cell<bool>>,
    identity_search: gtk::SearchEntry,
    updating_filters: std::rc::Rc<std::cell::Cell<bool>>,
    savings_banner: gtk::Box,
    savings_title: gtk::Label,
    savings_value: gtk::Label,
    savings_unit: gtk::Label,
    savings_detail: gtk::Label,
    savings_summary: gtk::Label,
    savings_old_estimate: gtk::Label,
    savings_share: gtk::Button,
    expanded_stat_servers: std::rc::Rc<std::cell::RefCell<std::collections::HashSet<String>>>,
    server_stat_order: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
    expanded_activity_rows: ActivityExpansionState,
    /// Persisted security review markers. Tool drift uses a durable identity;
    /// high-severity findings record the newest reviewed timestamp so a later
    /// recurrence becomes visible again.
    security_dismissed: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
}

impl ActivityPage {
    fn new(app: &adw::Application) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("toolport-content");
        let header = adw::HeaderBar::new();
        header.add_css_class("toolport-header");
        header.set_show_back_button(true);
        header.set_title_widget(Some(&{
            let widget = gtk::Label::builder()
                .label(crate::i18n::tr("Activity"))
                .css_classes(["title"])
                .build();
            l10n::bind_label(&widget, "Activity");
            widget
        }));
        let clear_button = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text(crate::i18n::tr("Clear retained activity"))
            .build();
        clear_button.add_css_class("flat");
        let refresh_button = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text(crate::i18n::tr("Refresh activity"))
            .build();
        let export_button = gtk::MenuButton::builder()
            .icon_name("document-save-symbolic")
            .tooltip_text(crate::i18n::tr("Export activity"))
            .build();
        let export_menu = toolport_menu_popover();
        let export_actions = gtk::Box::new(gtk::Orientation::Vertical, 4);
        export_actions.set_margin_top(6);
        export_actions.set_margin_bottom(6);
        export_actions.set_margin_start(6);
        export_actions.set_margin_end(6);
        let export_json = toolport_menu_source_button(crate::i18n::msg("Export JSON"));
        export_actions.append(&export_json);
        let export_csv = toolport_menu_source_button(crate::i18n::msg("Export CSV"));
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
        page.set_margin_top(20);
        page.set_margin_bottom(20);
        page.set_margin_start(20);
        page.set_margin_end(20);
        page.append(&{
            let widget = gtk::Label::builder()
                .label(crate::i18n::tr("Calls and approvals, visible locally"))
                .halign(gtk::Align::Fill)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["title-2"])
                .build();
            l10n::bind_label(&widget, "Calls and approvals, visible locally");
            widget
        });
        page.append(
            &{ let widget = gtk::Label::builder()
                .label(crate::i18n::tr("Toolport records outcomes and timing, never tool arguments or result data. Recent activity is shown below; export includes all saved history."))
                .halign(gtk::Align::Fill)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["toolport-muted"])
                .build(); l10n::bind_label(&widget, "Toolport records outcomes and timing, never tool arguments or result data. Recent activity is shown below; export includes all saved history."); widget },
        );
        let feedback = gtk::Label::builder()
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-feedback"])
            .build();
        l10n::bind_text(&feedback, "Loading retained activity…");
        page.append(&feedback);
        let telemetry_feedback = gtk::Label::builder()
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .wrap(true)
            .visible(false)
            .css_classes(["toolport-feedback"])
            .build();
        page.append(&telemetry_feedback);

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
            ("…", crate::i18n::msg("Calls recorded")),
            ("…", crate::i18n::msg("Success rate")),
            ("…", crate::i18n::msg("Average latency")),
            ("…", crate::i18n::msg("Tokens saved")),
        ] {
            let (item, value) = summary_item(value, label);
            values.push(value);
            summary.insert(&item, -1);
        }
        page.append(&summary);

        let security_status = gtk::Button::new();
        security_status.add_css_class("toolport-security-status");
        l10n::bind_tooltip(&security_status, "Review tool changes");
        let security_status_row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let security_status_icon = gtk::Image::from_icon_name("security-high-symbolic");
        security_status_icon.add_css_class("toolport-security-status-icon");
        security_status_row.append(&security_status_icon);
        let security_status_copy = gtk::Box::new(gtk::Orientation::Vertical, 1);
        security_status_copy.set_hexpand(true);
        let security_status_title = {
            let widget = gtk::Label::builder()
                .label(crate::i18n::tr("Checking protection…"))
                .halign(gtk::Align::Start)
                .xalign(0.0)
                .css_classes(["heading"])
                .build();
            l10n::bind_label(&widget, "Checking protection…");
            widget
        };
        security_status_copy.append(&security_status_title);
        let security_status_detail = {
            let widget = gtk::Label::builder()
                .label(crate::i18n::tr("Loading saved review state."))
                .halign(gtk::Align::Start)
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(["toolport-muted", "caption"])
                .build();
            l10n::bind_label(&widget, "Loading saved review state.");
            widget
        };
        security_status_copy.append(&security_status_detail);
        security_status_row.append(&security_status_copy);
        security_status_row.append(&gtk::Image::from_icon_name("go-next-symbolic"));
        security_status.set_child(Some(&security_status_row));
        page.append(&security_status);

        let performance = gtk::Box::new(gtk::Orientation::Vertical, 12);
        performance.set_margin_top(10);
        let savings_banner = gtk::Box::new(gtk::Orientation::Vertical, 6);
        savings_banner.add_css_class("toolport-card");
        savings_banner.set_visible(false);
        let savings_header = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let savings_title = gtk::Label::builder()
            .halign(gtk::Align::Start)
            .hexpand(true)
            .css_classes(["heading"])
            .build();
        savings_header.append(&savings_title);
        let savings_share = l10n::button("Share");
        savings_share.add_css_class("toolport-secondary-action");
        savings_share.set_valign(gtk::Align::Center);
        savings_share.set_tooltip_text(Some(&crate::i18n::tr(
            "Copy a shareable savings line to the clipboard",
        )));
        savings_header.append(&savings_share);
        savings_banner.append(&savings_header);
        let savings_row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let savings_value = gtk::Label::builder()
            .halign(gtk::Align::Start)
            .css_classes(["title-2"])
            .build();
        savings_row.append(&savings_value);
        let savings_unit = {
            let widget = gtk::Label::builder()
                .label(crate::i18n::tr(
                    "tokens of tool definitions kept out of agent context",
                ))
                .halign(gtk::Align::Fill)
                .xalign(0.0)
                .valign(gtk::Align::End)
                .wrap(true)
                .css_classes(["toolport-muted"])
                .build();
            l10n::bind_label(
                &widget,
                "tokens of tool definitions kept out of agent context",
            );
            widget
        };
        savings_row.append(&savings_unit);
        savings_banner.append(&savings_row);
        let savings_detail = gtk::Label::builder()
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-muted", "caption"])
            .build();
        let savings_summary = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-muted", "caption"])
            .build();
        savings_banner.append(&savings_summary);
        let savings_old_estimate = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-muted", "caption"])
            .build();
        savings_banner.append(&savings_old_estimate);
        let savings_counting = {
            let widget = gtk::Expander::builder()
                .expanded(false)
                .child(&savings_detail)
                .build();
            widget.set_label_widget(Some(&l10n::label("How this is measured")));
            widget
        };
        savings_banner.append(&savings_counting);
        performance.append(&savings_banner);

        performance.append(&{
            let widget = gtk::Label::builder()
                .label(crate::i18n::tr("Per-server performance"))
                .halign(gtk::Align::Start)
                .css_classes(["heading"])
                .build();
            l10n::bind_label(&widget, "Per-server performance");
            widget
        });
        let stats_list = gtk::Box::new(gtk::Orientation::Vertical, 10);
        performance.append(&stats_list);
        page.append(&{
            let widget = gtk::Label::builder()
                .label(crate::i18n::tr("Recent calls and approvals"))
                .halign(gtk::Align::Start)
                .css_classes(["heading"])
                .build();
            l10n::bind_label(&widget, "Recent calls and approvals");
            widget
        });
        let filter_row = wrapping_actions();
        let filter_server = gtk::DropDown::from_strings(&[&crate::i18n::tr("All servers")]);
        filter_server.add_css_class("toolport-input");
        filter_server.add_css_class("toolport-activity-filter");
        filter_server.set_valign(gtk::Align::Center);
        l10n::bind_tooltip(&filter_server, "Show events from one server");
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            let item = item
                .downcast_ref::<gtk::ListItem>()
                .expect("a dropdown list item");
            item.set_child(Some(
                &gtk::Label::builder()
                    .xalign(0.0)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .max_width_chars(24)
                    .build(),
            ));
        });
        factory.connect_bind(|_, item| {
            let item = item
                .downcast_ref::<gtk::ListItem>()
                .expect("a dropdown list item");
            if let (Some(value), Some(label)) = (
                item.item().and_downcast::<gtk::StringObject>(),
                item.child().and_downcast::<gtk::Label>(),
            ) {
                label.set_label(&value.string());
                label.set_tooltip_text(Some(&value.string()));
            }
        });
        filter_server.set_factory(Some(&factory));
        filter_row.insert(&filter_server, -1);
        let errors_only = gtk::ToggleButton::new();
        l10n::bind_button(errors_only.upcast_ref(), "Errors only");
        errors_only.add_css_class("toolport-secondary-action");
        errors_only.add_css_class("toolport-activity-filter");
        errors_only.set_valign(gtk::Align::Center);
        filter_row.insert(&errors_only, -1);
        let filter_count = gtk::Label::builder()
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-muted", "caption"])
            .build();
        page.append(&filter_row);
        page.append(&filter_count);
        let list = gtk::Box::new(gtk::Orientation::Vertical, 10);
        page.append(&list);
        let show_more_calls = l10n::button("Show more events");
        show_more_calls.add_css_class("toolport-secondary-action");
        show_more_calls.set_halign(gtk::Align::Center);
        show_more_calls.set_visible(false);
        page.append(&show_more_calls);

        let performance_expander = {
            let widget = gtk::Expander::new(None);
            widget.set_label_widget(Some(&l10n::label("Performance and savings")));
            widget
        };
        performance_expander.add_css_class("toolport-details-expander");
        performance_expander.set_child(Some(&performance));
        page.append(&performance_expander);

        let diagnostics = gtk::Box::new(gtk::Orientation::Vertical, 10);
        diagnostics.set_margin_top(10);
        diagnostics.append(&{
            let widget = gtk::Label::builder()
                .label(crate::i18n::tr(
                    "Review protection history or inspect the gateway's technical records.",
                ))
                .halign(gtk::Align::Fill)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["toolport-muted"])
                .build();
            l10n::bind_label(
                &widget,
                "Review protection history or inspect the gateway's technical records.",
            );
            widget
        });
        let security = gtk::Box::new(gtk::Orientation::Vertical, 8);
        security.set_margin_top(8);
        security.append(&{
            let widget = gtk::Label::builder()
                .label(crate::i18n::tr(
                    "Review changes to your servers and their tools.",
                ))
                .halign(gtk::Align::Fill)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["toolport-muted", "caption"])
                .build();
            l10n::bind_label(&widget, "Review changes to your servers and their tools.");
            widget
        });
        let security_list = gtk::Box::new(gtk::Orientation::Vertical, 10);
        security.append(&security_list);
        let security_expander = {
            let widget = gtk::Expander::new(None);
            widget.set_label_widget(Some(&l10n::label("Protection history")));
            widget
        };
        security_expander.add_css_class("toolport-card");
        security_expander.set_child(Some(&security));
        diagnostics.append(&security_expander);

        let technical = gtk::Box::new(gtk::Orientation::Vertical, 12);
        technical.set_margin_top(8);
        technical.append(&{
            let widget = gtk::Label::builder()
                .label(crate::i18n::tr("Tool identities"))
                .halign(gtk::Align::Start)
                .css_classes(["heading"])
                .build();
            l10n::bind_label(&widget, "Tool identities");
            widget
        });
        let identity_search = {
            let widget = gtk::SearchEntry::builder()
                .placeholder_text(crate::i18n::tr("Filter tools or servers"))
                .css_classes(["toolport-search"])
                .build();
            l10n::bind_placeholder(&widget, "Filter tools or servers");
            widget
        };
        technical.append(&identity_search);
        let identity_list = gtk::Box::new(gtk::Orientation::Vertical, 10);
        technical.append(&identity_list);
        technical.append(&{
            let widget = gtk::Label::builder()
                .label(crate::i18n::tr("Discovery traces"))
                .halign(gtk::Align::Start)
                .css_classes(["heading"])
                .build();
            l10n::bind_label(&widget, "Discovery traces");
            widget
        });
        let search_list = gtk::Box::new(gtk::Orientation::Vertical, 10);
        technical.append(&search_list);
        technical.append(&{
            let widget = gtk::Label::builder()
                .label(crate::i18n::tr("Live inspector"))
                .halign(gtk::Align::Start)
                .css_classes(["heading"])
                .build();
            l10n::bind_label(&widget, "Live inspector");
            widget
        });
        let inspect_list = gtk::Box::new(gtk::Orientation::Vertical, 10);
        technical.append(&inspect_list);
        let technical_expander = {
            let widget = gtk::Expander::new(None);
            widget.set_label_widget(Some(&l10n::label("Technical records")));
            widget
        };
        technical_expander.add_css_class("toolport-card");
        technical_expander.set_child(Some(&technical));
        diagnostics.append(&technical_expander);
        let diagnostics_expander = {
            let widget = gtk::Expander::new(None);
            widget.set_label_widget(Some(&l10n::label("Security and diagnostics")));
            widget
        };
        diagnostics_expander.add_css_class("toolport-details-expander");
        diagnostics_expander.set_child(Some(&diagnostics));
        page.append(&diagnostics_expander);
        scroller.set_child(Some(&page));
        root.append(&scroller);

        let activity_page = Self {
            app: app.clone(),
            root,
            scroller,
            list,
            security_status,
            security_status_icon,
            security_status_title,
            security_status_detail,
            stats_list,
            security_list,
            security_expander,
            diagnostics_expander,
            identity_list,
            search_list,
            inspect_list,
            feedback,
            telemetry_feedback,
            call_count: values.remove(0),
            success_rate: values.remove(0),
            average_latency: values.remove(0),
            tokens_saved: values.remove(0),
            refresh_button,
            clear_button,
            loading: std::rc::Rc::new(std::cell::Cell::new(false)),
            dirty: std::rc::Rc::new(std::cell::Cell::new(true)),
            revision: Default::default(),
            revision_error: Default::default(),
            revision_loading: Default::default(),
            health_loading: Default::default(),
            health_generation: Default::default(),
            recent_rows: Default::default(),
            stat_rows: Default::default(),
            last_snapshot: std::rc::Rc::new(std::cell::RefCell::new(None)),
            filter_server,
            filter_server_options: std::rc::Rc::new(std::cell::RefCell::new(vec![
                crate::i18n::tr("All servers").to_string(),
            ])),
            errors_only,
            filter_count,
            show_more_calls,
            show_all_recent: std::rc::Rc::new(std::cell::Cell::new(false)),
            identity_search,
            updating_filters: std::rc::Rc::new(std::cell::Cell::new(false)),
            savings_banner,
            savings_title,
            savings_value,
            savings_unit,
            savings_detail,
            savings_summary,
            savings_old_estimate,
            savings_share: savings_share.clone(),
            expanded_stat_servers: std::rc::Rc::new(std::cell::RefCell::new(
                std::collections::HashSet::new(),
            )),
            server_stat_order: std::rc::Rc::new(std::cell::RefCell::new(Vec::new())),
            expanded_activity_rows: std::rc::Rc::new(std::cell::RefCell::new(
                std::collections::HashSet::new(),
            )),
            security_dismissed: std::rc::Rc::new(
                std::cell::RefCell::new(load_security_dismissed()),
            ),
        };
        let page_for_share = activity_page.clone();
        savings_share.connect_clicked(move |_| {
            let (tokens, loads, searches, bytes) = page_for_share
                .last_snapshot
                .borrow()
                .as_ref()
                .map(|snapshot| {
                    (
                        snapshot.tokens_saved,
                        snapshot.savings_tokenized_loads,
                        snapshot.savings_discovery_count,
                        snapshot.savings_discovery_bytes,
                    )
                })
                .unwrap_or((0, 0, 0, 0));
            if let Some(display) = gtk::gdk::Display::default() {
                display
                    .clipboard()
                    .set_text(&savings_share_line(tokens, loads, searches, bytes));
                page_for_share
                    .feedback
                    .set_label(&crate::i18n::tr("Savings copied, paste them anywhere."));
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
        let page_for_more = activity_page.clone();
        activity_page.show_more_calls.connect_clicked(move |_| {
            page_for_more
                .show_all_recent
                .set(!page_for_more.show_all_recent.get());
            page_for_more.render_recent();
        });
        let page_for_identity_search = activity_page.clone();
        activity_page
            .identity_search
            .connect_search_changed(move |_| page_for_identity_search.render_identities());
        let page_for_security = activity_page.clone();
        activity_page
            .security_status
            .connect_clicked(move |_| page_for_security.reveal_security());
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
            .title(crate::i18n::tr("Export activity"))
            .modal(true)
            .accept_label(crate::i18n::tr("Export"))
            .initial_name(format!("toolport-activity.{format}"))
            .build();
        set_file_dialog_folder(&dialog);
        let page = self.clone();
        dialog.save(Some(&parent), gtk::gio::Cancellable::NONE, move |result| {
            let Ok(file) = result else {
                return;
            };
            let Some(path) = file.path() else {
                page.show_error(&crate::i18n::tr(
                    "the selected destination does not have a local path",
                ));
                return;
            };
            page.feedback
                .set_label(&crate::i18n::tr("Exporting retained activity…"));
            let page = page.clone();
            gtk::glib::spawn_future_local(async move {
                let result = gtk::gio::spawn_blocking(move || {
                    crate::diagnostics_controller::export_audit(&path, format)
                })
                .await;
                match result {
                    Ok(Ok(())) => {
                        l10n::bind_formatted_label(
                            &page.feedback,
                            "Exported retained activity as {value0}.",
                            &[("value0", &format.to_uppercase().to_string())],
                        );
                        page.feedback.remove_css_class("error");
                        page.feedback.add_css_class("success");
                    }
                    Ok(Err(error)) => page.show_error(&error),
                    Err(_) => page.show_error(&crate::i18n::tr("the export stopped unexpectedly")),
                }
            });
        });
    }

    fn refresh(&self) {
        self.refresh_with_feedback(true)
    }

    fn attach(&self, window: &adw::ApplicationWindow) {
        let page = self.clone();
        io::watch_data(
            window,
            15,
            false,
            crate::observability_controller::activity_file,
            move || {
                if !page.root.is_mapped() {
                    page.dirty.set(true);
                    return;
                }
                page.check_revision();
            },
        );
        let page = self.clone();
        io::while_mapped(&self.root, 15, move || {
            // Also audit the content revision when the GIO monitor is active:
            // remote/symlink writes and missed events still need the fallback.
            page.check_revision();
            page.refresh_health();
        });
        let page = self.clone();
        self.root.connect_map(move |_| page.refresh_quietly());
        let page = self.clone();
        self.root.connect_unmap(move |_| page.release_rows());
    }

    fn release_rows(&self) {
        crate::observability_controller::release_activity_revision();
        self.revision.borrow_mut().take();
        self.revision_error.borrow_mut().take();
        self.health_generation
            .set(self.health_generation.get().wrapping_add(1));
        self.dirty.set(true);
        self.last_snapshot.borrow_mut().take();
        self.recent_rows.borrow_mut().clear();
        self.stat_rows.borrow_mut().clear();
        for list in [
            &self.list,
            &self.stats_list,
            &self.security_list,
            &self.identity_list,
            &self.search_list,
            &self.inspect_list,
        ] {
            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
        }
        self.feedback
            .set_label(&crate::i18n::tr("Loading retained activity…"));
        self.feedback.set_visible(true);
        self.security_status_title
            .set_label(&crate::i18n::tr("Checking protection…"));
        self.security_status_detail
            .set_label(&crate::i18n::tr("Loading saved review state."));
        self.security_status.remove_css_class("attention");
        self.security_status.remove_css_class("review");
        for value in [
            &self.call_count,
            &self.success_rate,
            &self.average_latency,
            &self.tokens_saved,
        ] {
            value.set_label("…");
        }
        self.savings_banner.set_visible(false);
        self.telemetry_feedback.set_label("");
        self.telemetry_feedback.set_visible(false);
    }

    fn refresh_health(&self) {
        if self.loading.get()
            || self.last_snapshot.borrow().is_none()
            || self.health_loading.replace(true)
        {
            return;
        }
        let generation = self.health_generation.get();
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(|| {
                crate::telemetry::activity_notices(&crate::telemetry::activity_health())
            })
            .await;
            page.health_loading.set(false);
            if !page.root.is_mapped() || page.health_generation.get() != generation {
                return;
            }
            let notices = result.unwrap_or_else(|_| vec![
                crate::i18n::tr("Gateway telemetry health is unavailable. Activity and savings may be incomplete."),
            ]);
            page.apply_health_notices(notices);
        });
    }

    fn apply_health_notices(&self, notices: Vec<String>) {
        let mut snapshot = self.last_snapshot.borrow_mut();
        let Some(snapshot) = snapshot.as_mut() else {
            return;
        };
        if snapshot.telemetry_notices == notices {
            return;
        }
        snapshot.telemetry_notices = notices;
        self.render_activity_notices(&snapshot.telemetry_notices);
    }

    fn check_revision(&self) {
        if self.loading.get() || self.revision_loading.replace(true) {
            return;
        }
        let generation = self.health_generation.get();
        let revision_generation = crate::observability_controller::activity_revision_generation();
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                crate::observability_controller::activity_revision_for(revision_generation)
            })
            .await;
            page.revision_loading.set(false);
            if !page.root.is_mapped() {
                return;
            }
            if generation != page.health_generation.get() {
                return;
            }
            let revision = result.unwrap_or_else(|_| {
                Err(crate::i18n::tr(
                    "Activity change check stopped unexpectedly",
                ))
            });
            page.set_revision_error(revision.as_ref().err().cloned());
            let revision = revision.ok();
            if revision.is_none() || *page.revision.borrow() != revision {
                page.dirty.set(true);
                page.refresh_quietly();
            }
        });
    }

    fn set_revision_error(&self, error: Option<String>) {
        *self.revision_error.borrow_mut() = error;
        let snapshot = self.last_snapshot.borrow();
        self.render_activity_notices(
            snapshot
                .as_ref()
                .map_or(&[], |snapshot| snapshot.telemetry_notices.as_slice()),
        );
    }

    fn render_activity_notices(&self, health: &[String]) {
        let mut notices = health.to_vec();
        if let Some(error) = self.revision_error.borrow().as_ref() {
            notices.push(crate::i18n::trf(
                "Activity change checks are unavailable. Displayed activity may be stale. {value0}",
                &[("value0", &error.to_string())],
            ));
        }
        self.telemetry_feedback.set_label(&notices.join("\n"));
        self.telemetry_feedback.set_visible(!notices.is_empty());
    }

    fn refresh_quietly(&self) {
        if self.dirty.get() {
            self.refresh_with_feedback(false);
        }
    }

    fn refresh_with_feedback(&self, announce: bool) {
        self.health_generation
            .set(self.health_generation.get().wrapping_add(1));
        if self.loading.replace(true) {
            if announce {
                self.dirty.set(true);
            }
            return;
        }
        if announce || self.last_snapshot.borrow().is_none() {
            self.refresh_button.set_sensitive(false);
            self.feedback
                .set_label(&crate::i18n::tr("Loading retained activity…"));
            self.feedback.remove_css_class("error");
            self.feedback.set_visible(true);
        }
        let mut revision_generation =
            crate::observability_controller::activity_revision_generation();
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            loop {
                if !page.root.is_mapped() {
                    break;
                }
                page.dirty.set(false);
                let result = gtk::gio::spawn_blocking(move || {
                    let revision =
                        crate::observability_controller::activity_revision_for(revision_generation);
                    (revision, state::load_activity_snapshot())
                })
                .await;
                if !page.root.is_mapped() {
                    page.dirty.set(true);
                    break;
                }
                if page.dirty.get() {
                    revision_generation =
                        crate::observability_controller::activity_revision_generation();
                    continue;
                }
                match result {
                    Ok((revision, Ok(snapshot))) => {
                        page.set_revision_error(revision.as_ref().err().cloned());
                        *page.revision.borrow_mut() = revision.ok();
                        if announce || page.last_snapshot.borrow().as_ref() != Some(&snapshot) {
                            page.render(snapshot);
                        }
                    }
                    Ok((_, Err(error))) => page.show_error(&error),
                    Err(_) => {
                        page.show_error(&crate::i18n::tr("the activity read stopped unexpectedly"))
                    }
                }
                break;
            }
            page.loading.set(false);
            page.refresh_button.set_sensitive(true);
        });
    }

    fn render(&self, snapshot: state::ActivitySnapshot) {
        let (security_changed, identities_changed, traces_changed, inspect_changed) = {
            let previous = self.last_snapshot.borrow();
            let previous = previous.as_ref();
            (
                activity_section_changed(
                    previous.map(|snapshot| snapshot.security_events.as_slice()),
                    &snapshot.security_events,
                ),
                activity_section_changed(
                    previous.map(|snapshot| snapshot.tool_identities.as_slice()),
                    &snapshot.tool_identities,
                ) || previous.and_then(|snapshot| snapshot.tool_identities_error.as_deref())
                    != snapshot.tool_identities_error.as_deref(),
                activity_section_changed(
                    previous.map(|snapshot| snapshot.search_traces.as_slice()),
                    &snapshot.search_traces,
                ) || previous.is_some_and(|previous| {
                    previous
                        .recent
                        .iter()
                        .any(|row| row.internal && row.tool == "search")
                        != snapshot
                            .recent
                            .iter()
                            .any(|row| row.internal && row.tool == "search")
                }),
                activity_section_changed(
                    previous.map(|snapshot| snapshot.inspect_calls.as_slice()),
                    &snapshot.inspect_calls,
                ),
            )
        };
        if traces_changed {
            while let Some(child) = self.search_list.first_child() {
                self.search_list.remove(&child);
            }
        }
        if inspect_changed {
            while let Some(child) = self.inspect_list.first_child() {
                self.inspect_list.remove(&child);
            }
        }
        self.call_count.set_label(&snapshot.call_count.to_string());
        // Do not evaluate the numerator when there are no calls.
        #[allow(clippy::manual_checked_ops)]
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
                .map(format_duration)
                .unwrap_or_else(|| "–".to_string()),
        );
        self.tokens_saved
            .set_label(&if snapshot.savings_tokenized_loads > 0 {
                format_saved_tokens(snapshot.tokens_saved)
            } else {
                crate::i18n::tr("Not measured").to_string()
            });
        self.tokens_saved.set_tooltip_text(Some(
            &crate::i18n::tr("One full catalog baseline per session minus all catalog and search responses, counted with cl100k_base. Unverifiable history excluded; not a billing figure."),
        ));
        self.feedback.set_label("");
        self.feedback.remove_css_class("error");
        self.feedback.remove_css_class("success");
        self.feedback.set_visible(false);
        self.render_activity_notices(&snapshot.telemetry_notices);
        self.clear_button.set_sensitive(
            snapshot.call_count > 0
                || snapshot.savings_list_loads > 0
                || snapshot.savings_historical_loads > 0
                || snapshot.savings_discovery_count > 0
                || !snapshot.search_traces.is_empty()
                || !snapshot.inspect_calls.is_empty(),
        );

        // Rebuild the server filter's choices, keeping the current selection when
        // that server still exists in the data.
        let server_options = activity_server_filter_options(&snapshot.recent);
        if *self.filter_server_options.borrow() != server_options {
            self.updating_filters.set(true);
            let previous = self.selected_filter_server();
            let option_refs = server_options
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>();
            self.filter_server
                .set_model(Some(&gtk::StringList::new(&option_refs)));
            let selected = previous
                .as_deref()
                .and_then(|previous| server_options.iter().position(|server| server == previous))
                .map(|index| index as u32)
                .unwrap_or(0);
            self.filter_server.set_selected(selected);
            *self.filter_server_options.borrow_mut() = server_options;
            self.updating_filters.set(false);
        }

        *self.last_snapshot.borrow_mut() = Some(snapshot);
        self.render_recent();
        if identities_changed {
            self.render_identities();
        }
        self.render_savings();
        if security_changed {
            self.render_security();
        }
        let snapshot = self
            .last_snapshot
            .borrow()
            .clone()
            .expect("the snapshot was stored above");

        if snapshot.server_stats.is_empty() {
            self.stat_rows.borrow_mut().clear();
            while let Some(child) = self.stats_list.first_child() {
                self.stats_list.remove(&child);
            }
            self.expanded_stat_servers.borrow_mut().clear();
            self.server_stat_order.borrow_mut().clear();
            self.stats_list
                .append(&empty_activity_source_label(crate::i18n::msg(
                    "Per-server statistics appear once calls are retained.",
                )));
        } else {
            let mut stats = snapshot.server_stats.clone();
            let current_servers = stats.iter().map(stat_server_name).collect::<Vec<_>>();
            let positions = {
                let mut order = self.server_stat_order.borrow_mut();
                order.retain(|server| current_servers.contains(server));
                for server in current_servers {
                    if !order.contains(&server) {
                        order.push(server);
                    }
                }
                order
                    .iter()
                    .enumerate()
                    .map(|(index, server)| (server.clone(), index))
                    .collect::<std::collections::HashMap<_, _>>()
            };
            stats.sort_by_key(|stat| {
                positions
                    .get(&stat_server_name(stat))
                    .copied()
                    .unwrap_or(usize::MAX)
            });
            reconcile_rows(&self.stats_list, &self.stat_rows, &stats, |stat| {
                server_stat_row(stat, self.expanded_stat_servers.clone()).upcast()
            });
        }
        if traces_changed {
            if snapshot.search_traces.is_empty() {
                self.search_list.append(&empty_activity_source_label(if snapshot.recent.iter().any(|row| row.internal && row.tool == "search") { crate::i18n::msg("Recent activity includes tool searches; detailed search records are not available.") } else { crate::i18n::msg("Nothing searched yet.") }));
            } else {
                for trace in &snapshot.search_traces {
                    self.search_list.append(&search_trace_card(
                        trace,
                        self.expanded_activity_rows.clone(),
                    ));
                }
            }
        }
        if inspect_changed {
            if snapshot.inspect_calls.is_empty() {
                self.inspect_list
                    .append(&empty_activity_source_label(crate::i18n::msg(
                        "Live inspection is off or has not captured a call.",
                    )));
            } else {
                for capture in &snapshot.inspect_calls {
                    self.inspect_list
                        .append(&inspect_card(capture, self.expanded_activity_rows.clone()));
                }
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
            self.set_security_status(&[]);
            self.security_expander
                .set_label(Some(&crate::i18n::tr("Protection history · Clear")));
            self.security_list
                .append(&empty_activity_source_label(crate::i18n::msg(
                    "No tool changes or suspicious content to review.",
                )));
            return;
        }
        let live: Vec<serde_json::Value> = {
            let dismissed = self.security_dismissed.borrow();
            crate::integrity::dedupe_security(&snapshot.security_events)
                .into_iter()
                .filter(|event| !security_event_is_dismissed(event, &dismissed))
                .collect()
        };
        if live.is_empty() {
            self.set_security_status(&[]);
            self.security_expander
                .set_label(Some(&crate::i18n::tr("Protection history · Reviewed")));
            // The calm "you're protected" state: a protection the user never
            // sees builds no trust.
            self.security_list.append(&empty_activity_source_label(crate::i18n::msg("Protection active. Toolport is watching tool definitions and results. Nothing needs your attention right now.")));
            return;
        }
        let (older, live): (Vec<_>, Vec<_>) = live
            .into_iter()
            .partition(|event| event["historical"] == true);
        self.set_security_status(&live);
        let groups = crate::integrity::group_tool_changes(&live);
        self.security_expander
            .set_label(Some(&if groups.is_empty() {
                crate::i18n::tr("Protection history · Security notices").to_string()
            } else {
                crate::i18n::trf(
                    if groups.len() == 1 {
                        crate::i18n::msg("Protection history · {value0} server update")
                    } else {
                        crate::i18n::msg("Protection history · {value0} server updates")
                    },
                    &[("value0", &format!("{}", groups.len()))],
                )
            }));
        for group in groups {
            self.security_list
                .append(&tool_change_group_card(&group, self.clone()));
        }
        if !older.is_empty() {
            let history = {
                let widget = gtk::Expander::builder().build();
                widget.set_label_widget(Some(&l10n::formatted_label(
                    "{value0} older tool-change records. Open history to review them.",
                    &[("value0", &format!("{}", older.len()))],
                )));
                widget
            };
            let rows = gtk::Box::new(gtk::Orientation::Vertical, 8);
            let page = self.clone();
            let history_rows = rows.clone();
            history.connect_expanded_notify(move |history| {
                if history.is_expanded() && history_rows.first_child().is_none() {
                    for group in crate::integrity::group_tool_changes(&older) {
                        history_rows.append(&tool_change_group_card(&group, page.clone()));
                    }
                }
            });
            history.set_child(Some(&rows));
            self.security_list.append(&history);
        }
        for (event, count) in crate::integrity::collapse_security_by_identity(
            &live
                .into_iter()
                .filter(|event| event["type"] != "tool_drift")
                .collect::<Vec<_>>(),
        ) {
            self.security_list
                .append(&security_notice_card(&event, count, self.clone()));
        }
    }

    fn accept_security(&self, events: Vec<serde_json::Value>) {
        if events.iter().any(|event| event["review_required"] == true) {
            if let Some(action) = self.app.lookup_action("review-quarantine") {
                action.activate(None);
            }
            return;
        }
        let keys = events.iter().map(security_dismissal_key).collect();
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || -> Result<(), String> {
                for event in events {
                    let tool = event["tool"].as_str().unwrap_or("");
                    if let Some(profiles) = event["blocked_profiles"].as_array() {
                        for profile in profiles.iter().filter_map(serde_json::Value::as_str) {
                            crate::integrity::release_definition(
                                (!profile.is_empty()).then_some(profile),
                                tool,
                                event["new_fp"].as_str().ok_or("This older finding has no saved definition. Review the active block in Settings.")?,
                            )?;
                        }
                    }
                }
                Ok(())
            })
            .await;
            match result {
                Ok(Ok(())) => {
                    page.dismiss_security(keys);
                    page.refresh();
                }
                Ok(Err(error)) => page.show_error(&crate::i18n::trf(
                    "could not accept changes: {value0}",
                    &[("value0", &error.to_string())],
                )),
                Err(_) => {
                    page.show_error(&crate::i18n::tr("accepting changes stopped unexpectedly"))
                }
            }
        });
    }

    fn set_security_status(&self, important: &[serde_json::Value]) {
        self.security_status.remove_css_class("attention");
        self.security_status.remove_css_class("review");
        self.security_status_icon.remove_css_class("review");
        self.security_status_icon.remove_css_class("attention");
        if important.is_empty() {
            self.security_status_title
                .set_label(&crate::i18n::tr("Protection active"));
            self.security_status_detail
                .set_label(&crate::i18n::tr("No changes need review."));
            self.security_status.set_tooltip_text(Some(&crate::i18n::tr(
                "Open security and diagnostic history",
            )));
            return;
        }
        if important
            .iter()
            .any(|event| event["blocked"] == true || event["type"] == "result_injection_blocked")
        {
            self.security_status.add_css_class("attention");
            self.security_status_icon.add_css_class("attention");
        } else {
            self.security_status.add_css_class("review");
            self.security_status_icon.add_css_class("review");
        }
        let groups = crate::integrity::group_tool_changes(important);
        let incidents = security_attention_incidents(important);
        let collapsed = crate::integrity::collapse_security_by_identity(&incidents);
        let count = groups.len()
            + collapsed
                .iter()
                .filter(|(event, _)| event["type"] != "tool_drift")
                .count();
        l10n::bind_formatted_label(
            &self.security_status_title,
            if count == 1 {
                crate::i18n::msg("{value0} update to review")
            } else {
                crate::i18n::msg("{value0} updates to review")
            },
            &[("value0", &format!("{}", count))],
        );
        self.security_status_detail
            .set_label(&crate::i18n::lookup(security_status_detail(important)));
        self.security_status.set_tooltip_text(Some(&crate::i18n::tr(
            "Review changes in Protection history",
        )));
    }

    fn reveal_security(&self) {
        self.diagnostics_expander.set_expanded(true);
        self.security_expander.set_expanded(true);
        let target = self.security_expander.clone();
        let scroller = self.scroller.clone();
        gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(180), move || {
            target.grab_focus();
            let Some(content) = scroller.child() else {
                return;
            };
            let Some(bounds) = target.compute_bounds(&content) else {
                return;
            };
            let adjustment = scroller.vadjustment();
            adjustment.set_value(aligned_scroll_value(
                adjustment.value(),
                f64::from(bounds.y()),
                adjustment.lower(),
                adjustment.upper(),
                adjustment.page_size(),
            ));
        });
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
            const MAX_DISMISSED: usize = 2000;
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
        let has_catalog = snapshot.savings_tokenized_loads > 0;
        let discovered = snapshot.savings_discovery_count > 0;
        self.savings_banner.set_visible(
            has_catalog
                || discovered
                || snapshot.savings_historical_loads > 0
                || snapshot.savings_previous_tokens != 0,
        );
        l10n::bind_text(
            &self.savings_title,
            if has_catalog {
                crate::i18n::msg("Net tool description savings")
            } else if discovered {
                crate::i18n::msg("Discovery payload returned")
            } else {
                crate::i18n::msg("Savings history")
            },
        );
        l10n::bind_text(
            &self.savings_summary,
            if has_catalog {
                crate::i18n::msg("Full catalog baselines minus all catalog and search responses. Counted locally; not a billing figure.")
            } else {
                crate::i18n::msg("Older records remain available below. Comparable session measurements start with this version.")
            },
        );
        let (primary, unit) = if has_catalog || discovered {
            savings_primary_display(
                snapshot.tokens_saved,
                snapshot.savings_tokenized_loads,
                snapshot.savings_discovery_bytes,
            )
        } else {
            (crate::i18n::tr("Not measured yet").to_string(), "")
        };
        self.savings_share.set_visible(has_catalog);
        self.savings_value.set_label(&primary);
        l10n::bind_label(&self.savings_unit, unit);
        let old = savings_old_estimate_display(snapshot.savings_previous_tokens);
        self.savings_old_estimate.set_visible(old.is_some());
        self.savings_old_estimate
            .set_label(old.as_deref().unwrap_or(""));
        let mut detail = crate::i18n::tr("The baseline is the full-mode tools array for the same allowed servers and policies at the first tools/list in each session. We count compact JSON and search response text with cl100k_base, then subtract every tools/list and search response. Sessionless HTTP, initial catalogs above known client limits, and older records are excluded. Client caching and formatting may change model usage; this is not a billing figure.");
        if has_catalog || discovered {
            detail.push_str(&crate::i18n::trf("\n{value0} baseline tokens − {value1} catalog response tokens − {value2} search response tokens = {value3} net tokens saved.\n{value4} client sessions; {value5} catalog responses; {value6} search responses.\n{value7} baseline catalog text; {value8} catalog text sent; {value9} search text sent.", &[("value0", &grouped_number(snapshot.savings_full_tokens).to_string()), ("value1", &grouped_number(snapshot.savings_exposed_tokens).to_string()), ("value2", &grouped_number(snapshot.savings_discovery_tokens).to_string()), ("value3", &format!("{}", snapshot.tokens_saved)), ("value4", &format!("{}", snapshot.savings_tokenized_loads)), ("value5", &format!("{}", snapshot.savings_list_loads)), ("value6", &format!("{}", snapshot.savings_discovery_count)), ("value7", &format_byte_count(snapshot.savings_full_bytes).to_string()), ("value8", &format_byte_count(snapshot.savings_exposed_bytes).to_string()), ("value9", &format_byte_count(snapshot.savings_discovery_bytes).to_string())]));
        }
        if snapshot.savings_historical_loads > 0 {
            detail.push_str(&crate::i18n::trf("\nExcluded history: {value0} catalog records ({value1} with byte measurements). Session identity and repeat loads cannot be verified.", &[("value0", &grouped_number(snapshot.savings_historical_loads).to_string()), ("value1", &grouped_number(snapshot.savings_historical_measured).to_string())]));
        }
        if snapshot.savings_legacy_tokens > 0 || snapshot.savings_old_v2_tokens > 0 {
            detail.push_str(&crate::i18n::trf("\nOlder estimated records: {value0} legacy tokens and {value1} size-based tokens, excluded.", &[("value0", &grouped_number(snapshot.savings_legacy_tokens).to_string()), ("value1", &grouped_number(snapshot.savings_old_v2_tokens).to_string())]));
        }
        self.savings_detail.set_label(&detail);
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
        if snapshot.recent.is_empty() {
            self.clear_recent_rows();
            self.filter_count.set_label("");
            self.show_more_calls.set_visible(false);
            self.list.append(&state_card(
                "view-list-symbolic",
                crate::i18n::msg("No activity yet"),
                crate::i18n::msg("Calls and approval outcomes will appear here with their timing."),
                false,
            ));
            return;
        }
        let server = self.selected_filter_server();
        let errors_only = self.errors_only.is_active();
        let filtered = filter_calls(&snapshot.recent, server.as_deref(), errors_only);
        if filtered.is_empty() {
            self.clear_recent_rows();
            self.filter_count
                .set_label(&crate::i18n::tr("0 matching rows"));
            self.show_more_calls.set_visible(false);
            self.list.append(&state_card(
                "edit-find-symbolic",
                crate::i18n::msg("No matching activity"),
                crate::i18n::msg("Nothing saved matches the current filter."),
                false,
            ));
            return;
        }
        let match_count = filtered.len();
        let visible_count = if self.show_all_recent.get() {
            match_count
        } else {
            match_count.min(RECENT_CALL_PREVIEW_LIMIT)
        };
        self.filter_count.set_label(&activity_count_summary(
            snapshot.call_count,
            visible_count,
            snapshot.recent.len(),
            match_count != snapshot.recent.len(),
        ));
        self.show_more_calls
            .set_visible(match_count > RECENT_CALL_PREVIEW_LIMIT);
        let show_more_label = if self.show_all_recent.get() {
            crate::i18n::tr("Show fewer").to_string()
        } else {
            crate::i18n::trf(
                "Show all {value0} rows",
                &[("value0", &format!("{}", match_count))],
            )
        };
        self.show_more_calls.set_label(&show_more_label);
        let visible = filtered
            .into_iter()
            .take(visible_count)
            .cloned()
            .collect::<Vec<_>>();
        reconcile_rows(&self.list, &self.recent_rows, &visible, |row| {
            activity_card(row).upcast()
        });
    }

    fn clear_recent_rows(&self) {
        self.recent_rows.borrow_mut().clear();
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
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
        if let Some(error) = &snapshot.tool_identities_error {
            let message = empty_activity_label(&crate::i18n::trf(
                "Tool identities are unavailable: {value0}",
                &[("value0", &error.to_string())],
            ));
            message.remove_css_class("toolport-muted");
            message.add_css_class("error");
            self.identity_list.append(&message);
            return;
        }
        if snapshot.tool_identities.is_empty() {
            self.identity_list.append(&empty_activity_source_label(crate::i18n::msg("No tool baselines pinned yet. Identities appear after a client lists tools through the gateway.")));
            return;
        }
        let query = self.identity_search.text().to_lowercase();
        let filtered = filter_tool_identities(&snapshot.tool_identities, &query);
        if filtered.is_empty() {
            self.identity_list
                .append(&empty_activity_source_label(crate::i18n::msg(
                    "No pinned tool matches the filter.",
                )));
            return;
        }
        let groups = state::group_tool_identities(&filtered);
        if !query.is_empty() {
            self.identity_list
                .append(&empty_activity_label(&crate::i18n::trf(
                    if filtered.len() == 1 {
                        if groups.len() == 1 {
                            crate::i18n::msg("{value0} tool in {value2} server")
                        } else {
                            crate::i18n::msg("{value0} tool in {value2} servers")
                        }
                    } else {
                        if groups.len() == 1 {
                            crate::i18n::msg("{value0} tools in {value2} server")
                        } else {
                            crate::i18n::msg("{value0} tools in {value2} servers")
                        }
                    },
                    &[
                        ("value0", &format!("{}", filtered.len())),
                        ("value2", &format!("{}", groups.len())),
                    ],
                )));
        }
        for (server, identities) in groups {
            // A search forces matches open so they are visible without a click.
            self.identity_list.append(&tool_identity_group(
                &server,
                identities,
                !query.is_empty(),
                self.expanded_activity_rows.clone(),
            ));
        }
    }

    fn show_error(&self, error: &str) {
        // A failed refresh over real rows must never read as "all clear": keep
        // the last loaded data on screen and say it is stale.
        if self.last_snapshot.borrow().is_some() {
            l10n::bind_formatted_label(&self.feedback, "Could not refresh; showing the last loaded activity. Retry with the refresh button. ({value0})", &[("value0", error)]);
        } else {
            l10n::bind_formatted_label(
                &self.feedback,
                "Could not load activity: {value0}",
                &[("value0", error)],
            );
            self.security_status_title
                .set_label(&crate::i18n::tr("Protection state unavailable"));
            self.security_status_detail
                .set_label(&crate::i18n::tr("Retry Activity to check saved reviews."));
        }
        self.feedback.remove_css_class("success");
        self.feedback.add_css_class("error");
        self.feedback.set_visible(true);
    }

    fn confirm_clear(&self) {
        let Some(parent) = self.app.active_window() else {
            return;
        };
        #[allow(deprecated)]
        let dialog = adw::MessageDialog::new(Some(&parent), Some(&crate::i18n::tr("Clear retained activity?")), Some(&crate::i18n::tr("This permanently removes the local call audit, discovery traces, live inspector captures, and savings tally. Security drift history remains available for review.")));
        l10n::bind_dialog_heading(&dialog, "Clear retained activity?");
        l10n::bind_dialog_body(&dialog, "This permanently removes the local call audit, discovery traces, live inspector captures, and savings tally. Security drift history remains available for review.");
        dialog.add_response("cancel", &crate::i18n::tr("Cancel"));
        l10n::bind_dialog_response(&dialog, "cancel", "Cancel");
        dialog.add_response("clear", &crate::i18n::tr("Clear activity"));
        l10n::bind_dialog_response(&dialog, "clear", "Clear activity");
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
        self.feedback
            .set_label(&crate::i18n::tr("Clearing retained activity…"));
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let result =
                gtk::gio::spawn_blocking(crate::observability_controller::clear_activity_logs)
                    .await;
            match result {
                Ok(Ok(())) => page.refresh(),
                Ok(Err(error)) => page.show_error(&crate::i18n::trf(
                    "could not clear activity: {value0}",
                    &[("value0", &error.to_string())],
                )),
                Err(_) => {
                    page.show_error(&crate::i18n::tr("the clear operation stopped unexpectedly"))
                }
            }
        });
    }
}

fn empty_activity_source_label(source: &'static str) -> gtk::Label {
    let label = empty_activity_label(source);
    l10n::bind_label(&label, source);
    label
}

fn empty_activity_label(message: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(message)
        .halign(gtk::Align::Fill)
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

fn aligned_scroll_value(current: f64, target_y: f64, lower: f64, upper: f64, page: f64) -> f64 {
    let desired = current + target_y - 12.0;
    let maximum = (upper - page).max(lower);
    desired.clamp(lower, maximum)
}

fn security_event_kind(event: &serde_json::Value) -> &'static str {
    match event.get("type").and_then(serde_json::Value::as_str) {
        Some("result_injection_blocked") => {
            crate::i18n::msg("Toolport blocked an injected tool result")
        }
        Some("result_injection") => {
            crate::i18n::msg("Toolport detected instructions inside a tool result")
        }
        Some("tool_poison_flag") => {
            crate::i18n::msg("Toolport found suspicious content in a tool definition")
        }
        Some("pins_load_failed") => {
            crate::i18n::msg("The tool integrity baseline could not be verified")
        }
        Some("tool_drift") => crate::i18n::msg("A previously known tool definition changed"),
        Some("sync_change_refused") => crate::i18n::msg("Toolport blocked a synced change"),
        _ => crate::i18n::msg("A security finding was recorded"),
    }
}

fn security_dismissal_key(event: &serde_json::Value) -> String {
    let identity = crate::integrity::security_key(event);
    if event["type"] != "tool_drift" && crate::integrity::event_severity(event) != "high" {
        return identity;
    }
    let timestamp = event
        .get("ts")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    format!("{identity}@{timestamp}")
}

fn security_event_is_dismissed(event: &serde_json::Value, dismissed: &[String]) -> bool {
    if event["type"] == "tool_drift" && event["profiles"].is_array() && event["blocked"] != false {
        return false;
    }
    let identity = crate::integrity::security_key(event);
    // Preserve v1 review markers only for old records known to be unblocked.
    if event["type"] == "tool_drift"
        && event["new_fp"].as_str().is_none()
        && event["blocked"] == false
        && dismissed.contains(&identity)
    {
        return true;
    }
    if event["type"] != "tool_drift" && crate::integrity::event_severity(event) != "high" {
        return dismissed.contains(&identity);
    }
    let timestamp = event
        .get("ts")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let prefix = format!("{identity}@");
    dismissed.iter().any(|marker| {
        marker
            .strip_prefix(&prefix)
            .and_then(|value| value.parse::<u64>().ok())
            .is_some_and(|reviewed_through| reviewed_through >= timestamp)
    })
}

type ActivityExpansionState = std::rc::Rc<std::cell::RefCell<std::collections::HashSet<String>>>;

fn activity_section_changed<T: PartialEq>(previous: Option<&[T]>, current: &[T]) -> bool {
    previous != Some(current)
}

fn activity_row_key(namespace: &str, value: &serde_json::Value) -> String {
    let field = |key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
    };
    let timestamp = value
        .get("ts")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    format!(
        "{namespace}:{timestamp}:{}:{}:{}",
        field("server"),
        field("tool"),
        field("query")
    )
}

fn bind_activity_expander(
    expander: &gtk::Expander,
    key: String,
    expanded_rows: ActivityExpansionState,
    force_expanded: bool,
) {
    let expanded = force_expanded || expanded_rows.borrow().contains(&key);
    expander.set_expanded(expanded);
    expander.connect_expanded_notify(move |expander| {
        remember_activity_expansion(
            &mut expanded_rows.borrow_mut(),
            &key,
            expander.is_expanded(),
        );
    });
}

fn remember_activity_expansion(
    expanded_rows: &mut std::collections::HashSet<String>,
    key: &str,
    expanded: bool,
) {
    if expanded {
        expanded_rows.insert(key.to_string());
    } else {
        expanded_rows.remove(key);
    }
}

fn security_status_detail(events: &[serde_json::Value]) -> &'static str {
    let changes: Vec<_> = events
        .iter()
        .filter(|event| event["type"] == "tool_drift")
        .collect();
    if changes.is_empty() {
        crate::i18n::msg("Security findings need review. Open Protection history for details.")
    } else if changes.iter().any(|event| event["blocked"] == true) {
        crate::i18n::msg("Some tools are blocked. Review the changes or accept them.")
    } else if changes.iter().any(|event| event["blocked"].is_null()) {
        crate::i18n::msg("Blocking status unavailable. Review in Settings > Quarantined tools.")
    } else {
        crate::i18n::msg("Not blocked. Review the changes or accept them.")
    }
}

fn change_can_be_accepted(event: &serde_json::Value) -> bool {
    event["review_required"] != true
        && (event["blocked"] == false
            || (event["blocked"] == true
                && event["new_fp"].is_string()
                && event["blocked_profiles"]
                    .as_array()
                    .is_some_and(|profiles| !profiles.is_empty())))
}

fn group_can_be_accepted(group: &crate::integrity::ToolChangeGroup) -> bool {
    group
        .tools
        .iter()
        .all(|event| event.get("signatures").is_none() && change_can_be_accepted(event))
}

fn tool_change_group_card(
    group: &crate::integrity::ToolChangeGroup,
    page: ActivityPage,
) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
    card.add_css_class("toolport-card");
    let name = group
        .tools
        .first()
        .and_then(|event| event["server_name"].as_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(&group.server);
    let heading = gtk::Button::new();
    let heading_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let arrow = gtk::Image::from_icon_name("pan-end-symbolic");
    heading_row.append(&arrow);
    heading_row.append(
        &gtk::Label::builder()
            .label(format!(
                "{name}: {}, {}",
                crate::integrity::tool_change_summary(&group.tools),
                relative_activity_time(group.ts)
            ))
            .xalign(0.0)
            .hexpand(true)
            .wrap(true)
            .css_classes(["heading"])
            .build(),
    );
    heading.set_child(Some(&heading_row));
    heading.add_css_class("flat");
    heading.set_halign(gtk::Align::Fill);
    l10n::bind_tooltip(&heading, "Expand to review the changed tools");
    let tools = gtk::Box::new(gtk::Orientation::Vertical, 8);
    tools.set_margin_top(8);
    let reveal = gtk::Revealer::new();
    reveal.set_child(Some(&tools));
    let expansion_key = format!(
        "security:server:{}:{}:{}",
        group.profile, group.server, group.ts
    );
    let expanded_rows = page.expanded_activity_rows.clone();
    reveal.set_reveal_child(expanded_rows.borrow().contains(&expansion_key));
    let detail_events = group.tools.clone();
    let detail_page = page.clone();
    if reveal.reveals_child() {
        for event in &detail_events {
            tools.append(&security_notice_card(event, 1, detail_page.clone()));
        }
    }
    arrow.set_icon_name(Some(if reveal.reveals_child() {
        "pan-down-symbolic"
    } else {
        "pan-end-symbolic"
    }));
    let reveal_for_toggle = reveal.clone();
    heading.connect_clicked(move |_| {
        let expanded = !reveal_for_toggle.reveals_child();
        if expanded && tools.first_child().is_none() {
            for event in &detail_events {
                tools.append(&security_notice_card(event, 1, detail_page.clone()));
            }
        }
        reveal_for_toggle.set_reveal_child(expanded);
        arrow.set_icon_name(Some(if expanded {
            "pan-down-symbolic"
        } else {
            "pan-end-symbolic"
        }));
        remember_activity_expansion(&mut expanded_rows.borrow_mut(), &expansion_key, expanded);
    });
    card.append(&heading);
    let blocked = group
        .tools
        .iter()
        .filter(|event| event["blocked"] == true)
        .count();
    let unknown = group.tools.iter().any(|event| event["blocked"].is_null());
    let review_required = group
        .tools
        .iter()
        .any(|event| event["review_required"] == true);
    let status = if review_required {
        crate::i18n::tr("Review the captured definition in Settings before accepting it.")
            .to_string()
    } else if blocked > 0 {
        crate::i18n::trf(
            if blocked == 1 {
                crate::i18n::msg("{value0} tool is blocked. Review the changes or accept them.")
            } else {
                crate::i18n::msg("{value0} tools are blocked. Review the changes or accept them.")
            },
            &[("value0", &format!("{}", blocked))],
        )
    } else if unknown {
        crate::i18n::tr("Blocking status unavailable. Review in Settings > Quarantined tools.")
            .to_string()
    } else {
        crate::i18n::tr("Not blocked. Review the changes or accept them.").to_string()
    };
    let label = empty_activity_label(&status);
    if blocked > 0 {
        label.add_css_class("error");
    }
    card.append(&label);
    let accept = l10n::button("Accept all for this server");
    accept.set_halign(gtk::Align::Start);
    accept.set_sensitive(group_can_be_accepted(group));
    if group
        .tools
        .iter()
        .any(|event| event.get("signatures").is_some())
    {
        card.append(&empty_activity_source_label(crate::i18n::msg(
            "Suspicious content found. Review and accept each tool separately.",
        )));
    }
    let events = group.tools.clone();
    accept.connect_clicked(move |button| {
        button.set_sensitive(false);
        page.accept_security(events.clone());
    });
    card.append(&accept);
    if review_required {
        let review = l10n::button("Review in Settings");
        review.set_halign(gtk::Align::Start);
        review.set_action_name(Some("app.review-quarantine"));
        card.append(&review);
    }
    card.append(&reveal);
    card
}

/// One loud (or quiet-lane) security finding. The summary stays compact; expanding
/// it reveals the evidence Toolport retained for review.
fn security_notice_card(
    event: &serde_json::Value,
    count: usize,
    page: ActivityPage,
) -> gtk::Expander {
    let card = gtk::Expander::new(None);
    card.add_css_class("toolport-card");
    l10n::bind_tooltip(&card, "Click to review what changed");
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let kind = match event.get("type").and_then(serde_json::Value::as_str) {
        Some("sync_change_refused") => crate::i18n::tr("synced change blocked").to_string(),
        Some("tool_drift") => crate::i18n::tr("Tool changed").to_string(),
        other => other.unwrap_or("security_event").replace('_', " "),
    };
    let badge = gtk::Label::new(Some(&kind));
    badge.add_css_class("toolport-badge");
    badge.set_valign(gtk::Align::Center);
    badge.set_halign(gtk::Align::Start);
    badge.add_css_class(
        if event["blocked"] == true || event["type"] == "result_injection_blocked" {
            "error"
        } else {
            "review"
        },
    );
    row.append(&badge);
    let subject = ["tool", "server"]
        .into_iter()
        .filter_map(|field| event.get(field).and_then(serde_json::Value::as_str))
        .find(|value| !value.is_empty())
        .unwrap_or("integrity baseline");
    let subject = if event["type"] == "tool_drift" {
        subject
            .split_once("__")
            .map(|(_, tool)| tool)
            .unwrap_or(subject)
            .replace('_', " ")
    } else {
        subject.to_string()
    };
    row.append(
        &gtk::Label::builder()
            .label(subject)
            .halign(gtk::Align::Fill)
            .hexpand(true)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["heading"])
            .build(),
    );
    if count > 1 {
        let recurrence = gtk::Label::new(Some(&format!("×{count}")));
        recurrence.add_css_class("toolport-badge");
        recurrence.set_valign(gtk::Align::Center);
        recurrence.set_halign(gtk::Align::Start);
        recurrence.add_css_class("review");
        recurrence.set_tooltip_text(Some(&crate::i18n::trf(
            "Recurred in {value0} separate time windows",
            &[("value0", &format!("{}", count))],
        )));
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
    let is_change = event["type"] == "tool_drift";
    let review_required = is_change && event["review_required"] == true;
    let dismiss = gtk::Button::builder()
        .label(if review_required {
            "Review in Settings"
        } else if is_change {
            "Accept this tool"
        } else {
            "Reviewed"
        })
        .tooltip_text(crate::i18n::tr(
            "Accept this occurrence; a later change will reappear",
        ))
        .css_classes(["flat"])
        .build();
    dismiss.set_sensitive(review_required || !is_change || change_can_be_accepted(event));
    let accepted_event = event.clone();
    let key = security_dismissal_key(event);
    let expansion_key = format!("security:{}", key);
    let page_for_dismiss = page.clone();
    let expansion_key_for_dismiss = expansion_key.clone();
    dismiss.connect_clicked(move |_| {
        page_for_dismiss
            .expanded_activity_rows
            .borrow_mut()
            .remove(&expansion_key_for_dismiss);
        if review_required {
            if let Some(action) = page_for_dismiss.app.lookup_action("review-quarantine") {
                action.activate(None);
            }
        } else if is_change {
            page_for_dismiss.accept_security(vec![accepted_event.clone()]);
        } else {
            page_for_dismiss.dismiss_security(vec![key.clone()]);
        }
    });
    row.append(&dismiss);
    card.set_label_widget(Some(&row));

    let details = gtk::Box::new(gtk::Orientation::Vertical, 5);
    details.set_margin_top(10);
    for line in security_review_lines(event) {
        details.append(
            &gtk::Label::builder()
                .label(line)
                .halign(gtk::Align::Fill)
                .xalign(0.0)
                .wrap(true)
                .selectable(true)
                .css_classes(["toolport-muted", "caption"])
                .build(),
        );
    }
    card.set_child(Some(&details));
    bind_activity_expander(
        &card,
        expansion_key,
        page.expanded_activity_rows.clone(),
        false,
    );
    card
}

fn security_review_lines(event: &serde_json::Value) -> Vec<String> {
    let event_type = event
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("security_event");
    let change = event
        .get("change")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let mut lines = Vec::new();

    match (event_type, change) {
        ("tool_drift", "changed") => {
            lines.push(crate::i18n::tr("This tool changed since it was last accepted.").to_string());
            let fields: Vec<&str> = event
                .get("changed_fields")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .collect();
            for field in &fields {
                let label = match *field {
                    "description" => crate::i18n::tr("Description"),
                    "input_schema" => crate::i18n::tr("Inputs"),
                    "output_schema" => crate::i18n::tr("Output format"),
                    "annotations" => crate::i18n::tr("Safety hints"),
                    other => other.to_string(),
                };
                lines.push(crate::i18n::trf("Changed field: {value0}", &[("value0", &label.to_string())]));
            }
            let annotation_lines = annotation_review_lines(event);
            if fields.is_empty() && annotation_lines.is_empty() {
                lines.push(
                    crate::i18n::tr("This older event does not contain field-level change details.").to_string(),
                );
            }
            lines.extend(annotation_lines);
        }
        ("tool_drift", "added") => {
            lines.push(crate::i18n::tr("Tool was added to an established server definition.").to_string());
        }
        ("tool_drift", "removed") => {
            lines.push(crate::i18n::tr("Tool was removed from an established server definition.").to_string());
        }
        ("result_injection_blocked", _) => lines.push(
            crate::i18n::tr("Toolport blocked this result before it reached the requesting agent.").to_string(),
        ),
        ("result_injection", _) => {
            lines.push(crate::i18n::tr("A tool result matched prompt-injection patterns.").to_string())
        }
        ("tool_poison_flag", _) => {
            lines.push(crate::i18n::tr("The tool definition matched suspicious instruction patterns.").to_string())
        }
        ("pins_load_failed", _) => lines.push(
            event["detail"]
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| crate::i18n::tr("Toolport could not read the integrity baseline because it was unreadable or corrupt. See Settings for the current safety policy.")),
        ),
        ("sync_change_refused", _) => lines.push(
            event
                .get("detail")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| crate::i18n::tr("A synced change was blocked.")),
        ),
        _ => lines.push(crate::i18n::tr("Toolport retained this security event for review.").to_string()),
    }

    if event_type == "tool_drift" {
        if event["review_required"] == true {
            lines.push(
                crate::i18n::tr("Review the captured definition in Settings before accepting it.")
                    .to_string(),
            );
        }
        if let Some(parameters) = event.get("parameters") {
            for (key, label) in [
                ("added", crate::i18n::msg("Added parameters: {names}")),
                ("removed", crate::i18n::msg("Removed parameters: {names}")),
                ("changed", crate::i18n::msg("Changed parameters: {names}")),
            ] {
                let names: Vec<_> = parameters[key]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(serde_json::Value::as_str)
                    .collect();
                if !names.is_empty() {
                    lines.push(crate::i18n::lookup(label).replace("{names}", &names.join(", ")));
                }
            }
        } else if change == "changed" {
            lines.push(
                crate::i18n::tr("Parameter details were not saved for this older change.")
                    .to_string(),
            );
        }
        lines.push(match event["blocked"].as_bool() {
            Some(true) => crate::i18n::tr("Blocked until you accept the changes."),
            Some(false) => crate::i18n::tr("Not blocked."),
            None => crate::i18n::tr(
                "Blocking status unavailable. Review in Settings > Quarantined tools.",
            ),
        });
    }

    if let Some(signatures) = event
        .get("signatures")
        .and_then(serde_json::Value::as_array)
    {
        let signatures: Vec<String> = signatures
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(|signature| signature.replace('_', " "))
            .collect();
        if !signatures.is_empty() {
            lines.push(crate::i18n::trf(
                "Matched signals: {value0}",
                &[("value0", &signatures.join(", ").to_string())],
            ));
        }
    }
    if let Some(score) = event.get("score").and_then(serde_json::Value::as_f64) {
        lines.push(crate::i18n::trf(
            "Detection score: {value0}",
            &[("value0", &format!("{:.2}", score))],
        ));
    }
    if let Some(evidence) = event
        .get("evidence")
        .and_then(serde_json::Value::as_str)
        .filter(|evidence| !evidence.is_empty())
    {
        lines.push(crate::i18n::trf(
            "Matched text: {value0}",
            &[("value0", evidence)],
        ));
    }
    lines
}

fn annotation_review_lines(event: &serde_json::Value) -> Vec<String> {
    [
        ("readOnlyHint", "prev_ro", "new_ro"),
        ("destructiveHint", "prev_dh", "new_dh"),
    ]
    .into_iter()
    .filter_map(|(label, previous, next)| {
        let previous = event.get(previous);
        let next = event.get(next);
        (previous != next).then(|| {
            crate::i18n::trf(
                "{value0}: {value1} to {value2}",
                &[
                    ("value0", label),
                    ("value1", &security_hint_value(previous).to_string()),
                    ("value2", &security_hint_value(next).to_string()),
                ],
            )
        })
    })
    .collect()
}

fn security_hint_value(value: Option<&serde_json::Value>) -> String {
    match value {
        Some(serde_json::Value::Bool(value)) => value.to_string(),
        Some(serde_json::Value::Null) | None => crate::i18n::tr("not set").to_string(),
        Some(value) => value.to_string(),
    }
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
        crate::i18n::trf(
            "{value0} direct + {value1} fallback",
            &[
                ("value0", &format!("{}", total)),
                ("value1", &format!("{}", fallbacks)),
            ],
        )
    } else if returned > 0 {
        crate::i18n::trf(
            "{value0} of {value1}",
            &[
                ("value0", &format!("{}", returned)),
                ("value1", &format!("{}", total)),
            ],
        )
    } else {
        crate::i18n::tr("no match").to_string()
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
                        crate::i18n::tr("pinned prerequisite").to_string()
                    } else if row
                        .get("fallback")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false)
                    {
                        crate::i18n::tr("fallback candidate").to_string()
                    } else if !matched.is_empty() {
                        crate::i18n::trf(
                            "matched {value0}",
                            &[("value0", &matched.join(", ").to_string())],
                        )
                    } else if trace.get("mode").and_then(serde_json::Value::as_str)
                        == Some("semantic")
                    {
                        crate::i18n::tr("semantic match").to_string()
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
                        crate::i18n::trf("{name} (top)", &[("name", name)])
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
    if let Some(bytes) = trace
        .get("responseContentBytes")
        .and_then(serde_json::Value::as_u64)
    {
        let schemas = trace
            .get("returned")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let catalog = trace
            .get("catalogSchemaBytes")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        return crate::i18n::trf("Returned {value0} of discovery content containing {value1} matching schemas; full scoped catalog schemas: {value2}. ≈{value3} tokens (UTF-8 bytes ÷ 4).", &[("value0", &format_byte_count(bytes).to_string()), ("value1", &format!("{}", schemas)), ("value2", &format_byte_count(catalog).to_string()), ("value3", &state::format_token_count(bytes.div_ceil(4)).to_string())]);
    }
    let number = |key: &str| {
        trace
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    };
    let returned = number("returnedTokens");
    let flat = number("flatTokens");
    crate::i18n::trf("Legacy schema-only estimates: ≈{value0} returned vs ≈{value1} catalog; search guidance text was not counted.", &[("value0", &state::format_token_count(returned).to_string()), ("value1", &state::format_token_count(flat).to_string())])
}

fn search_trace_card(
    trace: &serde_json::Value,
    expanded_rows: ActivityExpansionState,
) -> gtk::Expander {
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
            list.append(&empty_activity_source_label(crate::i18n::msg(
                "No tools matched this query.",
            )));
        }
        for line in lines {
            list.append(
                &gtk::Label::builder()
                    .label(line)
                    .halign(gtk::Align::Fill)
                    .xalign(0.0)
                    .wrap(true)
                    .css_classes(["toolport-muted", "caption", "monospace"])
                    .build(),
            );
        }
        list.append(
            &gtk::Label::builder()
                .label(trace_token_line(&trace_for_expand))
                .halign(gtk::Align::Fill)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["toolport-muted", "caption"])
                .build(),
        );
        expander.set_child(Some(&list));
    });
    bind_activity_expander(
        &expander,
        activity_row_key("trace", trace),
        expanded_rows,
        false,
    );
    expander
}

fn inspect_card(capture: &serde_json::Value, expanded_rows: ActivityExpansionState) -> gtk::Box {
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
        .map(|duration| format!(" · {}", format_duration(duration)))
        .unwrap_or_default();
    card.append(
        &gtk::Label::builder()
            .label(format!("{server} / {tool}"))
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["heading"])
            .build(),
    );
    card.append(&{
        let widget = gtk::Label::builder()
            .label("")
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-muted"])
            .build();
        l10n::bind_formatted_label(
            &widget,
            if ok {
                crate::i18n::msg(
                    "Succeeded{value1} · captured locally while live inspection was enabled",
                )
            } else {
                crate::i18n::msg(
                    "Failed{value1} · captured locally while live inspection was enabled",
                )
            },
            &[("value1", &duration.to_string())],
        );
        widget
    });
    let body = serde_json::to_string_pretty(&serde_json::json!({
        "request": capture.get("request").cloned().unwrap_or(serde_json::Value::Null),
        "response": capture.get("response").cloned().unwrap_or(serde_json::Value::Null),
    }))
    .unwrap_or_else(|_| crate::i18n::tr("Could not format capture").to_string());
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
    let details = {
        let widget = gtk::Expander::builder().child(&scroll).build();
        widget.set_label_widget(Some(&l10n::label("Inspect request and response")));
        widget
    };
    bind_activity_expander(
        &details,
        activity_row_key("inspect", capture),
        expanded_rows,
        false,
    );
    card.append(&details);
    card
}

#[cfg(test)]
fn savings_detail_line(list_loads: u64, peak_catalog: u64, since: Option<String>) -> String {
    let mut parts = vec![crate::i18n::trf(
        if list_loads == 1 {
            crate::i18n::msg("{value0} catalog load")
        } else {
            crate::i18n::msg("{value0} catalog loads")
        },
        &[("value0", &format!("{}", list_loads))],
    )];
    if peak_catalog > 4 {
        parts.push(crate::i18n::trf(
            "peak {value0} tools",
            &[("value0", &format!("{}", peak_catalog))],
        ));
    }
    if let Some(since) = since {
        parts.push(crate::i18n::trf(
            "since {value0}",
            &[("value0", &since.to_string())],
        ));
    }
    parts.join(" · ")
}

#[cfg(test)]
fn savings_banner_visible(loads: u64, searches: u64) -> bool {
    loads > 0 || searches > 0
}

#[cfg(test)]
fn savings_title(loads: u64) -> &'static str {
    if loads == 0 {
        crate::i18n::msg("Discovery payload returned")
    } else {
        crate::i18n::msg("Net tool description savings")
    }
}

fn activity_count_summary(total: usize, visible: usize, recent: usize, filtered: bool) -> String {
    let shown = if filtered {
        crate::i18n::trf(
            "Showing {value0} matching rows from the latest {value1}.",
            &[
                ("value0", &format!("{}", visible)),
                ("value1", &format!("{}", recent)),
            ],
        )
    } else if visible == recent {
        crate::i18n::trf(
            "Showing the latest {value0}.",
            &[("value0", &format!("{}", visible))],
        )
    } else {
        crate::i18n::trf(
            "Showing {value0} of the latest {value1}.",
            &[
                ("value0", &format!("{}", visible)),
                ("value1", &format!("{}", recent)),
            ],
        )
    };
    crate::i18n::trf(
        if total == 1 {
            crate::i18n::msg("{value0} call recorded on this computer. {value2}")
        } else {
            crate::i18n::msg("{value0} calls recorded on this computer. {value2}")
        },
        &[
            ("value0", &grouped_number(total as u64).to_string()),
            ("value2", &shown.to_string()),
        ],
    )
}

fn format_saved_tokens(tokens_saved: i64) -> String {
    format!(
        "{}{}",
        if tokens_saved < 0 { "-" } else { "" },
        state::format_token_count(tokens_saved.unsigned_abs())
    )
}

fn savings_old_estimate_display(tokens: i64) -> Option<String> {
    (tokens != 0).then(|| {
        let sign = if tokens < 0 { "-" } else { "" };
        crate::i18n::trf("Old estimate: {value0}{value1} tokens\nFrom previous versions; excluded from new savings.", &[("value0", sign), ("value1", &grouped_number(tokens.unsigned_abs()).to_string())])
    })
}

fn savings_primary_display(
    tokens_saved: i64,
    loads: u64,
    discovery_bytes: u64,
) -> (String, &'static str) {
    if loads == 0 {
        return (
            format_byte_count(discovery_bytes),
            crate::i18n::msg("discovery text returned"),
        );
    }
    (
        format_saved_tokens(tokens_saved),
        crate::i18n::msg("net tokens saved"),
    )
}

fn savings_share_line(tokens_saved: i64, loads: u64, searches: u64, bytes: u64) -> String {
    if loads == 0 {
        return crate::i18n::trf("Toolport recorded {value0} discovery searches returning {value1} of text at its MCP boundary. toolport.app", &[("value0", &format!("{}", searches)), ("value1", &format_byte_count(bytes).to_string())]);
    }
    crate::i18n::trf(
        if tokens_saved < 0 {
            crate::i18n::msg("Toolport recorded -{value1} net tokens saved. Full catalog baselines minus all catalog and search responses; counted locally with cl100k_base, not a billing figure. toolport.app")
        } else {
            crate::i18n::msg("Toolport recorded {value1} net tokens saved. Full catalog baselines minus all catalog and search responses; counted locally with cl100k_base, not a billing figure. toolport.app")
        },
        &[(
            "value1",
            &state::format_token_count(tokens_saved.unsigned_abs()).to_string(),
        )],
    )
}

fn format_byte_count(bytes: u64) -> String {
    if bytes >= 999_950_000_000 {
        crate::i18n::trf(
            "{value0} TB",
            &[(
                "value0",
                &format!("{:.1}", bytes as f64 / 1_000_000_000_000.0),
            )],
        )
    } else if bytes >= 999_950_000 {
        crate::i18n::trf(
            "{value0} GB",
            &[("value0", &format!("{:.1}", bytes as f64 / 1_000_000_000.0))],
        )
    } else if bytes >= 999_950 {
        crate::i18n::trf(
            "{value0} MB",
            &[("value0", &format!("{:.1}", bytes as f64 / 1_000_000.0))],
        )
    } else if bytes >= 1_000 {
        crate::i18n::trf(
            "{value0} KB",
            &[("value0", &format!("{:.1}", bytes as f64 / 1_000.0))],
        )
    } else {
        format!("{bytes} B")
    }
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
            crate::i18n::trf("{value0} values masked, incomplete", &[("value0", &format!("{}", replaced))]),
            "review",
            crate::i18n::msg("The pseudonymization pass did not fully apply - some values reached the model in the clear (the session map was full, or the result exceeded the scan cap)."),
        ));
    }
    if replaced == 0 {
        return None;
    }
    Some((
        crate::i18n::trf(if replaced == 1 { crate::i18n::msg("{value0} value masked") } else { crate::i18n::msg("{value0} values masked") }, &[("value0", &format!("{}", replaced))]),
        "success",
        crate::i18n::msg("Personal values in this result were replaced before reaching the model. The values themselves are never logged."),
    ))
}

fn activity_title(activity: &state::ActivityView) -> String {
    if activity.internal {
        match activity.tool.as_str() {
            "search" => crate::i18n::msg("Searched tools"),
            "describe" => crate::i18n::msg("Looked up a tool"),
            "status" => crate::i18n::msg("Checked Toolport status"),
            "fetch" => crate::i18n::msg("Retrieved a tool result"),
            _ => crate::i18n::msg("Used Toolport"),
        }
        .into()
    } else {
        format!("{} / {}", activity.server, activity.tool)
    }
}

// Canonical caller strings are identity/control data. Only these known fallback
// cases are presentation sources; user-reported names are inserted verbatim.
fn localized_caller_name(name: Option<&str>, label: Option<&str>) -> String {
    let canonical = crate::clients::display_caller_name(name, label);
    if name.is_some_and(|name| {
        !name.is_empty()
            && !matches!(
                name,
                "An AI client" | "Unknown client" | "Unrecorded client"
            )
    }) {
        return canonical;
    }
    if let Some(name) = label.filter(|label| !label.is_empty()) {
        return crate::i18n::trf("{name} (reported)", &[("name", name)]);
    }
    match canonical.as_str() {
        "Unknown client" => crate::i18n::tr("Unknown client"),
        "Unrecorded client" => crate::i18n::tr("Unrecorded client"),
        _ => canonical,
    }
}
fn unrecorded_client_tooltip() -> String {
    crate::i18n::tr("Older Toolport versions did not record callers for these rows.")
}
fn activity_client_display(activity: &state::ActivityView) -> String {
    let canonical = crate::clients::display_caller_name(
        activity.client.as_deref(),
        activity.client_label.as_deref(),
    );
    let display =
        localized_caller_name(activity.client.as_deref(), activity.client_label.as_deref());
    if canonical.ends_with(" (reported)") {
        return display;
    }
    match activity
        .client_label
        .as_deref()
        .filter(|label| !crate::clients::label_only_adds_version(&canonical, label))
    {
        Some(label) => crate::i18n::trf(
            "{value0} (reports \"{value1}\")",
            &[("value0", &display), ("value1", label)],
        ),
        None => display,
    }
}
fn activity_detail(activity: &state::ActivityView) -> String {
    let outcome = activity.approval_decision.as_deref().map(approval_outcome);
    let mut detail = vec![
        activity_client_display(activity),
        relative_activity_time(activity.timestamp_ms),
    ];

    if let Some(kind) = &activity.failure_kind {
        detail.push(kind.replace('_', " "));
    }

    if let Some(duration) = activity.duration_ms {
        detail.push(if outcome.is_some() {
            crate::i18n::trf(
                "waited {value0}",
                &[("value0", &format_duration(duration).to_string())],
            )
        } else {
            format_duration(duration)
        });
    }
    detail.join(" · ")
}
fn activity_detail_tooltip(activity: &state::ActivityView) -> String {
    format!(
        "{}{}{}{}",
        if activity_client_name(activity) == "Unrecorded client" {
            unrecorded_client_tooltip()
        } else {
            activity.client_id.as_deref().unwrap_or_default().to_owned()
        },
        activity
            .run_id
            .as_ref()
            .map(|id| crate::i18n::trf("\nRun: {value0}", &[("value0", &id.to_string())]))
            .unwrap_or_default(),
        activity
            .cold
            .map(|cold| if cold {
                crate::i18n::tr("\nCold catalog")
            } else {
                crate::i18n::tr("\nWarm catalog")
            })
            .unwrap_or_default(),
        activity
            .dispatch_ms
            .map(|ms| crate::i18n::trf(
                "\nDispatch: {value0}",
                &[("value0", &format_duration(ms).to_string())]
            ))
            .unwrap_or_default()
    )
}

fn activity_card(activity: &state::ActivityView) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    card.add_css_class("toolport-card");
    let outcome = activity.approval_decision.as_deref().map(approval_outcome);
    let icon = gtk::Image::from_icon_name(outcome.map(|(_, icon, _, _)| icon).unwrap_or(
        if activity.internal {
            "edit-find-symbolic"
        } else if activity.ok {
            "emblem-ok-symbolic"
        } else {
            "dialog-error-symbolic"
        },
    ));
    if let Some((_, _, tone, _)) = outcome {
        icon.add_css_class(tone);
    }
    icon.add_css_class("toolport-card-icon");
    icon.set_valign(gtk::Align::Center);
    if let Some((_, _, tone, Some(emblem))) = outcome {
        let overlay = gtk::Overlay::new();
        overlay.set_valign(gtk::Align::Center);
        overlay.set_child(Some(&icon));
        let emblem = gtk::Image::from_icon_name(emblem);
        emblem.set_pixel_size(10);
        emblem.set_halign(gtk::Align::Center);
        emblem.set_valign(gtk::Align::Center);
        emblem.add_css_class(tone);
        overlay.add_overlay(&emblem);
        card.append(&overlay);
    } else {
        card.append(&icon);
    }

    let copy = gtk::Box::new(gtk::Orientation::Vertical, 3);
    copy.set_hexpand(true);
    let heading = gtk::Label::builder()
        .label(activity_title(activity))
        .tooltip_text(format!("{} / {}", activity.server, activity.tool))
        .halign(gtk::Align::Fill)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["heading"])
        .build();
    if activity.internal {
        let activity = activity.clone();
        l10n::bind_label_with(&heading, move || {
            crate::i18n::lookup(&activity_title(&activity))
        });
    }
    copy.append(&heading);
    let detail_label = gtk::Label::builder()
        .label(activity_detail(activity))
        .tooltip_text(activity_detail_tooltip(activity))
        .halign(gtk::Align::Fill)
        .xalign(0.0)
        .single_line_mode(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["toolport-muted"])
        .build();
    let activity_for_detail = activity.clone();
    l10n::bind_label_with(&detail_label, move || activity_detail(&activity_for_detail));
    let activity_for_tooltip = activity.clone();
    l10n::bind_tooltip_with(&detail_label, move || {
        activity_detail_tooltip(&activity_for_tooltip)
    });
    copy.append(&detail_label);
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
        l10n::bind_formatted_label(
            &badge,
            if activity.pii_incomplete {
                crate::i18n::msg("{value0} values masked, incomplete")
            } else if activity.pii_replaced == Some(1) {
                crate::i18n::msg("{value0} value masked")
            } else {
                crate::i18n::msg("{value0} values masked")
            },
            &[(
                "value0",
                &activity.pii_replaced.unwrap_or_default().to_string(),
            )],
        );
        badge.add_css_class("toolport-badge");
        badge.set_valign(gtk::Align::Center);
        badge.set_halign(gtk::Align::Start);
        badge.add_css_class(class);
        l10n::bind_tooltip(&badge, tooltip);
        card.append(&badge);
    }
    let (status, class) = if let Some((label, _, tone, _)) = outcome {
        (label, tone)
    } else if activity.held {
        (crate::i18n::msg("Held"), "review")
    } else if activity.ok {
        (crate::i18n::msg("Succeeded"), "success")
    } else {
        (crate::i18n::msg("Failed"), "disabled")
    };
    let badge = l10n::label(status);
    badge.add_css_class("toolport-badge");
    badge.set_valign(gtk::Align::Center);
    badge.set_halign(gtk::Align::Start);
    badge.add_css_class(class);
    card.append(&badge);
    card
}

fn approval_outcome(
    decision: &str,
) -> (
    &'static str,
    &'static str,
    &'static str,
    Option<&'static str>,
) {
    // Adwaita symbolic icons, also available in Omarchy's inherited icon theme.
    match decision {
        "approved" => (
            crate::i18n::msg("Approved"),
            "security-medium-symbolic",
            "success",
            Some("object-select-symbolic"),
        ),
        "denied" => (
            crate::i18n::msg("Denied"),
            "security-medium-symbolic",
            "approval-denied",
            Some("window-close-symbolic"),
        ),
        "no_response" => (
            crate::i18n::msg("No answer"),
            "preferences-system-time-symbolic",
            "approval-warning",
            None,
        ),
        "withdrawn" => (
            crate::i18n::msg("Withdrawn"),
            "edit-undo-symbolic",
            "disabled",
            None,
        ),
        "stale_state" => (
            crate::i18n::msg("Changed after approval"),
            "dialog-warning-symbolic",
            "approval-warning",
            None,
        ),
        "unreachable" => (
            crate::i18n::msg("No approver available"),
            "dialog-warning-symbolic",
            "approval-warning",
            None,
        ),
        _ => (
            crate::i18n::msg("Approval event"),
            "dialog-information-symbolic",
            "disabled",
            None,
        ),
    }
}

fn format_duration(ms: u64) -> String {
    if ms < 1000 {
        crate::i18n::trf("{value0} ms", &[("value0", &format!("{}", ms))])
    } else if ms < 60000 {
        let tenths = (ms + 50) / 100;
        format!("{}.{} s", tenths / 10, tenths % 10)
    } else {
        let seconds = ms.saturating_add(500) / 1000;
        format!("{}m {}s", seconds / 60, seconds % 60)
    }
}

/// Run a quick user action on its own thread. Slow startup work (server probes,
/// gateway cleanup) can fill GLib's shared pool, and a settings toggle queued
/// behind it took about 20 seconds to save.
pub(super) async fn run_user_action<T: Send + 'static>(
    action: impl FnOnce() -> T + Send + 'static,
) -> Result<T, ()> {
    let (sender, receiver) = futures_channel::oneshot::channel();
    std::thread::Builder::new()
        .name("toolport-action".into())
        .spawn(move || {
            let _ = sender.send(std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                action,
            )));
        })
        .map_err(|_| ())?;
    match receiver.await {
        Ok(Ok(value)) => Ok(value),
        _ => Err(()),
    }
}

fn activity_client_name(activity: &state::ActivityView) -> String {
    let name = crate::clients::display_caller_name(
        activity.client.as_deref(),
        activity.client_label.as_deref(),
    );
    if name.ends_with(" (reported)") {
        return name;
    }
    match activity
        .client_label
        .as_deref()
        .filter(|label| !crate::clients::label_only_adds_version(&name, label))
    {
        Some(label) => crate::i18n::trf(
            "{value0} (reports \"{value1}\")",
            &[("value0", &name.to_string()), ("value1", label)],
        ),
        None => name.to_string(),
    }
}

/// One aggregation row's metrics, shared by the server line and its tool lines:
/// calls, errors, and latency where the log carried durations.
fn stat_metrics_line(stat: &serde_json::Value) -> String {
    let number = |key: &str| stat.get(key).and_then(serde_json::Value::as_u64);
    let calls = number("calls").unwrap_or(0);
    let errors = number("errors").unwrap_or(0);
    let mut parts = vec![
        crate::i18n::trf(
            if calls == 1 {
                crate::i18n::msg("{count} call")
            } else {
                crate::i18n::msg("{count} calls")
            },
            &[("count", &calls.to_string())],
        ),
        crate::i18n::trf(
            if errors == 1 {
                crate::i18n::msg("{count} error")
            } else {
                crate::i18n::msg("{count} errors")
            },
            &[("count", &errors.to_string())],
        ),
    ];
    if let Some(avg) = number("avgMs") {
        parts.push(crate::i18n::trf(
            "avg {value0}",
            &[("value0", &format_duration(avg).to_string())],
        ));
    }
    if let Some(p95) = number("p95Ms") {
        parts.push(format!("p95 {}", format_duration(p95)));
    }
    parts.join(" · ")
}

/// One server's row in the per-server breakdown. Expanding it reveals the
/// per-tool breakdown, built on first expansion like the identity groups.
fn stat_server_name(stat: &serde_json::Value) -> String {
    stat.get("server")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|server| !server.is_empty())
        .unwrap_or("Unknown server")
        .to_string()
}

fn server_stat_row(
    stat: &serde_json::Value,
    expanded_servers: std::rc::Rc<std::cell::RefCell<std::collections::HashSet<String>>>,
) -> gtk::Expander {
    let server = stat_server_name(stat);
    let expander = gtk::Expander::new(None);
    expander.add_css_class("toolport-card");
    let title = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    title.append(
        &gtk::Label::builder()
            .label(
                if stat
                    .get("server")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|name| !name.trim().is_empty())
                {
                    server.clone()
                } else {
                    crate::i18n::tr("Unknown server")
                },
            )
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["heading"])
            .build(),
    );
    title.append(
        &gtk::Label::builder()
            .label(stat_metrics_line(stat))
            .halign(gtk::Align::End)
            .xalign(1.0)
            .css_classes(["toolport-muted", "caption", "toolport-stat-metrics"])
            .build(),
    );
    expander.set_label_widget(Some(&title));
    let tools = stat
        .get("tools")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let pending = std::rc::Rc::new(std::cell::RefCell::new(Some(tools)));
    let server_for_expansion = server.clone();
    let expanded_for_notify = expanded_servers.clone();
    expander.connect_expanded_notify(move |expander| {
        if !expander.is_expanded() {
            expanded_for_notify
                .borrow_mut()
                .remove(&server_for_expansion);
            return;
        }
        expanded_for_notify
            .borrow_mut()
            .insert(server_for_expansion.clone());
        let Some(tools) = pending.borrow_mut().take() else {
            return;
        };
        let list = gtk::Box::new(gtk::Orientation::Vertical, 4);
        list.set_margin_top(8);
        if tools.is_empty() {
            list.append(&empty_activity_source_label(crate::i18n::msg(
                "No per-tool rows retained.",
            )));
        }
        for tool in tools {
            let name = tool
                .get("tool")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| crate::i18n::tr("Unknown tool"));
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            row.add_css_class("toolport-stat-row");
            row.append(
                &gtk::Label::builder()
                    .label(name)
                    .halign(gtk::Align::Start)
                    .xalign(0.0)
                    .hexpand(true)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .css_classes(["toolport-muted"])
                    .build(),
            );
            row.append(
                &gtk::Label::builder()
                    .label(stat_metrics_line(&tool))
                    .halign(gtk::Align::End)
                    .xalign(1.0)
                    .css_classes(["toolport-muted", "caption", "toolport-stat-metrics"])
                    .build(),
            );
            list.append(&row);
        }
        expander.set_child(Some(&list));
    });
    if expanded_servers.borrow().contains(&server) {
        expander.set_expanded(true);
    }
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
/// Stable filter choices. Keeping this model unchanged prevents an open dropdown
/// from being dismissed by an unrelated live-metrics refresh.
fn activity_server_filter_options(calls: &[state::ActivityView]) -> Vec<String> {
    let mut options = vec![crate::i18n::tr("All servers").to_string()];
    for call in calls {
        if !options.iter().any(|server| server == &call.server) {
            options.push(call.server.clone());
        }
    }
    options
}

/// Which of the recent calls the current filter keeps.
fn filter_calls<'a>(
    calls: &'a [state::ActivityView],
    server: Option<&str>,
    errors_only: bool,
) -> Vec<&'a state::ActivityView> {
    calls
        .iter()
        .filter(|call| server.is_none_or(|server| call.server == server))
        .filter(|call| !errors_only || (call.approval_decision.is_none() && !call.ok))
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
    expanded_rows: ActivityExpansionState,
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
        label.push_str(&crate::i18n::trf(
            " · {value0} quarantined",
            &[("value0", &format!("{}", quarantined))],
        ));
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
    bind_activity_expander(
        &expander,
        format!("identity:{}", server),
        expanded_rows,
        expanded,
    );
    expander
}

fn tool_identity_row(identity: &crate::integrity::ToolIdentity) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 3);
    let title = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let alias = gtk::Label::builder()
        .label(&identity.alias)
        .halign(gtk::Align::Fill)
        .hexpand(true)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["heading", "monospace"])
        .build();
    title.append(&alias);
    if identity.quarantined {
        let badge = l10n::label("Quarantined");
        badge.add_css_class("toolport-badge");
        badge.set_valign(gtk::Align::Center);
        badge.set_halign(gtk::Align::Start);
        badge.add_css_class("review");
        title.append(&badge);
    }
    row.append(&title);
    let mut detail = vec![
        if identity.upstream.is_empty() {
            crate::i18n::tr("unattributed alias").to_string()
        } else {
            crate::i18n::trf(
                "upstream {value0}",
                &[("value0", &identity.upstream.to_string())],
            )
        },
        short_fingerprint(&identity.fingerprint),
    ];
    if identity.last_changed > 0 {
        detail.push(crate::i18n::trf(
            "changed {value0}",
            &[(
                "value0",
                &relative_activity_time(identity.last_changed).to_string(),
            )],
        ));
    }
    if !identity.profiles.is_empty() {
        detail.push(crate::i18n::trf(
            "profiles {value0}",
            &[("value0", &identity.profiles.join(", ").to_string())],
        ));
    }
    row.append(
        &gtk::Label::builder()
            .label(detail.join(" · "))
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-muted", "caption"])
            .build(),
    );
    row
}

fn relative_activity_time(timestamp_ms: u64) -> String {
    if timestamp_ms == 0 {
        return crate::i18n::tr("Unknown time").to_string();
    }
    let age_ms = epoch_ms().saturating_sub(timestamp_ms);
    let seconds = age_ms / 1000;
    if seconds < 60 {
        crate::i18n::tr("Just now").to_string()
    } else if seconds < 3_600 {
        crate::i18n::trf("{value0}m ago", &[("value0", &format!("{}", seconds / 60))])
    } else if seconds < 86_400 {
        crate::i18n::trf(
            "{value0}h ago",
            &[("value0", &format!("{}", seconds / 3_600))],
        )
    } else {
        crate::i18n::trf(
            "{value0}d ago",
            &[("value0", &format!("{}", seconds / 86_400))],
        )
    }
}

/// The badge label and CSS class for one client's instructions state (shared with the Teams page).
fn rule_apply_state(state: crate::instructions::ApplyState) -> (&'static str, &'static str) {
    match state {
        crate::instructions::ApplyState::Applied => (crate::i18n::msg("Applied"), "success"),
        crate::instructions::ApplyState::Stale => (crate::i18n::msg("Needs update"), "review"),
        crate::instructions::ApplyState::Unsupported => {
            (crate::i18n::msg("Unsupported"), "disabled")
        }
        crate::instructions::ApplyState::BlockedOverride => {
            (crate::i18n::msg("Blocked by override"), "review")
        }
        crate::instructions::ApplyState::TooLong => (crate::i18n::msg("Too long"), "review"),
        crate::instructions::ApplyState::Error => (crate::i18n::msg("Could not apply"), "disabled"),
    }
}

#[derive(Clone)]
struct ApprovalPage {
    root: gtk::Box,
    count: gtk::Label,
    list: gtk::Box,
    broker: crate::approval_broker::ApprovalBroker,
    rendered: std::rc::Rc<std::cell::RefCell<Vec<(String, u64)>>>,
    /// True while the panel is explaining that another process owns approvals,
    /// so the explanation renders once rather than on every poll.
    showing_inert: std::rc::Rc<std::cell::RefCell<bool>>,
    deadlines: std::rc::Rc<std::cell::RefCell<Vec<(u64, gtk::Label)>>>,
    notified: std::rc::Rc<std::cell::RefCell<std::collections::HashSet<String>>>,
    notification: notify::ApprovalNotification,
    stopped: std::rc::Rc<std::cell::Cell<bool>>,
}

impl ApprovalPage {
    fn new(app: &adw::Application, broker: crate::approval_broker::ApprovalBroker) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
        root.add_css_class("toolport-approvals");
        root.add_css_class("toolport-global-alert");
        root.set_visible(false);

        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        heading.append(&{
            let widget = gtk::Label::builder()
                .label(crate::i18n::tr("Approval required"))
                .halign(gtk::Align::Start)
                .hexpand(true)
                .css_classes(["heading"])
                .build();
            l10n::bind_label(&widget, "Approval required");
            widget
        });
        let count = gtk::Label::new(None);
        count.add_css_class("toolport-badge");
        count.set_valign(gtk::Align::Center);
        count.set_halign(gtk::Align::Start);
        count.add_css_class("warning");
        heading.append(&count);
        root.append(&heading);

        let list = gtk::Box::new(gtk::Orientation::Vertical, 10);
        root.append(&list);
        let page = Self {
            root,
            count,
            list,
            broker,
            rendered: std::rc::Rc::new(std::cell::RefCell::new(Vec::new())),
            showing_inert: std::rc::Rc::new(std::cell::RefCell::new(false)),
            deadlines: std::rc::Rc::new(std::cell::RefCell::new(Vec::new())),
            notified: std::rc::Rc::new(std::cell::RefCell::new(std::collections::HashSet::new())),
            notification: notify::ApprovalNotification::new(app),
            stopped: Default::default(),
        };
        let action = gtk::gio::SimpleAction::new("approval-queue-changed", None);
        let page_for_event = page.clone();
        action.connect_activate(move |_, _| page_for_event.refresh());
        app.add_action(&action);
        page
    }

    fn attach(&self, window: &adw::ApplicationWindow) {
        let stopped = self.stopped.clone();
        io::on_window_closed(window, move || stopped.set(true));
        // A request can arrive before the window finishes building. Publish its
        // initial snapshot to both the panel and the sidebar badge.
        if let Some(action) = window
            .application()
            .and_then(|app| app.lookup_action("approval-queue-changed"))
        {
            action.activate(None);
        } else {
            self.refresh();
        }
        let page = self.clone();
        io::while_mapped(&self.root, 1, move || page.refresh());
    }

    fn refresh(&self) {
        let mut pending = self.broker.list();
        pending.sort_by_key(|view| view.deadline_ms);
        let signature = pending
            .iter()
            .map(|view| (view.id.clone(), view.deadline_ms))
            .collect::<Vec<_>>();
        self.reconcile_notifications(&pending);
        // Another Toolport already holds the approval endpoint, so this window
        // will never receive a prompt: the gateway hands every gated call to
        // whichever process started first. Staying hidden would leave someone
        // watching an empty queue while a call waits in the other window.
        if !self.broker.owns_endpoint() {
            self.root.set_visible(true);
            self.count.set_visible(false);
            if !self.showing_inert.replace(true) {
                self.rendered.borrow_mut().clear();
                while let Some(child) = self.list.first_child() {
                    self.list.remove(&child);
                }
                self.deadlines.borrow_mut().clear();
                self.list.append(&inert_broker_card());
            }
            return;
        }
        if self.showing_inert.replace(false) {
            while let Some(child) = self.list.first_child() {
                self.list.remove(&child);
            }
        }
        self.root.set_visible(!pending.is_empty());
        self.count.set_visible(pending.len() > 1);
        l10n::bind_formatted_label(
            &self.count,
            "{value0} pending",
            &[("value0", &format!("{}", pending.len()))],
        );
        if *self.rendered.borrow() == signature {
            self.update_deadlines();
            return;
        }
        *self.rendered.borrow_mut() = signature;
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        self.deadlines.borrow_mut().clear();
        // One decision at a time keeps an approval burst from taking over the
        // window. Resolving the oldest immediately advances the queue.
        for view in pending.into_iter().take(1) {
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
            .map(|view| view.id.clone())
            .collect::<std::collections::HashSet<_>>();
        let mut notified = self.notified.borrow_mut();
        if *notified == current {
            return;
        }
        *notified = current;
        if pending.is_empty() {
            self.notification.withdraw();
            return;
        }
        let (title, body) = approval_queue_notification(pending)
            .expect("a non-empty approval queue has notification copy");
        self.notification.show(&title, &body);
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
                        let page = self.clone();
                        let save = persist_always_allow(key);
                        gtk::glib::spawn_future_local(async move {
                            let result = save.await;
                            if page.stopped.get() {
                                return;
                            }
                            if let Err(error) = result {
                                eprintln!("toolport-gtk: approved the call but could not save its allow rule: {error}");
                                if let Some(parent) = page.root.root().and_downcast::<gtk::Window>()
                                {
                                    #[allow(deprecated)]
                                    let dialog = adw::MessageDialog::new(Some(&parent), Some(&crate::i18n::tr("Could not save Always allow")), Some(&crate::i18n::trf("This call was approved for this session. Try Always allow again to keep the rule after restarting. {value0}", &[("value0", &error.to_string())])));
                                    l10n::bind_formatted_dialog_body(&dialog, "This call was approved for this session. Try Always allow again to keep the rule after restarting. {value0}", &[("value0", &error.to_string())]);
                                    l10n::bind_dialog_heading(
                                        &dialog,
                                        "Could not save Always allow",
                                    );
                                    dialog.add_response("close", &crate::i18n::tr("Close"));
                                    l10n::bind_dialog_response(&dialog, "close", "Close");
                                    dialog.set_close_response("close");
                                    dialog.connect_response(None, |dialog, _| dialog.close());
                                    dialog.present();
                                }
                            }
                        });
                    }
                }
            }
            Err(error) => eprintln!("toolport-gtk: could not resolve approval: {error}"),
        }
        self.refresh();
    }
}

type AllowSave = std::sync::mpsc::Receiver<()>;
static ALLOW_SAVES: std::sync::Mutex<Vec<AllowSave>> = std::sync::Mutex::new(Vec::new());

// Start before returning a future: immediate quit must not cancel scheduling.
fn persist_always_allow(key: String) -> impl std::future::Future<Output = Result<(), String>> {
    let (done, pending) = std::sync::mpsc::channel();
    let (send, result) = futures_channel::oneshot::channel();
    let mut saves = ALLOW_SAVES
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    saves.retain(|save| matches!(save.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)));
    saves.push(pending);
    drop(saves);
    std::thread::spawn(move || {
        let saved = save_always_allow(key);
        if let Err(error) = &saved {
            eprintln!("toolport-gtk: could not save the allow rule: {error}");
        }
        let _ = send.send(saved);
        let _ = done.send(());
    });
    async move {
        result.await.map_err(|_| {
            crate::i18n::tr("The allow-rule worker stopped unexpectedly").to_string()
        })?
    }
}

fn finish_allow_saves() {
    let saves = std::mem::take(
        &mut *ALLOW_SAVES
            .lock()
            .unwrap_or_else(|error| error.into_inner()),
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    for save in saves {
        if save
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .is_err()
        {
            eprintln!("toolport-gtk: an allow-rule save did not finish before shutdown");
        }
    }
}

async fn read_pair_target(origin: String, team: String) -> Result<bool, String> {
    gtk::gio::spawn_blocking(move || pair_target_is_signed_in(&origin, &team))
        .await
        .map_err(|_| crate::i18n::tr("The pairing worker stopped unexpectedly").to_string())
}

fn save_always_allow(key: String) -> Result<(), String> {
    crate::registry::update(|registry| {
        registry.allow_tool(key);
        Ok(())
    })
    .map(|_| ())
}

fn pair_target_is_signed_in(origin: &str, team: &str) -> bool {
    crate::registry::load().is_ok_and(|reg| {
        crate::teams::pair_target_is_current(&reg, origin, team)
            && !crate::personal_sync::state(&reg).is_ok_and(|st| st.sign_in_required)
    })
}

/// Shown when another Toolport process owns the approval endpoint. Both shells
/// read the same data directory and only one can hold that lock, so the other
/// can never answer a prompt and has to say so rather than look idle.
fn inert_broker_card() -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 4);
    card.add_css_class("toolport-card");
    card.append(&{
        let widget = gtk::Label::builder()
            .label(crate::i18n::tr("Another Toolport is handling approvals"))
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .hexpand(true)
            .css_classes(["heading"])
            .build();
        l10n::bind_label(&widget, "Another Toolport is handling approvals");
        widget
    });
    card.append(
        &{ let widget = gtk::Label::builder()
            .label(crate::i18n::tr("Prompts go to whichever Toolport started first, so they will not appear here. Quit the other one and reopen this window to take over."))
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .hexpand(true)
            .css_classes(["toolport-muted"])
            .build(); l10n::bind_label(&widget, "Prompts go to whichever Toolport started first, so they will not appear here. Quit the other one and reopen this window to take over."); widget },
    );
    card
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
        .halign(gtk::Align::Fill)
        .xalign(0.0)
        .hexpand(true)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .css_classes(["heading"])
        .build();
    title_row.append(&title);
    let deadline = gtk::Label::builder()
        .css_classes(["toolport-approval-deadline"])
        .build();
    title_row.append(&deadline);
    card.append(&title_row);

    let requester =
        crate::clients::display_caller_name(Some(&view.client_name), view.client_label.as_deref());
    card.append(&{
        let widget = gtk::Label::builder()
            .tooltip_text(if requester == "Unrecorded client" {
                unrecorded_client_tooltip()
            } else {
                view.client.as_deref().unwrap_or("").to_owned()
            })
            .label("")
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-muted"])
            .build();
        let view_for_requester = view.clone();
        l10n::bind_label_with(&widget, move || {
            crate::i18n::trf(
                "{value0} wants to run this · {value1}",
                &[
                    (
                        "value0",
                        &localized_caller_name(
                            Some(&view_for_requester.client_name),
                            view_for_requester.client_label.as_deref(),
                        ),
                    ),
                    (
                        "value1",
                        &crate::i18n::lookup(approval_reason(view_for_requester.reason)),
                    ),
                ],
            )
        });
        if requester == "Unrecorded client" {
            l10n::bind_tooltip(
                &widget,
                "Older Toolport versions did not record callers for these rows.",
            );
        }
        widget
    });

    if let Some(label) = view
        .client_label
        .as_deref()
        .filter(|label| *label != requester && !requester.ends_with(" (reported)"))
    {
        card.append(&{
            let widget = gtk::Label::builder()
                .label("")
                .tooltip_text(label)
                .xalign(0.0)
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .css_classes(["toolport-muted"])
                .build();
            l10n::bind_formatted_label(
                &widget,
                "Reports itself as: {value0}",
                &[(
                    "value0",
                    &crate::approval::shorten_client_label(label, 60).to_string(),
                )],
            );
            widget
        });
    }
    if let Some(url) = &view.url_elicitation {
        card.append(&{
            let widget = gtk::Label::builder()
                .label("")
                .halign(gtk::Align::Start)
                .xalign(0.0)
                .wrap(true)
                .selectable(true)
                .build();
            l10n::bind_formatted_label(
                &widget,
                "Browser destination: {value0}\n{value1}",
                &[
                    ("value0", &url.origin.to_string()),
                    ("value1", &url.message.to_string()),
                ],
            );
            widget
        });
    }

    if let Some(release) = &view.pii_release {
        let values = release
            .values
            .iter()
            .map(|value| {
                let origins = if value.origins.is_empty() {
                    crate::i18n::tr("no recorded source").to_string()
                } else {
                    crate::i18n::trf(
                        "from {value0}",
                        &[("value0", &value.origins.join(", ").to_string())],
                    )
                };
                format!("{} → {} ({origins})", value.token, value.value)
            })
            .collect::<Vec<_>>()
            .join("\n");
        card.append(&{
            let widget = gtk::Label::builder()
                .label("")
                .halign(gtk::Align::Fill)
                .xalign(0.0)
                .wrap(true)
                .selectable(true)
                .css_classes(["toolport-sensitive-review"])
                .build();
            l10n::bind_formatted_label(
                &widget,
                "Would send to {value0}:\n{value1}",
                &[
                    ("value0", &release.server.to_string()),
                    ("value1", &values.to_string()),
                ],
            );
            widget
        });
    }

    let arguments = serde_json::to_string_pretty(&view.arguments)
        .unwrap_or_else(|_| crate::i18n::tr("Arguments could not be displayed"));
    let details = {
        let widget = gtk::Expander::builder().build();
        widget.set_label_widget(Some(&l10n::label("Review arguments")));
        widget
    };
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
                let open = l10n::button("Open browser");
                let target = url.url.clone();
                open.connect_clicked(move |_| {
                    let _ = crate::oauth::open_web_url(&target);
                });
                actions.append(&open);
            }
            Err(error) => {
                actions.append(&{
                    let widget = gtk::Label::builder()
                        .label("")
                        .halign(gtk::Align::Fill)
                        .xalign(0.0)
                        .wrap(true)
                        .css_classes(["toolport-feedback", "error"])
                        .build();
                    l10n::bind_formatted_label(
                        &widget,
                        "Browser link refused: {value0}",
                        &[("value0", &error.to_string())],
                    );
                    widget
                });
            }
        }
    }
    let deny = l10n::button(if view.url_elicitation.is_some() {
        crate::i18n::msg("Cancel")
    } else {
        crate::i18n::msg("Deny")
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
            .label(crate::i18n::tr("Remember approval"))
            .tooltip_text(crate::i18n::tr(
                "Approve this definition without asking again",
            ))
            .build();
        remember.add_css_class("toolport-secondary-action");
        let popover = toolport_menu_popover();
        let choices = gtk::Box::new(gtk::Orientation::Vertical, 4);
        choices.set_margin_top(6);
        choices.set_margin_bottom(6);
        choices.set_margin_start(6);
        choices.set_margin_end(6);
        for (label, scope) in [
            (crate::i18n::msg("For this session"), "session"),
            (crate::i18n::msg("Always for this definition"), "always"),
        ] {
            let choice = toolport_menu_source_button(label);
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

    let approve = l10n::button(if view.url_elicitation.is_some() {
        crate::i18n::msg("Continue")
    } else {
        crate::i18n::msg("Approve once")
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
        crate::approval::ApprovalReason::Destructive => crate::i18n::msg("destructive tool"),
        crate::approval::ApprovalReason::UntrustedSource => crate::i18n::msg("untrusted source"),
        crate::approval::ApprovalReason::DestructiveAndUntrusted => {
            crate::i18n::msg("destructive tool from an untrusted source")
        }
        crate::approval::ApprovalReason::PiiCrossServer => {
            crate::i18n::msg("cross-server data release")
        }
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
    header.set_title_widget(Some(&{
        let widget = gtk::Label::builder()
            .label(crate::i18n::tr("Servers"))
            .css_classes(["title"])
            .build();
        l10n::bind_label(&widget, "Servers");
        widget
    }));
    let add_server = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text(crate::i18n::tr("Add server"))
        .css_classes(["flat", "toolport-header-add"])
        .build();
    header.pack_end(&add_server);
    // Catalog left the sidebar in 2.0, so an install that already has servers
    // never sees the onboarding catalog link. Keep it reachable from the Servers
    // header for both fresh and existing setups.
    let browse_catalog = l10n::button("Browse catalog");
    browse_catalog.set_tooltip_text(Some(&crate::i18n::tr(
        "Browse Toolport's curated server catalog",
    )));
    browse_catalog.add_css_class("toolport-secondary-action");
    {
        let app = app.clone();
        browse_catalog.connect_clicked(move |_| {
            if let Some(action) = app.lookup_action("show-catalog") {
                action.activate(None);
            }
        });
    }
    header.pack_end(&browse_catalog);
    let menu_popover = gtk::Popover::new();
    menu_popover.add_css_class("toolport-main-menu");
    menu_popover.set_has_arrow(false);
    let menu_content = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let menu_action = |label: &'static str, action: &str, shortcut: Option<&str>| {
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 18);
        content.append(&{
            let widget = gtk::Label::builder()
                .label(label)
                .halign(gtk::Align::Start)
                .hexpand(true)
                .build();
            l10n::bind_label(&widget, label);
            widget
        });
        if let Some(shortcut) = shortcut {
            content.append(
                &gtk::Label::builder()
                    .label(shortcut)
                    .css_classes(["toolport-menu-shortcut"])
                    .build(),
            );
        }
        let button = gtk::Button::builder()
            .child(&content)
            .css_classes(["flat", "toolport-main-menu-item"])
            .build();
        button.set_action_name(Some(action));
        button
    };
    for (label, action) in [
        (crate::i18n::msg("Import setup…"), "app.import-setup"),
        (crate::i18n::msg("Import pasted setup…"), "app.paste-setup"),
        (crate::i18n::msg("Export setup…"), "app.export-setup"),
        (crate::i18n::msg("Share setup…"), "app.share-setup"),
    ] {
        menu_content.append(&menu_action(label, action, None));
    }
    menu_content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    menu_content.append(&menu_action(
        crate::i18n::msg("Run setup assistant…"),
        "app.show-onboarding",
        None,
    ));
    menu_content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let quit = menu_action(
        crate::i18n::msg("Quit Toolport"),
        "app.quit",
        Some("Ctrl+Q"),
    );
    quit.add_css_class("toolport-main-menu-quit");
    menu_content.append(&quit);
    menu_popover.set_child(Some(&menu_content));
    let menu_button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .tooltip_text(crate::i18n::tr("Import, export and setup"))
        .build();
    menu_button.set_popover(Some(&menu_popover));
    header.pack_end(&menu_button);
    let check_servers = gtk::Button::builder()
        .icon_name("view-refresh-symbolic")
        .tooltip_text(crate::i18n::tr("Check servers again (Ctrl+R)"))
        .build();
    header.pack_end(&check_servers);
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

    let intro = {
        let widget = gtk::Label::builder()
            .label(crate::i18n::tr("One local gateway for every AI client"))
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["title-2"])
            .build();
        l10n::bind_label(&widget, "One local gateway for every AI client");
        widget
    };
    page.append(&intro);

    let description = {
        let widget = gtk::Label::builder()
            .label(crate::i18n::tr("Your MCP servers, available everywhere."))
            .halign(gtk::Align::Fill)
            .wrap(true)
            .xalign(0.0)
            .css_classes(["toolport-muted"])
            .build();
        l10n::bind_label(&widget, "Your MCP servers, available everywhere.");
        widget
    };
    page.append(&description);

    let feedback = gtk::Label::builder()
        .halign(gtk::Align::Fill)
        .xalign(0.0)
        .wrap(true)
        .height_request(48)
        .css_classes(["toolport-feedback", "idle"])
        .build();
    page.append(&feedback);

    let approval_page = ApprovalPage::new(app, broker);

    let summary = gtk::Grid::new();
    summary.add_css_class("toolport-summary");
    summary.set_column_spacing(10);
    summary.set_column_homogeneous(true);
    let mut values = Vec::new();
    for (column, (value, label)) in [("0", crate::i18n::msg("Servers")), ("0", "Enabled")]
        .into_iter()
        .enumerate()
    {
        let (item, value) = summary_item(value, label);
        item.set_size_request(0, -1);
        item.set_hexpand(true);
        values.push(value);
        summary.attach(&item, column as i32, 0, 1, 1);
    }
    page.append(&summary);
    let short = adw::Breakpoint::new(
        adw::BreakpointCondition::parse("max-height: 450px")
            .expect("static short-window condition"),
    );
    short.add_setter(&intro, "visible", Some(&false.to_value()));
    short.add_setter(&description, "visible", Some(&false.to_value()));
    short.add_setter(&summary, "visible", Some(&false.to_value()));
    let profile_actions = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .tooltip_text(crate::i18n::tr("Enable or disable all servers"))
        .build();
    profile_actions.add_css_class("flat");
    let profile_popover = toolport_menu_popover();
    let profile_action_list = gtk::Box::new(gtk::Orientation::Vertical, 4);
    profile_action_list.set_margin_top(6);
    profile_action_list.set_margin_bottom(6);
    profile_action_list.set_margin_start(6);
    profile_action_list.set_margin_end(6);
    let enable_all = toolport_menu_source_button(crate::i18n::msg("Enable all reviewed servers"));
    profile_action_list.append(&enable_all);
    let disable_all = toolport_menu_source_button(crate::i18n::msg("Disable all servers"));
    profile_action_list.append(&disable_all);
    profile_popover.set_child(Some(&profile_action_list));
    profile_actions.set_popover(Some(&profile_popover));
    header.pack_end(&profile_actions);

    let section_title = {
        let widget = gtk::Label::builder()
            .label(crate::i18n::tr("Servers"))
            .halign(gtk::Align::Start)
            .css_classes(["heading"])
            .build();
        l10n::bind_label(&widget, "Servers");
        widget
    };
    page.append(&section_title);

    let posture = gtk::Label::builder()
        .halign(gtk::Align::Fill)
        .xalign(0.0)
        .wrap(true)
        .visible(false)
        .css_classes(["toolport-muted"])
        .build();
    page.append(&posture);

    let search = {
        let widget = gtk::SearchEntry::builder()
            .placeholder_text(crate::i18n::tr("Search servers"))
            .hexpand(true)
            .css_classes(["toolport-search"])
            .build();
        l10n::bind_placeholder(&widget, "Search servers");
        widget
    };
    page.append(&search);

    let list = gtk::Box::new(gtk::Orientation::Vertical, 12);
    page.append(&list);

    scroller.set_child(Some(&page));
    let adaptive = adw::BreakpointBin::builder()
        .child(&scroller)
        .width_request(200)
        .height_request(200)
        .vexpand(true)
        .build();
    adaptive.add_breakpoint(short);
    root.append(&adaptive);
    let server_page = (
        root,
        ServerPage {
            app: app.clone(),
            share_requests: Default::default(),
            server_count: values.remove(0),
            enabled_count: values.remove(0),
            section_title,
            posture,
            search: search.clone(),
            feedback,
            list,
            last_snapshot: std::rc::Rc::new(std::cell::RefCell::new(None)),
            feedback_timer: std::rc::Rc::new(std::cell::RefCell::new(None)),
            health_rows: std::rc::Rc::new(
                std::cell::RefCell::new(std::collections::HashMap::new()),
            ),
            rows: std::rc::Rc::new(std::cell::RefCell::new(Vec::new())),
            no_matches: std::rc::Rc::new(std::cell::RefCell::new(None)),
            off_heading: std::rc::Rc::new(std::cell::RefCell::new(None)),
            health: std::rc::Rc::new(std::cell::RefCell::new(health::HealthCache::default())),
        },
    );
    let page_for_check = server_page.1.clone();
    check_servers
        .connect_clicked(move |_| page_for_check.probe_servers(health::ProbeReason::Refresh));
    let page_for_add = server_page.1.clone();
    add_server.connect_clicked(move |_| open_server_editor(None, page_for_add.clone()));
    let add_action = gtk::gio::SimpleAction::new("add-server", None);
    let page_for_action = server_page.1.clone();
    add_action.connect_activate(move |_, _| open_server_editor(None, page_for_action.clone()));
    app.add_action(&add_action);
    app.set_accels_for_action("app.add-server", &["<Primary>n"]);
    let page_for_search = server_page.1.clone();
    search.connect_search_changed(move |_| page_for_search.apply_filter());
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
            page_for_share
                .show_feedback(&crate::i18n::tr("The shared setup link was invalid."), true);
            return;
        };
        open_shared_setup(url, page_for_share.clone());
    });
    app.add_action(&open_share_action);
    let page_for_enable_all = server_page.1.clone();
    enable_all.connect_clicked(move |_| {
        let Some(snapshot) = page_for_enable_all.last_snapshot.borrow().clone() else {
            return;
        };
        let profile_id = snapshot.active_profile_id;
        run_profile_mutation(
            page_for_enable_all.clone(),
            crate::i18n::msg("Enabled reviewed servers"),
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
            crate::i18n::msg("Disabled all servers"),
            move || crate::registry_controller::set_all_enabled(&profile_id, false),
        );
    });
    (server_page.0, server_page.1, approval_page)
}

fn open_shared_setup(url: &str, page: ServerPage) {
    let generation = page.share_requests.begin();
    if let Some((origin, team)) = crate::teams::parse_pair_link(url) {
        gtk::glib::spawn_future_local(async move {
            let current = read_pair_target(origin.clone(), team.clone()).await;
            if !page.share_requests.is_current(generation) {
                return;
            }
            let current = match current {
                Ok(current) => current,
                Err(error) => {
                    page.show_feedback(&error, true);
                    return;
                }
            };
            if current {
                if let Some(action) = page.app.lookup_action("show-teams") {
                    action.activate(None);
                }
                if let Some(window) = page.app.active_window() {
                    window.present();
                }
                return;
            }
            request_pairing(origin, team, page);
        });
        return;
    }
    let Some(id) = crate::sharing_controller::parse_share_url(url) else {
        page.show_feedback(&crate::i18n::tr("The shared setup link was invalid."), true);
        return;
    };
    page.show_feedback(&crate::i18n::tr("Fetching shared setup for review…"), false);
    gtk::glib::spawn_future_local(async move {
        let result = gtk::gio::spawn_blocking(move || {
            let json = crate::sharing_controller::fetch_shared_setup(&id)?;
            let preview = crate::sharing_controller::preview_import(&json)?;
            Ok::<_, String>((json, preview))
        })
        .await;
        if !page.share_requests.is_current(generation) {
            return;
        }
        match result {
            Ok(Ok((json, preview))) => show_setup_import_review(json, preview, page),
            Ok(Err(error)) => page.show_feedback(
                &crate::i18n::trf(
                    "Could not open shared setup: {value0}",
                    &[("value0", &error.to_string())],
                ),
                true,
            ),
            Err(_) => page.show_feedback(
                &crate::i18n::tr("The shared setup fetch stopped unexpectedly."),
                true,
            ),
        }
    });
}

fn request_pairing(origin: String, team: String, page: ServerPage) {
    for window in page.app.windows() {
        if window.widget_name() == "toolport-onboarding" {
            window.close();
        }
    }
    let (parent_app, connected_app, feedback) = (page.app.clone(), page.app.clone(), page.clone());
    let hooks = pairing::PairingHooks {
        parent: Box::new(move || parent_app.active_window()),
        feedback: Box::new(move |message, error| feedback.show_feedback(message, error)),
        connected: Box::new(move || {
            if let Some(action) = connected_app.lookup_action("show-teams") {
                action.activate(None);
            }
            if let Some(window) = connected_app.active_window() {
                window.present();
            }
        }),
        open_url: Box::new(|url| {
            let _ = crate::oauth::open_web_url(url);
        }),
    };
    let pair_origin = origin.clone();
    pairing::request(
        hooks,
        &origin,
        Box::new(move |cancel, show| {
            crate::teams::pair_device(&pair_origin, &team, cancel, show).map(|_| ())
        }),
    );
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
    let window = {
        let widget = adw::Window::builder()
            .application(&page.app)
            .transient_for(&parent)
            .modal(true)
            .title(crate::i18n::tr("Share this setup"))
            .default_width(640)
            .default_height(640)
            .build();
        l10n::bind_window_title(&widget, "Share this setup");
        widget
    };
    window.add_css_class("toolport-editor");
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = adw::HeaderBar::new();
    let done = l10n::button("Done");
    done.add_css_class("toolport-secondary-action");
    header.pack_end(&done);
    root.append(&header);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.add_css_class("toolport-editor-body");
    body.append(&editor_intro("send-to-symbolic", crate::i18n::msg("Share a secret-free setup"), crate::i18n::msg("Keychain values, inline credentials, and the local gateway are always excluded. Share links expire after 90 days.")));
    let feedback = gtk::Label::builder()
        .halign(gtk::Align::Fill)
        .xalign(0.0)
        .wrap(true)
        .visible(false)
        .css_classes(["toolport-feedback"])
        .build();
    body.append(&feedback);
    let name = {
        let widget = gtk::Entry::builder()
            .placeholder_text(crate::i18n::tr("Setup name (optional)"))
            .css_classes(["toolport-input"])
            .build();
        l10n::bind_placeholder(&widget, "Setup name (optional)");
        widget
    };
    body.append(&editor_field(crate::i18n::msg("Name"), &name));
    let description = {
        let widget = gtk::Entry::builder()
            .placeholder_text(crate::i18n::tr("What this setup is for (optional)"))
            .css_classes(["toolport-input"])
            .build();
        l10n::bind_placeholder(&widget, "What this setup is for (optional)");
        widget
    };
    body.append(&editor_field(crate::i18n::msg("Description"), &description));
    body.append(&section_heading(
        crate::i18n::msg("Servers to include"),
        crate::i18n::msg("Everything is included unless you uncheck it."),
    ));
    let select_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let select_all = l10n::button("All");
    select_all.add_css_class("flat");
    let select_none = l10n::button("None");
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
    let copy_json = l10n::button("Copy JSON");
    copy_json.add_css_class("toolport-secondary-action");
    let save_file = l10n::button("Save to file…");
    save_file.add_css_class("toolport-secondary-action");
    let copy_link = l10n::button("Copy share link");
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
        let feedback = feedback.clone();
        copy_link.connect_clicked(move |button| {
            let (name, description, selected) = collect();
            if selected.is_empty() {
                show(
                    &feedback,
                    &crate::i18n::tr("Select at least one server to share."),
                    true,
                );
                return;
            }
            button.set_sensitive(false);
            show(
                &feedback,
                &crate::i18n::tr("Creating a secret-free share link…"),
                false,
            );
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
                            show(
                                &feedback,
                                &crate::i18n::tr("Copied the secret-free share link."),
                                false,
                            );
                        } else {
                            show(
                                &feedback,
                                &crate::i18n::tr("Could not access the desktop clipboard."),
                                true,
                            );
                        }
                    }
                    Ok(Err(error)) => show(
                        &feedback,
                        &crate::i18n::trf(
                            "Could not create the share link: {value0}",
                            &[("value0", &error.to_string())],
                        ),
                        true,
                    ),
                    Err(_) => show(
                        &feedback,
                        &crate::i18n::tr("The sharing task stopped unexpectedly."),
                        true,
                    ),
                }
            });
        });
    }
    {
        let collect = collect.clone();
        let feedback = feedback.clone();
        copy_json.connect_clicked(move |_| {
            let (name, description, selected) = collect();
            if selected.is_empty() {
                show(
                    &feedback,
                    &crate::i18n::tr("Select at least one server to export."),
                    true,
                );
                return;
            }
            match export(name, description, selected) {
                Ok(json) => {
                    if let Some(display) = gtk::gdk::Display::default() {
                        display.clipboard().set_text(&json);
                        show(
                            &feedback,
                            &crate::i18n::tr("Copied the setup JSON to the clipboard."),
                            false,
                        );
                    }
                }
                Err(error) => show(
                    &feedback,
                    &crate::i18n::trf("Export failed: {value0}", &[("value0", &error.to_string())]),
                    true,
                ),
            }
        });
    }
    {
        let window_for_save = window.clone();
        let feedback = feedback.clone();
        save_file.connect_clicked(move |_| {
            let (name, description, selected) = collect();
            if selected.is_empty() {
                show(
                    &feedback,
                    &crate::i18n::tr("Select at least one server to export."),
                    true,
                );
                return;
            }
            let dialog = gtk::FileDialog::builder()
                .title(crate::i18n::tr("Export Toolport setup"))
                .initial_name("toolport-setup.json")
                .accept_label(crate::i18n::tr("Export"))
                .modal(true)
                .build();
            set_file_dialog_folder(&dialog);
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
                            &crate::i18n::tr("The selected destination is not a local file."),
                            true,
                        );
                        return;
                    };
                    let outcome = export(name.clone(), description.clone(), selected.clone())
                        .and_then(|json| crate::sharing_controller::write_setup_file(&path, &json));
                    match outcome {
                        Ok(()) => show(
                            &feedback,
                            &crate::i18n::tr(
                                "Exported the setup without keychain values or inline credentials.",
                            ),
                            false,
                        ),
                        Err(error) => show(
                            &feedback,
                            &crate::i18n::trf(
                                "Export failed: {value0}",
                                &[("value0", &error.to_string())],
                            ),
                            true,
                        ),
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
    let window = {
        let widget = adw::Window::builder()
            .application(&page.app)
            .transient_for(&parent)
            .modal(true)
            .title(crate::i18n::tr("Import pasted setup"))
            .default_width(640)
            .default_height(480)
            .build();
        l10n::bind_window_title(&widget, "Import pasted setup");
        widget
    };
    window.add_css_class("toolport-editor");
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = adw::HeaderBar::new();
    let cancel = l10n::button("Cancel");
    cancel.add_css_class("toolport-secondary-action");
    let review = l10n::button("Review and import");
    review.add_css_class("suggested-action");
    header.pack_start(&cancel);
    header.pack_end(&review);
    root.append(&header);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.add_css_class("toolport-editor-body");
    body.append(&editor_intro(
        "edit-paste-symbolic",
        crate::i18n::msg("Paste a Toolport setup"),
        crate::i18n::msg(
            "Paste the JSON someone exported. Nothing is imported until you review it.",
        ),
    ));
    let feedback = gtk::Label::builder()
        .halign(gtk::Align::Fill)
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
            l10n::bind_text(&feedback, "That is too large to be a Toolport setup.");
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
                    l10n::bind_text(&feedback, "The import review stopped unexpectedly.");
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
        .title(crate::i18n::tr("Export Toolport setup"))
        .initial_name("toolport-setup.json")
        .accept_label(crate::i18n::tr("Export"))
        .modal(true)
        .build();
    dialog.save(Some(&parent), gtk::gio::Cancellable::NONE, move |result| {
        let Ok(file) = result else {
            return;
        };
        let Some(path) = file.path() else {
            page.show_feedback(
                &crate::i18n::tr("The selected destination is not a local file."),
                true,
            );
            return;
        };
        page.show_feedback(&crate::i18n::tr("Exporting a secret-free setup…"), false);
        let page = page.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                let json = crate::sharing_controller::export_json(None, None, None)?;
                crate::sharing_controller::write_setup_file(&path, &json)
            })
            .await;
            match result {
                Ok(Ok(())) => page.show_confirmation(&crate::i18n::tr(
                    "Exported the setup without keychain values or inline credentials.",
                )),
                Ok(Err(error)) => page.show_feedback(
                    &crate::i18n::trf("Export failed: {value0}", &[("value0", &error.to_string())]),
                    true,
                ),
                Err(_) => {
                    page.show_feedback(&crate::i18n::tr("The export stopped unexpectedly."), true)
                }
            }
        });
    });
}

fn set_file_dialog_folder(dialog: &gtk::FileDialog) {
    if let Some(folder) = dirs::download_dir()
        .filter(|folder| folder.is_dir())
        .or_else(dirs::home_dir)
    {
        dialog.set_initial_folder(Some(&gtk::gio::File::for_path(folder)));
    }
}

fn choose_setup_import(page: ServerPage) {
    let Some(parent) = page.app.active_window() else {
        return;
    };
    let dialog = gtk::FileDialog::builder()
        .title(crate::i18n::tr("Import Toolport setup"))
        .accept_label(crate::i18n::tr("Review"))
        .modal(true)
        .build();
    dialog.open(Some(&parent), gtk::gio::Cancellable::NONE, move |result| {
        let Ok(file) = result else {
            return;
        };
        let Some(path) = file.path() else {
            page.show_feedback(
                &crate::i18n::tr("The selected setup is not a local file."),
                true,
            );
            return;
        };
        page.show_feedback(&crate::i18n::tr("Reading setup for review…"), false);
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
                Ok(Err(error)) => page.show_feedback(
                    &crate::i18n::trf("Import failed: {value0}", &[("value0", &error.to_string())]),
                    true,
                ),
                Err(_) => page.show_feedback(
                    &crate::i18n::tr("The import review stopped unexpectedly."),
                    true,
                ),
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
    let dialog = adw::MessageDialog::new(Some(&parent), Some(&crate::i18n::tr("Import this Toolport setup?")), Some(&crate::i18n::tr("Review every launch target below. Existing server names are skipped, and imported servers remain disabled until you enable them.")));
    l10n::bind_dialog_heading(&dialog, "Import this Toolport setup?");
    l10n::bind_dialog_body(&dialog, "Review every launch target below. Existing server names are skipped, and imported servers remain disabled until you enable them.");
    dialog.add_response("cancel", &crate::i18n::tr("Cancel"));
    l10n::bind_dialog_response(&dialog, "cancel", "Cancel");
    dialog.add_response(
        "import",
        &crate::i18n::trf("Import {value0}", &[("value0", &format!("{}", new_count))]),
    );
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
                    crate::i18n::trf(
                        "{value0} · already present",
                        &[("value0", &item.name.to_string())],
                    )
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
                .halign(gtk::Align::Fill)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["toolport-muted"])
                .build(),
        );
        for line in &item.reference_review {
            row.append(
                &gtk::Label::builder()
                    .label(line)
                    .xalign(0.0)
                    .wrap(true)
                    .build(),
            );
        }
        // The two facts that change the risk calculus, stated on the row that
        // carries them - not buried in the dialog preamble.
        for warning in crate::sharing_controller::import_item_warnings(&item) {
            row.append(
                &gtk::Label::builder()
                    .label(warning)
                    .halign(gtk::Align::Fill)
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
                page.show_feedback(&crate::i18n::tr("Nothing selected; nothing was imported."), true);
                dialog.close();
                return;
            }
            page.show_feedback(&crate::i18n::tr("Importing reviewed servers…"), false);
            let json = json.clone();
            let page = page.clone();
            gtk::glib::spawn_future_local(async move {
                let result = gtk::gio::spawn_blocking(move || {
                    crate::sharing_controller::import_json_selected(&json, &selected)
                })
                .await;
                match result {
                    Ok(Ok((_, added))) => page.show_confirmation(&crate::i18n::trf(if added == 1 { crate::i18n::msg("Imported {value0} server. Review credentials before enabling them.") } else { crate::i18n::msg("Imported {value0} servers. Review credentials before enabling them.") }, &[("value0", &format!("{}", added))])),
                    Ok(Err(error)) => page.show_feedback(&crate::i18n::trf("Import failed: {value0}", &[("value0", &error.to_string())]), true),
                    Err(_) => page.show_feedback(&crate::i18n::tr("The import stopped unexpectedly."), true),
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
            if !args.is_empty() {
                format!(" {args}")
            } else {
                Default::default()
            }
        );
    }
    format!(
        "{} · {}",
        item.transport,
        item.url
            .as_deref()
            .map(crate::registry::redact_url_userinfo)
            .unwrap_or_else(|| crate::i18n::tr("No launch target").to_string())
    )
}

fn run_profile_mutation(
    page: ServerPage,
    success: &'static str,
    operation: impl FnOnce() -> Result<crate::registry::Registry, String> + Send + 'static,
) {
    gtk::glib::spawn_future_local(async move {
        let result = gtk::gio::spawn_blocking(operation).await;
        match result {
            Ok(Ok(registry)) => {
                page.render(state::RegistryState::Ready(
                    state::RegistrySnapshot::from_registry(registry),
                ));
                page.show_confirmation(&crate::i18n::lookup(success));
            }
            Ok(Err(error)) => page.restore_after_error(&crate::i18n::trf(
                "Server update failed: {value0}",
                &[("value0", &error.to_string())],
            )),
            Err(_) => {
                page.restore_after_error(&crate::i18n::tr("Server update stopped unexpectedly"))
            }
        }
    });
}

fn focus_path(widget: &gtk::Widget, root: &gtk::Widget) -> Vec<usize> {
    let mut current = widget.clone();
    let mut path = Vec::new();
    while &current != root {
        let Some(parent) = current.parent() else {
            break;
        };
        let mut child = parent.first_child();
        let mut index = 0;
        while let Some(w) = child {
            if w == current {
                break;
            }
            child = w.next_sibling();
            index += 1;
        }
        path.push(index);
        current = parent;
    }
    path.reverse();
    path
}
fn follow_focus_path(root: &gtk::Widget, path: &[usize]) -> Option<gtk::Widget> {
    let mut current = root.clone();
    for index in path {
        let mut child = current.first_child()?;
        for _ in 0..*index {
            child = child.next_sibling()?;
        }
        current = child;
    }
    Some(current)
}
fn set_nav_label(button: &gtk::Button, text: &str) {
    if let Some(label) = button
        .child()
        .and_then(|row| row.first_child())
        .and_then(|icon| icon.next_sibling())
        .and_then(|w| w.downcast::<gtk::Label>().ok())
    {
        label.set_label(text);
    }
}
fn open_popover(widget: &gtk::Widget) -> Option<gtk::Popover> {
    if let Ok(menu) = widget.clone().downcast::<gtk::MenuButton>() {
        if let Some(popover) = menu.popover().filter(|p| p.is_visible()) {
            return Some(popover);
        }
    }
    let mut child = widget.first_child();
    while let Some(w) = child {
        if let Some(p) = open_popover(&w) {
            return Some(p);
        }
        child = w.next_sibling();
    }
    None
}
fn server_matches_query(server: &state::ServerView, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty()
        || server.name.to_lowercase().contains(&query)
        || server.transport.to_lowercase().contains(&query)
}

fn summary_item(value: &str, label: &'static str) -> (gtk::Box, gtk::Label) {
    let item = gtk::Box::new(gtk::Orientation::Vertical, 2);
    item.set_size_request(150, -1);
    item.add_css_class("toolport-summary-item");
    let value = gtk::Label::builder()
        .label(value)
        .halign(gtk::Align::Start)
        .css_classes(["title-3"])
        .build();
    item.append(&value);
    item.append(&{
        let widget = gtk::Label::builder()
            .label(label)
            .halign(gtk::Align::Start)
            .css_classes(["caption", "toolport-muted"])
            .build();
        l10n::bind_label(&widget, label);
        widget
    });
    (item, value)
}

/// Window size persisted across launches. Position, stacking, and maximized
/// state belong to the compositor. Restoring a stale maximized flag made
/// Hyprland sometimes reuse the previous tile geometry in a different layout.
#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct WindowState {
    width: i32,
    height: i32,
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

fn save_window_state(state: WindowState) {
    let Some(path) = window_state_path() else {
        return;
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
fn mark_tray_hint_shown() -> bool {
    let Some(dir) = crate::registry::conduit_dir() else {
        return false;
    };
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dir.join(".tray-hint-shown"))
        .is_ok()
}

fn show_tray_hint(app: &adw::Application) {
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
    let (title, body) = crate::approval_broker::localized_approval_notification(view);
    (title.to_string(), body)
}

fn approval_queue_notification(
    pending: &[crate::approval_broker::PendingView],
) -> Option<(String, String)> {
    let first = pending.first()?;
    let (title, mut body) = approval_notification(first);
    if pending.len() == 1 {
        return Some((title, body));
    }
    let remaining = pending.len() - 1;
    body.push_str(&crate::i18n::trf(
        if remaining == 1 {
            crate::i18n::msg(" {value0} more is waiting.")
        } else {
            crate::i18n::msg(" {value0} more are waiting.")
        },
        &[("value0", &format!("{}", remaining))],
    ));
    Some((
        crate::i18n::trf(
            "Toolport: {value0} approvals required",
            &[("value0", &format!("{}", pending.len()))],
        ),
        body,
    ))
}

fn probe_auth_action(probe: &crate::server_runtime::ProbeResult) -> &'static str {
    if probe.auth_target == Some(crate::call_failure::AuthTarget::ServiceCredential) {
        crate::i18n::msg("Edit service key")
    } else {
        crate::i18n::msg("Sign in")
    }
}

/// One row's health line and badge class from a finished probe.
fn probe_status_line(probe: &crate::server_runtime::ProbeResult) -> (String, &'static str) {
    if probe.ok {
        (
            crate::i18n::trf(
                if probe.tool_count == 1 {
                    crate::i18n::msg("Ready · {value0} tool")
                } else {
                    crate::i18n::msg("Ready · {value0} tools")
                },
                &[("value0", &format!("{}", probe.tool_count))],
            ),
            "success",
        )
    } else {
        (
            probe.failure_label().to_string(),
            if probe.auth_required {
                "review"
            } else {
                "error"
            },
        )
    }
}

/// The one-line posture summary above the server list.
fn posture_line(ready: usize, auth: usize, errors: usize, pending: usize, total: usize) -> String {
    if pending > 0 {
        return crate::i18n::trf(
            if total == 1 {
                crate::i18n::msg("Checking {value0} of {value1} enabled server…")
            } else {
                crate::i18n::msg("Checking {value0} of {value1} enabled servers…")
            },
            &[
                ("value0", &format!("{}", pending)),
                ("value1", &format!("{}", total)),
            ],
        );
    }
    if auth == 0 && errors == 0 {
        return crate::i18n::trf(
            if total == 1 {
                crate::i18n::msg("All {value0} enabled server is ready.")
            } else {
                crate::i18n::msg("All {value0} enabled servers are ready.")
            },
            &[("value0", &format!("{}", total))],
        );
    }
    let mut parts = vec![crate::i18n::trf(
        "{value0} ready",
        &[("value0", &format!("{}", ready))],
    )];
    if auth > 0 {
        parts.push(crate::i18n::trf(
            if auth == 1 {
                crate::i18n::msg("{value0} needs sign-in")
            } else {
                crate::i18n::msg("{value0} need sign-in")
            },
            &[("value0", &format!("{}", auth))],
        ));
    }
    if errors > 0 {
        parts.push(crate::i18n::trf(
            "{value0} failing",
            &[("value0", &format!("{}", errors))],
        ));
    }
    parts.join(" · ")
}

fn server_groups(
    servers: &[state::ServerView],
) -> (Vec<&state::ServerView>, Vec<&state::ServerView>) {
    let mut servers = servers.iter().collect::<Vec<_>>();
    servers.sort_by_key(|server| server_order_key(server));
    servers.into_iter().partition(|server| server.enabled)
}

/// Keep alphabetical order within each group regardless of review or probe state.
fn server_order_key(server: &state::ServerView) -> (String, String) {
    (server.name.to_lowercase(), server.id.clone())
}

fn bounded_title(name: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(name)
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(24)
        .tooltip_text(name)
        .build()
}

fn open_server_details(server: &state::ServerView, page: &ServerPage, show_tools: bool) {
    let parent = page.app.active_window();
    let window = adw::Window::builder()
        .application(&page.app)
        .title(&server.name)
        .default_width(1000)
        .default_height(760)
        .modal(true)
        .build();
    window.set_transient_for(parent.as_ref());
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&bounded_title(&server.name)));
    root.append(&header);
    let stack = gtk::Stack::new();
    stack.set_vexpand(true);
    let overview = gtk::Box::new(gtk::Orientation::Vertical, 12);
    overview.set_margin_top(20);
    overview.set_margin_start(20);
    overview.set_margin_end(20);
    for text in [&server.name, &server.origin_label, &server.transport] {
        overview.append(
            &gtk::Label::builder()
                .label(text)
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .max_width_chars(24)
                .tooltip_text(text)
                .build(),
        );
    }
    let tools = ServerToolsPanel::new(&page.app, &server.id);
    l10n::bind_stack_page_title(
        &stack.add_titled(&overview, Some("overview"), &crate::i18n::tr("Overview")),
        "Overview",
    );
    l10n::bind_stack_page_title(
        &stack.add_titled(&tools.root, Some("tools"), &crate::i18n::tr("Tools")),
        "Tools",
    );
    let switcher = gtk::StackSwitcher::new();
    switcher.set_stack(Some(&stack));
    switcher.set_halign(gtk::Align::Center);
    root.append(&switcher);
    root.append(&stack);
    let tools_for_tab = tools.clone();
    stack.connect_visible_child_name_notify(move |stack| {
        if stack.visible_child_name().as_deref() == Some("tools") {
            tools_for_tab.refresh();
        }
    });
    if show_tools {
        stack.set_visible_child_name("tools");
    }
    window.set_content(Some(&root));
    window.present();
}

fn server_card(server: &state::ServerView, profile_id: &str, page: ServerPage) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    card.set_widget_name(&format!("toolport-profile:{profile_id}"));
    card.add_css_class("toolport-card");
    card.set_margin_top(1);
    card.set_margin_bottom(1);

    let icon = branding::server_logo(&server.name, &server.transport);
    card.append(&icon);

    let text = gtk::Box::new(gtk::Orientation::Vertical, 3);
    text.set_hexpand(true);
    let details = gtk::Button::builder()
        .child(&bounded_title(&server.name))
        .halign(gtk::Align::Fill)
        .css_classes(["flat", "heading", "toolport-card-title"])
        .tooltip_text(&server.name)
        .build();
    let server_for_details = server.clone();
    let page_for_details = page.clone();
    details.connect_clicked(move |_| {
        open_server_details(&server_for_details, &page_for_details, false)
    });
    text.append(&details);
    text.append(
        &gtk::Label::builder()
            .label(&server.origin_label)
            .halign(gtk::Align::Start)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["toolport-muted"])
            .build(),
    );
    // Transport and health share one line: a card per server is already the
    // densest thing on the page, and a third stacked line made each row read as
    // a paragraph.
    let health = gtk::Label::builder()
        .label(if server.enabled && !server.requires_review {
            crate::i18n::trf(
                "{value0} · Checking…",
                &[("value0", &server.transport.to_string())],
            )
        } else {
            server.transport.clone()
        })
        .halign(gtk::Align::Start)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["toolport-muted"])
        .build();
    text.append(&health);
    if server.team_route_removed {
        text.append(&{
            let widget = gtk::Label::builder()
                .label(crate::i18n::tr(
                    "Removed or disabled by the team. Your personal server stays off.",
                ))
                .halign(gtk::Align::Start)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["toolport-muted"])
                .build();
            l10n::bind_label(
                &widget,
                "Removed or disabled by the team. Your personal server stays off.",
            );
            widget
        });
    }
    if !server.secret_references.is_empty() {
        let providers: std::collections::BTreeSet<_> = server
            .secret_references
            .values()
            .filter_map(|r| crate::secret_refs::parse(r).ok().map(|p| p.name))
            .collect();
        text.append(&{
            let widget = gtk::Label::builder()
                .label("")
                .halign(gtk::Align::Start)
                .css_classes(["toolport-muted"])
                .build();
            l10n::bind_formatted_label(
                &widget,
                "Keys from {value0}",
                &[(
                    "value0",
                    &providers
                        .into_iter()
                        .collect::<Vec<_>>()
                        .join(", ")
                        .to_string(),
                )],
            );
            widget
        });
    }
    card.append(&text);

    let authenticate = l10n::button("Sign in");
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
    let copy_error = {
        let widget = gtk::Button::builder()
            .label(crate::i18n::tr("View log"))
            .tooltip_text("View the connection error and server output")
            .valign(gtk::Align::Center)
            .visible(false)
            .css_classes(["flat"])
            .build();
        l10n::bind_button(&widget, "View log");
        l10n::bind_tooltip(&widget, "View the connection error and server output");
        widget
    };
    {
        let health_for_log = health.clone();
        let app = page.app.clone();
        copy_error.connect_clicked(move |_| {
            let Some(error) = health_for_log.tooltip_text() else {
                return;
            };
            let window = {
                let widget = adw::Window::builder()
                    .application(&app)
                    .title(crate::i18n::tr("Connection log"))
                    .default_width(640)
                    .default_height(360)
                    .modal(true)
                    .build();
                l10n::bind_window_title(&widget, "Connection log");
                widget
            };
            window.set_transient_for(app.active_window().as_ref());
            let view = gtk::TextView::builder()
                .editable(false)
                .cursor_visible(false)
                .monospace(true)
                .wrap_mode(gtk::WrapMode::WordChar)
                .top_margin(16)
                .bottom_margin(16)
                .left_margin(16)
                .right_margin(16)
                .build();
            view.buffer().set_text(&error);
            let scroller = gtk::ScrolledWindow::builder()
                .child(&view)
                .vexpand(true)
                .build();
            let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
            root.append(&adw::HeaderBar::new());
            root.append(&scroller);
            window.set_content(Some(&root));
            window.present();
        });
    }
    let retry = {
        let widget = gtk::Button::builder()
            .label(crate::i18n::tr("Retry"))
            .tooltip_text(crate::i18n::tr("Retry"))
            .visible(false)
            .css_classes(["flat"])
            .build();
        l10n::bind_button(&widget, "Retry");
        l10n::bind_tooltip(&widget, "Retry");
        widget
    };
    let page_for_retry = page.clone();
    let retry_id = server.id.clone();
    retry.connect_clicked(move |_| page_for_retry.reprobe_after_auth_change(&retry_id));
    // Put recovery actions below the health line. Keeping them in the card's
    // horizontal row leaves too little room for the status in narrow windows.
    let health_actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    health_actions.set_halign(gtk::Align::Start);
    health_actions.append(&authenticate);
    health_actions.append(&retry);
    health_actions.append(&copy_error);
    text.append(&health_actions);
    if server.enabled && !server.requires_review {
        let mut all_rows = page.health_rows.borrow_mut();
        let rows = all_rows.entry(server.id.clone()).or_default();
        rows.retain(|row| row.label.root().is_some());
        rows.push(HealthRow {
            label: health.clone(),
            transport: server.transport.clone(),
            authenticate: authenticate.clone(),
            copy_error: copy_error.clone(),
            retry: retry.clone(),
        });
        drop(all_rows);
        // Show the last known result immediately unless a probe is replacing it.
        let cache = page.health.borrow();
        if let Some(previous) = cache
            .result(&server.id)
            .filter(|_| !cache.is_checking(&server.id))
        {
            let (status, class) = probe_status_line(previous);
            health.set_label(&format!(
                "{} · {status}",
                state::display_transport(&server.transport)
            ));
            health.add_css_class(class);
            authenticate.set_label(&crate::i18n::lookup(probe_auth_action(previous)));
            authenticate.set_visible(previous.auth_required);
            copy_error.set_visible(!previous.ok && previous.error.is_some());
            health.set_tooltip_text(previous.error.as_deref());
            retry.set_visible(!previous.ok && !previous.auth_required);
        }
    }

    if server.requires_review {
        let personal = page
            .last_snapshot
            .borrow()
            .as_ref()
            .is_some_and(|snapshot| snapshot.personal_sync);
        // It looks like a link, so it acts like one: open the page where the
        // review happens.
        let badge = l10n::button(if personal {
            crate::i18n::msg("Review in Sync")
        } else {
            crate::i18n::msg("Review in Teams")
        });
        badge.add_css_class("toolport-badge");
        badge.add_css_class("flat");
        badge.set_valign(gtk::Align::Center);
        badge.set_halign(gtk::Align::Start);
        badge.add_css_class("review");
        badge.set_tooltip_text(Some(
            "Review the exact command and credential references before enabling this server",
        ));
        let app = page.app.clone();
        badge.connect_clicked(move |_| {
            if let Some(action) = app.lookup_action("show-teams") {
                action.activate(None);
            }
        });
        // A server synced from another machine is reviewed right here; only
        // team servers still go to the Teams page.
        badge.set_visible(!personal);
        card.append(&badge);
        let review = l10n::button(if personal {
            crate::i18n::msg("Review and enable")
        } else {
            crate::i18n::msg("Review references")
        });
        review.set_visible(personal || !server.secret_references.is_empty());
        if personal {
            review.add_css_class("suggested-action");
        }
        review.set_valign(gtk::Align::Center);
        let page = page.clone();
        let id = server.id.clone();
        let profile = profile_id.to_string();
        let view = server.clone();
        review.connect_clicked(move |_| {
            let Ok(reg) = crate::registry::load() else {
                return;
            };
            let Some(entry) = reg.servers.iter().find(|s| s.id == id).cloned() else {
                return;
            };
            if !personal && !crate::secret_refs::has_references(&entry) {
                return;
            }
            let Some(parent) = page.app.active_window() else {
                return;
            };
            let dialog = adw::MessageDialog::new(
                Some(&parent),
                Some(&if personal {
                    crate::i18n::trf("Enable {value0}?", &[("value0", &entry.name.to_string())])
                } else {
                    "Approve references and enable?".into()
                }),
                Some(if personal {
                    "This server arrived from another machine. Check what it runs before turning it on here. Keys stay on each machine."
                } else {
                    "Review the definition and saved authentication. Enable only a setup you trust."
                }),
            );
            if personal {
                l10n::bind_formatted_dialog_heading(&dialog, "Enable {value0}?", &[("value0", &entry.name)]);
            } else {
                l10n::bind_dialog_heading(&dialog, "Approve references and enable?");
            }
            l10n::bind_dialog_body(&dialog, if personal {
                crate::i18n::msg("This server arrived from another machine. Check what it runs before turning it on here. Keys stay on each machine.")
            } else {
                crate::i18n::msg("Review the definition and saved authentication. Enable only a setup you trust.")
            });
            dialog.set_size_request(620, -1);
            dialog.set_extra_child(Some(&teams::execution_review_scroll(&entry)));
            dialog.add_response("cancel", &crate::i18n::tr("Cancel"));
l10n::bind_dialog_response(&dialog, "cancel", "Cancel");
            dialog.add_response("enable", &crate::i18n::tr("Enable"));
l10n::bind_dialog_response(&dialog, "enable", "Enable");
            dialog.set_default_response(Some("cancel"));
            let profile = profile.clone();
            let page = page.clone();
            let view = view.clone();
            dialog.connect_response(None, move |_, response| {
                if response != "enable" {
                    return;
                }
                let profile = profile.clone();
                let entry = entry.clone();
                let page = page.clone();
                let view = view.clone();
                gtk::glib::spawn_future_local(async move {
                    let result = gtk::gio::spawn_blocking(move || crate::registry_controller::set_server_enabled_after_reference_review(&profile, &entry)).await;
                    match result {
                        Ok(Ok(reg)) => page.render(state::RegistryState::Ready(state::RegistrySnapshot::from_registry(reg))),
                        // A key this machine still needs: open where it goes.
                        Ok(Err(e)) => match missing_value_editor(&e, &view.secret_keys) {
                            Some(editor) => {
                                page.show_feedback(
                                    &crate::i18n::trf("Could not turn on {value0}: {value1}", &[("value0", &view.name.to_string()), ("value1", &missing_value_hint(&e, &editor).to_string())]),
                                    true,
                                );
                                match editor {
                                    MissingValueEditor::Credentials => open_credentials_editor(view, page.clone()),
                                    MissingValueEditor::EditServer => open_server_editor(Some(view), page.clone()),
                                }
                            }
                            None => page.show_feedback(&e, true),
                        },
                        Err(_) => page.show_feedback(&crate::i18n::tr("The reference review stopped. Retry enabling the server."), true),
                    }
                });
            });
            dialog.present();
        });
        card.append(&review);
    } else {
        let status = l10n::label(if server.enabled {
            crate::i18n::msg("Enabled")
        } else {
            crate::i18n::msg("Disabled")
        });
        status.add_css_class("toolport-server-state");
        let toggle = gtk::Switch::builder()
            .active(server.enabled)
            .valign(gtk::Align::Center)
            .tooltip_text(crate::i18n::trf(
                if server.enabled {
                    crate::i18n::msg("Disable {server}")
                } else {
                    crate::i18n::msg("Enable {server}")
                },
                &[("server", &server.name)],
            ))
            .build();
        let server_id = server.id.clone();
        let server_name = server.name.clone();
        let profile_id = profile_id.to_string();
        let status_for_toggle = status.clone();
        let page_for_toggle = page.clone();
        let server_for_toggle = server.clone();
        let restoring = std::rc::Rc::new(std::cell::Cell::new(false));
        toggle.connect_state_set(move |toggle, enabled| {
            if restoring.get() {
                return gtk::glib::Propagation::Proceed;
            }
            toggle.set_sensitive(false);
            l10n::bind_text(&status_for_toggle, "Updating");

            let server_id = server_id.clone();
            let server_name = server_name.clone();
            let profile_id = profile_id.clone();
            let page = page_for_toggle.clone();
            let server = server_for_toggle.clone();
            let toggle = toggle.clone();
            let status = status_for_toggle.clone();
            let restoring = restoring.clone();
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
                        page.show_confirmation(&format!(
                            "{} {server_name}",
                            if enabled { "Enabled" } else { "Disabled" }
                        ));
                    }
                    Ok(Err(error)) => {
                        // A refused change leaves the registry as it was, so the
                        // page keeps this card and its switch must be put back here.
                        restore_server_switch(&toggle, &status, &restoring, !enabled);
                        let editor = enabled
                            .then(|| missing_value_editor(&error, &server.secret_keys))
                            .flatten();
                        match editor {
                            Some(editor) => {
                                page.restore_after_error(&crate::i18n::trf(
                                    "Could not turn on {value0}: {value1}",
                                    &[
                                        ("value0", &server_name.to_string()),
                                        (
                                            "value1",
                                            &missing_value_hint(&error, &editor).to_string(),
                                        ),
                                    ],
                                ));
                                match editor {
                                    MissingValueEditor::Credentials => {
                                        open_credentials_editor(server, page.clone())
                                    }
                                    MissingValueEditor::EditServer => {
                                        open_server_editor(Some(server), page.clone())
                                    }
                                }
                            }
                            None => page.restore_after_error(&crate::i18n::trf(
                                "Could not update {value0}: {value1}",
                                &[
                                    ("value0", &server_name.to_string()),
                                    ("value1", &error.to_string()),
                                ],
                            )),
                        }
                    }
                    Err(_) => {
                        restore_server_switch(&toggle, &status, &restoring, !enabled);
                        page.restore_after_error(&crate::i18n::trf(
                            "Could not update {value0}: the operation stopped",
                            &[("value0", &server_name.to_string())],
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
    let test = action_menu_button(
        crate::i18n::msg("Test connection"),
        "network-transmit-receive-symbolic",
    );
    let server_for_test = server.clone();
    let page_for_test = page.clone();
    test.connect_clicked(move |button| {
        test_server_connection(&server_for_test, button, page_for_test.clone());
    });
    actions.append(&test);

    let tools = action_menu_button(
        crate::i18n::msg("Server details and tools"),
        "applications-science-symbolic",
    );
    let server_for_tools = server.clone();
    let page_for_tools = page.clone();
    tools.connect_clicked(move |_| open_server_details(&server_for_tools, &page_for_tools, true));
    actions.append(&tools);

    let edit = action_menu_button(crate::i18n::msg("Edit"), "document-edit-symbolic");
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
    let settings = action_menu_button(
        crate::i18n::msg("Sync settings"),
        "emblem-synchronizing-symbolic",
    );
    let sync_id = server.id.clone();
    let sync_page = page.clone();
    settings.connect_clicked(move |_| open_sync_settings(&sync_id, &sync_page));
    actions.append(&settings);

    let duplicate = action_menu_button(crate::i18n::msg("Duplicate"), "edit-copy-symbolic");
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
        let authentication = action_menu_button(
            crate::i18n::msg("Authentication"),
            "system-lock-screen-symbolic",
        );
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

    let credentials =
        action_menu_button(crate::i18n::msg("Credentials"), "dialog-password-symbolic");
    let server_for_credentials = server.clone();
    let page_for_credentials = page.clone();
    credentials.connect_clicked(move |_| {
        open_credentials_editor(server_for_credentials.clone(), page_for_credentials.clone())
    });
    actions.append(&credentials);

    let remove = action_menu_button(crate::i18n::msg("Remove"), "user-trash-symbolic");
    remove.add_css_class("destructive-action");
    let server_id = server.id.clone();
    let server_name = server.name.clone();
    remove.connect_clicked(move |_| {
        confirm_remove_server(&server_id, &server_name, page.clone());
    });
    actions.append(&remove);
    let popover = gtk::Popover::new();
    popover.add_css_class("toolport-action-menu");
    popover.add_css_class("toolport-menu");
    popover.set_child(Some(&actions));
    let menu = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .tooltip_text(crate::i18n::trf(
            "Actions for {value0}",
            &[("value0", &server.name.to_string())],
        ))
        .popover(&popover)
        .css_classes(["flat"])
        .build();
    card.append(&menu);
    card
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
        summary.push_str(&crate::i18n::trf(
            " Detected {value0}.",
            &[("value0", vendor)],
        ));
    }
    if let Some(instructions) = info.instructions.as_deref() {
        summary.push(' ');
        summary.push_str(instructions);
    }
    if let Some(token_url) = info.token_url.as_deref() {
        summary.push_str(&crate::i18n::trf(
            " Get a token at {value0}",
            &[("value0", token_url)],
        ));
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
        .title(crate::i18n::trf(
            "Authentication for {value0}",
            &[("value0", &server.name.to_string())],
        ))
        .default_width(560)
        .default_height(440)
        .build();
    editor.add_css_class("toolport-editor");

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = adw::HeaderBar::new();
    let done = l10n::button("Done");
    done.add_css_class("toolport-secondary-action");
    header.pack_end(&done);
    root.append(&header);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
    content.add_css_class("toolport-editor-body");
    content.append(&editor_intro("system-lock-screen-symbolic", crate::i18n::msg("Remote authentication"), crate::i18n::msg("Use browser sign-in when supported, or paste a token supplied by your provider. Toolport stores credentials in the system keychain.")));
    let feedback = gtk::Label::builder()
        .halign(gtk::Align::Fill)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["toolport-feedback"])
        .build();
    l10n::bind_text(&feedback, "Checking the system keychain…");
    content.append(&feedback);

    // What the server actually wants, probed live so the user does not have to
    // guess between a token paste and a browser sign-in.
    let guidance = gtk::Label::builder()
        .halign(gtk::Align::Fill)
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
        crate::i18n::msg("Bearer token"),
        crate::i18n::msg(
            "Saving a manual token replaces any previous browser sign-in for this server.",
        ),
    ));
    let token = {
        let widget = gtk::PasswordEntry::builder()
            .placeholder_text("Paste a new token")
            .show_peek_icon(true)
            .hexpand(true)
            .css_classes(["toolport-input"])
            .build();
        l10n::bind_placeholder(&widget, "Paste a new token");
        widget
    };
    token_section.append(&token);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_halign(gtk::Align::End);
    let sign_in = l10n::button("Sign in with browser");
    sign_in.add_css_class("toolport-secondary-action");
    let cancel_sign_in = l10n::button("Cancel sign-in");
    cancel_sign_in.set_visible(false);
    let remove = l10n::button("Remove token");
    remove.add_css_class("destructive-action");
    remove.set_sensitive(false);
    let save = l10n::button("Store token");
    save.add_css_class("suggested-action");
    let browser_actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    browser_actions.set_halign(gtk::Align::Start);
    browser_actions.append(&sign_in);
    browser_actions.append(&cancel_sign_in);
    content.append(&browser_actions);
    actions.append(&remove);
    actions.append(&save);
    token_section.append(&actions);
    content.append(&token_section);

    let client_section = gtk::Box::new(gtk::Orientation::Vertical, 10);
    client_section.add_css_class("toolport-form-section");
    client_section.append(&section_heading(crate::i18n::msg("Client credentials"), crate::i18n::msg("For unattended OAuth servers. The client secret is stored only in the system keychain.")));
    let client_id = {
        let widget = gtk::Entry::builder()
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
        l10n::bind_placeholder(&widget, "OAuth client id");
        widget
    };
    let client_secret = {
        let widget = gtk::PasswordEntry::builder()
            .placeholder_text(if server.client_credentials.is_some() {
                "Leave blank to keep the stored secret"
            } else {
                "OAuth client secret"
            })
            .show_peek_icon(true)
            .css_classes(["toolport-input"])
            .build();
        l10n::bind_placeholder(
            &widget,
            if server.client_credentials.is_some() {
                crate::i18n::msg("Leave blank to keep the stored secret")
            } else {
                crate::i18n::msg("OAuth client secret")
            },
        );
        widget
    };
    let methods = gtk::StringList::new(&["client_secret_basic", "client_secret_post"]);
    let method = gtk::DropDown::new(Some(methods), None::<gtk::Expression>);
    method.set_selected(
        (server
            .client_credentials
            .as_ref()
            .and_then(|credentials| credentials.token_endpoint_auth_method.as_deref())
            == Some("client_secret_post")) as u32,
    );
    let scope = {
        let widget = gtk::Entry::builder()
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
        l10n::bind_placeholder(&widget, "Scopes (optional)");
        widget
    };
    client_section.append(&editor_field(crate::i18n::msg("Client id"), &client_id));
    client_section.append(&editor_field(
        crate::i18n::msg("Client secret"),
        &client_secret,
    ));
    // Whether a secret is actually vaulted is a keychain question, not a
    // registry one: a lost/locked keychain must not read as "stored", and
    // unknown must not read as absent (SBS-722).
    let secret_status = gtk::Label::builder()
        .halign(gtk::Align::Fill)
        .xalign(0.0)
        .wrap(true)
        .visible(false)
        .css_classes(["toolport-muted", "caption"])
        .build();
    client_section.append(&secret_status);
    let secret_retry = l10n::button("Retry the secret check");
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
                            l10n::bind_placeholder(
                                client_secret.upcast_ref::<gtk::Widget>(),
                                "Leave blank to keep the stored secret",
                            );
                        }
                        Ok(Ok(false)) => {
                            l10n::bind_text(&secret_status, "No client secret is stored yet.");
                            secret_retry.set_visible(false);
                            l10n::bind_placeholder(
                                client_secret.upcast_ref::<gtk::Widget>(),
                                "OAuth client secret",
                            );
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
    client_section.append(&editor_field(
        crate::i18n::msg("Token endpoint auth method"),
        &method,
    ));
    client_section.append(&editor_field(crate::i18n::msg("Scope"), &scope));
    let client_actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    client_actions.set_halign(gtk::Align::End);
    let remove_client = l10n::button("Remove client credentials");
    remove_client.add_css_class("destructive-action");
    remove_client.set_sensitive(server.client_credentials.is_some());
    let save_client = l10n::button("Save client credentials");
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

    let oauth_attempt = std::rc::Rc::new(std::cell::RefCell::new(None::<String>));
    let auth_epoch = std::rc::Rc::new(std::cell::Cell::new(0u64));
    {
        let attempt = oauth_attempt.clone();
        let epoch = auth_epoch.clone();
        editor.connect_close_request(move |_| {
            epoch.set(epoch.get().wrapping_add(1));
            if let Some(id) = attempt.borrow_mut().take() {
                crate::oauth_controller::request_cancel_attempt(&id);
                std::thread::spawn(move || crate::oauth_controller::cancel_attempt(&id));
            }
            gtk::glib::Propagation::Proceed
        });
    }
    let status_epoch = auth_epoch.clone();
    let server_id = server.id.clone();
    let feedback_for_status = feedback.clone();
    let remove_for_status = remove.clone();
    gtk::glib::spawn_future_local(async move {
        let result = gtk::gio::spawn_blocking(move || {
            crate::registry_controller::has_auth_token(&server_id)
        })
        .await;
        if status_epoch.get() != 0 {
            return;
        }
        match result {
            Ok(Ok(has_token)) => {
                l10n::bind_text(
                    &feedback_for_status,
                    if has_token {
                        crate::i18n::msg("A bearer token is stored in the system keychain.")
                    } else {
                        crate::i18n::msg("No bearer token is stored for this server.")
                    },
                );
                feedback_for_status.remove_css_class("error");
                feedback_for_status.add_css_class("success");
                remove_for_status.set_sensitive(has_token);
            }
            Ok(Err(error)) => {
                l10n::bind_formatted_label(
                    &feedback_for_status,
                    "Could not read authentication status: {value0}",
                    &[("value0", &error.to_string())],
                );
                feedback_for_status.add_css_class("error");
            }
            Err(_) => {
                l10n::bind_text(
                    &feedback_for_status,
                    "The keychain status check stopped unexpectedly.",
                );
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
                    l10n::bind_formatted_label(
                        &feedback,
                        "Client credentials for {value0} are configured.",
                        &[("value0", &server_name.to_string())],
                    );
                    feedback.remove_css_class("error");
                    feedback.add_css_class("success");
                    remove.set_sensitive(true);
                }
                Ok(Err(error)) => {
                    l10n::bind_formatted_label(
                        &feedback,
                        "Could not save client credentials: {value0}",
                        &[("value0", &error.to_string())],
                    );
                    feedback.add_css_class("error");
                }
                Err(_) => {
                    l10n::bind_text(
                        &feedback,
                        "The client-credentials update stopped unexpectedly.",
                    );
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
                    l10n::bind_formatted_label(
                        &feedback,
                        "Removed client credentials for {value0}.",
                        &[("value0", &server_name.to_string())],
                    );
                    feedback.remove_css_class("error");
                    feedback.add_css_class("success");
                    button.set_sensitive(false);
                }
                Ok(Err(error)) => {
                    button.set_sensitive(true);
                    l10n::bind_formatted_label(
                        &feedback,
                        "Could not remove client credentials: {value0}",
                        &[("value0", &error.to_string())],
                    );
                    feedback.add_css_class("error");
                }
                Err(_) => {
                    button.set_sensitive(true);
                    l10n::bind_text(
                        &feedback,
                        "The client-credentials removal stopped unexpectedly.",
                    );
                    feedback.add_css_class("error");
                }
            }
        });
    });

    let has_client = server.client_credentials.is_some();
    let remove_before_oauth = std::rc::Rc::new(std::cell::Cell::new(false));
    // Only the current editor attempt may update widgets after blocking work.
    // Cancellation retires it before a late callback/token response can return.
    let server_id = server.id.clone();
    let server_name = server.name.clone();
    let server_url = server.url.clone().unwrap_or_default();
    {
        let feedback = feedback.clone();
        let remove = remove.clone();
        let save = save.clone();
        let save_client = save_client.clone();
        let remove_client = remove_client.clone();
        let cancel = cancel_sign_in.clone();
        let page = page.clone();
        let attempt = oauth_attempt.clone();
        let epoch = auth_epoch.clone();
        let remove_before = remove_before_oauth.clone();
        sign_in.connect_clicked(move |button| {
            if server_url.is_empty() {
                l10n::bind_text(&feedback, "This server does not have a remote URL to sign in to.");
                feedback.add_css_class("error");
                return;
            }
            let id = crate::oauth_controller::start_attempt();
            *attempt.borrow_mut() = Some(id.clone());
            epoch.set(epoch.get().wrapping_add(1));
            let generation = epoch.get();
            button.set_sensitive(false);
            save.set_sensitive(false);
            save_client.set_sensitive(false);
            remove_client.set_sensitive(false);
            remove_before.set(remove.is_sensitive());
            remove.set_sensitive(false);
            cancel.set_visible(true);
            l10n::bind_text(&feedback, "Opening your browser. Approve access there. Waiting up to 3 minutes for the callback. Closed the browser? Cancel sign-in, then sign in again to reopen it. Closing this panel cancels sign-in. The main window closes to tray; Quit Toolport exits.");
            feedback.remove_css_class("error");
            feedback.remove_css_class("success");
            let server_id = server_id.clone();
            let reprobe_id = server_id.clone();
            let server_name = server_name.clone();
            let server_url = server_url.clone();
            let feedback = feedback.clone();
            let remove = remove.clone();
            let save = save.clone();
            let save_client = save_client.clone();
            let remove_client = remove_client.clone();

            let cancel = cancel.clone();
            let page = page.clone();
            let button = button.clone();
            let attempt = attempt.clone();
            let epoch = epoch.clone();
            let remove_before = remove_before.clone();
            gtk::glib::spawn_future_local(async move {
                let result = gtk::gio::spawn_blocking(move || {
                    crate::oauth_controller::authenticate(&server_id, &server_url, &id)
                }).await;
                if epoch.get() != generation { return; }
                attempt.borrow_mut().take();
                remove.set_sensitive(remove_before.get());
                button.set_sensitive(true);
                save.set_sensitive(true);
                save_client.set_sensitive(true);
                remove_client.set_sensitive(has_client);
                cancel.set_visible(false);
                match result {
                    Ok(Ok(())) => {
                        l10n::bind_text(&feedback, "Browser sign-in completed and the token is stored in the system keychain.");
                        feedback.remove_css_class("error");
                        feedback.add_css_class("success");
                        remove.set_sensitive(true);
                        page.show_confirmation(&crate::i18n::trf("Authenticated {value0}", &[("value0", &server_name.to_string())]));
                        page.reprobe_after_auth_change(&reprobe_id);
                    }
                    Ok(Err(error)) => {
                        feedback.set_label(&error);
                        feedback.remove_css_class("success");
                        feedback.add_css_class("error");
                    }
                    Err(_) => {
                        l10n::bind_text(&feedback, "Browser sign-in stopped unexpectedly. Sign in with browser to try again.");
                        feedback.add_css_class("error");
                    }
                }
            });
        });
    }
    {
        let attempt = oauth_attempt.clone();
        let epoch = auth_epoch.clone();
        let feedback = feedback.clone();
        let sign_in = sign_in.clone();
        let save = save.clone();
        let save_client = save_client.clone();
        let remove_client = remove_client.clone();
        let has_client = server.client_credentials.is_some();
        let remove = remove.clone();
        let remove_before = remove_before_oauth.clone();
        cancel_sign_in.connect_clicked(move |button| {
            let Some(id) = attempt.borrow_mut().take() else {
                return;
            };
            crate::oauth_controller::request_cancel_attempt(&id);
            epoch.set(epoch.get().wrapping_add(1));
            let generation = epoch.get();
            button.set_sensitive(false);
            l10n::bind_text(&feedback, "Cancelling browser sign-in…");
            let epoch = epoch.clone();
            let feedback = feedback.clone();
            let sign_in = sign_in.clone();
            let save = save.clone();
            let save_client = save_client.clone();
            let remove_client = remove_client.clone();
            let remove = remove.clone();
            let remove_before = remove_before.clone();
            let button = button.clone();
            gtk::glib::spawn_future_local(async move {
                let result =
                    gtk::gio::spawn_blocking(move || crate::oauth_controller::cancel_attempt(&id))
                        .await;
                if epoch.get() != generation {
                    return;
                }
                sign_in.set_sensitive(true);
                save.set_sensitive(true);
                save_client.set_sensitive(true);
                remove_client.set_sensitive(has_client);
                remove.set_sensitive(remove_before.get());
                button.set_sensitive(true);
                button.set_visible(false);
                feedback.set_label(&match result {
                    Ok(true) => crate::i18n::tr("Browser sign-in cancelled. Sign in with browser to try again."),
                    Ok(false) => crate::i18n::tr("Browser sign-in already finished. Close and reopen this panel to check token status."),
                    Err(_) => crate::i18n::tr("Cancellation stopped unexpectedly. Close this panel and try again."),
                });
            });
        });
    }

    let server_id = server.id.clone();
    let server_name = server.name.clone();
    let feedback_for_save = feedback.clone();
    let remove_for_save = remove.clone();
    let page_for_save = page.clone();
    save.connect_clicked(move |button| {
        let value = token.text().to_string();
        if value.is_empty() {
            l10n::bind_text(&feedback_for_save, "Paste a token before storing it.");
            feedback_for_save.add_css_class("error");
            return;
        }
        let remove_was_sensitive = remove_for_save.is_sensitive();
        button.set_sensitive(false);
        remove_for_save.set_sensitive(false);
        l10n::bind_text(&feedback_for_save, "Storing token in the system keychain…");
        feedback_for_save.remove_css_class("error");
        let server_id = server_id.clone();
        let reprobe_id = server_id.clone();
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
                    l10n::bind_text(&feedback, "Bearer token stored in the system keychain.");
                    feedback.remove_css_class("error");
                    feedback.add_css_class("success");
                    remove.set_sensitive(true);
                    page.show_confirmation(&crate::i18n::trf(
                        "Updated authentication for {value0}",
                        &[("value0", &server_name.to_string())],
                    ));
                    page.reprobe_after_auth_change(&reprobe_id);
                }
                Ok(Err(error)) => {
                    feedback.set_label(&error);
                    feedback.remove_css_class("success");
                    feedback.add_css_class("error");
                    remove.set_sensitive(remove_was_sensitive);
                }
                Err(_) => {
                    l10n::bind_text(&feedback, "The token update stopped unexpectedly.");
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
    let sign_in_for_remove = sign_in.clone();
    remove.connect_clicked(move |button| {
        if !sign_in_for_remove.is_sensitive() {
            return;
        }
        button.set_sensitive(false);
        l10n::bind_text(
            &feedback_for_remove,
            "Removing token from the system keychain…",
        );
        feedback_for_remove.remove_css_class("error");
        let server_id = server_id.clone();
        let reprobe_id = server_id.clone();
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
                    l10n::bind_text(&feedback, "No bearer token is stored for this server.");
                    feedback.remove_css_class("error");
                    feedback.add_css_class("success");
                    page.show_confirmation(&crate::i18n::trf(
                        "Removed authentication for {value0}",
                        &[("value0", &server_name.to_string())],
                    ));
                    page.reprobe_after_auth_change(&reprobe_id);
                }
                Ok(Err(error)) => {
                    button.set_sensitive(true);
                    feedback.set_label(&error);
                    feedback.remove_css_class("success");
                    feedback.add_css_class("error");
                }
                Err(_) => {
                    button.set_sensitive(true);
                    l10n::bind_text(&feedback, "The token removal stopped unexpectedly.");
                    feedback.add_css_class("error");
                }
            }
        });
    });
    editor.present();
}

fn test_server_connection(server: &state::ServerView, button: &gtk::Button, page: ServerPage) {
    button.set_sensitive(false);
    page.show_feedback(
        &crate::i18n::trf("Testing {value0}…", &[("value0", &server.name.to_string())]),
        false,
    );
    let server_id = server.id.clone();
    let server_name = server.name.clone();
    let button = button.clone();
    gtk::glib::spawn_future_local(async move {
        let result =
            gtk::gio::spawn_blocking(move || crate::server_runtime::probe_registered(&server_id))
                .await;
        button.set_sensitive(true);
        match result {
            Ok(Ok(probe)) if probe.ok => page.show_confirmation(&crate::i18n::trf(
                if probe.tool_count == 1 {
                    crate::i18n::msg("{value0} connected and exposed {value1} tool")
                } else {
                    crate::i18n::msg("{value0} connected and exposed {value1} tools")
                },
                &[
                    ("value0", &server_name.to_string()),
                    ("value1", &format!("{}", probe.tool_count)),
                ],
            )),
            Ok(Ok(probe)) if probe.auth_required => page.show_feedback(
                &crate::i18n::trf(
                    "{value0} needs credentials before it can connect",
                    &[("value0", &server_name.to_string())],
                ),
                true,
            ),
            Ok(Ok(probe)) => page.show_feedback(
                &crate::i18n::trf(
                    "Could not connect to {value0}: {value1}",
                    &[
                        ("value0", &server_name.to_string()),
                        (
                            "value1",
                            probe.error.as_deref().unwrap_or("unknown connection error"),
                        ),
                    ],
                ),
                true,
            ),
            Ok(Err(error)) => page.show_feedback(
                &crate::i18n::trf(
                    "Could not test {value0}: {value1}",
                    &[
                        ("value0", &server_name.to_string()),
                        ("value1", &error.to_string()),
                    ],
                ),
                true,
            ),
            Err(_) => page.show_feedback(
                &crate::i18n::trf(
                    "Could not test {value0}: the operation stopped",
                    &[("value0", &server_name.to_string())],
                ),
                true,
            ),
        }
    });
}

fn action_menu_button(label: &'static str, icon: &str) -> gtk::Button {
    let button = gtk::Button::new();
    button.set_halign(gtk::Align::Fill);
    button.add_css_class("toolport-action-item");
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 9);
    row.append(&gtk::Image::from_icon_name(icon));
    row.append(&{
        let widget = gtk::Label::builder()
            .label(label)
            .halign(gtk::Align::Start)
            .hexpand(true)
            .build();
        l10n::bind_label(&widget, label);
        widget
    });
    button.set_child(Some(&row));
    button
}

fn restore_server_switch(
    toggle: &gtk::Switch,
    status: &gtk::Label,
    restoring: &std::cell::Cell<bool>,
    enabled: bool,
) {
    restoring.set(true);
    toggle.set_active(enabled);
    toggle.set_state(enabled);
    restoring.set(false);
    toggle.set_sensitive(true);
    l10n::bind_text(
        status,
        if enabled {
            crate::i18n::msg("Enabled")
        } else {
            crate::i18n::msg("Disabled")
        },
    );
}

const SERVER_SETUP_SUFFIX: &str = " Open server setup to add it.";
const CREDENTIALS_SUFFIX: &str = " > Credentials.";

#[derive(Debug, PartialEq, Eq)]
enum MissingValueEditor {
    Credentials,
    EditServer,
}

/// Enabling was refused because a value that does not sync is missing here.
/// Secrets are added in Credentials and plain values in Edit server.
fn missing_value_editor(error: &str, secret_keys: &[String]) -> Option<MissingValueEditor> {
    if error.ends_with(CREDENTIALS_SUFFIX) {
        return Some(MissingValueEditor::Credentials);
    }
    let needs = error.strip_suffix(SERVER_SETUP_SUFFIX)?;
    let key = needs.rsplit(" needs ").next()?.trim_end_matches('.');
    let key = key.strip_prefix("environment setting ").unwrap_or(key);
    Some(if secret_keys.iter().any(|secret| secret == key) {
        MissingValueEditor::Credentials
    } else {
        MissingValueEditor::EditServer
    })
}

fn missing_value_hint(error: &str, editor: &MissingValueEditor) -> String {
    let Some(needs) = error.strip_suffix(SERVER_SETUP_SUFFIX) else {
        return error.to_string();
    };
    let place = match editor {
        MissingValueEditor::Credentials => "Credentials",
        MissingValueEditor::EditServer => "Edit server",
    };
    crate::i18n::trf(
        "{value0} Add it in {value1}, then turn the server on.",
        &[("value0", needs), ("value1", place)],
    )
}

fn open_credentials_editor(server: state::ServerView, page: ServerPage) {
    let Some(parent) = page.app.active_window() else {
        return;
    };
    let editor = adw::Window::builder()
        .application(&page.app)
        .transient_for(&parent)
        .modal(true)
        .title(crate::i18n::trf(
            "Credentials for {value0}",
            &[("value0", &server.name.to_string())],
        ))
        .default_width(620)
        .default_height(640)
        .build();
    editor.add_css_class("toolport-editor");

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = adw::HeaderBar::new();
    let done = l10n::button("Done");
    done.add_css_class("suggested-action");
    header.pack_end(&done);
    root.append(&header);

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
    content.add_css_class("toolport-editor-body");
    let intro = editor_intro("dialog-password-symbolic", crate::i18n::msg("Server credentials"), crate::i18n::msg("Paste a key into the system keychain or save a password manager reference. Only references sync."));
    content.append(&intro);
    let feedback = gtk::Label::builder()
        .halign(gtk::Align::Fill)
        .xalign(0.0)
        .wrap(true)
        .visible(false)
        .css_classes(["toolport-feedback", "error"])
        .build();
    content.append(&feedback);

    let stored = gtk::Box::new(gtk::Orientation::Vertical, 10);
    stored.add_css_class("toolport-form-section");
    stored.append(&section_heading(
        crate::i18n::msg("Stored credentials"),
        crate::i18n::msg("Enter a new value only when you want to replace one."),
    ));
    // Secret command arguments are vaulted like keys but are not environment
    // variables, so they save through the launch-input path.
    let launch_secrets: Vec<(String, String)> = server
        .launch
        .iter()
        .flat_map(|launch| {
            launch
                .inputs
                .iter()
                .filter(|input| input.secret)
                .map(|input| {
                    (
                        input.key.clone(),
                        crate::launch_inputs::input_display_name(&server.args, launch, &input.key),
                    )
                })
        })
        .collect();
    if server.secret_keys.is_empty() && launch_secrets.is_empty() {
        stored.append(&{
            let widget = gtk::Label::builder()
                .label(crate::i18n::tr(
                    "No credential keys are declared for this server.",
                ))
                .halign(gtk::Align::Fill)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["toolport-muted"])
                .build();
            l10n::bind_label(&widget, "No credential keys are declared for this server.");
            widget
        });
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
            .tooltip_text(crate::i18n::trf(
                "Remove {value0}",
                &[("value0", &key.to_string())],
            ))
            .css_classes(["flat", "toolport-destructive-icon"])
            .build();
        key_row.append(&remove);
        row.append(&key_row);
        let replace_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let value = {
            let widget = gtk::PasswordEntry::builder()
                .placeholder_text("Enter a replacement value")
                .show_peek_icon(true)
                .hexpand(true)
                .css_classes(["toolport-input"])
                .build();
            l10n::bind_placeholder(&widget, "Enter a replacement value");
            widget
        };
        replace_row.append(&value);
        let replace = l10n::button("Replace");
        replace.add_css_class("toolport-secondary-action");
        replace_row.append(&replace);
        let (source_choice, reference_box) = secret_reference_controls(
            &server.id,
            key,
            server.secret_references.get(key).map(String::as_str),
            &page,
            &editor,
            &feedback,
        );
        replace_row.set_visible(source_choice.selected() == 0);
        let paste_row = replace_row.clone();
        source_choice
            .connect_selected_notify(move |choice| paste_row.set_visible(choice.selected() == 0));
        row.append(&source_choice);
        row.append(&replace_row);
        row.append(&reference_box);
        stored.append(&row);

        let server_id = server.id.clone();
        let key_for_replace = key.clone();
        let feedback_for_replace = feedback.clone();
        let page_for_replace = page.clone();
        let editor_for_replace = editor.clone();
        replace.connect_clicked(move |button| {
            let secret = value.text().to_string();
            if secret.is_empty() {
                l10n::bind_text(&feedback_for_replace, "Enter a replacement value first.");
                feedback_for_replace.set_visible(true);
                return;
            }
            button.set_sensitive(false);
            let server_id = server_id.clone();
            let reprobe_id = server_id.clone();
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
                finish_credential_update(
                    result,
                    &reprobe_id,
                    &page,
                    &editor,
                    &feedback,
                    &button,
                    "Updated",
                );
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
    for (key, name) in &launch_secrets {
        let row = gtk::Box::new(gtk::Orientation::Vertical, 10);
        row.add_css_class("toolport-credential-row");
        row.append(
            &gtk::Label::builder()
                .label(name)
                .halign(gtk::Align::Start)
                .wrap(true)
                .css_classes(["heading"])
                .build(),
        );
        let save_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let value = {
            let widget = gtk::PasswordEntry::builder()
                .placeholder_text("Enter the value for this machine")
                .show_peek_icon(true)
                .hexpand(true)
                .css_classes(["toolport-input"])
                .build();
            l10n::bind_placeholder(&widget, "Enter the value for this machine");
            widget
        };
        save_row.append(&value);
        let save = l10n::button("Save");
        save.add_css_class("toolport-secondary-action");
        save_row.append(&save);
        let (source_choice, reference_box) = secret_reference_controls(
            &server.id,
            key,
            server.secret_references.get(key).map(String::as_str),
            &page,
            &editor,
            &feedback,
        );
        save_row.set_visible(source_choice.selected() == 0);
        let paste_row = save_row.clone();
        source_choice
            .connect_selected_notify(move |choice| paste_row.set_visible(choice.selected() == 0));
        row.append(&source_choice);
        row.append(&save_row);
        row.append(&reference_box);
        stored.append(&row);

        let server_id = server.id.clone();
        let key = key.clone();
        let feedback = feedback.clone();
        let page = page.clone();
        let editor = editor.clone();
        save.connect_clicked(move |button| {
            let secret = value.text().to_string();
            if secret.is_empty() {
                l10n::bind_text(&feedback, "Enter a value first.");
                feedback.set_visible(true);
                return;
            }
            button.set_sensitive(false);
            let server_id = server_id.clone();
            let reprobe_id = server_id.clone();
            let key = key.clone();
            let page = page.clone();
            let editor = editor.clone();
            let feedback = feedback.clone();
            let button = button.clone();
            gtk::glib::spawn_future_local(async move {
                let result = gtk::gio::spawn_blocking(move || {
                    crate::registry_controller::set_launch_secret(&server_id, &key, &secret)
                })
                .await;
                finish_credential_update(
                    result,
                    &reprobe_id,
                    &page,
                    &editor,
                    &feedback,
                    &button,
                    "Updated",
                );
            });
        });
    }
    content.append(&stored);

    let add_section = gtk::Box::new(gtk::Orientation::Vertical, 10);
    add_section.add_css_class("toolport-form-section");
    add_section.append(&section_heading(
        crate::i18n::msg("Add credential"),
        crate::i18n::msg("Use the environment variable name expected by this server."),
    ));
    let add_row = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let new_key = gtk::Entry::builder()
        .placeholder_text("VARIABLE_NAME")
        .css_classes(["toolport-input"])
        .build();
    let new_value = {
        let widget = gtk::PasswordEntry::builder()
            .placeholder_text("Secret value")
            .show_peek_icon(true)
            .hexpand(true)
            .css_classes(["toolport-input"])
            .build();
        l10n::bind_placeholder(&widget, "Secret value");
        widget
    };
    let add = l10n::button("Store");
    add.add_css_class("suggested-action");
    add_row.append(&new_key);
    let (source_choice, reference_box) = secret_reference_controls_dynamic(
        &server.id,
        new_key.clone(),
        None,
        &page,
        &editor,
        &feedback,
    );
    add_row.append(&source_choice);
    let value_for_source = new_value.clone();
    let add_for_source = add.clone();
    source_choice.connect_selected_notify(move |choice| {
        value_for_source.set_visible(choice.selected() == 0);
        add_for_source.set_visible(choice.selected() == 0);
    });
    add_row.append(&new_value);
    add_row.append(&reference_box);
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
            l10n::bind_text(
                &feedback_for_add,
                "Enter both a variable name and a secret value.",
            );
            feedback_for_add.set_visible(true);
            return;
        }
        button.set_sensitive(false);
        let server_id = server_id.clone();
        let reprobe_id = server_id.clone();
        let page = page_for_add.clone();
        let editor = editor_for_add.clone();
        let feedback = feedback_for_add.clone();
        let button = button.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                crate::registry_controller::set_server_secret(&server_id, &key, &secret)
            })
            .await;
            finish_credential_update(
                result,
                &reprobe_id,
                &page,
                &editor,
                &feedback,
                &button,
                "Stored",
            );
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
        Some(&crate::i18n::trf("Remove {value0}?", &[("value0", key)])),
        Some(&crate::i18n::tr(
            "The stored value will be removed from the system keychain.",
        )),
    );
    l10n::bind_formatted_dialog_heading(&dialog, "Remove {value0}?", &[("value0", key)]);
    l10n::bind_dialog_body(
        &dialog,
        "The stored value will be removed from the system keychain.",
    );
    dialog.add_response("cancel", &crate::i18n::tr("Cancel"));
    l10n::bind_dialog_response(&dialog, "cancel", "Cancel");
    dialog.add_response("remove", &crate::i18n::tr("Remove"));
    l10n::bind_dialog_response(&dialog, "remove", "Remove");
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
        let reprobe_id = server_id.clone();
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
            finish_credential_update(
                result,
                &reprobe_id,
                &page,
                &editor,
                &feedback,
                &button,
                "Removed",
            );
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
    server_id: &str,
    page: &ServerPage,
    editor: &adw::Window,
    feedback: &gtk::Label,
    button: &gtk::Button,
    action: &str,
) {
    match result {
        Ok(Ok(registry)) => {
            // Secret values live in the keychain, outside the fingerprint the
            // health cache compares, so the new value needs its own probe.
            page.health.borrow_mut().invalidate(server_id);
            page.render(state::RegistryState::Ready(
                state::RegistrySnapshot::from_registry(registry),
            ));
            page.show_confirmation(&crate::i18n::trf(
                "{value0} credential",
                &[("value0", action)],
            ));
            editor.close();
        }
        Ok(Err(error)) => {
            l10n::bind_formatted_label(
                feedback,
                "Could not update credential: {value0}",
                &[("value0", &error.to_string())],
            );
            feedback.set_visible(true);
            button.set_sensitive(true);
        }
        Err(_) => {
            l10n::bind_text(
                feedback,
                "Could not update credential: the operation stopped",
            );
            feedback.set_visible(true);
            button.set_sensitive(true);
        }
    }
}

fn open_server_editor(server: Option<state::ServerView>, page: ServerPage) {
    let server_id = server.as_ref().map(|server| server.id.clone());
    open_server_editor_prefilled(server, server_id, page, None);
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
    open_server_editor_prefilled(Some(copy), None, page, None);
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

/// `url_placeholder` overrides the generic URL example for catalog entries that
/// carry a `url_hint`: the user has to supply their own instance endpoint, so
/// the hint is a placeholder, never prefilled text they could save verbatim.
/// Returns the editor window so callers can refresh once it closes.
fn open_server_editor_prefilled(
    server: Option<state::ServerView>,
    server_id: Option<String>,
    page: ServerPage,
    url_placeholder: Option<&str>,
) -> Option<adw::Window> {
    let parent = page.app.active_window()?;
    let editing = server_id.is_some();
    let editor = adw::Window::builder()
        .application(&page.app)
        .transient_for(&parent)
        .modal(true)
        .title(crate::i18n::tr(if editing {
            crate::i18n::msg("Edit server")
        } else {
            crate::i18n::msg("Add server")
        }))
        .default_width(620)
        .default_height(660)
        .build();
    editor.add_css_class("toolport-editor");
    l10n::bind_window_title(&editor, if editing { "Edit server" } else { "Add server" });

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = adw::HeaderBar::new();
    let test = l10n::button("Test connection");
    test.add_css_class("toolport-secondary-action");
    let save = l10n::button(if editing {
        crate::i18n::msg("Save")
    } else {
        crate::i18n::msg("Add")
    });
    save.add_css_class("suggested-action");
    header.pack_end(&save);
    root.append(&header);

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .build();
    let form = gtk::Box::new(gtk::Orientation::Vertical, 12);
    form.add_css_class("toolport-editor-body");
    form.append(&{
        let widget = gtk::Label::builder()
            .label(crate::i18n::tr(
                "Connect a local command or remote MCP endpoint to every Toolport client.",
            ))
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-editor-lede"])
            .build();
        l10n::bind_label(
            &widget,
            "Connect a local command or remote MCP endpoint to every Toolport client.",
        );
        widget
    });

    let feedback = gtk::Label::builder()
        .halign(gtk::Align::Fill)
        .xalign(0.0)
        .wrap(true)
        .visible(false)
        .css_classes(["toolport-feedback", "error"])
        .build();
    form.append(&feedback);

    let local_only = l10n::check_button("This machine only");
    local_only.set_visible(
        !editing && crate::registry::load().is_ok_and(|r| crate::personal_sync::is_personal(&r)),
    );
    l10n::bind_tooltip(
        &local_only,
        "Keep this new server on this machine from the start.",
    );
    form.append(&local_only);

    // Pasted env values wait here until the add commits; the entry itself never
    // carries secret values, they go straight to the keychain on save.
    type SnippetEnv = Vec<(String, Option<String>)>;
    let snippet_env: std::rc::Rc<std::cell::RefCell<SnippetEnv>> =
        std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));

    let name = {
        let widget = gtk::Entry::builder()
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
        l10n::bind_placeholder(&widget, "My MCP server");
        widget
    };
    let identity = gtk::Box::new(gtk::Orientation::Vertical, 8);
    identity.add_css_class("toolport-form-section");
    identity.append(&section_heading(
        crate::i18n::msg("Identity"),
        crate::i18n::msg("Choose a clear name that will make sense in every AI client."),
    ));
    identity.append(&editor_field(crate::i18n::msg("Server name"), &name));
    if !editing {
        // Soft warning only: multiple accounts under one name are legitimate,
        // the user just needs to know a second entry is what they will get.
        let duplicate_warning = gtk::Label::builder()
            .halign(gtk::Align::Fill)
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
                l10n::bind_formatted_label(&duplicate_warning, "Another server is already named \"{value0}\". That's fine for multiple accounts; it'll be saved as a separate entry.", &[("value0", name.text().trim())]);
            }
            duplicate_warning.set_visible(duplicate);
        });
    }
    form.append(&identity);

    let transport = gtk::DropDown::from_strings(&[
        &crate::i18n::tr("Local stdio"),
        &crate::i18n::tr("Remote HTTP"),
        &crate::i18n::tr("Remote SSE"),
    ]);
    l10n::bind_dropdown_choices(&transport, &["Local stdio", "Remote HTTP", "Remote SSE"]);
    transport.add_css_class("toolport-input");
    transport.set_selected(
        match server.as_ref().map(|server| server.transport_id.as_str()) {
            Some("http") => 1,
            Some("sse") => 2,
            _ => 0,
        },
    );
    let connection = gtk::Box::new(gtk::Orientation::Vertical, 10);
    connection.add_css_class("toolport-form-section");
    connection.append(&section_heading(
        crate::i18n::msg("Connection"),
        crate::i18n::msg("Toolport will validate these fields before saving."),
    ));
    connection.append(&editor_field(crate::i18n::msg("Transport"), &transport));

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
    let command_row = editor_field(crate::i18n::msg("Command"), &command);
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
    let args_row = editor_field(crate::i18n::msg("Arguments, one per line"), &args_scroller);
    connection.append(&args_row);

    let original_launch = server.as_ref().and_then(|server| server.launch.clone());
    let original_command = server.as_ref().and_then(|server| server.command.clone());
    let original_args = server
        .as_ref()
        .map(|server| server.args.clone())
        .unwrap_or_default();
    type LaunchEntry = (
        crate::registry::LaunchInput,
        gtk::Entry,
        Option<(gtk::DropDown, gtk::Entry)>,
    );
    let mut launch_entries: Vec<LaunchEntry> = Vec::new();
    if let Some(launch) = &original_launch {
        if !launch.inputs.is_empty() {
            let section = gtk::Box::new(gtk::Orientation::Vertical, 8);
            section.add_css_class("toolport-form-section");
            section.append(&section_heading(crate::i18n::msg("Launch setup"), crate::i18n::msg("Paste a key or use a password manager reference. Leave a saved key blank to keep it.")));
            for input in &launch.inputs {
                let field = {
                    let widget = gtk::Entry::builder()
                        .text(if input.secret {
                            ""
                        } else {
                            input.value.as_deref().unwrap_or("")
                        })
                        .placeholder_text(if input.secret {
                            "Saved value stays in vault"
                        } else {
                            "Required before enabling"
                        })
                        .visibility(!input.secret)
                        .hexpand(true)
                        .css_classes(["toolport-input"])
                        .build();
                    l10n::bind_placeholder(
                        &widget,
                        if input.secret {
                            crate::i18n::msg("Saved value stays in vault")
                        } else {
                            crate::i18n::msg("Required before enabling")
                        },
                    );
                    widget
                };
                let row = editor_field_data(&input.label, &field);
                let source = if input.secret {
                    let existing = crate::secret_refs::source(&input.unknown_fields)
                        .ok()
                        .flatten();
                    let (choice, controls, reference) = secret_reference_fields(
                        server_id.as_deref().unwrap_or(""),
                        existing,
                        &feedback,
                    );
                    field.set_visible(choice.selected() == 0);
                    let paste = field.clone();
                    choice.connect_selected_notify(move |c| paste.set_visible(c.selected() == 0));
                    row.append(&choice);
                    row.append(&controls);
                    Some((choice, reference))
                } else {
                    None
                };
                section.append(&row);
                launch_entries.push((input.clone(), field, source));
            }
            connection.append(&section);
        }
        let binding_warning = {
            let widget = gtk::Label::builder()
            .label(crate::i18n::tr("Editing the command or arguments removes catalog launch setup. Replace <launch-input> before saving, or restore the catalog preset."))
            .halign(gtk::Align::Fill).xalign(0.0).wrap(true).visible(false)
            .css_classes(["toolport-feedback", "error"]).build();
            l10n::bind_label(&widget, "Editing the command or arguments removes catalog launch setup. Replace <launch-input> before saving, or restore the catalog preset.");
            widget
        };
        connection.append(&binding_warning);
        let args_for_warning = args.clone();
        let command_for_warning = command.clone();
        let old_args = original_args.clone();
        let old_command = original_command.clone();
        let update_warning = move || {
            let buffer = args_for_warning.buffer();
            let text = buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), false)
                .to_string();
            let parsed = text.lines().map(str::to_string).collect::<Vec<_>>();
            binding_warning.set_visible(
                parsed != old_args || Some(command_for_warning.text().to_string()) != old_command,
            );
        };
        let update_warning = std::rc::Rc::new(update_warning);
        let on_args = update_warning.clone();
        args.buffer().connect_changed(move |_| on_args());
        command.connect_changed(move |_| update_warning());
    }

    let cwd = {
        let widget = gtk::Entry::builder()
            .text(
                server
                    .as_ref()
                    .and_then(|server| server.cwd.as_deref())
                    .unwrap_or(""),
            )
            .placeholder_text(crate::i18n::tr("Optional working directory"))
            .hexpand(true)
            .css_classes(["toolport-input"])
            .build();
        l10n::bind_placeholder(&widget, "Optional working directory");
        widget
    };
    let cwd_row = editor_field(crate::i18n::msg("Working directory"), &cwd);
    let inherit_env = gtk::Switch::builder()
        .active(server.as_ref().is_some_and(|server| server.inherit_env))
        .valign(gtk::Align::Center)
        .build();
    let inherit_row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    inherit_row.set_margin_top(6);
    let inherit_copy = gtk::Box::new(gtk::Orientation::Vertical, 3);
    inherit_copy.set_hexpand(true);
    let inherit_label = {
        let widget = gtk::Label::builder()
            .label(crate::i18n::tr("Use my shell environment"))
            .halign(gtk::Align::Start)
            .css_classes(["toolport-field-label"])
            .build();
        l10n::bind_label(&widget, "Use my shell environment");
        widget
    };
    inherit_copy.append(&inherit_label);
    inherit_copy.append(
        &{ let widget = gtk::Label::builder()
            .label(crate::i18n::tr("Gives this server every variable from your shell, such as AWS, GitHub or kube settings. When off it gets only PATH, HOME and other basics, plus the variables you set."))
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-muted", "caption"])
            .build(); l10n::bind_label(&widget, "Gives this server every variable from your shell, such as AWS, GitHub or kube settings. When off it gets only PATH, HOME and other basics, plus the variables you set."); widget },
    );
    inherit_env.update_relation(&[gtk::accessible::Relation::LabelledBy(&[
        inherit_label.upcast_ref()
    ])]);
    inherit_row.append(&inherit_copy);
    inherit_row.append(&inherit_env);
    cwd_row.append(&inherit_row);
    connection.append(&cwd_row);

    let url = gtk::Entry::builder()
        .text(
            server
                .as_ref()
                .and_then(|server| server.url.as_deref())
                .unwrap_or(""),
        )
        .placeholder_text(url_placeholder.unwrap_or("https://example.com/mcp"))
        .hexpand(true)
        .css_classes(["toolport-input"])
        .build();
    let url_row = editor_field(crate::i18n::msg("Server URL"), &url);
    connection.append(&url_row);
    test.set_halign(gtk::Align::End);
    connection.append(&test);
    form.append(&connection);

    // Plain values can only be edited once the server exists; a new server
    // takes them from a pasted config.
    let plain_env = editing.then(|| {
        let personal = page
            .last_snapshot
            .borrow()
            .as_ref()
            .is_some_and(|snapshot| snapshot.personal_sync);
        let (section, rows) = plain_env_section(
            server
                .as_ref()
                .map(|server| server.plain_env.as_slice())
                .unwrap_or_default(),
            personal,
        );
        form.append(&section);
        (section, rows)
    });

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
        let paste = {
            let widget = gtk::Expander::new(None);
            widget.set_label_widget(Some(&l10n::label("Start from a pasted config")));
            widget
        };
        paste.add_css_class("toolport-paste-expander");
        let paste_content = gtk::Box::new(gtk::Orientation::Vertical, 8);
        paste_content.set_margin_top(6);
        paste_content.set_margin_bottom(12);
        paste_content.append(
            &{ let widget = gtk::Label::builder()
                .label(crate::i18n::tr("Paste JSON, TOML, YAML, or a claude mcp add command and Toolport will fill the form."))
                .halign(gtk::Align::Fill)
                .hexpand(true)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["toolport-muted"])
                .build(); l10n::bind_label(&widget, "Paste JSON, TOML, YAML, or a claude mcp add command and Toolport will fill the form."); widget },
        );
        let snippet = gtk::TextView::new();
        snippet.set_monospace(true);
        snippet.set_wrap_mode(gtk::WrapMode::WordChar);
        snippet.set_top_margin(8);
        snippet.set_bottom_margin(8);
        snippet.set_left_margin(8);
        snippet.set_right_margin(8);
        let snippet_scroller = gtk::ScrolledWindow::builder()
            .child(&snippet)
            .min_content_height(64)
            .build();
        snippet_scroller.add_css_class("toolport-text-area");
        paste_content.append(&snippet_scroller);
        let fill = l10n::button("Fill from snippet");
        fill.add_css_class("toolport-secondary-action");
        fill.set_halign(gtk::Align::Start);
        paste_content.append(&fill);
        paste.set_child(Some(&paste_content));
        form.insert_child_after(&paste, Some(&feedback));

        let feedback_for_fill = feedback.clone();
        let name_for_fill = name.clone();
        let transport_for_fill = transport.clone();
        let command_for_fill = command.clone();
        let args_for_fill = args.clone();
        let url_for_fill = url.clone();
        let cwd_for_fill = cwd.clone();
        let env_for_fill = snippet_env.clone();
        let page_for_fill = page.clone();
        fill.connect_clicked(move |fill| {
            let buffer = snippet.buffer();
            let text = buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), false)
                .to_string();
            const MAX_SNIPPET_BYTES: usize = 256 * 1024;
            let parsed = if text.len() > MAX_SNIPPET_BYTES {
                Err(crate::i18n::trf("Snippet is {value0} KB; limit is {value1} KB. Paste a single server config, not an entire file.", &[("value0", &format!("{}", text.len() / 1024)), ("value1", &format!("{}", MAX_SNIPPET_BYTES / 1024))]))
            } else {
                crate::clients::parse_snippet(&text).map_err(|_| crate::i18n::tr("Could not read the pasted config. Check its syntax and retry.").to_string())
            };
            feedback_for_fill.set_visible(true);
            match parsed {
                Ok(servers) => {
                    if servers.len() > 1 {
                        let Some(parent) = fill.root().and_downcast::<gtk::Window>() else { return; };
                        // Snippets have no launch metadata; Import::prepare requires every env key.
                        let items = servers.iter().enumerate().map(|(i, s)| crate::registry_controller::SetupItem {key:i.to_string(),name:s.name.clone(),transport:s.transport.clone(),command:s.command.clone(),args:s.args.clone(),url:s.url.clone(),env_keys:s.env.iter().map(|e| e.key.clone()).collect(),is_new:true,credentials:s.env.iter().map(|e|crate::registry_controller::CredentialReview{key:e.key.clone(),secret:crate::import_credentials::secret_env(&e.key,e.value.as_deref()),present:e.value.as_deref().is_some_and(crate::import_credentials::provided),required:true}).collect(),updates:Vec::new(),unsupported:None}).collect();
                        setup::review(&parent, &crate::i18n::tr("Review pasted servers"), items, &crate::i18n::tr("Review each command and URL. Credentials go to the keychain. Missing inputs stay off."), crate::i18n::msg("Add selected servers"), false, move |selected,choices,inputs| {
                            let outcome = crate::registry_controller::add_snippet_servers_inputs(&text, &selected, &choices, &inputs)?;
                            if !outcome.failed.is_empty() { return Err(crate::i18n::tr("Could not save credentials. Open Credentials and retry.")); }
                            Ok(outcome.servers.iter().map(|s|format!("{}: {}",s.name,s.status)).collect::<Vec<_>>().join("\n").into())
                        }, { let page = page_for_fill.clone(); move || run_profile_mutation(page.clone(), crate::i18n::msg("Added selected servers"), crate::registry_controller::registry_for_disconnect) }, None);
                        return;
                    }
                    let Some(first) = servers.first() else {
                        l10n::bind_text(&feedback_for_fill, "No servers found in the pasted config.");
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
                        // Count what the import will vault, not every variable:
                        // plain settings are saved with the server instead.
                        first
                            .env
                            .iter()
                            .filter(|env| {
                                crate::import_credentials::secret_env(&env.key, env.value.as_deref())
                            })
                            .count(),
                    ));
                    feedback_for_fill.remove_css_class("error");
                    feedback_for_fill.add_css_class("success");
                }
                Err(error) => {
                    l10n::bind_formatted_label(&feedback_for_fill, "Could not parse the snippet: {value0}", &[("value0", &error.to_string())]);
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
    let env_section = plain_env.as_ref().map(|(section, _)| section.clone());
    if let Some(section) = &env_section {
        section.set_visible(transport.selected() == 0);
    }
    transport.connect_selected_notify(move |transport| {
        update_editor_transport(
            transport.selected(),
            &command_row_for_transport,
            &args_row_for_transport,
            &cwd_row_for_transport,
            &url_row_for_transport,
        );
        if let Some(section) = &env_section {
            section.set_visible(transport.selected() == 0);
        }
    });
    let plain_env = plain_env.map(|(_, rows)| rows);

    let clamp = adw::Clamp::builder()
        .maximum_size(680)
        .tightening_threshold(520)
        .child(&form)
        .build();
    scroller.set_child(Some(&clamp));
    root.append(&scroller);
    editor.set_content(Some(&root));

    let server_id_for_test = server_id.clone();
    let feedback_for_test = feedback.clone();
    let name_for_test = name.clone();
    let transport_for_test = transport.clone();
    let command_for_test = command.clone();
    let args_for_test = args.clone();
    let url_for_test = url.clone();
    let cwd_for_test = cwd.clone();
    let inherit_env_for_test = inherit_env.clone();
    let launch_for_test = launch_entries.clone();
    let launch_definition_for_test = original_launch.clone();
    let original_command_for_test = original_command.clone();
    let original_args_for_test = original_args.clone();
    let plain_env_for_test = plain_env.clone();
    test.connect_clicked(move |button| {
        button.set_sensitive(false);
        l10n::bind_text(&feedback_for_test, "Testing connection…");
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
            &inherit_env_for_test,
        );
        let binding_changed =
            fields.command != original_command_for_test || fields.args != original_args_for_test;
        let launch_values = launch_for_test
            .iter()
            .map(|(input, field, source)| {
                launch_editor_value(
                    input,
                    &field.text(),
                    source
                        .as_ref()
                        .filter(|(c, _)| c.selected() == 1)
                        .map(|(_, r)| r.text().to_string())
                        .as_deref(),
                )
            })
            .collect::<Vec<_>>();
        let launch_definition = launch_definition_for_test.clone();
        let env_edits = plain_env_for_test.as_ref().map(collect_plain_env);
        let server_id = server_id_for_test.clone();
        let feedback = feedback_for_test.clone();
        let button = button.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                let mut entry = crate::registry_controller::server_entry_for_probe(
                    server_id.as_deref(),
                    fields,
                )?;
                if let Some(edits) = env_edits {
                    crate::registry_controller::apply_plain_env(&mut entry, edits)?;
                }
                if !binding_changed {
                    if entry.launch.is_none() {
                        entry.launch = launch_definition;
                    }
                    if let Some(launch) = &mut entry.launch {
                        for input in &mut launch.inputs {
                            if let Some((edited, value)) = launch_values
                                .iter()
                                .find(|(edited, _)| edited.key == input.key)
                            {
                                input.unknown_fields = edited.unknown_fields.clone();
                                apply_launch_probe_value(input, value);
                            }
                        }
                    }
                }
                Ok::<_, String>(crate::server_runtime::probe_one_bounded(&entry))
            })
            .await;
            button.set_sensitive(true);
            match result {
                Ok(Ok(probe)) if probe.ok => {
                    l10n::bind_formatted_label(
                        &feedback,
                        if probe.tool_count == 1 {
                            crate::i18n::msg("Connected successfully and found {value0} tool.")
                        } else {
                            crate::i18n::msg("Connected successfully and found {value0} tools.")
                        },
                        &[("value0", &format!("{}", probe.tool_count))],
                    );
                    feedback.remove_css_class("error");
                    feedback.add_css_class("success");
                }
                Ok(Ok(probe)) if probe.auth_required => {
                    l10n::bind_text(&feedback, "The server responded but needs credentials.");
                    feedback.remove_css_class("success");
                    feedback.add_css_class("error");
                }
                Ok(Ok(probe)) => {
                    l10n::bind_formatted_label(
                        &feedback,
                        "Connection failed: {value0}",
                        &[(
                            "value0",
                            probe.error.as_deref().unwrap_or("unknown connection error"),
                        )],
                    );
                    feedback.remove_css_class("success");
                    feedback.add_css_class("error");
                }
                Ok(Err(error)) => {
                    l10n::bind_formatted_label(
                        &feedback,
                        "Could not test: {value0}",
                        &[("value0", &error.to_string())],
                    );
                    feedback.remove_css_class("success");
                    feedback.add_css_class("error");
                }
                Err(_) => {
                    l10n::bind_text(&feedback, "Could not test: the operation stopped");
                    feedback.remove_css_class("success");
                    feedback.add_css_class("error");
                }
            }
        });
    });

    let editor_for_save = editor.clone();
    let launch_for_save = launch_entries.clone();
    let original_launch_for_save = original_launch.clone();
    let original_command_for_save = original_command.clone();
    let original_args_for_save = original_args.clone();
    save.connect_clicked(move |save| {
        save.set_sensitive(false);
        feedback.set_visible(false);
        let server_id = server_id.clone();
        let mut fields =
            collect_server_fields(&name, &transport, &command, &args, &url, &cwd, &inherit_env);
        fields.sync_local_only = local_only.is_active();
        let binding_changed =
            fields.command != original_command_for_save || fields.args != original_args_for_save;
        let launch_values = launch_for_save
            .iter()
            .map(|(input, field, source)| {
                launch_editor_value(
                    input,
                    &field.text(),
                    source
                        .as_ref()
                        .filter(|(c, _)| c.selected() == 1)
                        .map(|(_, r)| r.text().to_string())
                        .as_deref(),
                )
            })
            .collect::<Vec<_>>();
        let launch_definition = original_launch_for_save.clone();
        let display_name = fields.name.trim().to_string();
        let env = snippet_env.borrow().clone();
        let env_edits = plain_env.as_ref().map(collect_plain_env);
        let page = page.clone();
        let save = save.clone();
        let feedback = feedback.clone();
        let editor = editor_for_save.clone();
        gtk::glib::spawn_future_local(async move {
            let update = gtk::gio::spawn_blocking(move || {
                if binding_changed
                    && launch_definition.is_some()
                    && fields.args.iter().any(|arg| arg == "<launch-input>")
                {
                    return Err(crate::i18n::tr(
                        "Replace <launch-input> with a literal argument before saving",
                    ));
                }
                match server_id {
                    Some(server_id) => {
                        let mut registry =
                            crate::registry_controller::update_server_fields_and_env(
                                &server_id, fields, env_edits,
                            )?;
                        if !binding_changed {
                            registry = save_launch_entries(&server_id, &launch_values, registry)?;
                        }
                        Ok((registry, Vec::new(), Vec::new()))
                    }
                    None if env.is_empty() => {
                        let mut registry = if !binding_changed {
                            match launch_definition {
                                Some(launch) => crate::registry_controller::add_server_with_launch(
                                    fields, launch,
                                )?,
                                None => crate::registry_controller::add_server(fields)?,
                            }
                        } else {
                            crate::registry_controller::add_server(fields)?
                        };
                        if !binding_changed {
                            if let Some(id) =
                                registry.servers.last().map(|server| server.id.clone())
                            {
                                registry = save_launch_entries(&id, &launch_values, registry)?;
                            }
                        }
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
                }
            })
            .await;
            match update {
                Ok(Ok((registry, declared_without_value, failed_env))) => {
                    page.render(state::RegistryState::Ready(
                        state::RegistrySnapshot::from_registry(registry),
                    ));
                    let message = server_saved_feedback(
                        editing,
                        &display_name,
                        &declared_without_value,
                        &failed_env,
                    );
                    if failed_env.is_empty() {
                        page.show_confirmation(&message);
                    } else {
                        page.show_feedback(&message, true);
                    }
                    editor.close();
                }
                Ok(Err(error)) => {
                    l10n::bind_formatted_label(
                        &feedback,
                        "Could not save: {value0}",
                        &[("value0", &error.to_string())],
                    );
                    feedback.set_visible(true);
                    save.set_sensitive(true);
                }
                Err(_) => {
                    l10n::bind_text(&feedback, "Could not save: the operation stopped");
                    feedback.set_visible(true);
                    save.set_sensitive(true);
                }
            }
        });
    });
    editor.present();
    Some(editor)
}

fn apply_launch_probe_value(input: &mut crate::registry::LaunchInput, value: &str) {
    // Only a blank secret means "keep the vaulted value". Clearing a plain
    // field must also clear the saved value used by this unsaved probe.
    if !input.secret || !value.is_empty() {
        input.value = (!value.is_empty()).then(|| value.to_string());
    }
}

fn launch_editor_value(
    input: &crate::registry::LaunchInput,
    value: &str,
    reference: Option<&str>,
) -> (crate::registry::LaunchInput, String) {
    let mut input = input.clone();
    if let Some(reference) = reference {
        input.value = None;
        input
            .unknown_fields
            .insert("source".into(), serde_json::json!({"ref": reference}));
        (input, String::new())
    } else {
        input.unknown_fields.remove("source");
        (input, value.to_string())
    }
}

fn open_sync_settings(server_id: &str, page: &ServerPage) {
    let Ok(reg) = crate::registry::load() else {
        return;
    };
    let Some(server) = reg.servers.iter().find(|s| s.id == server_id) else {
        return;
    };
    let Some(parent) = page.app.active_window() else {
        return;
    };
    #[allow(deprecated)]
    let dialog = adw::MessageDialog::new(Some(&parent), Some(&crate::i18n::tr("Sync settings")), Some(&crate::i18n::tr("Only values marked nonsecret can be the same on every machine. Secret values and approvals always stay on this machine.")));
    l10n::bind_dialog_heading(&dialog, "Sync settings");
    l10n::bind_dialog_body(&dialog, "Only values marked nonsecret can be the same on every machine. Secret values and approvals always stay on this machine.");
    let fields = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let local = l10n::check_button("Keep on this machine only");
    local.set_active(crate::personal_sync::keep_local(server));
    local.set_sensitive(crate::personal_sync::is_personal(&reg));
    fields.append(&local);
    fields.append(&{ let widget = gtk::Label::builder().label(crate::i18n::tr("Stops future sync for this server on this machine. Copies already on your other machines stay there.")).wrap(true).xalign(0.0).build(); l10n::bind_label(&widget, "Stops future sync for this server on this machine. Copies already on your other machines stay there."); widget });
    let mut portable = Vec::new();
    for (kind, key, label, secret, unknown) in server
        .env
        .iter()
        .map(|i| ("env", &i.key, &i.key, i.secret, &i.unknown_fields))
        .chain(
            server
                .launch
                .iter()
                .flat_map(|l| &l.inputs)
                .map(|i| ("input", &i.key, &i.label, i.secret, &i.unknown_fields)),
        )
    {
        if secret || unknown.contains_key("source") {
            continue;
        }
        let check = gtk::CheckButton::with_label(&crate::i18n::trf(
            "{value0}: same on every machine",
            &[("value0", &label.to_string())],
        ));
        check.set_active(unknown.get("portable") == Some(&serde_json::json!(true)));
        fields.append(&check);
        portable.push((kind.to_string(), key.clone(), check));
    }
    dialog.set_extra_child(Some(&fields));
    dialog.add_response("cancel", &crate::i18n::tr("Cancel"));
    l10n::bind_dialog_response(&dialog, "cancel", "Cancel");
    dialog.add_response("save", &crate::i18n::tr("Save"));
    l10n::bind_dialog_response(&dialog, "save", "Save");
    dialog.set_default_response(Some("save"));
    dialog.set_close_response("cancel");
    let id = server_id.to_string();
    let page = page.clone();
    let personal = crate::personal_sync::is_personal(&reg);
    dialog.connect_response(None, move |_, response| {
        if response != "save" {
            return;
        }
        let id = id.clone();
        let page = page.clone();
        let local = local.is_active();
        let marks: Vec<_> = portable
            .iter()
            .map(|(k, key, c)| (k.clone(), key.clone(), c.is_active()))
            .collect();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                if personal {
                    crate::personal_sync::set_local_only(&id, local)?;
                }
                for (kind, key, enabled) in marks {
                    crate::personal_sync::set_portable(&id, &kind, &key, enabled)?;
                }
                crate::registry::load()
            })
            .await;
            match result {
                Ok(Ok(reg)) => page.render(state::RegistryState::Ready(
                    state::RegistrySnapshot::from_registry(reg),
                )),
                Ok(Err(e)) => page.restore_after_error(&e),
                Err(_) => {
                    page.restore_after_error(&crate::i18n::tr("Could not save sync settings"))
                }
            }
        });
    });
    dialog.present();
}

fn save_launch_entries(
    server_id: &str,
    values: &[(crate::registry::LaunchInput, String)],
    mut registry: crate::registry::Registry,
) -> Result<crate::registry::Registry, String> {
    for (input, value) in values {
        if let Some(reference) =
            crate::secret_refs::source(&input.unknown_fields).map_err(|e| e.to_string())?
        {
            registry =
                crate::registry_controller::set_secret_reference(server_id, &input.key, reference)?;
        } else if input.secret {
            if value.is_empty() {
                registry = crate::registry::update(|reg| {
                    crate::registry_controller::apply_launch_secret_generation(
                        reg, server_id, &input.key,
                    )
                })?
                .0;
            } else {
                registry =
                    crate::registry_controller::set_launch_secret(server_id, &input.key, value)?;
            }
        } else if input.value.as_deref().unwrap_or("") != value {
            registry = crate::registry_controller::set_launch_input_value(
                server_id,
                &input.key,
                (!value.is_empty()).then_some(value.clone()),
            )?;
        }
    }
    Ok(registry)
}

fn editor_field_data(label: &str, child: &impl IsA<gtk::Widget>) -> gtk::Box {
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

fn editor_field(label: &'static str, child: &impl IsA<gtk::Widget>) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 6);
    row.append(&{
        let widget = gtk::Label::builder()
            .label(label)
            .halign(gtk::Align::Start)
            .css_classes(["toolport-field-label"])
            .build();
        l10n::bind_label(&widget, label);
        widget
    });
    row.append(child);
    row
}

/// What the editor reports after filling the form from a pasted snippet.
fn snippet_fill_feedback(name: &str, server_count: usize, env_count: usize) -> String {
    let mut message = if server_count > 1 {
        crate::i18n::trf(
            "Found {value0} servers; filled \"{value1}\". Add the rest separately.",
            &[("value0", &format!("{}", server_count)), ("value1", name)],
        )
    } else {
        crate::i18n::trf("Parsed \"{value0}\" from the snippet.", &[("value0", name)])
    };
    if env_count > 0 {
        message.push_str(&crate::i18n::trf(if env_count == 1 { crate::i18n::msg(" {value0} credential value will be stored in the system keychain when you add the server.") } else { crate::i18n::msg(" {value0} credential values will be stored in the system keychain when you add the server.") }, &[("value0", &format!("{}", env_count))]));
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
        message.push_str(&crate::i18n::trf(
            ". Add a value for {value0} in Credentials",
            &[("value0", &declared_without_value.join(", ").to_string())],
        ));
    }
    if !failed_env.is_empty() {
        message.push_str(&crate::i18n::trf(
            if failed_env.len() == 1 {
                crate::i18n::msg(". Could not store {value0}; use Credentials to add it")
            } else {
                crate::i18n::msg(". Could not store {value0}; use Credentials to add them")
            },
            &[("value0", &failed_env.join(", ").to_string())],
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
    inherit_env: &gtk::Switch,
) -> crate::registry_controller::ServerFields {
    crate::registry_controller::ServerFields {
        sync_local_only: false,
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
        inherit_env: Some(inherit_env.is_active()),
    }
}

fn editor_intro(icon: &str, title: &'static str, subtitle: &'static str) -> gtk::Box {
    let intro = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    intro.add_css_class("toolport-editor-intro");
    let icon = gtk::Image::from_icon_name(icon);
    icon.add_css_class("toolport-editor-icon");
    intro.append(&icon);
    let copy = gtk::Box::new(gtk::Orientation::Vertical, 3);
    copy.set_hexpand(true);
    copy.append(&{
        let widget = gtk::Label::builder()
            .label(title)
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["title-2"])
            .build();
        l10n::bind_label(&widget, title);
        widget
    });
    copy.append(&{
        let widget = gtk::Label::builder()
            .label(subtitle)
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-muted"])
            .build();
        l10n::bind_label(&widget, subtitle);
        widget
    });
    intro.append(&copy);
    intro
}

fn section_heading(title: &'static str, subtitle: &'static str) -> gtk::Box {
    let heading = gtk::Box::new(gtk::Orientation::Vertical, 2);
    heading.append(&{
        let widget = gtk::Label::builder()
            .label(title)
            .halign(gtk::Align::Start)
            .css_classes(["toolport-section-label"])
            .build();
        l10n::bind_label(&widget, title);
        widget
    });
    heading.append(&{
        let widget = gtk::Label::builder()
            .label(subtitle)
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-muted", "caption"])
            .build();
        l10n::bind_label(&widget, subtitle);
        widget
    });
    heading
}

/// One editable plain environment value in Edit server.
#[derive(Clone)]
struct PlainEnvRow {
    row: gtk::Box,
    key: gtk::Entry,
    value: gtk::Entry,
    local: Option<gtk::CheckButton>,
    /// Set once the user flips "This machine only"; until then the box follows
    /// the saved choice, or sync's default for a new name.
    touched: std::rc::Rc<std::cell::Cell<bool>>,
}

type PlainEnvRows = std::rc::Rc<std::cell::RefCell<Vec<PlainEnvRow>>>;

fn plain_env_section(values: &[state::PlainEnvView], personal: bool) -> (gtk::Box, PlainEnvRows) {
    let section = gtk::Box::new(gtk::Orientation::Vertical, 8);
    section.add_css_class("toolport-form-section");
    section.append(&section_heading(crate::i18n::msg("Environment"), crate::i18n::msg("Plain settings the command reads, such as a region or a feature flag. Keys and tokens belong in Credentials.")));
    let list = gtk::Box::new(gtk::Orientation::Vertical, 8);
    section.append(&list);
    let rows: PlainEnvRows = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    for saved in values {
        add_plain_env_row(&list, &rows, Some(saved.clone()), personal);
    }
    let add = l10n::button("Add value");
    add.add_css_class("toolport-secondary-action");
    add.set_halign(gtk::Align::Start);
    let list_for_add = list.clone();
    let rows_for_add = rows.clone();
    add.connect_clicked(move |_| {
        let key = add_plain_env_row(&list_for_add, &rows_for_add, None, personal);
        key.grab_focus();
    });
    section.append(&add);
    (section, rows)
}

fn add_plain_env_row(
    list: &gtk::Box,
    rows: &PlainEnvRows,
    saved: Option<state::PlainEnvView>,
    personal: bool,
) -> gtk::Entry {
    let row = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let key = gtk::Entry::builder()
        .text(saved.as_ref().map(|s| s.key.as_str()).unwrap_or(""))
        .placeholder_text("NAME")
        .width_chars(16)
        .css_classes(["toolport-input", "monospace"])
        .build();
    let value = {
        let widget = gtk::Entry::builder()
            .text(saved.as_ref().map(|s| s.value.as_str()).unwrap_or(""))
            .placeholder_text(crate::i18n::tr("Value"))
            .hexpand(true)
            .css_classes(["toolport-input"])
            .build();
        l10n::bind_placeholder(&widget, "Value");
        widget
    };
    let remove = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text(crate::i18n::tr("Remove this value"))
        .valign(gtk::Align::Center)
        .css_classes(["flat", "toolport-destructive-icon"])
        .build();
    line.append(&key);
    line.append(&value);
    line.append(&remove);
    row.append(&line);
    let touched = std::rc::Rc::new(std::cell::Cell::new(false));
    let local = personal.then(|| {
        let check = l10n::check_button("This machine only");
        row.append(&check);
        let setting = std::rc::Rc::new(std::cell::Cell::new(false));
        let refresh = {
            let (key, value, check, touched, setting) = (
                key.clone(),
                value.clone(),
                check.clone(),
                touched.clone(),
                setting.clone(),
            );
            move || {
                let name = key.text();
                let name = name.trim();
                let risky = crate::personal_sync::risky_sync_env(name);
                let active = if risky {
                    true
                } else if touched.get() {
                    check.is_active()
                } else {
                    match &saved {
                        Some(saved) if saved.key == name => saved.portable != Some(true),
                        _ => crate::personal_sync::looks_machine_local(name, &value.text()),
                    }
                };
                setting.set(true);
                check.set_active(active);
                setting.set(false);
                check.set_sensitive(!risky);
                check.set_tooltip_text(Some(&if risky {
                    crate::i18n::tr("This variable changes how programs run, so it always stays on this machine.")
                } else {
                    crate::i18n::tr("Keep this value off your other machines.")
                }));
            }
        };
        refresh();
        let refresh = std::rc::Rc::new(refresh);
        let on_key = refresh.clone();
        key.connect_changed(move |_| on_key());
        value.connect_changed(move |_| refresh());
        let touched = touched.clone();
        check.connect_toggled(move |_| {
            if !setting.get() {
                touched.set(true);
            }
        });
        check
    });
    list.append(&row);
    rows.borrow_mut().push(PlainEnvRow {
        row: row.clone(),
        key: key.clone(),
        value,
        local,
        touched,
    });
    let list = list.clone();
    let rows_for_remove = rows.clone();
    remove.connect_clicked(move |_| {
        list.remove(&row);
        rows_for_remove.borrow_mut().retain(|r| r.row != row);
    });
    key
}

fn collect_plain_env(rows: &PlainEnvRows) -> Vec<crate::registry_controller::PlainEnvEdit> {
    rows.borrow()
        .iter()
        .map(|row| crate::registry_controller::PlainEnvEdit {
            key: row.key.text().to_string(),
            value: row.value.text().to_string(),
            local_only: row
                .local
                .as_ref()
                .filter(|_| row.touched.get())
                .map(gtk::CheckButton::is_active),
        })
        .collect()
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

fn remove_server_copy(server_id: &str) -> &'static str {
    let syncs = crate::registry::load().is_ok_and(|r| {
        crate::personal_sync::is_personal(&r)
            && r.servers
                .iter()
                .find(|s| s.id == server_id)
                .is_some_and(|s| !crate::personal_sync::keep_local(s))
    });
    if syncs {
        crate::i18n::msg("This removes the server from every profile and your other machines. This cannot be undone.")
    } else {
        crate::i18n::msg(
            "This removes the server from every profile on this machine. This cannot be undone.",
        )
    }
}
fn confirm_remove_server(server_id: &str, server_name: &str, page: ServerPage) {
    let Some(parent) = page.app.active_window() else {
        return;
    };
    #[allow(deprecated)]
    let dialog = adw::MessageDialog::new(
        Some(&parent),
        Some(&crate::i18n::trf(
            "Remove {value0}?",
            &[("value0", server_name)],
        )),
        Some(&crate::i18n::lookup(remove_server_copy(server_id))),
    );
    l10n::bind_formatted_dialog_heading(&dialog, "Remove {value0}?", &[("value0", server_name)]);
    l10n::bind_dialog_body(&dialog, remove_server_copy(server_id));
    dialog.add_response("cancel", &crate::i18n::tr("Cancel"));
    l10n::bind_dialog_response(&dialog, "cancel", "Cancel");
    dialog.add_response("remove", &crate::i18n::tr("Remove"));
    l10n::bind_dialog_response(&dialog, "remove", "Remove");
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
                    page.show_confirmation(&crate::i18n::trf(
                        "Removed {value0}",
                        &[("value0", &server_name.to_string())],
                    ));
                }
                Ok(Err(error)) => {
                    page.restore_after_error(&crate::i18n::trf(
                        "Could not remove {value0}: {value1}",
                        &[
                            ("value0", &server_name.to_string()),
                            ("value1", &error.to_string()),
                        ],
                    ));
                }
                Err(_) => page.restore_after_error(&crate::i18n::trf(
                    "Could not remove {value0}: the operation stopped",
                    &[("value0", &server_name.to_string())],
                )),
            }
        });
        dialog.close();
    });
    dialog.present();
}

fn state_card(icon_name: &str, title: &'static str, body: &'static str, error: bool) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
    card.add_css_class("toolport-state-card");
    if error {
        card.add_css_class("error");
    }
    let icon = gtk::Image::from_icon_name(icon_name);
    icon.set_pixel_size(32);
    icon.add_css_class("toolport-state-icon");
    card.append(&icon);
    card.append(&{
        let widget = gtk::Label::builder()
            .label(title)
            .wrap(true)
            .justify(gtk::Justification::Center)
            .css_classes(["heading"])
            .build();
        l10n::bind_label(&widget, title);
        widget
    });
    card.append(&{
        let widget = gtk::Label::builder()
            .label(body)
            .wrap(true)
            .justify(gtk::Justification::Center)
            .max_width_chars(52)
            .css_classes(["toolport-muted"])
            .build();
        l10n::bind_label(&widget, body);
        widget
    });
    card
}

fn state_card_formatted(
    icon: &str,
    title: &'static str,
    source: &'static str,
    values: &[(&str, &str)],
    error: bool,
) -> gtk::Box {
    let card = state_card(icon, title, "", error);
    let body = card
        .last_child()
        .and_downcast::<gtk::Label>()
        .expect("state card body label");
    l10n::bind_formatted_label(&body, source, values);
    card
}

#[cfg(test)]
mod tests {
    #[test]
    fn immediate_shutdown_drains_allow_save_without_polling_future() {
        use super::*;
        let data = crate::registry::DataDirTestEnv::new("gtk-immediate-allow-save");
        let lock = crate::registry::lock_at(&data.dir.join("registry.json")).unwrap();
        let save = persist_always_allow("fixture/read@hash".into());
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            drop(lock);
        });
        // No GTK iteration and no poll of the completion future before shutdown.
        finish_allow_saves();
        release.join().unwrap();
        drop(save);
        let registry = crate::registry::load().unwrap();
        assert!(registry.is_tool_allowed("fixture/read@hash"));
    }

    #[test]
    fn approval_persistence_and_pair_reads_do_not_block_main_context() {
        use super::*;
        let data = crate::registry::DataDirTestEnv::new("gtk-approval-pair-workers");
        let context = gtk::glib::MainContext::new();
        context
            .with_thread_default(|| {
                context.block_on(async {
                    let lock = crate::registry::lock_at(&data.dir.join("registry.json")).unwrap();
                    let persisted = std::rc::Rc::new(std::cell::Cell::new(false));
                    let done = persisted.clone();
                    let writer = gtk::glib::spawn_future_local(async move {
                        persist_always_allow("fixture/read@hash".into())
                            .await
                            .unwrap();
                        done.set(true);
                    });
                    let paired = std::rc::Rc::new(std::cell::Cell::new(false));
                    let done = paired.clone();
                    let reader = gtk::glib::spawn_future_local(async move {
                        assert!(
                            !read_pair_target("https://team.invalid".into(), "fixture".into())
                                .await
                                .unwrap()
                        );
                        done.set(true);
                    });
                    gtk::glib::timeout_future(std::time::Duration::from_millis(80)).await;
                    assert!(
                        !persisted.get() && !paired.get(),
                        "registry IO waits on the worker while the context advances"
                    );
                    drop(lock);
                    writer.await.unwrap();
                    reader.await.unwrap();
                    assert!(persisted.get() && paired.get());
                });
            })
            .unwrap();
        assert!(crate::registry::load()
            .unwrap()
            .is_tool_allowed("fixture/read@hash"));
    }

    #[test]
    #[ignore = "requires an isolated GTK desktop without a tray host; run in omabox"]
    fn hidden_fallback_handles_both_startup_activation_orders() {
        use super::*;
        let data = crate::registry::DataDirTestEnv::new("gtk-startup-activation-orders");
        std::fs::write(data.dir.join("gtk-onboarding-complete"), "complete").unwrap();
        adw::init().unwrap();
        gtk::glib::MainContext::default().block_on(async {
            for activate_first in [true, false] {
                let app = adw::Application::builder()
                    .application_id(if activate_first {
                        "com.tsout.Toolport.TestStartupEarly"
                    } else {
                        "com.tsout.Toolport.TestStartupLate"
                    })
                    .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
                    .build();
                app.register(gtk::gio::Cancellable::NONE).unwrap();
                let activation = std::rc::Rc::new(NativeActivation {
                    state: Default::default(),
                    requested: Default::default(),
                    present: Default::default(),
                    urls: Default::default(),
                    launch_hidden: true,
                    fallback_scheduled: std::cell::Cell::new(false),
                });
                let handler = activation.clone();
                app.connect_activate(move |app| {
                    handler.activate(app, std::time::Duration::from_millis(80));
                });
                if activate_first {
                    app.activate();
                    assert!(activation.requested.get());
                    assert!(
                        app.windows().is_empty(),
                        "early activation queues until readiness"
                    );
                }
                activation.ready(
                    &app,
                    Ok(NativeStartup {
                        window: WindowStartup {
                            paired: false,
                            saved_state: None,
                            removed: Vec::new(),
                            access_notice: false,
                            recovery: None,
                            writers: Vec::new(),
                        },
                        broker: crate::approval_broker::start_native(),
                        bridge: http_bridge::BridgeController::default(),
                    }),
                );
                if !activate_first {
                    assert!(
                        app.windows().is_empty(),
                        "readiness waits for the first activation"
                    );
                    app.activate();
                }
                let window = app.windows().into_iter().next().unwrap();
                assert!(
                    !window.is_visible(),
                    "both orders preserve the initial hidden launch"
                );
                assert!(activation.fallback_scheduled.get());
                gtk::glib::timeout_future(std::time::Duration::from_millis(150)).await;
                assert!(
                    window.is_visible(),
                    "no tray host must reveal either activation order"
                );
                window.destroy();
                if let Some(Ok(startup)) = activation.state.borrow_mut().take() {
                    for writer in startup.window.writers {
                        writer.finish();
                    }
                    startup.bridge.shutdown();
                    startup.broker.clear_endpoint();
                }
                gtk::glib::timeout_future(std::time::Duration::from_millis(50)).await;
            }
        });
    }

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn registry_failure_window_close_quits_even_with_application_hold() {
        use super::*;
        let _data = crate::registry::DataDirTestEnv::new("gtk-startup-error-close");
        adw::init().unwrap();
        let app = adw::Application::builder()
            .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(gtk::gio::Cancellable::NONE).unwrap();
        let _hold = app.hold();
        let timed_out = std::rc::Rc::new(std::cell::Cell::new(false));
        let failed = timed_out.clone();
        let timeout_app = app.clone();
        let timeout =
            gtk::glib::timeout_add_local_once(std::time::Duration::from_secs(2), move || {
                failed.set(true);
                timeout_app.quit();
            });
        app.connect_activate(|app| {
            show_registry_startup_failure(app, "fixture unreadable registry");
            let window = app.active_window().unwrap();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(50), move || {
                window.close()
            });
        });
        app.run_with_args::<&str>(&[]);
        assert!(
            !timed_out.get(),
            "window close must quit despite the background hold"
        );
        timeout.remove();
    }

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn superseded_pair_link_does_not_open_a_pairing_window() {
        use super::*;
        let data = crate::registry::DataDirTestEnv::new("gtk-pair-link-stale");
        adw::init().unwrap();
        let app = adw::Application::builder()
            .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(gtk::gio::Cancellable::NONE).unwrap();
        let broker = crate::approval_broker::start_native();
        let (root, page, _) = build_content(&app, broker);
        let window = adw::ApplicationWindow::builder()
            .application(&app)
            .content(&root)
            .build();
        let requests = page.share_requests.clone();
        io::on_window_closed(&window, move || requests.stop());
        let lock = crate::registry::lock_at(&data.dir.join("registry.json")).unwrap();
        open_shared_setup(
            "toolport://teams/connect?origin=https%3A%2F%2Fteam.invalid&team=fixture",
            page.clone(),
        );
        gtk::glib::MainContext::default().block_on(async {
            gtk::glib::timeout_future(std::time::Duration::from_millis(80)).await;
            open_shared_setup("invalid", page.clone());
            assert_eq!(page.feedback.text(), "The shared setup link was invalid.");
            drop(lock);
            gtk::glib::timeout_future(std::time::Duration::from_millis(150)).await;
            assert_eq!(
                app.windows().len(),
                1,
                "the older pair read cannot open a dialog"
            );
            assert_eq!(page.feedback.text(), "The shared setup link was invalid.");
            window.destroy();
            assert!(!page.share_requests.is_current(page.share_requests.begin()));
        });
    }

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn tool_change_history_and_details_build_only_on_expand() {
        use super::*;
        let _data = crate::registry::DataDirTestEnv::new("gtk-lazy-security-history");
        adw::init().unwrap();
        let app = adw::Application::builder().build();
        let page = ActivityPage::new(&app);
        let mut snapshot = state::load_activity_snapshot().unwrap();
        snapshot.security_events = (0..2000).map(|i| serde_json::json!({
            "type":"tool_drift", "server":"srv", "tool":format!("srv__read_{i}"),
            "change":"changed", "severity":"info", "ts":1, "blocked":false, "historical":true,
        })).collect();
        *page.last_snapshot.borrow_mut() = Some(snapshot);
        page.render_security();
        let history = page
            .security_list
            .first_child()
            .unwrap()
            .downcast::<gtk::Expander>()
            .unwrap();
        let rows = history.child().unwrap().downcast::<gtk::Box>().unwrap();
        assert!(
            rows.first_child().is_none(),
            "collapsed history must build no group cards"
        );
        history.set_expanded(true);
        let card = rows.first_child().unwrap().downcast::<gtk::Box>().unwrap();
        let reveal = card
            .last_child()
            .unwrap()
            .downcast::<gtk::Revealer>()
            .unwrap();
        let details = reveal.child().unwrap().downcast::<gtk::Box>().unwrap();
        assert!(
            details.first_child().is_none(),
            "collapsed groups must build no tool cards"
        );
        let heading = card
            .first_child()
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap();
        heading.emit_clicked();
        let mut count = 0;
        let mut child = details.first_child();
        while let Some(row) = child {
            count += 1;
            child = row.next_sibling();
        }
        assert_eq!(count, 2000);
        let first = details.first_child().unwrap();
        heading.emit_clicked();
        heading.emit_clicked();
        assert_eq!(
            details.first_child(),
            Some(first),
            "reopening reuses the built cards"
        );
        history.set_expanded(false);
        history.set_expanded(true);
        assert_eq!(
            rows.first_child(),
            Some(card.upcast()),
            "history reuses its group cards"
        );
    }

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn personal_sync_rows_keep_menu_focus_icon_and_notice_space() {
        adw::init().unwrap();
        let _data = crate::registry::DataDirTestEnv::new("gtk-personal-sync-rows");
        let app = adw::Application::builder()
            .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(gtk::gio::Cancellable::NONE).unwrap();
        let (root, page, _) = super::build_content(&app, crate::approval_broker::start_native());
        let window = adw::ApplicationWindow::builder()
            .application(&app)
            .content(&root)
            .build();
        let mut reg = crate::registry::Registry { team: Some(serde_json::from_value(serde_json::json!({"teamId":"solo","role":"admin","serverUrl":"http://127.0.0.1:1","reportingDeviceId":"fixture","accountStatus":{"personalSync":true,"plan":"pro","canReceiveConfig":true}})).unwrap()), servers: ["A", "B"].into_iter().map(|name| serde_json::from_value(serde_json::json!({"id":name,"name":name,"transport":"http","url":"https://example.com/mcp","env":[],"enabled":false})).unwrap()).collect(), ..Default::default() };
        crate::registry::save(&reg).unwrap();
        page.render(super::state::RegistryState::Ready(
            super::state::RegistrySnapshot::from_registry(reg.clone()),
        ));
        window.present();
        let context = gtk::glib::MainContext::default();
        while context.pending() {
            context.iteration(false);
        }
        let card = page.rows.borrow()[0].1.clone();
        let mut queue = vec![card.clone().upcast::<gtk::Widget>()];
        let menu = loop {
            let w = queue.pop().expect("server menu");
            if let Ok(menu) = w.clone().downcast::<gtk::MenuButton>() {
                break menu;
            }
            let mut child = w.first_child();
            while let Some(w) = child {
                child = w.next_sibling();
                queue.push(w);
            }
        };
        menu.popup();
        while context.pending() {
            context.iteration(false);
        }
        let popover = menu.popover().unwrap();
        assert!(popover.is_visible());
        page.render(super::state::RegistryState::Ready(
            super::state::RegistrySnapshot::from_registry(reg.clone()),
        ));
        assert_eq!(page.rows.borrow()[0].1, card);
        assert!(popover.is_visible());
        reg.servers[1].name = "B changed".into();
        page.render(super::state::RegistryState::Ready(
            super::state::RegistrySnapshot::from_registry(reg.clone()),
        ));
        assert!(popover.is_visible());
        assert_eq!(page.rows.borrow()[0].1, card);
        menu.popdown();
        while context.pending() {
            context.iteration(false);
        }
        assert_eq!(page.rows.borrow()[1].0.name, "B changed");
        menu.grab_focus();
        reg.servers[0].name = "A changed".into();
        page.render(super::state::RegistryState::Ready(
            super::state::RegistrySnapshot::from_registry(reg),
        ));
        assert!(gtk::prelude::RootExt::focus(&window)
            .is_some_and(|w| w.is_ancestor(&page.rows.borrow()[0].1)));
        let nav = gtk::Button::new();
        let children = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let icon = gtk::Image::from_icon_name("system-users-symbolic");
        let label = gtk::Label::new(Some("Sign in to sync"));
        children.append(&icon);
        children.append(&label);
        nav.set_child(Some(&children));
        super::set_nav_label(&nav, "Sync (1)");
        assert_eq!(label.label(), "Sync (1)");
        assert_eq!(children.first_child(), Some(icon.upcast()));
        page.show_confirmation("Saved");
        let height = page.feedback.height_request();
        page.hide_feedback();
        assert!(page.feedback.is_visible());
        assert_eq!(page.feedback.height_request(), height);
        assert!(height >= 48);
        window.close();
    }

    #[test]
    fn returning_to_keychain_with_blank_launch_input_clears_reference() {
        let input:crate::registry::LaunchInput=serde_json::from_value(serde_json::json!({"key":"TOKEN","label":"Token","secret":true,"required":true,"source":{"ref":"op://v/i/key"}})).unwrap();
        let (edited, value) = super::launch_editor_value(&input, "", None);
        assert_eq!(value, "");
        assert!(!edited.unknown_fields.contains_key("source"));
        let mut reg = crate::registry::Registry::default();
        reg.servers.push(serde_json::from_value(serde_json::json!({"id":"ref","name":"Ref","transport":"stdio","command":"fixture","env":[],"launch":{"inputs":[input],"bindings":[]}})).unwrap());
        crate::registry_controller::apply_launch_secret_generation(&mut reg, "ref", "TOKEN")
            .unwrap();
        assert!(!reg.servers[0].launch.as_ref().unwrap().inputs[0]
            .unknown_fields
            .contains_key("source"));
    }
    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn savings_old_estimate_visual_fixture() {
        use super::*;
        let _data = crate::registry::DataDirTestEnv::new("savings-old-estimate-visual");
        adw::init().unwrap();
        let app = adw::Application::builder()
            .application_id("com.tsout.Toolport.SavingsFixture")
            .build();
        app.register(None::<&gtk::gio::Cancellable>).unwrap();
        let page = ActivityPage::new(&app);
        let mut history = state::load_activity_snapshot().unwrap();
        history.savings_historical_loads = 5884;
        history.savings_previous_tokens = 195_640_064;
        page.render(history.clone());
        assert_eq!(page.savings_value.text(), "Not measured yet");
        assert!(page.savings_old_estimate.is_visible());
        assert!(page
            .savings_old_estimate
            .text()
            .contains("195,640,064 tokens"));
        assert!(!page.savings_share.is_visible());
        let mut mixed = history.clone();
        mixed.tokens_saved = -10;
        mixed.savings_tokenized_loads = 1;
        mixed.savings_list_loads = 2;
        mixed.savings_previous_tokens = -291;
        page.render(mixed.clone());
        assert_eq!(page.savings_value.text(), "-10");
        assert!(page.savings_old_estimate.text().contains("-291 tokens"));
        assert!(page.savings_share.is_visible());
        page.render(
            if std::env::var("TOOLPORT_SAVINGS_FIXTURE").as_deref() == Ok("mixed") {
                mixed
            } else {
                history
            },
        );
        let parent = page
            .savings_banner
            .parent()
            .unwrap()
            .downcast::<gtk::Box>()
            .unwrap();
        parent.remove(&page.savings_banner);
        let panel = gtk::Box::new(gtk::Orientation::Vertical, 12);
        panel.set_margin_top(20);
        panel.set_margin_bottom(20);
        panel.set_margin_start(20);
        panel.set_margin_end(20);
        panel.append(&page.savings_banner);
        let counting = page
            .savings_banner
            .last_child()
            .unwrap()
            .downcast::<gtk::Expander>()
            .unwrap();
        assert!(!counting.is_expanded());
        let window = adw::ApplicationWindow::builder()
            .application(&app)
            .title("Toolport savings fixture")
            .default_width(1000)
            .default_height(400)
            .content(&panel)
            .build();
        theme::ThemeController::new().attach(&window);
        window.present();
        let main_loop = gtk::glib::MainLoop::new(None, false);
        let stop = main_loop.clone();
        gtk::glib::timeout_add_local_once(std::time::Duration::from_secs(30), move || stop.quit());
        main_loop.run();
        window.close();
    }

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn activity_polish_visual_fixture() {
        use super::*;
        let data = crate::registry::DataDirTestEnv::new("activity-polish-visual");
        adw::init().unwrap();
        let app = adw::Application::builder()
            .application_id("com.tsout.Toolport.PolishFixture")
            .build();
        app.register(None::<&gtk::gio::Cancellable>).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let rows = [
            serde_json::json!({"ts":now-60000,"server":"GitHub","tool":"list_issues","ok":true,"client":"adapter:codex","clientName":"Codex","durationMs":12,"piiReplaced":1}),
            serde_json::json!({"ts":now,"kind":"internal","server":"toolport","tool":"search","ok":true,"client":"adapter:codex","clientName":"Codex","durationMs":5}),
        ];
        std::fs::write(
            data.dir.join("audit.jsonl"),
            rows.iter()
                .map(|row| format!("{row}\n"))
                .collect::<String>(),
        )
        .unwrap();
        let activity = ActivityPage::new(&app);
        let mut snapshot = state::load_activity_snapshot().unwrap();
        snapshot.tokens_saved = 35100;
        snapshot.savings_tokenized_loads = 12;
        snapshot.savings_measured_loads = 12;
        snapshot.savings_peak_catalog = 75;
        snapshot.savings_full_bytes = 180000;
        snapshot.savings_exposed_bytes = 15600;
        snapshot.savings_avoided_bytes = 164400;
        snapshot.savings_since_ts = now - 86400000;
        assert_eq!(snapshot.call_count, 1);
        assert_eq!(snapshot.server_stats.len(), 1);
        assert_eq!(snapshot.server_stats[0]["calls"], 1);
        activity.render(snapshot);
        let counting = activity
            .savings_banner
            .last_child()
            .unwrap()
            .downcast::<gtk::Expander>()
            .unwrap();
        assert!(!counting.is_expanded());
        assert!(activity.savings_detail.text().contains("cl100k_base"));
        assert!(!activity.savings_detail.text().contains("bytes/4"));
        let server_page = ServerPage {
            app: app.clone(),
            share_requests: Default::default(),
            server_count: gtk::Label::new(None),
            enabled_count: gtk::Label::new(None),
            section_title: gtk::Label::new(None),
            posture: gtk::Label::new(None),
            search: gtk::SearchEntry::new(),
            feedback: gtk::Label::new(None),
            list: gtk::Box::new(gtk::Orientation::Vertical, 0),
            last_snapshot: Default::default(),
            feedback_timer: Default::default(),
            health_rows: Default::default(),
            rows: Default::default(),
            no_matches: Default::default(),
            off_heading: Default::default(),
            health: Default::default(),
        };
        let clients = ClientPage::new(&app, server_page);
        let fixture_clients = [
            ("codex", "Codex", state::ClientGatewayState::Connected),
            ("cursor", "Cursor", state::ClientGatewayState::Disconnected),
            (
                "claude-code",
                "Claude Code",
                state::ClientGatewayState::Customized,
            ),
        ]
        .into_iter()
        .map(|(id, name, gateway_state)| state::ClientView {
            id: id.into(),
            name: name.into(),
            app_present: true,
            config_exists: true,
            uses_connectors: false,
            server_count: 2,
            movable_server_count: 0,
            other_server_count: 2,
            gateway_state,
            shared_http: false,
            legacy_bearer_argv: false,
            scope_id: None,
            scope_name: None,
            discovery_mode: Some("lazy".into()),
            inherited_discovery: None,
            config_error: false,
            location_removable: false,
        })
        .collect();
        clients.render(state::ClientSnapshot { clients: fixture_clients, profiles: Vec::new(), sessions_error: false, sessions: vec![serde_json::json!({"clientName":"Codex","clientType":"codex","sessionCount":5,"callsToday":60,"lastActiveMs":now-180000,"firstCatalogSize":1711})] });
        let approvals = gtk::Box::new(gtk::Orientation::Vertical, 12);
        approvals.set_margin_top(20);
        approvals.set_margin_bottom(20);
        approvals.set_margin_start(20);
        approvals.set_margin_end(20);
        approvals.append(
            &gtk::Label::builder()
                .label("Action required")
                .xalign(0.0)
                .css_classes(["title-2"])
                .build(),
        );
        let broker = crate::approval_broker::start_native();
        let approval_page = ApprovalPage::new(&app, broker);
        for (index, name) in ["inbox", "inbox (reported)", "Unrecorded client"]
            .into_iter()
            .enumerate()
        {
            let view = crate::approval_broker::PendingView {
                id: format!("polish-{index}"),
                client: None,
                client_name: name.into(),
                client_label: None,
                server: "GitHub".into(),
                tool: "delete_issue".into(),
                tool_fingerprint: None,
                reason: crate::approval::ApprovalReason::Destructive,
                arguments: serde_json::json!({"issue":42}),
                url_elicitation: None,
                pii_release: None,
                deadline_ms: now + 120000,
            };
            let (card, countdown) = approval_card(view, approval_page.clone());
            countdown.set_label("120s left");
            approvals.append(&card);
        }
        let approval_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&approvals)
            .build();
        let stack = gtk::Stack::new();
        stack.add_titled(&clients.root, Some("clients"), "Clients");
        stack.add_titled(&activity.root, Some("activity"), "Activity");
        stack.add_titled(&approval_scroll, Some("approvals"), "Approvals");
        stack.set_vexpand(true);
        let switcher = gtk::StackSwitcher::builder()
            .stack(&stack)
            .halign(gtk::Align::Center)
            .build();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&switcher);
        root.append(&stack);
        let window = adw::ApplicationWindow::builder()
            .application(&app)
            .title("Toolport polish fixture")
            .default_width(1280)
            .default_height(800)
            .content(&root)
            .build();
        let theme = theme::ThemeController::new();
        theme.attach(&window);
        window.present();
        let main_loop = gtk::glib::MainLoop::new(None, false);
        let closed = main_loop.clone();
        window.connect_close_request(move |_| {
            closed.quit();
            gtk::glib::Propagation::Proceed
        });
        let stop = main_loop.clone();
        gtk::glib::timeout_add_local_once(std::time::Duration::from_secs(180), move || stop.quit());
        main_loop.run();
    }

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn session_identity_visual_fixture() {
        use super::*;
        let _data = crate::registry::DataDirTestEnv::new("f3-gtk-identity");
        adw::init().unwrap();
        let app = adw::Application::builder()
            .application_id("com.tsout.Toolport.SessionFixture")
            .build();
        app.register(None::<&gtk::gio::Cancellable>).unwrap();
        let broker = crate::approval_broker::start_native();
        let approval = ApprovalPage::new(&app, broker);
        let view = crate::approval_broker::PendingView {
            id: "fixture-approval".into(),
            client: None,
            client_name: "Unknown app (via Cursor)".into(),
            client_label: Some("kt 1".into()),
            server: "team-slack".into(),
            tool: "delete_issue".into(),
            tool_fingerprint: None,
            reason: crate::approval::ApprovalReason::Destructive,
            arguments: serde_json::json!({"issue":42}),
            url_elicitation: None,
            pii_release: None,
            deadline_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64
                + 120_000,
        };
        let (card, _) = approval_card(view, approval);
        let mut activity = call("team-slack", false);
        activity.client = Some("Unknown app (via Cursor)".into());
        activity.client_label = Some("kt 1".into());
        activity.tool = "delete_issue".into();
        activity.duration_ms = Some(120);
        activity.dispatch_ms = Some(3);
        activity.cold = Some(false);
        activity.failure_kind = Some("downstream_failure".into());
        activity.run_id = Some("0123456789abcdef0123456789abcdef".into());
        let server_page = ServerPage {
            app: app.clone(),
            share_requests: Default::default(),
            server_count: gtk::Label::new(None),
            enabled_count: gtk::Label::new(None),
            section_title: gtk::Label::new(None),
            posture: gtk::Label::new(None),
            search: gtk::SearchEntry::new(),
            feedback: gtk::Label::new(None),
            list: gtk::Box::new(gtk::Orientation::Vertical, 0),
            last_snapshot: Default::default(),
            feedback_timer: Default::default(),
            health_rows: Default::default(),
            rows: Default::default(),
            no_matches: Default::default(),
            off_heading: Default::default(),
            health: Default::default(),
        };
        let clients = ClientPage::new(&app, server_page);
        clients.render(state::ClientSnapshot { clients: Vec::new(), profiles: Vec::new(), sessions_error: false, sessions: vec![serde_json::json!({
            "clientName":"Unknown app (via Cursor)", "clientLabel":"kt 1", "phase":"close",
            "toolsListCount":3, "listChangedCount":1, "firstCatalogSize":4, "contentChanged":true,
        })] });
        clients
            .list
            .parent()
            .unwrap()
            .downcast::<gtk::Box>()
            .unwrap()
            .remove(&clients.list);
        let root = gtk::Box::new(gtk::Orientation::Vertical, 18);
        root.set_margin_top(24);
        root.set_margin_bottom(24);
        root.set_margin_start(24);
        root.set_margin_end(24);
        for (title, widget) in [
            ("Approval required", card.upcast::<gtk::Widget>()),
            ("Activity", activity_card(&activity).upcast()),
            ("Clients", clients.list.clone().upcast()),
        ] {
            root.append(
                &gtk::Label::builder()
                    .label(title)
                    .halign(gtk::Align::Start)
                    .css_classes(["title-2"])
                    .build(),
            );
            root.append(&widget);
        }
        fn labels(widget: &gtk::Widget, out: &mut Vec<String>) {
            if let Some(label) = widget.downcast_ref::<gtk::Label>() {
                out.push(label.text().to_string());
            }
            let mut child = widget.first_child();
            while let Some(w) = child {
                labels(&w, out);
                child = w.next_sibling();
            }
        }
        let mut text = Vec::new();
        labels(root.upcast_ref(), &mut text);
        assert!(text.iter().any(|t| t == "Unknown app (via Cursor)"));
        assert!(text.iter().any(|t| t == "Reports itself as: kt 1"));
        assert!(text.iter().any(|t| t.contains("dispatch 3 ms")));
        assert!(
            text.iter()
                .position(|t| t.contains("Unknown app (via Cursor)"))
                .unwrap()
                < text.iter().position(|t| t.contains("kt 1")).unwrap()
        );
        if std::env::var_os("TOOLPORT_F3_VISUAL").is_some() {
            let window = adw::ApplicationWindow::builder()
                .application(&app)
                .title("Toolport session observability")
                .default_width(1040)
                .default_height(860)
                .content(&root)
                .build();
            let theme = theme::ThemeController::new();
            theme.attach(&window);
            window.present();
            let main_loop = gtk::glib::MainLoop::new(None, false);
            let stop = main_loop.clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_secs(45), move || {
                stop.quit()
            });
            main_loop.run();
        }
    }

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn client_rows_keep_disconnected_copy_short_and_badges_compact() {
        use super::*;
        adw::init().unwrap();
        let app = adw::Application::builder()
            .application_id("com.tsout.Toolport.ClientRowsFixture")
            .build();
        let server_page = ServerPage {
            app: app.clone(),
            share_requests: Default::default(),
            server_count: gtk::Label::new(None),
            enabled_count: gtk::Label::new(None),
            section_title: gtk::Label::new(None),
            posture: gtk::Label::new(None),
            search: gtk::SearchEntry::new(),
            feedback: gtk::Label::new(None),
            list: gtk::Box::new(gtk::Orientation::Vertical, 0),
            last_snapshot: Default::default(),
            feedback_timer: Default::default(),
            health_rows: Default::default(),
            rows: Default::default(),
            no_matches: Default::default(),
            off_heading: Default::default(),
            health: Default::default(),
        };
        let page = ClientPage::new(&app, server_page);
        let mut client = state::ClientView {
            id: "cursor".into(),
            name: "Cursor".into(),
            app_present: true,
            config_exists: true,
            uses_connectors: false,
            server_count: 2,
            movable_server_count: 0,
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
        };
        let mut heights = Vec::new();
        for state in [
            state::ClientGatewayState::Disconnected,
            state::ClientGatewayState::Customized,
            state::ClientGatewayState::Connected,
        ] {
            client.gateway_state = state;
            let row = client_card(&client, page.clone());
            let copy = row.first_child().unwrap().next_sibling().unwrap();
            let name = copy.first_child().unwrap();
            assert!(name.tooltip_text().unwrap().contains("Client id: cursor"));
            let detail = name.next_sibling().unwrap();
            let text = detail.clone().downcast::<gtk::Label>().unwrap().text();
            if state == state::ClientGatewayState::Connected {
                assert_eq!(text, "Toolport gateway · 2 other servers · stdio");
            } else {
                assert!(text.starts_with("2 local MCP servers"));
            }
            assert_eq!(
                detail.next_sibling().is_some(),
                state == state::ClientGatewayState::Connected
            );
            let after_copy = copy.next_sibling().unwrap();
            if state == state::ClientGatewayState::Customized {
                let badge = after_copy.clone().downcast::<gtk::Label>().unwrap();
                assert_eq!(badge.text(), "Customized");
                assert_eq!(badge.valign(), gtk::Align::Center);
                assert_eq!(badge.halign(), gtk::Align::Start);
            } else {
                assert!(!after_copy.has_css_class("toolport-badge"));
            }
            if state == state::ClientGatewayState::Disconnected {
                assert_eq!(after_copy.valign(), gtk::Align::Center);
                let button = after_copy
                    .first_child()
                    .unwrap()
                    .downcast::<gtk::Button>()
                    .unwrap();
                assert_eq!(button.label().as_deref(), Some("Connect"));
            }
            heights.push(row.measure(gtk::Orientation::Vertical, 880).1);
        }
        assert!(
            heights[0] < heights[2],
            "Disconnected rows should be shorter: {heights:?}"
        );
    }

    #[test]
    fn gtk_legacy_reset_preserves_scope_and_uses_one_migration_action() {
        use super::*;
        let mut client = state::ClientView {
            id: "cursor".into(),
            name: "Cursor".into(),
            app_present: true,
            config_exists: true,
            uses_connectors: false,
            server_count: 1,
            movable_server_count: 0,
            other_server_count: 2,
            gateway_state: state::ClientGatewayState::Customized,
            shared_http: true,
            legacy_bearer_argv: true,
            scope_id: Some("work".into()),
            scope_name: Some("Work".into()),
            discovery_mode: None,
            inherited_discovery: None,
            config_error: false,
            location_removable: false,
        };
        let mut registry = crate::registry::Registry::default();
        registry.profiles[0].id = "work".into();
        registry.profiles[0].name = "Work".into();
        registry.set_client_scope(&client.id, client.scope_id.as_deref());
        let scope = client_reset_scope(&client);
        crate::registry_controller::apply_client_stdio_update(
            &mut registry,
            &client.id,
            scope.as_deref(),
            None,
        );
        assert_eq!(
            registry.client_scopes.get(&client.id),
            Some(&"work".to_string())
        );
        assert!(!client_needs_reset_action(&client));
        assert_eq!(client_reset_label(&client), "Migrate to stdio");
        client.legacy_bearer_argv = false;
        assert!(client_needs_reset_action(&client));
        assert_eq!(client_reset_label(&client), "Reset and connect");
        client.scope_id = Some(registry.all_access_id());
        let scope = client_reset_scope(&client);
        crate::registry_controller::apply_client_stdio_update(
            &mut registry,
            &client.id,
            scope.as_deref(),
            None,
        );
        assert_eq!(
            registry.client_scopes.get(&client.id),
            Some(&registry.all_access_id())
        );
        client.scope_id = Some(String::new());
        let scope = client_reset_scope(&client);
        crate::registry_controller::apply_client_stdio_update(
            &mut registry,
            &client.id,
            scope.as_deref(),
            None,
        );
        assert_eq!(registry.client_scopes.get(&client.id), Some(&String::new()));
        let access = registry.resolve_profile_id(registry.client_scopes.get(&client.id).unwrap());
        assert_eq!(access, registry.default_access_id());
        assert_ne!(access, registry.all_access_id());
    }

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn reference_controls_test_reports_status_without_showing_value() {
        use adw::prelude::*;
        adw::init().unwrap();
        let key = "TOOLPORT_NATIVE_REFERENCE_TEST_483762";
        std::env::set_var(key, "synthetic-native-key-value");
        let feedback = gtk::Label::new(None);
        let (choice, controls, reference) =
            super::secret_reference_fields("", Some(&format!("env:{key}")), &feedback);
        assert_eq!(choice.selected(), 1);
        assert!(controls.is_visible());
        let provider = controls
            .first_child()
            .unwrap()
            .downcast::<gtk::DropDown>()
            .unwrap();
        assert_eq!(
            provider.selected(),
            (crate::secret_refs::PROVIDERS.len() - 1) as u32
        );
        let test = controls
            .last_child()
            .unwrap()
            .first_child()
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap();
        test.emit_clicked();
        let context = gtk::glib::MainContext::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !test.is_sensitive() && std::time::Instant::now() < deadline {
            context.iteration(false);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        std::env::remove_var(key);
        assert!(test.is_sensitive(), "reference test must finish");
        assert_eq!(feedback.text(), "Success in the desktop app environment. The MCP client gateway may use different environment variables or PATH.");
        assert_eq!(reference.text(), format!("env:{key}"));
        assert!(!feedback.text().contains("synthetic-native-key-value"));
        provider.set_selected(0);
        assert_eq!(reference.text(), "");
        assert_eq!(
            reference.placeholder_text().as_deref(),
            Some(crate::secret_refs::PROVIDERS[0].example)
        );
        choice.set_selected(0);
        assert!(!controls.is_visible());
    }

    #[test]
    fn launch_reference_edit_discards_pasted_value_and_switches_back_to_vault() {
        let input = crate::registry::LaunchInput {
            key: "TOKEN".into(),
            label: "Key".into(),
            secret: true,
            required: true,
            value: None,
            unknown_fields: Default::default(),
        };
        let (mut edited, value) =
            super::launch_editor_value(&input, "synthetic-pasted-value", Some("env:TOKEN"));
        assert!(value.is_empty());
        super::apply_launch_probe_value(&mut edited, &value);
        assert!(edited.value.is_none());
        assert_eq!(
            crate::secret_refs::source(&edited.unknown_fields).unwrap(),
            Some("env:TOKEN")
        );
        assert!(!serde_json::to_string(&edited)
            .unwrap()
            .contains("synthetic-pasted-value"));
        let (mut pasted, value) = super::launch_editor_value(&edited, "replacement", None);
        super::apply_launch_probe_value(&mut pasted, &value);
        assert!(crate::secret_refs::source(&pasted.unknown_fields)
            .unwrap()
            .is_none());
        assert_eq!(pasted.value.as_deref(), Some("replacement"));
    }

    #[test]
    fn star_prompt_is_spent_once_even_without_a_click() {
        let dir = preview_scratch("star-prompt");
        let marker = dir.join(".gtk-star-prompt-done");
        assert!(spend_star_prompt(&marker));
        assert!(!spend_star_prompt(&marker));
        assert!(!spend_star_prompt(&dir.join("missing/marker")));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn star_prompt_waits_for_value_and_an_unobstructed_window() {
        let entries = vec![
            serde_json::json!({"ts":86400000,"ok":true}),
            serde_json::json!({"ts":172800000,"ok":true}),
        ];
        assert!(!star_value_reached(2, &entries));
        assert!(!star_value_reached(3, &entries[..1]));
        assert!(star_value_reached(3, &entries));
        assert!(!star_value_reached(
            3,
            &[
                entries[0].clone(),
                serde_json::json!({"ts":172800000,"ok":true,"kind":"approval"})
            ]
        ));
        assert!(!star_should_show(true, true, false));
        assert!(!star_should_show(true, false, true));
        assert!(!star_should_show(false, true, true));
        assert!(star_should_show(true, true, true));
    }

    /// The 2.0 sidebar is the four fixed views in order, with Team appended only
    /// on a paired install. Catalog is no longer a row even though
    /// their pages and actions stay reachable.
    #[test]
    fn sidebar_rows_are_the_four_views_plus_team_only_when_paired() {
        let rows = |paired| {
            super::NAV_SECTIONS
                .iter()
                .map(|(target, _, _)| *target)
                .filter(|target| super::is_sidebar_row(target, paired))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            rows(false),
            vec!["servers", "clients", "activity", "settings", "teams"]
        );
        assert_eq!(
            rows(true),
            vec!["servers", "clients", "activity", "settings", "teams"]
        );
    }

    #[test]
    fn sidebar_shortcuts_number_only_the_fixed_views() {
        assert_eq!(
            super::NAV_SHORTCUTS,
            ["servers", "clients", "activity", "settings"].as_slice()
        );
    }

    #[test]
    fn cleared_launch_field_is_missing_in_native_probe_but_blank_secret_keeps_vault() {
        let mut input = crate::registry::LaunchInput {
            key: "DIRECTORY".into(),
            label: "Directory".into(),
            secret: false,
            required: true,
            value: Some("/previous/directory".into()),
            unknown_fields: Default::default(),
        };
        super::apply_launch_probe_value(&mut input, "");
        assert_eq!(input.value, None, "the probe must use the cleared field");
        input.secret = true;
        super::apply_launch_probe_value(&mut input, "");
        assert_eq!(
            input.value, None,
            "blank saved secrets resolve from the vault"
        );
        super::apply_launch_probe_value(&mut input, "unsaved-secret");
        assert_eq!(input.value.as_deref(), Some("unsaved-secret"));
    }

    fn preview_scratch(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "toolport-preview-migration-{}-{label}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    /// An upgraded preview install must not keep a login entry pointing at a
    /// binary the package no longer installs, and launch-at-login must survive
    /// the rename rather than being silently switched off.
    #[test]
    fn retiring_the_preview_carries_launch_at_login_across() {
        let dir = preview_scratch("both");
        let autostart = dir.join("ToolportNativePreview.desktop");
        let handler = dir.join("com.tsout.Toolport.NativePreview.desktop");
        std::fs::write(&autostart, "[Desktop Entry]").unwrap();
        std::fs::write(&handler, "[Desktop Entry]").unwrap();

        let carried = std::cell::Cell::new(false);
        let retired = retire_preview_files(Some(&autostart), &handler, || carried.set(true));

        assert!(
            carried.get(),
            "the new autostart entry must be written first"
        );
        assert_eq!(
            retired,
            RetiredPreview {
                autostart: true,
                handler: true
            }
        );
        assert!(!autostart.exists() && !handler.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Someone who never turned launch-at-login on has no preview autostart
    /// file, and the migration must not create one for them.
    #[test]
    fn retiring_the_preview_does_not_switch_launch_at_login_on() {
        let dir = preview_scratch("handler-only");
        let autostart = dir.join("ToolportNativePreview.desktop");
        let handler = dir.join("com.tsout.Toolport.NativePreview.desktop");
        std::fs::write(&handler, "[Desktop Entry]").unwrap();

        let carried = std::cell::Cell::new(false);
        let retired = retire_preview_files(Some(&autostart), &handler, || carried.set(true));

        assert!(
            !carried.get(),
            "no preview entry means it was never enabled"
        );
        assert_eq!(
            retired,
            RetiredPreview {
                autostart: false,
                handler: true
            }
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A fresh install has neither file, and the migration must be a no-op
    /// rather than reporting work it did not do.
    #[test]
    fn retiring_the_preview_on_a_fresh_install_does_nothing() {
        let dir = preview_scratch("fresh");
        let retired = retire_preview_files(
            Some(&dir.join("ToolportNativePreview.desktop")),
            &dir.join("com.tsout.Toolport.NativePreview.desktop"),
            || panic!("must not touch autostart when the preview never ran"),
        );
        assert_eq!(retired, RetiredPreview::default());
        let _ = std::fs::remove_dir_all(&dir);
    }
    use super::*;

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn activity_revision_worker_refreshes_rewrites_and_releases_hidden_state() {
        adw::init().unwrap();
        let data = crate::registry::DataDirTestEnv::new("activity-revision-worker");
        let path = data.dir.join("audit.jsonl");
        let first = "{\"ts\":1,\"tool\":\"server__first\",\"server\":\"server\",\"ok\":true}\n";
        let second = first.replace("first", "other");
        assert_eq!(first.len(), second.len());
        std::fs::write(&path, first).unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let app = adw::Application::builder()
            .application_id("com.tsout.ActivityRevisionTest")
            .build();
        app.register(gtk::gio::Cancellable::NONE).unwrap();
        let page = ActivityPage::new(&app);
        let window = adw::ApplicationWindow::builder()
            .application(&app)
            .content(&page.root)
            .build();
        page.attach(&window);
        window.present();
        let settle = || {
            gtk::glib::MainContext::default().block_on(async {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                loop {
                    gtk::glib::timeout_future(std::time::Duration::from_millis(10)).await;
                    if !page.loading.get() && !page.revision_loading.get() && !page.dirty.get() {
                        break;
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "Activity worker did not settle"
                    );
                }
            });
        };
        settle();
        assert_eq!(
            page.last_snapshot.borrow().as_ref().unwrap().recent[0].tool,
            "server__first"
        );
        let before = page.revision.borrow().clone();
        std::fs::write(&path, second).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        page.check_revision();
        settle();
        assert_ne!(*page.revision.borrow(), before);
        assert_eq!(
            page.last_snapshot.borrow().as_ref().unwrap().recent[0].tool,
            "server__other"
        );
        // Begin another worker then hide before its UI completion is delivered.
        page.check_revision();
        window.set_visible(false);
        gtk::glib::MainContext::default().block_on(async {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while page.revision_loading.get() || page.loading.get() || page.health_loading.get() {
                gtk::glib::timeout_future(std::time::Duration::from_millis(10)).await;
                assert!(
                    std::time::Instant::now() < deadline,
                    "Hidden workers did not finish"
                );
            }
        });
        assert!(page.revision.borrow().is_none());
        assert!(page.last_snapshot.borrow().is_none());
        assert!(page.list.first_child().is_none());
        window.close();
        crate::observability_controller::release_activity_revision();
    }

    #[test]
    #[ignore = "requires isolated GTK and the disposable read-gate interposer; run in omabox"]
    fn activity_unmap_during_real_digest_io_is_nonblocking() {
        adw::init().unwrap();
        let data = crate::registry::DataDirTestEnv::new("activity-slow-digest");
        crate::observability_controller::release_activity_revision();
        let watches = || {
            std::fs::read_dir("/proc/self/fd")
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| {
                    std::fs::read_link(entry.path())
                        .is_ok_and(|path| path.to_string_lossy().contains("inotify"))
                })
                .count()
        };
        let path = data.dir.join("audit.jsonl");
        std::fs::write(
            &path,
            "{\"ts\":1,\"tool\":\"server__first\",\"server\":\"server\",\"ok\":true}\n",
        )
        .unwrap();
        let entered = data.dir.join("entered");
        let permit = data.dir.join("permit");
        assert!(std::env::var("LD_PRELOAD")
            .unwrap_or_default()
            .contains("read-gate"));
        std::env::set_var("REVIEW_HOOK_TARGET", &path);
        std::env::set_var("REVIEW_HOOK_ENTERED", &entered);
        std::env::set_var("REVIEW_HOOK_PERMIT", &permit);
        let app = adw::Application::builder()
            .application_id("com.tsout.ActivitySlowDigestTest")
            .build();
        app.register(gtk::gio::Cancellable::NONE).unwrap();
        let page = ActivityPage::new(&app);
        let window = adw::ApplicationWindow::builder()
            .application(&app)
            .content(&page.root)
            .build();
        page.attach(&window);
        window.present();
        gtk::glib::MainContext::default().block_on(async {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while !entered.exists() {
                gtk::glib::timeout_future(std::time::Duration::from_millis(10)).await;
                assert!(
                    std::time::Instant::now() < deadline,
                    "real digest read did not enter gate"
                );
            }
        });
        let in_flight_watches = watches();
        // An independent supervisor prevents a broken implementation hanging the
        // test forever. It does not open the gate until the UI has had one second.
        let proceeded = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = proceeded.clone();
        let supervisor = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(1));
            let before_io_returned = observed.load(std::sync::atomic::Ordering::SeqCst);
            std::fs::write(permit, "go").unwrap();
            before_io_returned
        });
        let start = std::time::Instant::now();
        window.set_visible(false);
        let unmap = start.elapsed();
        assert!(page.last_snapshot.borrow().is_none());
        assert!(page.list.first_child().is_none());
        gtk::glib::MainContext::default().block_on(async {
            gtk::glib::timeout_future(std::time::Duration::from_millis(10)).await;
            proceeded.store(true, std::sync::atomic::Ordering::SeqCst);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while page.loading.get() || page.revision_loading.get() {
                gtk::glib::timeout_future(std::time::Duration::from_millis(10)).await;
                assert!(
                    std::time::Instant::now() < deadline,
                    "hidden worker did not finish"
                );
            }
        });
        assert!(
            supervisor.join().unwrap(),
            "UI stalled until digest IO returned"
        );
        assert!(
            unmap < std::time::Duration::from_millis(100),
            "unmap waited {unmap:?}"
        );
        assert_eq!(
            watches(),
            in_flight_watches - 1,
            "cancelled owner must close its watch"
        );
        assert!(page.revision.borrow().is_none());
        assert!(page.last_snapshot.borrow().is_none());
        assert!(page.list.first_child().is_none());
        println!(
            "slow real digest: unmap={unmap:?}, UI proceeded before read permit, watch closed"
        );
        window.close();
        for key in [
            "REVIEW_HOOK_TARGET",
            "REVIEW_HOOK_ENTERED",
            "REVIEW_HOOK_PERMIT",
        ] {
            std::env::remove_var(key);
        }
    }

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn activity_updates_retain_widgets_and_release_hidden_rows() {
        adw::init().unwrap();
        let data = crate::registry::DataDirTestEnv::new("activity-retained-rows");
        let app = adw::Application::builder()
            .application_id("com.tsout.ActivityRowsTest")
            .build();
        let page = ActivityPage::new(&app);
        assert_eq!(page.security_status_title.text(), "Checking protection…");
        assert!(!page.security_status_detail.text().contains("No changes"));
        std::fs::write(
            data.dir.join("audit.jsonl"),
            "{\"ts\":1,\"tool\":\"server__first\",\"server\":\"server\",\"ok\":true}\n",
        )
        .unwrap();
        page.render(state::load_activity_snapshot().unwrap());
        let old = page.list.first_child().unwrap();
        let stats = page.stats_list.first_child().unwrap();
        assert!(
            page.root.measure(gtk::Orientation::Horizontal, -1).0 <= 480,
            "Activity must fit a 480px window with loaded counts"
        );
        page.apply_health_notices(vec![
            "Gateway telemetry health is unavailable. Activity and savings may be incomplete."
                .into(),
        ]);
        assert!(page
            .telemetry_feedback
            .text()
            .contains("health is unavailable"));
        assert_eq!(page.list.first_child().unwrap(), old);
        assert_eq!(page.stats_list.first_child().unwrap(), stats);
        page.show_error("retained store unreadable");
        assert!(page.feedback.text().contains("Could not refresh"));
        page.set_revision_error(Some("revision read failed".into()));
        page.apply_health_notices(Vec::new());
        assert!(page
            .telemetry_feedback
            .text()
            .contains("Activity change checks are unavailable"));
        page.render(state::load_activity_snapshot().unwrap());
        assert!(page
            .telemetry_feedback
            .text()
            .contains("revision read failed"));
        page.set_revision_error(None);
        assert!(!page.telemetry_feedback.is_visible());
        page.render(state::load_activity_snapshot().unwrap());
        assert_eq!(page.list.first_child().unwrap(), old);
        assert_eq!(page.stats_list.first_child().unwrap(), stats);
        std::fs::write(data.dir.join("audit.jsonl"), "{\"ts\":1,\"tool\":\"server__first\",\"server\":\"server\",\"ok\":true}\n{\"ts\":2,\"tool\":\"server__next\",\"server\":\"server\",\"ok\":true}\n").unwrap();
        page.render(state::load_activity_snapshot().unwrap());
        assert_eq!(
            page.list.first_child().unwrap().next_sibling().unwrap(),
            old
        );
        assert_eq!(page.recent_rows.borrow().len(), 2);
        page.errors_only.set_active(true);
        assert!(page.recent_rows.borrow().is_empty());
        page.errors_only.set_active(false);
        assert_eq!(page.recent_rows.borrow().len(), 2);
        page.release_rows();
        assert!(page.last_snapshot.borrow().is_none());
        assert!(page.recent_rows.borrow().is_empty());
        assert!(page.stat_rows.borrow().is_empty());
        assert!(page.list.first_child().is_none());
        assert_eq!(page.security_status_title.text(), "Checking protection…");
        page.show_error("unreadable");
        assert_eq!(
            page.security_status_title.text(),
            "Protection state unavailable"
        );
    }

    #[test]
    fn unchanged_activity_sections_do_not_rebuild_open_rows() {
        let rows = vec![serde_json::json!({"ts": 1, "tool": "linear__save_issue"})];
        assert!(!activity_section_changed(Some(&rows), &rows));
        assert!(activity_section_changed(None, &rows));
        assert!(activity_section_changed(
            Some(&rows),
            &[serde_json::json!({"ts": 2, "tool": "linear__save_issue"})]
        ));
    }

    #[test]
    fn rebuilt_activity_rows_restore_each_expansion_independently() {
        let mut expanded = std::collections::HashSet::new();
        remember_activity_expansion(&mut expanded, "security:linear", true);
        remember_activity_expansion(&mut expanded, "trace:42", true);
        assert!(expanded.contains("security:linear"));
        assert!(expanded.contains("trace:42"));

        remember_activity_expansion(&mut expanded, "security:linear", false);
        assert!(!expanded.contains("security:linear"));
        assert!(expanded.contains("trace:42"));
    }

    #[test]
    fn drift_review_explains_changed_fields_and_annotation_values() {
        let event = serde_json::json!({
            "type": "tool_drift",
            "change": "changed",
            "changed_fields": ["description", "annotations"],
            "prev_ro": true,
            "new_ro": false,
            "prev_dh": null,
            "new_dh": null
        });
        assert_eq!(
            security_review_lines(&event),
            vec![
                "This tool changed since it was last accepted.",
                "Changed field: Description",
                "Changed field: Safety hints",
                "readOnlyHint: true to false",
                "Parameter details were not saved for this older change.",
                "Blocking status unavailable. Review in Settings > Quarantined tools.",
            ]
        );
    }

    #[test]
    fn tool_change_review_lists_parameter_deltas_and_requires_the_displayed_block() {
        let event = serde_json::json!({"type":"tool_drift", "change":"changed", "changed_fields":["input_schema"], "parameters":{"added":["comment"],"removed":["old_id"],"changed":["ttl"]}, "blocked":false});
        let lines = security_review_lines(&event);
        assert!(lines.contains(&"Added parameters: comment".to_string()));
        assert!(lines.contains(&"Removed parameters: old_id".to_string()));
        assert!(lines.contains(&"Changed parameters: ttl".to_string()));
        assert!(lines.contains(&"Not blocked.".to_string()));
        assert!(change_can_be_accepted(&event));
        let mut blocked = event;
        blocked["blocked"] = serde_json::json!(true);
        blocked["blocked_profiles"] = serde_json::json!(["work"]);
        assert!(!change_can_be_accepted(&blocked));
        blocked["new_fp"] = serde_json::json!("v2:reviewed");
        assert!(change_can_be_accepted(&blocked));
        blocked["blocked"] = serde_json::Value::Null;
        assert!(!change_can_be_accepted(&blocked));
    }

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn captured_activity_review_routes_to_settings_without_release() {
        let _data = crate::registry::DataDirTestEnv::new("gtk-captured-activity-routing");
        adw::init().unwrap();
        let app = adw::Application::builder()
            .application_id("com.toolport.DriftReviewTest")
            .build();
        app.register(None::<&gtk::gio::Cancellable>).unwrap();
        let reviewed = std::rc::Rc::new(std::cell::Cell::new(0));
        let calls = reviewed.clone();
        let action = gtk::gio::SimpleAction::new("review-quarantine", None);
        action.connect_activate(move |_, _| calls.set(calls.get() + 1));
        app.add_action(&action);
        let page = ActivityPage::new(&app);
        let event = serde_json::json!({"type":"tool_drift", "server":"srv", "tool":"srv__update", "ts":100, "change":"changed", "blocked":true, "blocked_profiles":["work"], "new_fp":"v3:captured", "review_required":true});
        let group = crate::integrity::group_tool_changes(std::slice::from_ref(&event)).remove(0);
        let card = tool_change_group_card(&group, page.clone());
        let window = adw::ApplicationWindow::builder()
            .application(&app)
            .content(&card)
            .build();
        let mut child = card.first_child();
        let mut found = false;
        while let Some(widget) = child {
            child = widget.next_sibling();
            if let Ok(button) = widget.downcast::<gtk::Button>() {
                if button.label().as_deref() == Some("Accept all for this server") {
                    assert!(!button.is_sensitive());
                }
                if button.label().as_deref() == Some("Review in Settings") {
                    button.emit_clicked();
                    found = true;
                }
            }
        }
        assert!(found);
        assert_eq!(reviewed.get(), 1);
        // Guard the callback too: an accidental direct invocation must only navigate.
        page.accept_security(vec![event]);
        assert_eq!(reviewed.get(), 2);
        window.close();
    }

    #[test]
    fn captured_activity_review_requires_settings_instead_of_summary_acceptance() {
        let event = serde_json::json!({"type":"tool_drift", "server":"srv", "tool":"srv__update", "ts":100, "change":"changed", "blocked":true, "blocked_profiles":["work"], "new_fp":"v3:captured", "review_required":true});
        assert!(!change_can_be_accepted(&event));
        let group = crate::integrity::group_tool_changes(std::slice::from_ref(&event)).remove(0);
        assert!(!group_can_be_accepted(&group));
        assert!(security_review_lines(&event)
            .iter()
            .any(|line| line.contains("Review the captured definition in Settings")));
    }

    #[test]
    fn legacy_quarantine_review_stays_in_the_main_view_and_cannot_be_accepted() {
        let data = crate::registry::DataDirTestEnv::new("gtk-legacy-quarantine-review");
        let mut registry = crate::registry::Registry::default();
        registry.profiles.push(crate::registry::Profile {
            id: "work".into(),
            name: "Work".into(),
            enabled_server_ids: vec![],
            tool_scope: Default::default(),
            instructions: None,
            unknown_fields: Default::default(),
        });
        crate::registry::save(&registry).unwrap();
        let store = data.dir.join(format!(
            "quarantine-v2-{}.json",
            crate::registry::profile_store_key("work")
        ));
        std::fs::write(
            store,
            r#"{"srv__update":{"tool":"srv__update","change":"changed"}}"#,
        )
        .unwrap();
        for profile in [serde_json::Value::Null, serde_json::json!("work")] {
            let event = serde_json::json!({"type":"tool_drift", "tool":"srv__update", "server":"srv", "change":"changed", "profile":profile, "ts":1});
            let mut lines = vec![event.to_string()];
            lines.extend((2..=56).map(|ts| serde_json::json!({"type":"tool_drift", "tool":format!("other__read{ts}"), "server":"other", "ts":ts}).to_string()));
            std::fs::write(data.dir.join("security.jsonl"), lines.join("\n") + "\n").unwrap();
            let snapshot = state::load_activity_snapshot().unwrap();
            let old = snapshot.security_events.last().unwrap();
            assert!(old["blocked"].is_null());
            assert_ne!(old["historical"], true);
            assert!(!change_can_be_accepted(old));
            assert!(!group_can_be_accepted(
                &crate::integrity::group_tool_changes(std::slice::from_ref(old)).remove(0)
            ));
            assert!(security_review_lines(old)
                .iter()
                .any(|line| line.contains("Settings > Quarantined tools")));
        }
    }

    #[test]
    fn poison_flagged_drift_requires_per_tool_review_and_shows_signatures() {
        let event = serde_json::json!({"type":"tool_drift", "server":"srv", "tool":"srv__update", "ts":100, "blocked":true, "blocked_profiles":["work"], "new_fp":"v2:reviewed", "signatures":["instruction_override"]});
        let group = crate::integrity::group_tool_changes(std::slice::from_ref(&event)).remove(0);
        assert!(!group_can_be_accepted(&group));
        assert!(change_can_be_accepted(&event));
        assert!(security_review_lines(&event)
            .iter()
            .any(|line| line == "Matched signals: instruction override"));
        let mut no_details = event;
        no_details["signatures"] = serde_json::json!([]);
        assert!(!group_can_be_accepted(
            &crate::integrity::group_tool_changes(&[no_details]).remove(0)
        ));
    }

    #[test]
    fn legacy_drift_review_is_honest_about_missing_detail() {
        let event = serde_json::json!({
            "type": "tool_drift",
            "change": "changed",
            "prev_ro": null,
            "new_ro": null,
            "prev_dh": null,
            "new_dh": null
        });
        assert_eq!(
            security_review_lines(&event),
            vec![
                "This tool changed since it was last accepted.",
                "This older event does not contain field-level change details.",
                "Parameter details were not saved for this older change.",
                "Blocking status unavailable. Review in Settings > Quarantined tools.",
            ]
        );
    }

    #[test]
    fn injection_review_exposes_detection_context() {
        let event = serde_json::json!({
            "type": "result_injection_blocked",
            "signatures": ["instruction_override"],
            "score": 0.94,
            "evidence": "ignore previous instructions"
        });
        assert_eq!(
            security_review_lines(&event),
            vec![
                "Toolport blocked this result before it reached the requesting agent.",
                "Matched signals: instruction override",
                "Detection score: 0.94",
                "Matched text: ignore previous instructions",
            ]
        );
    }

    fn call(server: &str, ok: bool) -> state::ActivityView {
        state::ActivityView {
            internal: false,
            timestamp_ms: 0,
            server: server.to_string(),
            tool: "tool".to_string(),
            client: None,
            client_label: None,
            client_id: None,
            cold: None,
            dispatch_ms: None,
            failure_kind: None,
            run_id: None,
            approval_decision: None,
            ok,
            held: false,
            duration_ms: None,
            error: None,
            pii_replaced: None,
            pii_incomplete: false,
        }
    }

    #[test]
    fn p08b_r1_hyphenated_activity_filter_uses_one_server() {
        let _env = crate::registry::DataDirTestEnv::new("p08b-r1-gtk-filter");
        crate::audit::record_routed_call(
            &crate::registry::Registry::default(),
            "team-slack",
            "read",
            true,
            Some(850),
            None,
            Some("adapter:claude-code"),
            None,
            None,
            None,
            None,
        );
        crate::audit::record_decision(
            "team-slack",
            "delete",
            Some("adapter:claude-code"),
            None,
            "destructive",
            "denied",
            &serde_json::json!({}),
            Some(1500),
        );
        let snapshot = state::load_activity_snapshot().unwrap();
        assert_eq!(
            activity_server_filter_options(&snapshot.recent),
            vec!["All servers", "team_slack"]
        );
        assert_eq!(
            filter_calls(&snapshot.recent, Some("team_slack"), false).len(),
            snapshot.recent.len()
        );
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
        assert_eq!(
            activity_server_filter_options(&calls),
            vec!["All servers", "github", "jira"]
        );
    }

    #[test]
    fn p10c_approval_badges_and_error_filter_keep_outcomes_separate() {
        let mut events = Vec::new();
        for (decision, label) in [
            ("denied", "Denied"),
            ("no_response", "No answer"),
            ("withdrawn", "Withdrawn"),
            ("stale_state", "Changed after approval"),
            ("approved", "Approved"),
            ("unreachable", "No approver available"),
        ] {
            assert_eq!(approval_outcome(decision).0, label);
            let mut event = call("github", false);
            event.approval_decision = Some(decision.into());
            events.push(event);
        }
        events.push(call("github", false));
        assert_eq!(filter_calls(&events, None, false).len(), 7);
        assert_eq!(filter_calls(&events, None, true).len(), 1);
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
    fn recorded_call_summary_uses_plain_counts() {
        assert_eq!(
            activity_count_summary(1, 2, 2, false),
            "1 call recorded on this computer. Showing the latest 2."
        );
        assert_eq!(
            activity_count_summary(5225, 5, 5, false),
            "5,225 calls recorded on this computer. Showing the latest 5."
        );
        assert_eq!(
            activity_count_summary(5225, 10, 100, false),
            "5,225 calls recorded on this computer. Showing 10 of the latest 100."
        );
        assert_eq!(
            activity_count_summary(5225, 2, 5, true),
            "5,225 calls recorded on this computer. Showing 2 matching rows from the latest 5."
        );
    }

    #[test]
    fn old_savings_estimate_preserves_signed_history() {
        assert_eq!(savings_old_estimate_display(0), None);
        assert_eq!(
            savings_old_estimate_display(195_640_064).unwrap(),
            "Old estimate: 195,640,064 tokens\nFrom previous versions; excluded from new savings."
        );
        assert!(savings_old_estimate_display(-291)
            .unwrap()
            .starts_with("Old estimate: -291 tokens"));
        assert!(savings_old_estimate_display(i64::MIN)
            .unwrap()
            .starts_with("Old estimate: -9,223,372,036,854,775,808 tokens"));
    }

    #[test]
    fn savings_detail_and_share_describe_net_tokenized_exposure() {
        assert_eq!(
            savings_primary_display(-123, 1, 0),
            ("-123".into(), "net tokens saved")
        );
        assert!(!savings_banner_visible(0, 0));
        assert!(
            savings_banner_visible(0, 1),
            "discovery-only telemetry is visible"
        );
        assert!(savings_banner_visible(1, 0));
        assert_eq!(
            savings_primary_display(41_100, 12, 0),
            ("41.1k".into(), "net tokens saved")
        );
        assert_eq!(
            savings_primary_display(0, 0, 12_340),
            ("12.3 KB".into(), "discovery text returned")
        );
        assert_eq!(savings_title(12), "Net tool description savings");
        assert_eq!(savings_title(0), "Discovery payload returned");
        assert_eq!(format_byte_count(999_949), "999.9 KB");
        assert_eq!(format_byte_count(999_999), "1.0 MB");
        assert_eq!(format_byte_count(999_999_999), "1.0 GB");
        assert_eq!(format_byte_count(999_949_999_999), "999.9 GB");
        assert_eq!(format_byte_count(1_000_000_000_000), "1.0 TB");
        assert_eq!(
            savings_detail_line(12, 80, Some("Mar 4".to_string())),
            "12 catalog loads · peak 80 tools · since Mar 4"
        );
        assert_eq!(savings_detail_line(1, 3, None), "1 catalog load");
        assert_eq!(
            savings_share_line(41_100, 12, 0, 0),
            "Toolport recorded 41.1k net tokens saved. Full catalog baselines minus all catalog and search responses; counted locally with cl100k_base, not a billing figure. toolport.app"
        );
        assert_eq!(savings_share_line(0, 0, 3, 12_340), "Toolport recorded 3 discovery searches returning 12.3 KB of text at its MCP boundary. toolport.app");
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
            "Legacy schema-only estimates: ≈900 returned vs ≈42.0k catalog; search guidance text was not counted."
        );
        let miss = serde_json::json!({ "query": "nothing", "returned": 0, "total": 12 });
        assert_eq!(
            trace_summary_line(&miss),
            "\u{201c}nothing\u{201d} · no match"
        );
        assert!(trace_ranking_lines(&miss).is_empty());
        let measured = serde_json::json!({"returned":3, "responseContentBytes":24_000,
            "catalogSchemaBytes":5_400_000});
        assert_eq!(trace_token_line(&measured),
            "Returned 24.0 KB of discovery content containing 3 matching schemas; full scoped catalog schemas: 5.4 MB. ≈6.0k tokens (UTF-8 bytes ÷ 4).");
    }

    #[test]
    fn client_activity_and_internal_calls_use_plain_language() {
        let detail =
            client_activity_detail(&serde_json::json!({"callsToday":12,"firstCatalogSize":1711}));
        assert_eq!(
            detail,
            "Last active time unavailable · 12 calls today · Last saw 1,711 tools"
        );
        let mut row = call("toolport", true);
        row.internal = true;
        for (tool, title) in [
            ("search", "Searched tools"),
            ("describe", "Looked up a tool"),
            ("status", "Checked Toolport status"),
            ("fetch", "Retrieved a tool result"),
        ] {
            row.tool = tool.into();
            assert_eq!(activity_title(&row), title);
        }
        assert_eq!(pii_badge(Some(1), false).unwrap().0, "1 value masked");
    }
    #[test]
    fn pii_badges_stay_silent_until_the_pass_did_something_and_warn_on_fail_open() {
        assert_eq!(pii_badge(None, false), None);
        assert_eq!(pii_badge(Some(0), false), None);
        let (label, class, _) = pii_badge(Some(3), false).unwrap();
        assert_eq!((label.as_str(), class), ("3 values masked", "success"));
        let (label, class, _) = pii_badge(Some(2), true).unwrap();
        assert_eq!(
            (label.as_str(), class),
            ("2 values masked, incomplete", "review")
        );
        // Fail-open with zero replacements still warns: values reached the model.
        let (label, class, _) = pii_badge(Some(0), true).unwrap();
        assert_eq!(
            (label.as_str(), class),
            ("0 values masked, incomplete", "review")
        );
    }

    #[test]
    fn window_state_round_trips_and_rejects_degenerate_sizes() {
        let state = WindowState {
            width: 1280,
            height: 800,
        };
        let encoded = serde_json::to_string(&state).unwrap();
        assert_eq!(parse_window_state(&encoded), Some(state));
        assert_eq!(parse_window_state("{\"width\":10,\"height\":800}"), None);
        assert_eq!(parse_window_state("not json"), None);
    }

    #[test]
    fn security_review_scroll_aligns_the_header_instead_of_jumping_to_the_bottom() {
        assert_eq!(
            aligned_scroll_value(900.0, -300.0, 0.0, 2000.0, 600.0),
            588.0
        );
        assert_eq!(aligned_scroll_value(10.0, -100.0, 0.0, 2000.0, 600.0), 0.0);
        assert_eq!(
            aligned_scroll_value(1300.0, 400.0, 0.0, 2000.0, 600.0),
            1400.0
        );
    }

    #[test]
    fn approval_notifications_name_browser_handoffs_for_what_they_are() {
        let mut view = crate::approval_broker::PendingView {
            id: "1".into(),
            client: Some("claude".into()),
            client_label: None,
            client_name: "Claude Code".into(),
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
                "github / create_issue needs approval. Requested by Claude Code.".to_string()
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
                "github needs a browser action. Requested by Claude Code. Review it in Toolport."
                    .to_string()
            )
        );

        let queue = vec![
            view.clone(),
            crate::approval_broker::PendingView {
                id: "2".into(),
                tool: "delete_issue".into(),
                url_elicitation: None,
                ..view
            },
        ];
        assert_eq!(
            approval_queue_notification(&queue),
            Some((
                "Toolport: 2 approvals required".to_string(),
                "github needs a browser action. Requested by Claude Code. Review it in Toolport. 1 more is waiting."
                    .to_string()
            ))
        );
        assert_eq!(approval_queue_notification(&[]), None);
    }

    #[test]
    fn quarantine_alert_identity_is_stable_across_poll_order() {
        let a = serde_json::json!({
            "profile": "work",
            "server": "linear",
            "tool": "save_issue"
        });
        let b = serde_json::json!({
            "profile": "",
            "server": "github",
            "tool": "delete_repo"
        });
        assert_eq!(
            quarantine_signature(&[a.clone(), b.clone()]),
            quarantine_signature(&[b, a])
        );
    }

    #[test]
    fn security_watch_baselines_startup_and_only_announces_new_attention() {
        let injection = serde_json::json!({
            "ts": 1000,
            "type": "result_injection",
            "server": "linear",
            "tool": "get_issue",
            "change": "result",
            "severity": "high"
        });
        let (baseline, startup) =
            newly_observed_security_events(None, std::slice::from_ref(&injection), &[]);
        assert!(startup.is_empty());
        let (_, unchanged) =
            newly_observed_security_events(Some(&baseline), std::slice::from_ref(&injection), &[]);
        assert!(unchanged.is_empty());

        let mut later = injection.clone();
        later["ts"] = serde_json::json!(2000);
        let (_, newcomers) = newly_observed_security_events(Some(&baseline), &[later], &[]);
        assert_eq!(newcomers.len(), 1);
    }

    #[test]
    fn damaged_quarantine_store_keeps_security_feed_and_activity_available() {
        let _data_dir_lock = crate::registry::data_dir_test_lock();
        struct CleanupDir(std::path::PathBuf);
        impl Drop for CleanupDir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let dir = std::env::temp_dir().join(format!(
            "toolport-native-damaged-quarantine-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _cleanup = CleanupDir(dir.clone());
        let _override = crate::registry::DataDirOverride::set(&dir);
        let event = serde_json::json!({
            "ts": 1000,
            "type": "tool_drift",
            "server": "srv",
            "tool": "srv__wipe",
            "change": "changed",
            "severity": "high"
        });
        std::fs::write(dir.join("security.jsonl"), format!("{event}\n")).unwrap();
        std::fs::write(dir.join("quarantine.json"), "{ not json").unwrap();

        let (events, quarantined) =
            read_security_watch_snapshot().expect("the security feed remains readable");
        assert_eq!(events, vec![event]);
        assert!(quarantined.is_err(), "quarantine state remains unknown");
        let (_, newcomers) = newly_observed_security_events(
            Some(&std::collections::HashSet::new()),
            &events,
            quarantined.as_deref().unwrap_or_default(),
        );
        assert_eq!(newcomers.len(), 1, "a new finding must still alert");

        let activity = state::load_activity_snapshot().expect("Activity remains readable");
        assert!(
            activity
                .tool_identities_error
                .as_deref()
                .is_some_and(|error| error.contains("quarantine")),
            "the identity panel must say why it is unknown: {activity:?}"
        );
        let mut unknown_events = events.clone();
        unknown_events[0]["blocked"] = serde_json::Value::Null;
        assert_eq!(activity.security_events, unknown_events);

        std::fs::write(dir.join("quarantine.json"), "{}").unwrap();
        std::fs::write(dir.join("tool-pins.json"), "{ not json").unwrap();
        let activity = state::load_activity_snapshot().expect("pin damage stays panel-local");
        assert!(
            activity
                .tool_identities_error
                .as_deref()
                .is_some_and(|error| error.contains("pin store")),
            "a lost pin baseline must not look like an empty identity panel: {activity:?}"
        );
        assert!(
            crate::integrity::all_quarantined().is_err(),
            "a lost trust store cannot establish that nothing is blocked"
        );
        assert_eq!(activity.security_events, unknown_events);
    }

    #[test]
    fn quarantine_suppresses_duplicate_drift_alerts_but_not_injection_alerts() {
        let quarantine = serde_json::json!({
            "server": "linear",
            "tool": "linear__save_issue",
            "change": "changed"
        });
        let drift = serde_json::json!({
            "ts": 1000,
            "type": "tool_drift",
            "server": "linear",
            "tool": "linear__save_issue",
            "change": "changed",
            "severity": "high"
        });
        let injection = serde_json::json!({
            "ts": 1001,
            "type": "result_injection_blocked",
            "server": "linear",
            "tool": "linear__save_issue",
            "change": "result",
            "severity": "high"
        });
        assert!(security_event_covered_by_quarantine(
            &drift,
            std::slice::from_ref(&quarantine)
        ));
        assert!(!security_event_covered_by_quarantine(
            &injection,
            &[quarantine]
        ));
    }

    #[test]
    fn a_blocked_injection_is_one_user_facing_incident() {
        let detected = serde_json::json!({
            "ts": 1000,
            "type": "result_injection",
            "server": "linear",
            "tool": "get_issue",
            "change": "result",
            "severity": "high"
        });
        let blocked = serde_json::json!({
            "ts": 1001,
            "type": "result_injection_blocked",
            "server": "linear",
            "tool": "get_issue",
            "change": "result",
            "severity": "high"
        });
        let incidents = security_attention_incidents(&[detected, blocked.clone()]);
        assert_eq!(incidents, vec![blocked.clone()]);
        assert_eq!(
            security_alert_copy(&incidents),
            (
                "Toolport: security finding".to_string(),
                "Toolport blocked an injected tool result. Review the retained details in Activity."
                    .to_string()
            )
        );
    }

    #[test]
    fn security_status_copy_uses_only_tool_changes_for_blocking_wording() {
        for ty in [
            "result_injection_blocked",
            "sync_change_refused",
            "tool_poison_flag",
        ] {
            let event = serde_json::json!({"type":ty, "blocked":false});
            assert_eq!(
                security_status_detail(&[event]),
                "Security findings need review. Open Protection history for details."
            );
        }
        let drift = serde_json::json!({"type":"tool_drift", "blocked":false});
        let injection = serde_json::json!({"type":"result_injection_blocked", "blocked":true});
        assert_eq!(
            security_status_detail(&[drift, injection]),
            "Not blocked. Review the changes or accept them."
        );
    }

    #[test]
    fn a_reviewed_shared_change_stays_visible_for_a_later_profile_block() {
        let mut event = serde_json::json!({"type":"tool_drift", "server":"srv", "tool":"srv__read", "profiles":["personal", "work"], "blocked":true, "ts":1});
        let markers = vec![security_dismissal_key(&event)];
        assert!(!security_event_is_dismissed(&event, &markers));
        event["blocked"] = serde_json::json!(false);
        assert!(security_event_is_dismissed(&event, &markers));
    }

    #[test]
    fn v1_drift_review_keeps_new_and_blocked_changes_visible() {
        let mut event = serde_json::json!({"type":"tool_drift", "server":"srv", "tool":"srv__read", "change":"changed", "severity":"info", "blocked":false, "ts":1});
        let markers = vec![crate::integrity::security_key(&event)];
        assert!(security_event_is_dismissed(&event, &markers));
        event["blocked"] = serde_json::Value::Null;
        assert!(!security_event_is_dismissed(&event, &markers));
        event["blocked"] = serde_json::json!(true);
        assert!(!security_event_is_dismissed(&event, &markers));
        event["blocked"] = serde_json::json!(false);
        event["new_fp"] = serde_json::json!("v3:new");
        assert!(!security_event_is_dismissed(&event, &markers));
    }

    #[test]
    fn reviewing_a_high_finding_does_not_hide_a_later_recurrence() {
        let finding = |ts| {
            serde_json::json!({
                "ts": ts,
                "type": "result_injection",
                "server": "linear",
                "tool": "get_issue",
                "change": "result",
                "severity": "high"
            })
        };
        let reviewed = finding(1000);
        let markers = vec![security_dismissal_key(&reviewed)];
        assert!(security_event_is_dismissed(&finding(999), &markers));
        assert!(security_event_is_dismissed(&reviewed, &markers));
        assert!(!security_event_is_dismissed(&finding(1001), &markers));
        let mut quiet_change = reviewed;
        quiet_change["type"] = serde_json::json!("tool_drift");
        quiet_change["severity"] = serde_json::json!("info");
        let accepted = vec![security_dismissal_key(&quiet_change)];
        assert!(security_event_is_dismissed(&quiet_change, &accepted));
        quiet_change["ts"] = serde_json::json!(1001);
        assert!(!security_event_is_dismissed(&quiet_change, &accepted));
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
            failure: None,
            auth_target: None,
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
            ("Connection failed".to_string(), "error")
        );
    }

    #[test]
    fn probe_auth_ownership_is_visible() {
        use crate::call_failure::AuthTarget;
        for (target, text, action) in [
            (AuthTarget::Endpoint, "Needs sign-in", "Sign in"),
            (AuthTarget::Scope, "Permission required", "Sign in"),
            (
                AuthTarget::ServiceCredential,
                "Service key required",
                "Edit service key",
            ),
        ] {
            let probe = crate::server_runtime::ProbeResult {
                server_id: "fixture".into(),
                ok: false,
                tool_count: 0,
                error: None,
                auth_required: true,
                failure: None,
                auth_target: Some(target),
            };
            assert_eq!(probe_status_line(&probe), (text.into(), "review"));
            assert_eq!(probe_auth_action(&probe), action);
        }
    }

    #[test]
    fn server_groups_keep_enabled_and_turned_off_alphabetical() {
        let zulu = server("Zulu", "Remote HTTP");
        let mut beta = server("beta", "Local command");
        beta.enabled = false;
        beta.requires_review = true;
        let alpha = server("Alpha", "Remote HTTP");
        let mut aardvark = server("Aardvark", "Remote HTTP");
        aardvark.enabled = false;
        let servers = [zulu, beta, alpha, aardvark];
        let (on, off) = server_groups(&servers);
        let on_names = on
            .iter()
            .map(|server| server.name.as_str())
            .collect::<Vec<_>>();
        let off_names = off
            .iter()
            .map(|server| server.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(on_names, ["Alpha", "Zulu"]);
        assert_eq!(off_names, ["Aardvark", "beta"]);
    }

    #[test]
    fn server_order_ignores_enable_review_and_probe_state() {
        let mut first = server("alpha", "Remote HTTP");
        first.name = "Alpha".into();
        let second = server("beta", "Local command");
        let before = server_order_key(&first);
        first.enabled = !first.enabled;
        first.requires_review = !first.requires_review;
        assert_eq!(server_order_key(&first), before);
        assert!(server_order_key(&first) < server_order_key(&second));
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
        assert_eq!(
            stat_server_name(&serde_json::json!({ "server": "" })),
            "Unknown server"
        );
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
            origin_label: "Personal".into(),
            id: "server".into(),
            name: name.into(),
            transport: transport.into(),
            transport_id: "stdio".into(),
            command: None,
            args: Vec::new(),
            launch: None,
            url: None,
            cwd: None,
            inherit_env: false,
            secret_keys: Vec::new(),
            plain_env: Vec::new(),
            secret_references: Default::default(),
            client_credentials: None,
            enabled: true,
            requires_review: false,
            team_route_removed: false,
            probe_fingerprint: 0,
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

    #[test]
    fn a_missing_value_refusal_opens_the_editor_that_holds_it() {
        let secrets = vec!["LINODE_API_TOKEN".to_string()];
        let secret = "Linode needs LINODE_API_TOKEN. Open server setup to add it.";
        let plain = "Linode needs environment setting REGION. Open server setup to add it.";
        let input = "Linode needs API key. Add it in Servers > Linode > Credentials.";

        assert_eq!(
            missing_value_editor(secret, &secrets),
            Some(MissingValueEditor::Credentials)
        );
        assert_eq!(
            missing_value_hint(secret, &MissingValueEditor::Credentials),
            "Linode needs LINODE_API_TOKEN. Add it in Credentials, then turn the server on."
        );
        assert_eq!(
            missing_value_editor(plain, &secrets),
            Some(MissingValueEditor::EditServer)
        );
        assert_eq!(
            missing_value_editor(input, &secrets),
            Some(MissingValueEditor::Credentials)
        );
        assert_eq!(
            missing_value_hint(input, &MissingValueEditor::Credentials),
            input
        );
        assert_eq!(
            missing_value_editor("could not save the registry", &secrets),
            None
        );
    }
}

#[cfg(test)]
mod p10c_r1_presentation_tests {
    use super::*;

    #[test]
    fn p10c_r1_duration_and_identity_meta_match_react() {
        for (ms, text) in [
            (850, "850 ms"),
            (1500, "1.5 s"),
            (59999, "60.0 s"),
            (60000, "1m 0s"),
            (90000, "1m 30s"),
            (119999, "2m 0s"),
        ] {
            assert_eq!(format_duration(ms), text);
        }
        let mut row = state::ActivityView {
            internal: false,
            timestamp_ms: 0,
            server: "team_slack".into(),
            tool: "delete".into(),
            client: Some("Claude Code".into()),
            client_id: Some("adapter:claude-code".into()),
            client_label: Some("Claude Code 2.1".into()),
            cold: None,
            dispatch_ms: None,
            failure_kind: None,
            run_id: None,
            approval_decision: Some("denied".into()),
            ok: true,
            held: true,
            duration_ms: Some(1500),
            error: None,
            pii_replaced: None,
            pii_incomplete: false,
        };
        assert_eq!(activity_client_name(&row), "Claude Code");
        row.client_label = Some("Someone else".into());
        assert_eq!(
            activity_client_name(&row),
            "Claude Code (reports \"Someone else\")"
        );
        row.client = None;
        assert_eq!(activity_client_name(&row), "Someone else (reported)");
        row.client_label = None;
        assert_eq!(activity_client_name(&row), "Unrecorded client");
        assert!(crate::clients::UNRECORDED_CLIENT_TOOLTIP.contains("Older Toolport"));
        assert_eq!(
            activity_server_filter_options(&[row.clone(), row]),
            vec!["All servers", "team_slack"]
        );
        assert_eq!(approval_outcome("denied").2, "approval-denied");
        assert_eq!(approval_outcome("withdrawn").2, "disabled");
    }
}

fn secret_reference_controls(
    server_id: &str,
    key: &str,
    existing: Option<&str>,
    page: &ServerPage,
    editor: &adw::Window,
    feedback: &gtk::Label,
) -> (gtk::DropDown, gtk::Box) {
    let key = gtk::Entry::builder().text(key).build();
    secret_reference_controls_dynamic(server_id, key, existing, page, editor, feedback)
}
fn secret_reference_controls_dynamic(
    server_id: &str,
    key: gtk::Entry,
    existing: Option<&str>,
    page: &ServerPage,
    editor: &adw::Window,
    feedback: &gtk::Label,
) -> (gtk::DropDown, gtk::Box) {
    let (choice, container, reference) = secret_reference_fields(server_id, existing, feedback);
    let save = l10n::button("Save reference");
    container.append(&save);
    let id = server_id.to_string();
    let page = page.clone();
    let editor = editor.clone();
    let feedback = feedback.clone();
    save.connect_clicked(move |button| {
        button.set_sensitive(false);
        let id = id.clone();
        let reprobe_id = id.clone();
        let key = key.text().to_string();
        let reference = reference.text().to_string();
        let page = page.clone();
        let editor = editor.clone();
        let feedback = feedback.clone();
        let button = button.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                crate::registry_controller::set_secret_reference(&id, &key, &reference)
            })
            .await;
            finish_credential_update(
                result,
                &reprobe_id,
                &page,
                &editor,
                &feedback,
                &button,
                &crate::i18n::tr("Reference saved"),
            );
        });
    });
    (choice, container)
}

fn secret_reference_fields(
    server_id: &str,
    existing: Option<&str>,
    feedback: &gtk::Label,
) -> (gtk::DropDown, gtk::Box, gtk::Entry) {
    let choice = gtk::DropDown::from_strings(&[
        &crate::i18n::tr("Paste a key"),
        &crate::i18n::tr("From a password manager"),
    ]);
    l10n::bind_dropdown_choices(&choice, &["Paste a key", "From a password manager"]);
    choice.set_selected(u32::from(existing.is_some()));
    let container = gtk::Box::new(gtk::Orientation::Vertical, 8);
    container.set_visible(existing.is_some());
    let providers = crate::secret_refs::PROVIDERS;
    let names: Vec<_> = providers.iter().map(|p| p.name).collect();
    let provider = gtk::DropDown::from_strings(&names);
    provider.set_selected(
        existing
            .and_then(|r| providers.iter().position(|p| r.starts_with(p.scheme)))
            .unwrap_or(0) as u32,
    );
    // The example is a hint, never a value: prefilled text could be saved as-is.
    let start = existing
        .and_then(|r| providers.iter().find(|p| r.starts_with(p.scheme)))
        .unwrap_or(&providers[0]);
    let reference = gtk::Entry::builder()
        .text(existing.unwrap_or(""))
        .placeholder_text(start.example)
        .build();

    let reference_for_provider = reference.clone();
    provider.connect_selected_notify(move |provider| {
        reference_for_provider.set_text("");
        reference_for_provider
            .set_placeholder_text(Some(providers[provider.selected() as usize].example));
    });
    container.append(&provider);
    container.append(&reference);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let test = l10n::button("Test");
    actions.append(&test);
    container.append(&actions);
    // A caption, not a tooltip: tooltips near the dialog edge were cut off.
    container.append(
        &{ let widget = gtk::Label::builder()
            .label(crate::i18n::tr("Test runs in this app's environment, which can differ from the gateway's. Only the reference syncs, never the key."))
            .wrap(true)
            .xalign(0.0)
            .css_classes(["toolport-muted", "caption"])
            .build(); l10n::bind_label(&widget, "Test runs in this app's environment, which can differ from the gateway's. Only the reference syncs, never the key."); widget },
    );
    let box_for_choice = container.clone();
    choice
        .connect_selected_notify(move |choice| box_for_choice.set_visible(choice.selected() == 1));
    let test_id = server_id.to_string();
    let test_reference = reference.clone();
    let test_provider = provider.clone();
    let feedback_for_changes = feedback.clone();
    reference.connect_changed(move |_| feedback_for_changes.set_visible(false));
    let test_feedback = feedback.clone();
    test.connect_clicked(move |button| {
        button.set_sensitive(false);
        test_reference.set_sensitive(false);
        test_provider.set_sensitive(false);
        let reference_field = test_reference.clone();
        let provider_field = test_provider.clone();
        let id = test_id.clone();
        let reference = test_reference.text().to_string();
        let button = button.clone();
        let feedback = test_feedback.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                crate::registry_controller::test_secret_reference(&id, &reference)
            })
            .await;
            button.set_sensitive(true);
            reference_field.set_sensitive(true);
            provider_field.set_sensitive(true);
            feedback.set_visible(true);
            feedback.set_label(&match result {
                Ok(Ok(())) => crate::i18n::tr("Success in the desktop app environment. The MCP client gateway may use different environment variables or PATH."),
                Ok(Err(e)) => e.to_string(),
                Err(_) => crate::i18n::tr("Reference test stopped"),
            });
        });
    });
    (choice, container, reference)
}
