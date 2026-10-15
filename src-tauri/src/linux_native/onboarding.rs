use super::i18n as l10n;
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;

use super::state::{self, ClientGatewayState, ClientSnapshot};

const MARKER: &str = "gtk-onboarding-complete";

pub(super) fn install(app: &adw::Application, parent: &adw::ApplicationWindow) {
    if app.lookup_action("show-onboarding").is_none() {
        let action = gtk::gio::SimpleAction::new("show-onboarding", None);
        let app_for_action = app.clone();
        let parent_for_action = parent.clone();
        action.connect_activate(move |_, _| present(&app_for_action, &parent_for_action));
        app.add_action(&action);
    }

    let app = app.clone();
    let parent = parent.clone();
    gtk::glib::spawn_future_local(async move {
        let needed = gtk::gio::spawn_blocking(first_run_needed).await;
        if matches!(needed, Ok(Ok(true))) {
            present(&app, &parent);
        } else if let Ok(Err(error)) = needed {
            eprintln!("toolport: could not check native onboarding state: {error}");
        }
    });
}

fn first_run_needed() -> Result<bool, String> {
    if onboarding_complete()? {
        return Ok(false);
    }
    let registry = crate::registry::load()?;
    let has_servers = registry
        .servers
        .iter()
        .any(|server| !crate::clients::is_gateway_server(server));
    if has_servers {
        return Ok(false);
    }
    Ok(!crate::clients::detect_clients()
        .iter()
        .any(|client| client.gateway_installed))
}

#[cfg(test)]
fn should_offer(registry: &crate::registry::Registry, clients: &ClientSnapshot) -> bool {
    !registry
        .servers
        .iter()
        .any(|server| !crate::clients::is_gateway_server(server))
        && !clients
            .clients
            .iter()
            .any(|client| client.gateway_state == ClientGatewayState::Connected)
}

fn marker_path() -> Result<PathBuf, String> {
    crate::registry::conduit_dir()
        .map(|dir| dir.join(MARKER))
        .ok_or_else(|| crate::i18n::tr("could not resolve the Toolport data directory").to_string())
}

fn onboarding_complete() -> Result<bool, String> {
    marker_path().and_then(|path| onboarding_complete_at(&path))
}

fn onboarding_complete_at(path: &Path) -> Result<bool, String> {
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(metadata.is_file()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(crate::i18n::trf(
            "could not inspect the setup marker: {value0}",
            &[("value0", &format!("{}", error))],
        )),
    }
}

pub(super) fn mark_complete() -> Result<(), String> {
    if onboarding_complete()? {
        return Ok(());
    }
    crate::registry::atomic_write(&marker_path()?, "complete\n").map_err(|error| {
        crate::i18n::trf(
            "could not save setup completion: {value0}",
            &[("value0", &error.to_string())],
        )
    })
}

fn present(app: &adw::Application, parent: &adw::ApplicationWindow) {
    if let Some(window) = app
        .windows()
        .into_iter()
        .find(|window| window.widget_name() == "toolport-onboarding")
    {
        window.present();
        return;
    }

    let window = {
        let widget = adw::ApplicationWindow::builder()
            .application(app)
            .title(crate::i18n::tr("Toolport setup"))
            .default_width(660)
            .default_height(600)
            .modal(true)
            .transient_for(parent)
            .resizable(true)
            .build();
        l10n::bind_window_title(&widget, "Toolport setup");
        widget
    };
    window.set_widget_name("toolport-onboarding");
    window.add_css_class("toolport-native");

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = adw::HeaderBar::new();
    header.add_css_class("toolport-header");
    header.set_title_widget(Some(&{
        let widget = gtk::Label::builder()
            .label(crate::i18n::tr("Set up Toolport"))
            .css_classes(["title"])
            .build();
        l10n::bind_label(&widget, "Set up Toolport");
        widget
    }));
    root.append(&header);

    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::SlideLeftRight)
        .transition_duration(180)
        .vexpand(true)
        .build();

    let welcome = wizard_page(
        crate::i18n::msg("Welcome to Toolport"),
        crate::i18n::msg(
            "Set up servers once, then share them with every supported AI client on this machine.",
        ),
    );
    let benefits = gtk::Box::new(gtk::Orientation::Vertical, 8);
    benefits.add_css_class("toolport-settings-group");
    for (title, detail, icon) in [
        (
            crate::i18n::msg("One gateway for every tool"),
            crate::i18n::msg("Each client shares the servers and profiles you manage here."),
            "network-server-symbolic",
        ),
        (
            crate::i18n::msg("Smaller tool catalogs"),
            crate::i18n::msg(
                "Lazy discovery keeps agents focused without hiding pinned prerequisites.",
            ),
            "edit-find-symbolic",
        ),
        (
            crate::i18n::msg("Local safety controls"),
            crate::i18n::msg("Approvals, integrity checks, and activity stay on this machine."),
            "security-high-symbolic",
        ),
    ] {
        benefits.append(&info_row(icon, title, detail));
    }
    welcome.append(&benefits);
    let detected = {
        let widget = gtk::Label::builder()
            .label(crate::i18n::tr("Checking for supported AI clients…"))
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-muted"])
            .build();
        l10n::bind_label(&widget, "Checking for supported AI clients…");
        widget
    };
    welcome.append(&detected);
    let welcome_feedback = feedback_label("");
    welcome_feedback.set_visible(false);
    welcome.append(&welcome_feedback);
    let welcome_actions = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let choose_mcp = l10n::button("Set up MCP servers");
    choose_mcp.add_css_class("suggested-action");
    welcome_actions.append(&choose_mcp);
    let choose_team = l10n::button("Sign in to sync");
    choose_team.add_css_class("toolport-secondary-action");
    welcome_actions.append(&choose_team);
    welcome.append(&welcome_actions);
    let skip = l10n::button("Skip setup");
    skip.add_css_class("flat");
    skip.set_halign(gtk::Align::End);
    welcome.append(&skip);
    stack.add_named(&scroll_page(welcome), Some("welcome"));

    let add = wizard_page(
        crate::i18n::msg("Add your first servers"),
        crate::i18n::msg("Review and connect a client to import your existing servers, or continue to the full catalog."),
    );
    let add_feedback = feedback_label(&crate::i18n::tr(
        "Review a client or browse the catalog when you are ready.",
    ));
    add.append(&add_feedback);
    let import = l10n::button("Review and connect clients");
    import.add_css_class("toolport-secondary-action");
    add.append(&import);
    let add_nav = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let back_add = l10n::button("Back");
    back_add.add_css_class("flat");
    add_nav.append(&back_add);
    let catalog = l10n::button("Browse full catalog");
    catalog.add_css_class("toolport-secondary-action");
    catalog.set_hexpand(true);
    catalog.set_halign(gtk::Align::End);
    add_nav.append(&catalog);
    let continue_add = l10n::button("Connect a client");
    continue_add.add_css_class("suggested-action");
    add_nav.append(&continue_add);
    add.append(&add_nav);
    stack.add_named(&scroll_page(add), Some("add"));

    let connect = wizard_page(
        crate::i18n::msg("Connect a client"),
        crate::i18n::msg("Point an installed AI client at Toolport. Existing client settings and unrelated MCP servers are preserved."),
    );
    let connect_feedback = feedback_label(&crate::i18n::tr("Scanning supported clients…"));
    connect.append(&connect_feedback);
    let connect_list = gtk::Box::new(gtk::Orientation::Vertical, 8);
    connect_list.add_css_class("toolport-settings-group");
    connect.append(&connect_list);
    let connect_nav = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let back_connect = l10n::button("Back");
    back_connect.add_css_class("flat");
    connect_nav.append(&back_connect);
    let rescan = l10n::button("Scan again");
    rescan.add_css_class("toolport-secondary-action");
    rescan.set_hexpand(true);
    rescan.set_halign(gtk::Align::End);
    connect_nav.append(&rescan);
    let continue_connect = l10n::button("Review setup");
    continue_connect.add_css_class("suggested-action");
    connect_nav.append(&continue_connect);
    connect.append(&connect_nav);
    stack.add_named(&scroll_page(connect), Some("connect"));

    let done = wizard_page(
        crate::i18n::msg("Review your setup"),
        crate::i18n::msg(
            "Toolport can verify enabled servers before you start using the connected clients.",
        ),
    );
    let summary = feedback_label(&crate::i18n::tr("Reading the current setup…"));
    done.append(&summary);
    let health = feedback_label(&crate::i18n::tr("Health check has not run yet."));
    done.append(&health);
    let check_health = l10n::button("Check enabled servers");
    check_health.add_css_class("toolport-secondary-action");
    check_health.set_halign(gtk::Align::Start);
    done.append(&check_health);
    let call_verifier = CallVerifier::new();
    done.append(&call_verifier.root);
    let done_actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let destination = l10n::button("Open server tools");
    destination.add_css_class("toolport-secondary-action");
    destination.set_hexpand(true);
    destination.set_halign(gtk::Align::End);
    done_actions.append(&destination);
    let finish = l10n::button("Finish setup");
    finish.add_css_class("suggested-action");
    done_actions.append(&finish);
    done.append(&done_actions);
    stack.add_named(&scroll_page(done), Some("done"));
    root.append(&stack);
    window.set_content(Some(&root));

    let window_for_skip = window.clone();
    let feedback_for_skip = welcome_feedback.clone();
    skip.connect_clicked(move |_| finish_window(&window_for_skip, &feedback_for_skip));

    let stack_for_mcp = stack.clone();
    choose_mcp.connect_clicked(move |_| {
        stack_for_mcp.set_visible_child_name("add");
    });

    let app_for_team = app.clone();
    let window_for_team = window.clone();
    let feedback_for_team = welcome_feedback.clone();
    choose_team.connect_clicked(move |_| {
        if complete_and_close(&window_for_team, &feedback_for_team) {
            activate_page(&app_for_team, "show-teams");
        }
    });

    let stack_for_back_add = stack.clone();
    back_add.connect_clicked(move |_| stack_for_back_add.set_visible_child_name("welcome"));
    let stack_for_continue = stack.clone();
    let list_for_continue = connect_list.clone();
    let feedback_for_continue = connect_feedback.clone();
    continue_add.connect_clicked(move |_| {
        load_clients(&list_for_continue, &feedback_for_continue);
        stack_for_continue.set_visible_child_name("connect");
    });
    let app_for_catalog = app.clone();
    let window_for_catalog = window.clone();
    let feedback_for_catalog = add_feedback.clone();
    catalog.connect_clicked(move |_| {
        if complete_and_close(&window_for_catalog, &feedback_for_catalog) {
            activate_page(&app_for_catalog, "show-catalog");
        }
    });
    let stack_for_import = stack.clone();
    let list_for_import = connect_list.clone();
    let feedback_for_import = connect_feedback.clone();
    import.connect_clicked(move |_| {
        load_clients(&list_for_import, &feedback_for_import);
        stack_for_import.set_visible_child_name("connect");
    });
    let list_for_rescan = connect_list.clone();
    let feedback_for_rescan = connect_feedback.clone();
    rescan.connect_clicked(move |_| load_clients(&list_for_rescan, &feedback_for_rescan));
    let stack_for_back_connect = stack.clone();
    back_connect.connect_clicked(move |_| {
        stack_for_back_connect.set_visible_child_name("add");
    });
    let stack_for_done = stack.clone();
    let summary_for_done = summary.clone();
    let destination_for_done = destination.clone();
    let health_for_done = check_health.clone();
    let verifier_for_done = call_verifier.clone();
    continue_connect.connect_clicked(move |_| {
        l10n::bind_text(&destination_for_done, "Open server tools");
        refresh_summary(&summary_for_done);
        stack_for_done.set_visible_child_name("done");
        health_for_done.emit_clicked();
        verifier_for_done.start();
    });
    let health_for_check = health.clone();
    check_health.connect_clicked(move |button| run_health_check(button, &health_for_check));
    let app_for_destination = app.clone();
    let window_for_destination = window.clone();
    let summary_for_destination = summary.clone();
    destination.connect_clicked(move |_| {
        if complete_and_close(&window_for_destination, &summary_for_destination) {
            activate_page(&app_for_destination, "show-server-tools");
        }
    });
    let window_for_finish = window.clone();
    let summary_for_finish = summary.clone();
    finish.connect_clicked(move |_| finish_window(&window_for_finish, &summary_for_finish));

    let feedback_for_close = welcome_feedback.clone();
    window.connect_close_request(move |_| match mark_complete() {
        Ok(()) => gtk::glib::Propagation::Proceed,
        Err(error) => {
            show_error(&feedback_for_close, &error);
            gtk::glib::Propagation::Stop
        }
    });

    let detected_for_load = detected.clone();
    gtk::glib::spawn_future_local(async move {
        let result = gtk::gio::spawn_blocking(state::detect_client_views).await;
        match result {
            Ok(Ok(snapshot)) => {
                let names = snapshot
                    .clients
                    .iter()
                    .filter(|client| client.app_present || client.config_exists)
                    .map(|client| client.name.as_str())
                    .collect::<Vec<_>>();
                let message = if names.is_empty() {
                    "No supported AI clients were detected yet. You can add servers now and connect a client later."
                        .to_string()
                } else if names.len() > 4 {
                    crate::i18n::trf(
                        "Detected {value0} supported clients, including {value1}.",
                        &[
                            ("value0", &format!("{}", names.len())),
                            (
                                "value1",
                                &names
                                    .into_iter()
                                    .take(4)
                                    .collect::<Vec<_>>()
                                    .join(", ")
                                    .to_string(),
                            ),
                        ],
                    )
                } else {
                    crate::i18n::trf(
                        "Detected: {value0}",
                        &[("value0", &names.join(", ").to_string())],
                    )
                };
                detected_for_load.set_label(&message);
            }
            Ok(Err(error)) => l10n::bind_formatted_label(
                &detected_for_load,
                "Client detection is unavailable: {value0}",
                &[("value0", &error.to_string())],
            ),
            Err(_) => detected_for_load
                .set_label(&crate::i18n::tr("Client detection stopped unexpectedly.")),
        }
    });
    window.present();
}

fn load_clients(list: &gtk::Box, feedback: &gtk::Label) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    l10n::bind_text(feedback, "Scanning supported clients…");
    feedback.remove_css_class("error");
    let list = list.clone();
    let feedback = feedback.clone();
    gtk::glib::spawn_future_local(async move {
        let result = gtk::gio::spawn_blocking(state::detect_client_views).await;
        match result {
            Ok(Ok(snapshot)) => render_clients(&list, &feedback, snapshot),
            Ok(Err(error)) => show_error(&feedback, &error),
            Err(_) => show_error(&feedback, "the client scan stopped unexpectedly"),
        }
    });
}

fn render_clients(list: &gtk::Box, feedback: &gtk::Label, snapshot: ClientSnapshot) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    let clients = snapshot
        .clients
        .into_iter()
        .filter(|client| client.app_present || client.config_exists)
        .collect::<Vec<_>>();
    if clients.is_empty() {
        feedback.set_label(
            &crate::i18n::tr("No supported clients are installed yet. You can finish setup and connect one later from Clients."),
        );
        return;
    }
    show_success(
        feedback,
        &crate::i18n::trf(
            if clients.len() == 1 {
                crate::i18n::msg("Found {value0} supported client.")
            } else {
                crate::i18n::msg("Found {value0} supported clients.")
            },
            &[("value0", &format!("{}", clients.len()))],
        ),
    );
    for client in clients {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        row.add_css_class("toolport-setting-row");
        let copy = gtk::Box::new(gtk::Orientation::Vertical, 2);
        copy.set_hexpand(true);
        copy.append(
            &gtk::Label::builder()
                .label(&client.name)
                .halign(gtk::Align::Start)
                .css_classes(["heading"])
                .build(),
        );
        let connected = client.gateway_state == ClientGatewayState::Connected;
        copy.append(&{
            let widget = gtk::Label::builder()
                .halign(gtk::Align::Start)
                .css_classes(["toolport-muted"])
                .build();
            l10n::bind_label(
                &widget,
                if connected {
                    crate::i18n::msg("Connected to Toolport")
                } else if client.gateway_state == ClientGatewayState::Customized {
                    crate::i18n::msg("Customized Toolport entry. Review it in Clients.")
                } else {
                    crate::i18n::msg("Ready to connect through stdio")
                },
            );
            widget
        });
        row.append(&copy);
        if connected {
            let status = l10n::label("Connected");
            status.add_css_class("toolport-badge");
            status.add_css_class("success");
            row.append(&status);
        } else if client.gateway_state != ClientGatewayState::Customized {
            let connect = l10n::button("Connect");
            connect.add_css_class("toolport-secondary-action");
            let client_id = client.id;
            let list = list.clone();
            let feedback = feedback.clone();
            connect.connect_clicked(move |button| {
                let Some(parent) = button.root().and_downcast::<gtk::Window>() else {
                    return;
                };
                let list = list.clone();
                let feedback = feedback.clone();
                super::setup::connect(
                    &parent,
                    client_id.clone(),
                    None,
                    false,
                    move || load_clients(&list, &feedback),
                    None,
                );
            });
            row.append(&connect);
        }
        list.append(&row);
    }
}

fn refresh_summary(label: &gtk::Label) {
    l10n::bind_text(label, "Reading the current setup…");
    let label = label.clone();
    gtk::glib::spawn_future_local(async move {
        let result = gtk::gio::spawn_blocking(|| {
            let registry = crate::registry::load()?;
            let servers = registry
                .servers
                .iter()
                .filter(|server| !crate::clients::is_gateway_server(server))
                .count();
            let clients = state::detect_client_views()?
                .clients
                .into_iter()
                .filter(|client| client.gateway_state == ClientGatewayState::Connected)
                .count();
            Ok::<_, String>((servers, clients))
        })
        .await;
        match result {
            Ok(Ok((servers, clients))) => {
                show_success(
                    &label,
                    &crate::i18n::trf(
                        if servers == 1 {
                            if clients == 1 {
                                crate::i18n::msg("Toolport manages {value0} server for {value2} connected client.")
                            } else {
                                crate::i18n::msg("Toolport manages {value0} server for {value2} connected clients.")
                            }
                        } else {
                            if clients == 1 {
                                crate::i18n::msg("Toolport manages {value0} servers for {value2} connected client.")
                            } else {
                                crate::i18n::msg("Toolport manages {value0} servers for {value2} connected clients.")
                            }
                        },
                        &[
                            ("value0", &format!("{}", servers)),
                            ("value2", &format!("{}", clients)),
                        ],
                    ),
                )
            }
            Ok(Err(error)) => show_error(&label, &error),
            Err(_) => show_error(&label, "the setup summary task stopped unexpectedly"),
        }
    });
}

#[derive(Clone)]
struct CallVerifier {
    root: gtk::Box,
    title: gtk::Label,
    status: gtk::Label,
    retry: gtk::Button,
    generation: Rc<Cell<u64>>,
    timer: Rc<RefCell<Option<gtk::glib::SourceId>>>,
}

impl CallVerifier {
    fn new() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        root.add_css_class("toolport-settings-group");
        root.set_visible(false);

        let title = gtk::Label::builder()
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["heading"])
            .build();
        root.append(&title);
        root.append(&{
            let widget = gtk::Label::builder()
                .label(crate::i18n::tr(
                    "In that client, ask your agent to run this read-only prompt:",
                ))
                .halign(gtk::Align::Fill)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["toolport-muted"])
                .build();
            l10n::bind_label(
                &widget,
                "In that client, ask your agent to run this read-only prompt:",
            );
            widget
        });

        let prompt_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        prompt_row.add_css_class("toolport-setting-row");
        let prompt = {
            let widget = gtk::Label::builder()
                .label(crate::i18n::tr(
                    "List the tools you can use through Toolport.",
                ))
                .halign(gtk::Align::Fill)
                .xalign(0.0)
                .selectable(true)
                .wrap(true)
                .hexpand(true)
                .css_classes(["monospace"])
                .build();
            l10n::bind_label(&widget, "List the tools you can use through Toolport.");
            widget
        };
        prompt_row.append(&prompt);
        let copy = l10n::button("Copy prompt");
        copy.add_css_class("toolport-secondary-action");
        copy.connect_clicked(move |button| {
            button.clipboard().set_text(&crate::i18n::tr(
                "List the tools you can use through Toolport.",
            ));
            l10n::bind_text(button, "Copied");
        });
        prompt_row.append(&copy);
        root.append(&prompt_row);

        let status = feedback_label(&crate::i18n::tr(
            "Reading the activity log so older calls cannot count.",
        ));
        root.append(&status);
        let retry = l10n::button("Retry verification");
        retry.add_css_class("flat");
        retry.set_halign(gtk::Align::Start);
        retry.set_visible(false);
        root.append(&retry);

        let verifier = Self {
            root,
            title,
            status,
            retry,
            generation: Rc::new(Cell::new(0)),
            timer: Rc::new(RefCell::new(None)),
        };
        let verifier_for_retry = verifier.clone();
        verifier
            .retry
            .connect_clicked(move |_| verifier_for_retry.start());
        verifier
    }

    fn start(&self) {
        self.cancel_timer();
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        self.root.set_visible(false);
        self.retry.set_visible(false);
        self.status.remove_css_class("error");
        self.status.remove_css_class("success");
        l10n::bind_text(
            &self.status,
            "Reading the activity log so older calls cannot count.",
        );

        let verifier = self.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(|| {
                let connected = state::detect_client_views()?
                    .clients
                    .into_iter()
                    .find(|client| client.gateway_state == ClientGatewayState::Connected)
                    .map(|client| client.name);
                let Some(client) = connected else {
                    return Ok::<_, String>(None);
                };
                let recent = crate::audit::read_activity(1).map_err(|error| {
                    crate::i18n::trf(
                        "could not read the activity log: {value0}",
                        &[("value0", &format!("{}", error))],
                    )
                })?;
                let since = recent
                    .first()
                    .and_then(|entry| entry.get("ts"))
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                Ok(Some((client, since)))
            })
            .await;

            if verifier.generation.get() != generation {
                return;
            }
            match result {
                Ok(Ok(Some((client, since)))) => {
                    verifier.root.set_visible(true);
                    l10n::bind_formatted_label(
                        &verifier.title,
                        "Prove it works in {value0}",
                        &[("value0", &client.to_string())],
                    );
                    l10n::bind_formatted_label(&verifier.status, "Waiting for a new Toolport call. If {value0} was already open, restart it first.", &[("value0", &client.to_string())]);
                    verifier.poll(since, Instant::now() + Duration::from_secs(75), generation);
                }
                Ok(Ok(None)) => verifier.root.set_visible(false),
                Ok(Err(error)) => verifier.show_snapshot_error(&error),
                Err(_) => verifier
                    .show_snapshot_error("the activity-log snapshot task stopped unexpectedly"),
            }
        });
    }

    fn poll(&self, since: u64, deadline: Instant, generation: u64) {
        let in_flight = Rc::new(Cell::new(false));
        let verifier = self.clone();
        let source = gtk::glib::timeout_add_local(Duration::from_secs(2), move || {
            if verifier.generation.get() != generation || !verifier.root.is_visible() {
                return gtk::glib::ControlFlow::Break;
            }
            if Instant::now() >= deadline {
                verifier.status.add_css_class("error");
                verifier.status.set_label(
                    &crate::i18n::tr("No call arrived yet. Restart the client, confirm it uses a profile with enabled servers, or test a tool in the server’s Tools tab."),
                );
                verifier.retry.set_visible(true);
                return gtk::glib::ControlFlow::Break;
            }
            if in_flight.replace(true) {
                return gtk::glib::ControlFlow::Continue;
            }
            let verifier_for_read = verifier.clone();
            let in_flight_for_read = in_flight.clone();
            gtk::glib::spawn_future_local(async move {
                let result = gtk::gio::spawn_blocking(move || {
                    crate::audit::read_activity(25)
                        .map(|entries| audit_proof_after(&entries, since))
                        .map_err(|error| {
                            crate::i18n::trf(
                                "could not read the activity log: {value0}",
                                &[("value0", &format!("{}", error))],
                            )
                        })
                })
                .await;
                in_flight_for_read.set(false);
                if verifier_for_read.generation.get() != generation {
                    return;
                }
                if let Ok(Ok(Some(proof))) = result {
                    verifier_for_read.status.remove_css_class("error");
                    verifier_for_read.status.add_css_class("success");
                    l10n::bind_formatted_label(
                        &verifier_for_read.status,
                        "It works. A new call reached {value0}: {value1}.",
                        &[
                            ("value0", &proof.server.to_string()),
                            ("value1", &proof.tool.to_string()),
                        ],
                    );
                    verifier_for_read.retry.set_visible(false);
                    verifier_for_read.cancel_timer();
                }
            });
            gtk::glib::ControlFlow::Continue
        });
        *self.timer.borrow_mut() = Some(source);
    }

    fn show_snapshot_error(&self, error: &str) {
        self.root.set_visible(true);
        self.title
            .set_label(&crate::i18n::tr("Prove Toolport is receiving calls"));
        self.status.add_css_class("error");
        l10n::bind_formatted_label(
            &self.status,
            "Verification did not start because {value0}. Older calls will never count as proof.",
            &[("value0", error)],
        );
        self.retry.set_visible(true);
    }

    fn cancel_timer(&self) {
        if let Some(source) = self.timer.borrow_mut().take() {
            source.remove();
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct AuditProof {
    server: String,
    tool: String,
}

fn audit_proof_after(entries: &[serde_json::Value], since: u64) -> Option<AuditProof> {
    entries.iter().find_map(|entry| {
        let timestamp = entry.get("ts")?.as_u64()?;
        if timestamp <= since || crate::audit::tool_call_ok(entry).is_none() {
            return None;
        }
        Some(AuditProof {
            server: entry
                .get("server")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("Toolport")
                .to_string(),
            tool: entry
                .get("tool")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown tool")
                .to_string(),
        })
    })
}

fn run_health_check(button: &gtk::Button, label: &gtk::Label) {
    button.set_sensitive(false);
    l10n::bind_text(label, "Checking enabled servers…");
    label.remove_css_class("error");
    let button = button.clone();
    let label = label.clone();
    gtk::glib::spawn_future_local(async move {
        let result = gtk::gio::spawn_blocking(|| {
            let registry = crate::registry::load()?;
            let servers = crate::server_runtime::enabled_servers(&registry);
            let names = servers
                .iter()
                .map(|server| (server.id.clone(), server.name.clone()))
                .collect::<std::collections::HashMap<_, _>>();
            Ok::<_, String>((crate::server_runtime::probe_many(servers), names))
        })
        .await;
        button.set_sensitive(true);
        match result {
            Ok(Ok((results, _))) if results.is_empty() => {
                l10n::bind_text(&label, "No enabled servers need checking yet.");
            }
            Ok(Ok((results, names))) => {
                let failed = results
                    .iter()
                    .filter(|result| !result.ok && !result.auth_required)
                    .count();
                let auth = results.iter().filter(|result| result.auth_required).count();
                if failed == 0 {
                    show_success(
                        &label,
                        &crate::i18n::trf(
                            if results.len() - auth == 1 {
                                if auth == 1 {
                                    crate::i18n::msg("{value0} server responded. {value2} still needs authentication.")
                                } else {
                                    crate::i18n::msg("{value0} server responded. {value2} still need authentication.")
                                }
                            } else {
                                if auth == 1 {
                                    crate::i18n::msg("{value0} servers responded. {value2} still needs authentication.")
                                } else {
                                    crate::i18n::msg("{value0} servers responded. {value2} still need authentication.")
                                }
                            },
                            &[
                                ("value0", &format!("{}", results.len() - auth)),
                                ("value2", &format!("{}", auth)),
                            ],
                        ),
                    );
                } else {
                    let details = results
                        .iter()
                        .filter(|result| !result.ok && !result.auth_required)
                        .take(3)
                        .map(|result| {
                            let name = names
                                .get(&result.server_id)
                                .map(String::as_str)
                                .unwrap_or(&result.server_id);
                            let error = result
                                .error
                                .as_deref()
                                .unwrap_or("could not start")
                                .split_whitespace()
                                .take(18)
                                .collect::<Vec<_>>()
                                .join(" ");
                            format!("{name}: {error}")
                        })
                        .collect::<Vec<_>>()
                        .join("  ");
                    show_error(
                        &label,
                        &crate::i18n::trf(
                            if failed == 1 {
                                crate::i18n::msg("{value0} server could not start. {value2}")
                            } else {
                                crate::i18n::msg("{value0} servers could not start. {value2}")
                            },
                            &[
                                ("value0", &format!("{}", failed)),
                                ("value2", &details.to_string()),
                            ],
                        ),
                    );
                }
            }
            Ok(Err(error)) => show_error(&label, &error),
            Err(_) => show_error(&label, "the health check task stopped unexpectedly"),
        }
    });
}

fn activate_page(app: &adw::Application, name: &str) {
    if let Some(action) = app.lookup_action(name) {
        action.activate(None);
    }
}

fn complete_and_close(window: &adw::ApplicationWindow, feedback: &gtk::Label) -> bool {
    match mark_complete() {
        Ok(()) => {
            window.close();
            true
        }
        Err(error) => {
            show_error(feedback, &error);
            false
        }
    }
}

fn finish_window(window: &adw::ApplicationWindow, feedback: &gtk::Label) {
    let _ = complete_and_close(window, feedback);
}

fn wizard_page(title: &'static str, subtitle: &'static str) -> gtk::Box {
    let page = gtk::Box::new(gtk::Orientation::Vertical, 14);
    page.add_css_class("toolport-page");
    page.set_margin_top(24);
    page.set_margin_bottom(24);
    page.set_margin_start(24);
    page.set_margin_end(24);
    page.append(&{
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
    page.append(&{
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
    page
}

fn scroll_page(page: gtk::Box) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&page)
        .build()
}

fn info_row(icon: &str, title: &'static str, detail: &'static str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    row.add_css_class("toolport-setting-row");
    row.append(&gtk::Image::from_icon_name(icon));
    let copy = gtk::Box::new(gtk::Orientation::Vertical, 2);
    copy.append(&{
        let widget = gtk::Label::builder()
            .label(title)
            .halign(gtk::Align::Start)
            .css_classes(["heading"])
            .build();
        l10n::bind_label(&widget, title);
        widget
    });
    copy.append(&{
        let widget = gtk::Label::builder()
            .label(detail)
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-muted"])
            .build();
        l10n::bind_label(&widget, detail);
        widget
    });
    row.append(&copy);
    row
}

fn feedback_label(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .halign(gtk::Align::Fill)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["toolport-feedback"])
        .build()
}

fn show_success(label: &gtk::Label, text: &str) {
    label.set_label(text);
    label.set_visible(true);
    label.remove_css_class("error");
    label.add_css_class("success");
}

fn show_error(label: &gtk::Label, text: &str) {
    l10n::bind_formatted_label(label, "Could not continue: {value0}", &[("value0", text)]);
    label.set_visible(true);
    label.remove_css_class("success");
    label.add_css_class("error");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_marker_distinguishes_missing_file_and_directory() {
        let dir = std::env::temp_dir().join(format!(
            "toolport-onboarding-marker-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join(MARKER);

        assert!(!onboarding_complete_at(&marker).unwrap());
        crate::registry::atomic_write(&marker, "complete\n").unwrap();
        assert!(onboarding_complete_at(&marker).unwrap());
        assert!(!onboarding_complete_at(&dir).unwrap());

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn startup_skips_setup_for_a_connected_client_without_an_ownership_record() {
        let data = crate::registry::DataDirTestEnv::new("location-native-first-run");
        let _env_lock = crate::clients::env_test_lock();
        let folder = data.dir.join(".claude-work");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(
            folder.join(".claude.json"),
            r#"{"mcpServers":{"toolport":{"command":"/usr/bin/toolport-gateway"}}}"#,
        )
        .unwrap();
        let _override = crate::clients::EnvRestore::set("CLAUDE_CONFIG_DIR", &folder);
        assert!(crate::registry::load()
            .unwrap()
            .client_managed_entries
            .is_empty());
        assert!(!first_run_needed().unwrap());
    }

    #[test]
    fn setup_is_offered_only_without_servers_or_connected_clients() {
        let mut registry = crate::registry::Registry::default();
        let empty = ClientSnapshot {
            clients: Vec::new(),
            profiles: Vec::new(),
            sessions: Vec::new(),
            sessions_error: false,
        };
        assert!(should_offer(&registry, &empty));

        let connected = ClientSnapshot {
            clients: vec![state::ClientView {
                id: "client".into(),
                name: "Client".into(),
                app_present: true,
                config_exists: true,
                uses_connectors: false,
                server_count: 1,
                movable_server_count: 0,
                other_server_count: 0,
                gateway_state: ClientGatewayState::Connected,
                shared_http: false,
                legacy_bearer_argv: false,
                scope_id: None,
                scope_name: None,
                discovery_mode: None,
                inherited_discovery: None,
                config_error: false,
                location_removable: false,
            }],
            profiles: Vec::new(),
            sessions: Vec::new(),
            sessions_error: false,
        };
        assert!(!should_offer(&registry, &connected));

        registry.servers.push(crate::registry::ServerEntry {
            enabled: false,
            inherit_env: false,
            id: "real-server".into(),
            name: "Real server".into(),
            transport: "http".into(),
            command: None,
            args: Vec::new(),
            env: Vec::new(),
            url: Some("https://example.com/mcp".into()),
            cwd: None,
            source: None,
            disabled_tools: Vec::new(),
            client_credentials: None,
            request_timeout_ms: None,
            initialize_timeout_ms: None,
            launch: None,
            unknown_fields: serde_json::Map::new(),
        });
        assert!(!should_offer(&registry, &empty));
    }

    #[test]
    fn verification_requires_a_new_tool_call() {
        let retained = serde_json::json!({
            "ts": 100,
            "server": "old-server",
            "tool": "old-tool",
            "ok": true,
        });
        let unrelated = serde_json::json!({ "ts": 102, "event": "approval" });
        let fresh = serde_json::json!({
            "ts": 103,
            "server": "linear",
            "tool": "list_issues",
            "ok": true,
        });

        assert_eq!(
            audit_proof_after(std::slice::from_ref(&retained), 100),
            None
        );
        assert_eq!(audit_proof_after(&[unrelated, retained], 100), None);
        assert_eq!(
            audit_proof_after(&[fresh], 100),
            Some(AuditProof {
                server: "linear".into(),
                tool: "list_issues".into(),
            })
        );
    }
}
