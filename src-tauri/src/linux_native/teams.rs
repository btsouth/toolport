use super::i18n as l10n;
use crate::i18n::{lookup, msg, tr, trf};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;

// Older paired registries have no account snapshot. Null deliberately produces
// the existing unknown/free state until a successful sync supplies one.
fn account_status(team: &crate::registry::TeamConnection) -> &serde_json::Value {
    team.unknown_fields
        .get("accountStatus")
        .filter(|value| value.is_object())
        .unwrap_or(&serde_json::Value::Null)
}

const SYNC_DESTINATIONS: &[&str] = &[msg("Every machine"), msg("This machine")];

#[derive(Clone)]
struct PendingJoin {
    server_url: String,
    request_token: String,
    member_name: Option<String>,
}

#[derive(Clone)]
pub(super) struct TeamsPage {
    pub(super) root: gtk::Box,
    app: adw::Application,
    server_page: super::ServerPage,
    content: gtk::Box,
    header: super::page_header::PageHeader,
    plan_badge: gtk::Label,
    intro: gtk::Label,
    feedback: gtk::Label,
    errors: gtk::Box,
    busy: Rc<Cell<bool>>,
    refresh_generation: Rc<Cell<u64>>,
    refresh_running: Rc<Cell<bool>>,
    personal: Rc<Cell<bool>>,
    sync_status: Rc<RefCell<crate::team_sync_status::SyncStatus>>,
    sync_state: Rc<RefCell<crate::personal_sync::SyncState>>,
    server_snapshot: Rc<RefCell<Option<super::state::RegistrySnapshot>>>,
    pending: Rc<RefCell<Option<PendingJoin>>>,
    polling: Rc<Cell<bool>>,
    poll_timer: Rc<RefCell<Option<gtk::glib::SourceId>>>,
    /// A removal or review/blocked notice from the last sync, applied by the
    /// next render so the async refresh cannot overwrite it.
    sync_notice: Rc<RefCell<Option<(String, bool)>>>,
    rendered_state: Rc<RefCell<Option<(String, bool)>>>,
    /// The personal status line, refreshed by polls without a full render.
    sync_line: Rc<RefCell<Option<gtk::Label>>>,
    /// The last failed action. Polls redraw the status line every few seconds,
    /// so without this an error vanished before anyone could read it.
    action_error: Rc<RefCell<Option<ActionError>>>,
}

/// What Retry reruns. Failures without one (validation, a declined join, a
/// failed save) show as plain inline feedback instead of a callout.
#[derive(Clone)]
enum ErrorRetry {
    Refresh,
    Sync(gtk::Button),
}

struct ActionError {
    message: String,
    at: std::time::Instant,
    retry: Option<ErrorRetry>,
}

impl TeamsPage {
    pub(super) fn new(app: &adw::Application, server_page: super::ServerPage) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("toolport-content");
        let header =
            super::page_header::PageHeader::new(msg("Sync"), msg("Your setup, on every machine"));
        let refresh = super::header_icon_button("view-refresh-symbolic");
        l10n::bind_tooltip(&refresh, "Refresh sync status");
        header.add_action(&refresh);
        root.append(&header.root);
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        let page = super::page_body(14);
        let title_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let plan_badge = source_label(
            msg("Free: 1 person, 1 device"),
            source_label(
                msg("Free: 1 person, 1 device"),
                gtk::Label::builder()
                    .label(tr("Free: 1 person, 1 device"))
                    .valign(gtk::Align::Center)
                    .css_classes(["toolport-badge", "success", "caption"])
                    .build(),
            ),
        );
        title_row.append(&plan_badge);
        page.append(&title_row);
        let intro = source_label(msg("Set up once. Your servers follow you to every machine. Secret values and approvals stay on this machine."), source_label(msg("Set up once. Your servers follow you to every machine. Secret values and approvals stay on this machine."), gtk::Label::builder().label(tr("Set up once. Your servers follow you to every machine. Secret values and approvals stay on this machine.")).halign(gtk::Align::Fill).xalign(0.0).wrap(true).css_classes(["toolport-muted"]).build()));
        page.append(&intro);
        let feedback = gtk::Label::builder()
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-feedback"])
            .build();
        feedback.set_visible(false);
        page.append(&feedback);
        let errors = gtk::Box::new(gtk::Orientation::Vertical, 0);
        page.append(&errors);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        page.append(&content);
        scroller.set_child(Some(&page));
        root.append(&scroller);
        let page = Self {
            root,
            app: app.clone(),
            server_page,
            content,
            header,
            plan_badge,
            intro,
            feedback,
            errors,
            busy: Rc::new(Cell::new(false)),
            refresh_generation: Rc::new(Cell::new(0)),
            refresh_running: Rc::new(Cell::new(false)),
            personal: Rc::new(Cell::new(true)),
            sync_status: Default::default(),
            sync_state: Default::default(),
            server_snapshot: Default::default(),
            pending: Rc::new(RefCell::new(None)),
            polling: Rc::new(Cell::new(false)),
            poll_timer: Rc::new(RefCell::new(None)),
            sync_notice: Rc::new(RefCell::new(None)),
            rendered_state: Rc::new(RefCell::new(None)),
            sync_line: Rc::new(RefCell::new(None)),
            action_error: Rc::new(RefCell::new(None)),
        };
        let page_for_refresh = page.clone();
        refresh.connect_clicked(move |_| page_for_refresh.refresh());
        page
    }

    pub(super) fn refresh(&self) {
        self.refresh_generation
            .set(self.refresh_generation.get().wrapping_add(1));
        if self.refresh_running.replace(true) {
            return;
        }
        while let Some(child) = self.errors.first_child() {
            self.errors.remove(&child);
        }
        self.feedback.set_label(&tr("Loading sync status…"));
        self.server_page.reprobe_if_stale();
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            loop {
                let generation = page.refresh_generation.get();
                let result = gtk::gio::spawn_blocking(|| {
                    crate::registry::load().map(TeamsSnapshot::from_registry)
                })
                .await;
                if generation != page.refresh_generation.get() {
                    continue;
                }
                match result {
                    Ok(Ok(snapshot)) => {
                        page.clear_action_error(|retry| matches!(retry, Some(ErrorRetry::Refresh)));
                        page.render_snapshot(snapshot)
                    }
                    Ok(Err(error)) => page.show_failure(&error, Some(ErrorRetry::Refresh)),
                    Err(_) => page.show_failure(
                        &tr("the team status read stopped unexpectedly"),
                        Some(ErrorRetry::Refresh),
                    ),
                }
                break;
            }
            page.refresh_running.set(false);
        });
    }

    pub(super) fn attach_background_sync(&self, window: &adw::ApplicationWindow) {
        let runtime = Rc::new(SyncLoop::default());
        sync_background_step(self.clone(), runtime.clone());
        let page = self.clone();
        let runtime_for_events = runtime.clone();
        super::io::watch_data(
            window,
            15,
            false,
            |name| name == "registry.json",
            move || {
                if page.root.is_mapped() {
                    page.refresh();
                }
                // Wake an unpaired worker promptly after pairing. A sync's own
                // registry writes must not defeat the server's retry/backoff.
                if runtime_for_events.unpaired.get() {
                    if let Some(timer) = runtime_for_events.timer.borrow_mut().take() {
                        timer.remove();
                    }
                    sync_background_step(page.clone(), runtime_for_events.clone());
                }
            },
        );
        super::io::on_window_closed(window, move || {
            runtime.stopped.set(true);
            if let Some(timer) = runtime.timer.borrow_mut().take() {
                timer.remove();
            }
        });
    }

    #[cfg(test)]
    fn render_sync_failure(&self, error: &str) {
        *self.sync_status.borrow_mut() = crate::team_sync_status::current();
        self.render_sync_status();
        self.set_status(&format!("{} {error}", self.feedback.label()), true);
    }

    fn render_sync_status(&self) {
        let status = self.sync_status.borrow();
        let last = status
            .last_success_ms
            .and_then(|ms| gtk::glib::DateTime::from_unix_local(ms as i64 / 1000).ok())
            .and_then(|date| date.format("%b %d, %Y at %H:%M").ok())
            .map(|date| date.to_string())
            .unwrap_or_else(|| tr("not recorded yet"));
        self.set_status(
            &trf(
                "{value1}. Last successful sync: {last}.",
                &[
                    (
                        "value1",
                        &(crate::team_sync_status::summary(&status)).to_string(),
                    ),
                    ("last", &(last).to_string()),
                ],
            ),
            matches!(status.state.as_str(), "offline" | "error"),
        );
        self.feedback.remove_css_class("success");
    }

    #[cfg(test)]
    fn render(&self, registry: crate::registry::Registry) {
        self.render_snapshot(TeamsSnapshot::from_registry(registry));
    }

    fn render_snapshot(&self, snapshot: TeamsSnapshot) {
        let registry = snapshot.registry;
        *self.sync_status.borrow_mut() = snapshot.sync_status;
        *self.sync_state.borrow_mut() = snapshot.sync_state;
        *self.server_snapshot.borrow_mut() = Some(snapshot.servers);
        let personal = registry.team.is_none() || crate::personal_sync::is_personal(&registry);
        self.personal.set(personal);
        self.header
            .set_title(if personal { msg("Sync") } else { msg("Team") });
        self.header.set_subtitle(if personal {
            msg("Your setup, on every machine")
        } else {
            msg("Share one MCP server set across your team")
        });
        self.plan_badge.set_visible(false);
        // The personal page states its status itself; the intro is for joining and teams.
        self.intro
            .set_visible(!(personal && registry.team.is_some()));
        l10n::bind_label(
            &self.intro,
            if personal {
                msg("Set up once. Your servers follow you to every machine. Secret values and approvals stay on this machine.")
            } else {
                msg("One shared server set, governed by your team. Credentials stay on each machine.")
            },
        );
        let notice = self.sync_notice.borrow_mut().take();
        let mut display = serde_json::to_value(&registry).unwrap_or_default();
        if let Some(st) = display
            .pointer_mut("/team/personalSyncState")
            .and_then(serde_json::Value::as_object_mut)
        {
            st.remove("lastSyncedAt");
        }
        let render_state = (
            format!(
                "{}{:?}",
                display,
                registry
                    .team
                    .as_ref()
                    .map(|t| crate::personal_sync::status_lines(
                        account_status(t),
                        self.sync_state.borrow().last_synced_at
                    ))
            ),
            self.pending.borrow().is_some(),
        );
        if notice.is_none() && self.rendered_state.borrow().as_ref() == Some(&render_state) {
            if registry.team.is_some() {
                self.show_sync_status(&registry);
            } else if self.pending.borrow().is_some() {
                self.set_status(&tr("Waiting for invitation approval."), false);
                self.feedback.remove_css_class("success");
            } else {
                self.set_status("", false);
                self.feedback.remove_css_class("success");
            }
            return;
        }
        *self.rendered_state.borrow_mut() = Some(render_state);
        while let Some(child) = self.content.first_child() {
            self.content.remove(&child);
        }
        if let Some((notice, is_error)) = notice {
            if personal {
                self.show_sync_status(&registry);
            } else {
                self.set_status(&notice, is_error);
                if !is_error {
                    self.feedback.add_css_class("success");
                }
            }
            if let Some(team) = registry.team.clone() {
                self.render_connected(registry, team);
            } else {
                self.render_join();
            }
            return;
        }
        if let Some(team) = registry.team.clone() {
            self.show_sync_status(&registry);
            self.render_connected(registry, team);
        } else {
            self.feedback.remove_css_class("success");
            if self.pending.borrow().is_some() {
                self.set_status(&tr("Waiting for invitation approval."), false);
            } else {
                self.set_status("", false);
            }
            self.render_join();
            if self.pending.borrow().is_some() {
                self.schedule_join_poll();
            }
        }
    }

    fn show_sync_status(&self, registry: &crate::registry::Registry) {
        let recent_error = self.action_error.borrow().as_ref().and_then(|error| {
            (error.at.elapsed() < std::time::Duration::from_secs(30))
                .then(|| (error.message.clone(), error.retry.clone()))
        });
        if !crate::personal_sync::is_personal(registry) {
            self.render_sync_status();
            // The status line stays; a failed sync or read keeps its Retry callout.
            if let Some((error, Some(retry))) = recent_error {
                self.show_callout(&error, retry);
            }
            return;
        }
        if let Some((error, retry)) = recent_error {
            // Replay with the original Retry so a sync failure still retries the sync.
            self.present_failure(&error, retry);
            return;
        }
        // The status line under the title says it; the banner only carries errors.
        self.set_status("", false);
        self.update_sync_line(registry);
    }
    fn render_join(&self) {
        if self.pending.borrow().is_some() {
            let cancel = l10n::button("Cancel request");
            let page = self.clone();
            cancel.connect_clicked(move |_| {
                page.cancel_join_poll();
                *page.pending.borrow_mut() = None;
                page.refresh();
            });
            self.content.append(&cancel);
        }
        let sign_in = l10n::button("Sign in to sync");
        sign_in.add_css_class("suggested-action");
        sign_in.set_halign(gtk::Align::Start);

        self.content.append(&sign_in);
        let pricing = gtk::Label::builder().wrap(true).xalign(0.0).build();
        l10n::bind_label_with(&pricing, || {
            format!("{} {}",
            trf("Free: {people} person, 1 device. No card required.", &[("people", &crate::teams_plan::FREE_SEATS.to_string())]),
            trf("Pro is ${monthly}/month or ${annual}/year for one person on unlimited devices. Try it free for 14 days, no card.", &[("monthly", "5"), ("annual", "48")]))
        });
        self.content.append(&pricing);
        let group = gtk::Box::new(gtk::Orientation::Vertical, 10);
        group.set_margin_top(16);
        let detail = l10n::label(msg("If your browser cannot open Toolport, copy the manual code shown after approving this device."));
        detail.set_wrap(true);
        detail.set_xalign(0.0);
        detail.add_css_class("toolport-muted");
        group.append(&detail);
        let url = gtk::Entry::builder()
            .text(crate::teams::HOSTED_TEAMS_URL)
            .build();
        let url_for_sign_in = url.clone();
        sign_in.connect_clicked(move |_| {
            if let Ok(origin) = crate::teams::sync_sign_in_url(url_for_sign_in.text().as_str()) {
                let _ = crate::oauth::open_web_url(&origin);
            }
        });
        let code = gtk::PasswordEntry::builder()
            .placeholder_text(tr("Manual code"))
            .show_peek_icon(true)
            .build();
        l10n::bind_placeholder(&code, msg("Manual code"));
        let name = gtk::Entry::builder()
            .placeholder_text(tr("Your name (optional)"))
            .build();
        l10n::bind_placeholder(&name, msg("Your name (optional)"));
        group.append(&field(msg("Sync service URL"), &url));
        group.append(&field(msg("Manual code"), &code));
        group.append(&field(msg("Your name"), &name));
        let button = l10n::button("Sign in with code");
        let page = self.clone();
        button.connect_clicked(move |b| {
            page.connect_team(
                url.text().to_string(),
                code.text().to_string(),
                name.text().to_string(),
                b.clone(),
            )
        });
        group.append(&button);
        let manual = gtk::Expander::builder()
            .label_widget(&l10n::label(msg("Use a manual code")))
            .expanded(false)
            .child(&group)
            .build();
        manual.add_css_class("toolport-manual-code");
        self.content.append(&manual);
    }

    fn render_connected(
        &self,
        registry: crate::registry::Registry,
        team: crate::registry::TeamConnection,
    ) {
        if let Some(error) = team
            .unknown_fields
            .get("accountStatusError")
            .and_then(serde_json::Value::as_str)
        {
            self.content.append(
                &gtk::Label::builder()
                    .label(error)
                    .wrap(true)
                    .xalign(0.0)
                    .build(),
            );
        }
        if crate::personal_sync::is_personal(&registry) {
            self.render_personal(registry, team);
            return;
        }
        let summary = gtk::Box::new(gtk::Orientation::Vertical, 8);
        summary.add_css_class("toolport-card");
        summary.append(
            &gtk::Label::builder()
                .label(team.team_name.clone().unwrap_or_else(|| {
                    trf("Team {value1}", &[("value1", &(team.team_id).to_string())])
                }))
                .halign(gtk::Align::Start)
                .css_classes(["heading"])
                .build(),
        );
        summary.append(&source_formatted_label(
            msg("{value1} · {value2} · config version {value3}"),
            &[
                ("value1", &(team.server_url).to_string()),
                ("value2", &(team.role).to_string()),
                ("value3", &(team.last_version).to_string()),
            ],
            gtk::Label::builder()
                .label(trf(
                    "{value1} · {value2} · config version {value3}",
                    &[
                        ("value1", &(team.server_url).to_string()),
                        ("value2", &(team.role).to_string()),
                        ("value3", &(team.last_version).to_string()),
                    ],
                ))
                .halign(gtk::Align::Start)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["toolport-muted"])
                .build(),
        ));
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let sync = l10n::button("Sync now");
        sync.add_css_class("suggested-action");
        let page_for_sync = self.clone();
        sync.connect_clicked(move |button| page_for_sync.sync(button.clone()));
        actions.append(&sync);
        if team.role == "admin" {
            let push = l10n::button("Share selected servers");
            push.add_css_class("toolport-secondary-action");
            let page_for_push = self.clone();
            push.connect_clicked(move |button| page_for_push.select_servers(button.clone()));
            actions.append(&push);
        }
        let account_link = l10n::button("Link portal account");
        let page_for_link = self.clone();
        account_link.connect_clicked(move |button| {
            let page = page_for_link.clone();
            let button = button.clone();
            button.set_sensitive(false);
            gtk::glib::spawn_future_local(async move {
                let result = gtk::gio::spawn_blocking(crate::teams::account_link).await;
                match result {
                    Ok(Ok(url)) => {
                        let _ = crate::oauth::open_web_url(&url);
                    }
                    Ok(Err(error)) => page.feedback.set_text(&error),
                    Err(_) => page
                        .feedback
                        .set_text(&tr("Could not prepare account link. Please try again.")),
                }
                button.set_sensitive(true);
            });
        });
        if team.account_linked != Some(true) {
            actions.append(&account_link);
        }
        let leave = l10n::button("Disconnect app");
        leave.add_css_class("toolport-destructive");
        let page_for_leave = self.clone();
        leave.connect_clicked(move |button| page_for_leave.confirm_leave(button.clone()));
        actions.append(&leave);
        summary.append(&actions);
        self.content.append(&summary);
        self.content.append(&source_label(msg("Open Clients to use your enabled team servers from an AI client. Review any remaining servers below before enabling them. Successful managed calls are reported automatically."), source_label(msg("Open Clients to use your enabled team servers from an AI client. Review any remaining servers below before enabling them. Successful managed calls are reported automatically."), gtk::Label::builder().label(tr("Open Clients to use your enabled team servers from an AI client. Review any remaining servers below before enabling them. Successful managed calls are reported automatically.")).wrap(true).xalign(0.0).build())));

        match crate::teams::member_review(&registry) {
            Ok(review) if !review.pending.is_empty() => {
                let button = gtk::Button::with_label(&trf(
                    "Review {value1} team changes",
                    &[("value1", &(review.pending.len()).to_string())],
                ));
                let page = self.clone();
                button.connect_clicked(move |_| {
                    if let Some(parent) = page.app.active_window() {
                        let dialog = member_review_dialog(&parent, &review);
                        let extra = dialog.extra_child().unwrap();
                        connect_member_decisions(&extra, &review, &page, &dialog);
                        dialog.present();
                    }
                });
                self.content.append(&source_label(msg("Held servers stay off and instructions stay unchanged. Safety floors can tighten immediately."), source_label(msg("Held servers stay off and instructions stay unchanged. Safety floors can tighten immediately."), gtk::Label::builder().label(tr("Held servers stay off and instructions stay unchanged. Safety floors can tighten immediately.")).wrap(true).xalign(0.0).build())));
                self.content.append(&button);
            }
            Err(error) => self.feedback.set_text(&error),
            _ => {}
        }

        let review = registry
            .servers
            .iter()
            .filter(|server| server.source.as_deref() == Some(&format!("team:{}", team.team_id)))
            .cloned()
            .collect::<Vec<_>>();
        if !review.is_empty() {
            self.content.append(&source_label(
                msg("Team-managed servers · review and local setup"),
                source_label(
                    msg("Team-managed servers · review and local setup"),
                    gtk::Label::builder()
                        .label(tr("Team-managed servers · review and local setup"))
                        .halign(gtk::Align::Start)
                        .css_classes(["heading"])
                        .build(),
                ),
            ));
            let snapshot = self.server_snapshot.borrow();
            let snapshot = snapshot
                .as_ref()
                .expect("the worker supplied the team snapshot");
            for server in review {
                self.content.append(&review_server_row(
                    server,
                    &registry,
                    snapshot,
                    self.clone(),
                ));
            }
        }

        // The member-facing Team Instructions status (spec W4): what the org pushed and how
        // each installed client currently holds it. The check reads client files on disk, so
        // it fills in asynchronously; a re-render clears this container along with the rest.
        let instructions = gtk::Box::new(gtk::Orientation::Vertical, 8);
        instructions.add_css_class("toolport-card");
        instructions.set_visible(false);
        self.content.append(&instructions);
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(crate::teams::instructions_status).await;
            let Ok(Some(status)) = result else {
                return;
            };
            render_instructions_status(&instructions, status);
            instructions.set_visible(true);
        });
    }

    fn render_personal(
        &self,
        registry: crate::registry::Registry,
        team: crate::registry::TeamConnection,
    ) {
        let sync = self.sync_state.borrow().clone();
        // One status line and one primary action, like the rest of the app.
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let line = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .wrap(true)
            .css_classes(["toolport-sync-status"])
            .build();
        top.append(&line);
        *self.sync_line.borrow_mut() = Some(line);
        self.update_sync_line(&registry);
        let primary = l10n::button(if sync.sign_in_required {
            msg("Sign in")
        } else {
            msg("Sync now")
        });
        primary.add_css_class("suggested-action");
        primary.set_valign(gtk::Align::Center);
        let page = self.clone();
        if sync.sign_in_required {
            // Missing sign-in pairs this machine again, the same approval flow
            // as the first sign-in.
            let origin = team.server_url.clone();
            let team_id = team.team_id.clone();
            primary.connect_clicked(move |_| {
                match crate::teams::reconnect_link(&origin, &team_id) {
                    Ok(link) => super::open_shared_setup(&link, page.server_page.clone()),
                    Err(error) => page.show_error(&error),
                }
            });
        } else {
            primary.connect_clicked(move |b| page.sync(b.clone()));
        }
        top.append(&primary);
        self.content.append(&top);

        let attention = gtk::Box::new(gtk::Orientation::Vertical, 0);
        attention.add_css_class("toolport-attention");
        for warning in sync.warnings.values() {
            attention.append(&attention_item(warning, None, None));
        }
        if sync.choose_local_servers {
            let done = l10n::button("Done");
            let page = self.clone();
            done.connect_clicked(move |_| {
                let page = page.clone();
                gtk::glib::spawn_future_local(async move {
                    match gtk::gio::spawn_blocking(crate::personal_sync::finish_local_selection)
                        .await
                    {
                        Ok(Ok(_)) => page.refresh(),
                        Ok(Err(e)) => page.show_error(&e),
                        Err(_) => page.show_error(&tr("Could not finish sync selection")),
                    }
                });
            });
            attention.append(&attention_item(
                &tr("Choose which servers sync"),
                Some(&tr("Servers you already had start on This machine. Switch any you want everywhere below.")),
                Some(done),
            ));
        }
        for server in registry.servers.iter().filter(|s| {
            !registry.server_enabled(&s.id)
                && s.needs_team_enable_review()
                && !crate::personal_sync::keep_local(s)
        }) {
            let open = l10n::button("Open in Servers");
            let app = self.app.clone();
            open.connect_clicked(move |_| {
                if let Some(action) = app.lookup_action("show-servers") {
                    action.activate(None);
                }
            });
            attention.append(&attention_item(
                &trf(
                    "{value1} arrived from another machine",
                    &[(
                        "value1",
                        &(crate::personal_sync::visible_text(&server.name)).to_string(),
                    )],
                ),
                Some(&tr("Review it in Servers before it runs here.")),
                Some(open),
            ));
        }
        for (id, remote) in &sync.conflicts {
            let name = conflict_name(&registry, &sync, id, remote);
            let compare = l10n::button("Compare");
            let page = self.clone();
            let id = id.clone();
            let remote = remote.clone();
            let local = sync.pending.get(&id).and_then(|m| m.after.clone());
            let title = name.clone();
            compare.connect_clicked(move |_| {
                page.compare_conflict(&title, &id, local.as_ref(), &remote)
            });
            attention.append(&attention_item(
                &trf(
                    "{name} changed on two machines",
                    &[("name", &(name).to_string())],
                ),
                Some(&tr("Pick which version to keep.")),
                Some(compare),
            ));
        }
        for (id, error) in &sync.publish_errors {
            let name = registry
                .servers
                .iter()
                .find(|s| sync.pending.get(id).is_some_and(|m| m.local_id == s.id))
                .map(|s| s.name.as_str())
                .unwrap_or(id);
            attention.append(&attention_item(
                &trf(
                    "{value1} could not sync",
                    &[(
                        "value1",
                        &(crate::personal_sync::visible_text(name)).to_string(),
                    )],
                ),
                Some(error),
                None,
            ));
        }
        if attention.first_child().is_some() {
            self.content.append(&section_label(msg("Needs you")));
            self.content.append(&attention);
        }

        // What syncs. Turning servers on or off belongs to Servers.
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let label = section_label(msg("Servers"));
        label.set_hexpand(true);
        label.set_margin_top(0);
        label.set_valign(gtk::Align::Center);
        heading.set_margin_top(14);
        heading.append(&label);
        heading.append(&source_label(
            msg("Sync new servers"),
            source_label(
                msg("Sync new servers"),
                gtk::Label::builder()
                    .label(tr("Sync new servers"))
                    .css_classes(["toolport-muted", "caption"])
                    .build(),
            ),
        ));
        let new_servers = gtk::DropDown::new(None::<gtk::StringList>, gtk::Expression::NONE);
        l10n::bind_dropdown_choices(&new_servers, SYNC_DESTINATIONS);
        new_servers.add_css_class("toolport-compact-select");
        new_servers.set_selected(u32::from(sync.new_servers_local_only));
        l10n::bind_tooltip(
            &new_servers,
            msg("Where servers you add on this machine go"),
        );
        let page = self.clone();
        new_servers.connect_selected_notify(move |dropdown| {
            if l10n::dropdown_rebinding(dropdown) {
                return;
            }
            let local = dropdown.selected() == 1;
            let page = page.clone();
            gtk::glib::spawn_future_local(async move {
                match gtk::gio::spawn_blocking(move || {
                    crate::personal_sync::set_new_servers_local_only(local)
                })
                .await
                {
                    Ok(Ok(_)) => page.refresh(),
                    Ok(Err(e)) => page.show_error(&e),
                    Err(_) => page.show_error(&tr("Could not save the new server choice")),
                }
            });
        });
        heading.append(&new_servers);
        self.content.append(&heading);
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        list.add_css_class("toolport-sync-list");
        for server in registry
            .servers
            .iter()
            .filter(|s| !crate::clients::is_gateway_server(s))
        {
            let original = server
                .unknown_fields
                .get("teamOriginalId")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(&server.id);
            let tag = if sync.conflicts.contains_key(original) {
                Some(msg("changed on two machines"))
            } else if !registry.server_enabled(&server.id)
                && server.needs_team_enable_review()
                && !crate::personal_sync::keep_local(server)
            {
                Some(msg("needs review"))
            } else {
                None
            };
            list.append(&sync_server_row(server, tag, self.clone()));
        }
        if list.first_child().is_none() {
            list.append(&source_label(
                msg("No servers yet. Servers you add here show up on your other machines."),
                gtk::Label::builder()
                    .label(tr(
                        "No servers yet. Servers you add here show up on your other machines.",
                    ))
                    .wrap(true)
                    .xalign(0.0)
                    .css_classes(["toolport-muted"])
                    .build(),
            ));
        }
        self.content.append(&list);
        self.content.append(
            &source_label(msg("Keys and sign-ins never leave a machine. Switching a server to This machine stops syncing it; other machines keep their copy until you remove it there."), source_label(msg("Keys and sign-ins never leave a machine. Switching a server to This machine stops syncing it; other machines keep their copy until you remove it there."), gtk::Label::builder()
                .label(tr("Keys and sign-ins never leave a machine. Switching a server to This machine stops syncing it; other machines keep their copy until you remove it there."))
                .wrap(true)
                .xalign(0.0)
                .css_classes(["toolport-muted", "caption"])
                .build())),
        );

        self.content.append(&section_label(msg("Account")));
        let account = gtk::Box::new(gtk::Orientation::Vertical, 0);
        account.add_css_class("toolport-sync-list");
        let mut account_lines = crate::personal_sync::account_display_lines(&registry);
        // The shared account presenter appends a timing row only when signed in.
        // The status row above owns that information, independently of its language.
        if !sync.sign_in_required {
            account_lines.pop();
        }
        let plan = account_lines.join(" · ");
        let manage = l10n::button("Manage plan");
        let origin = team.server_url.clone();
        manage.connect_clicked(move |_| {
            let _ = crate::oauth::open_web_url(&origin);
        });
        let sign_out = l10n::button("Sign out");
        let page = self.clone();
        sign_out.connect_clicked(move |b| page.confirm_leave(b.clone()));
        account.append(&list_row(
            &tr("Your account"),
            Some(&plan),
            &[manage, sign_out],
        ));
        let create = l10n::button("Create a team");
        create.connect_clicked(|_| {
            let _ = crate::oauth::open_web_url(&format!(
                "{}/?intent=create-team&from=app-sync",
                crate::teams::HOSTED_TEAMS_URL
            ));
        });
        account.append(&list_row(
            &tr("Teams"),
            Some(&tr(
                "Share servers with other people. Your own servers stay yours.",
            )),
            &[create],
        ));
        self.content.append(&account);
    }

    /// The status line under the title: the banner plus when sync last ran.
    fn update_sync_line(&self, registry: &crate::registry::Registry) {
        let Some(line) = self.sync_line.borrow().clone() else {
            return;
        };
        let (message, healthy) = crate::personal_sync::banner_display(registry);
        let last = crate::personal_sync::status_lines(
            &serde_json::Value::Null,
            self.sync_state.borrow().last_synced_at,
        )
        .pop()
        .unwrap_or_default();
        let waiting = crate::personal_sync::state(registry)
            .map(|state| !state.initialized || state.last_synced_at.is_none())
            .unwrap_or(true);
        let message = message.trim_end_matches('.');
        line.set_label(&if last.is_empty() || waiting {
            message.to_string()
        } else {
            format!("{message} · {last}")
        });
        if healthy {
            line.add_css_class("healthy");
        } else {
            line.remove_css_class("healthy");
        }
    }

    fn compare_conflict(
        &self,
        name: &str,
        id: &str,
        local: Option<&serde_json::Value>,
        remote: &serde_json::Value,
    ) {
        let Some(parent) = self.app.active_window() else {
            return;
        };
        #[allow(deprecated)]
        let dialog = adw::MessageDialog::new(
            Some(&parent),
            Some(&trf("{name} changed on two machines", &[("name", (name))])),
            Some(&tr(
                "Choose which version to keep. This machine's version is saved until you choose.",
            )),
        );
        l10n::bind_formatted_dialog_heading(
            &dialog,
            msg("{name} changed on two machines"),
            &[("name", (name))],
        );
        dialog.set_size_request(620, -1);
        dialog.set_extra_child(Some(&conflict_content(local, remote)));
        dialog.add_response("cancel", &tr("Not now"));
        l10n::bind_dialog_response(&dialog, "cancel", msg("Not now"));
        dialog.add_response("remote", &tr("Use synced version"));
        l10n::bind_dialog_response(&dialog, "remote", msg("Use synced version"));
        dialog.add_response("mine", &tr("Keep this machine's version"));
        l10n::bind_dialog_response(&dialog, "mine", msg("Keep this machine's version"));
        dialog.set_close_response("cancel");
        dialog.set_default_response(Some("cancel"));
        let page = self.clone();
        let id = id.to_string();
        let expected = crate::personal_sync::conflict_version(remote);
        dialog.connect_response(None, move |dialog, response| {
            if response == "mine" || response == "remote" {
                let keep = response == "mine";
                let id = id.clone();
                let expected = expected.clone();
                let page = page.clone();
                gtk::glib::spawn_future_local(async move {
                    let result = gtk::gio::spawn_blocking(move || {
                        crate::personal_sync::resolve_conflict(&id, &expected, keep)?;
                        crate::teams::sync_now()
                    })
                    .await;
                    match result {
                        Ok(Ok(r)) => page.absorb_sync_result(r),
                        Ok(Err(e)) => page.show_error(&e),
                        Err(_) => page.show_error(&tr("Could not resolve sync conflict")),
                    }
                });
            }
            dialog.close();
        });
        dialog.present();
    }
    fn connect_team(&self, url: String, code: String, name: String, button: gtk::Button) {
        if url.trim().is_empty() || code.trim().is_empty() {
            self.show_error(&tr("Enter the sync service URL and manual code"));
            return;
        }
        button.set_sensitive(false);
        self.feedback.set_label(&tr("Signing in to sync…"));
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let url_for_join = url.clone();
            let name = (!name.trim().is_empty()).then_some(name.trim().to_string());
            let name_for_join = name.clone();
            let result = gtk::gio::spawn_blocking(move || {
                crate::teams::connect(&url_for_join, code.trim(), name_for_join.as_deref())
            })
            .await;
            button.set_sensitive(true);
            match result {
                Ok(Ok(crate::teams::ConnectOutcome::Connected(_))) => {
                    page.cancel_join_poll();
                    *page.pending.borrow_mut() = None;
                    page.refresh();
                }
                Ok(Ok(crate::teams::ConnectOutcome::Pending { request_token })) => {
                    *page.pending.borrow_mut() = Some(PendingJoin {
                        server_url: url,
                        request_token,
                        member_name: name,
                    });
                    page.refresh();
                    page.schedule_join_poll();
                }
                Ok(Err(error)) => page.show_error(&error),
                Err(_) => page.show_error(&tr("the team connection stopped unexpectedly")),
            }
        });
    }

    fn schedule_join_poll(&self) {
        if self.pending.borrow().is_none() || self.poll_timer.borrow().is_some() {
            return;
        }
        let page = self.clone();
        let timer =
            gtk::glib::timeout_add_local_once(std::time::Duration::from_secs(4), move || {
                page.poll_timer.borrow_mut().take();
                page.run_join_poll(None);
            });
        *self.poll_timer.borrow_mut() = Some(timer);
    }

    fn cancel_join_poll(&self) {
        if let Some(timer) = self.poll_timer.borrow_mut().take() {
            timer.remove();
        }
    }

    fn run_join_poll(&self, button: Option<gtk::Button>) {
        let Some(pending) = self.pending.borrow().clone() else {
            return;
        };
        if self.polling.replace(true) {
            return;
        }
        self.cancel_join_poll();
        if let Some(button) = &button {
            button.set_sensitive(false);
        }
        self.feedback.set_label(&tr("Checking join approval…"));
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                crate::teams::poll_join(
                    &pending.server_url,
                    &pending.request_token,
                    pending.member_name.as_deref(),
                )
            })
            .await;
            page.polling.set(false);
            if let Some(button) = &button {
                button.set_sensitive(true);
            }
            match result {
                Ok(Ok(crate::teams::JoinPoll::Connected(_))) => {
                    page.cancel_join_poll();
                    *page.pending.borrow_mut() = None;
                    page.refresh();
                }
                Ok(Ok(crate::teams::JoinPoll::Pending)) => {
                    page.feedback.set_label(&tr(
                        "Still waiting for invitation approval. Checking again automatically…",
                    ));
                    page.schedule_join_poll();
                }
                Ok(Ok(crate::teams::JoinPoll::Denied)) => {
                    page.cancel_join_poll();
                    *page.pending.borrow_mut() = None;
                    page.show_error(&tr("the team administrator denied this join request"));
                }
                Ok(Ok(crate::teams::JoinPoll::Unknown)) => {
                    page.cancel_join_poll();
                    *page.pending.borrow_mut() = None;
                    page.show_error(&tr("the join request expired or is no longer available"));
                }
                Ok(Err(error)) => {
                    page.show_error(&trf(
                        "{error}. Retrying the join request automatically",
                        &[("error", &(error).to_string())],
                    ));
                    page.schedule_join_poll();
                }
                Err(_) => {
                    page.show_error(&tr(
                        "the approval check stopped unexpectedly; retrying automatically",
                    ));
                    page.schedule_join_poll();
                }
            }
        });
    }

    fn sync(&self, button: gtk::Button) {
        button.set_sensitive(false);
        self.feedback.set_label(&tr("Syncing configuration…"));
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(crate::teams::sync_now).await;
            button.set_sensitive(true);
            match result {
                Ok(Ok(outcome)) => page.absorb_sync_result(outcome),
                Ok(Err(error)) => page.show_sync_error(&error, &button),
                Err(_) => page.show_sync_error(&tr("the team sync stopped unexpectedly"), &button),
            }
        });
    }

    /// Route a finished sync into the UI. Removal and safety-blocked servers
    /// must be said out loud: servers vanishing (or silently never arriving)
    /// with no explanation reads as data loss. The notice is parked in page
    /// state because `refresh` renders asynchronously and would otherwise
    /// overwrite whatever is set here.
    fn absorb_sync_result(&self, result: crate::teams::SyncResult) {
        self.clear_action_error(|retry| matches!(retry, Some(ErrorRetry::Sync(_))));
        match result {
            crate::teams::SyncResult::Removed => {
                let message = tr("You were removed from the team. Its shared servers and this machine's team token have been cleared.");
                *self.sync_notice.borrow_mut() = Some((message.to_string(), true));
                let notification = gtk::gio::Notification::new(&tr("Removed from team"));
                notification.set_body(Some(&message));
                self.app
                    .send_notification(Some("toolport-team-removed"), &notification);
            }
            crate::teams::SyncResult::Ok { applied, .. } => {
                let outcome = applied.map(|(_, outcome)| outcome).unwrap_or_default();
                *self.sync_notice.borrow_mut() = team_review_line(outcome.review, outcome.blocked)
                    .map(|message| (message, true));
            }
        }
        self.refresh();
    }

    fn select_servers(&self, button: gtk::Button) {
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            match gtk::gio::spawn_blocking(crate::registry::load).await {
                Ok(Ok(registry)) => page.show_server_selection(button, registry),
                Ok(Err(error)) => page.show_error(&error),
                Err(_) => page.show_error(&tr("The server read stopped unexpectedly")),
            }
        });
    }

    fn show_server_selection(&self, button: gtk::Button, reg: crate::registry::Registry) {
        let Some(parent) = self.app.active_window() else {
            return;
        };
        #[allow(deprecated)]
        let dialog = adw::MessageDialog::new(Some(&parent), Some(&tr("Share with your team")), Some(&tr("Choose one working server to begin. Other team servers and your personal originals stay in place. Each member supplies credentials locally. The preview shows how each choice relates to the team before anything is uploaded.")));
        l10n::bind_dialog_heading(&dialog, msg("Share with your team"));
        l10n::bind_dialog_body(&dialog, msg("Choose one working server to begin. Other team servers and your personal originals stay in place. Each member supplies credentials locally. The preview shows how each choice relates to the team before anything is uploaded."));
        dialog.set_size_request(480, -1);
        dialog.add_response("cancel", &tr("Cancel"));
        l10n::bind_dialog_response(&dialog, "cancel", msg("Cancel"));
        dialog.add_response("select", &tr("Preview selected"));
        l10n::bind_dialog_response(&dialog, "select", msg("Preview selected"));
        dialog.set_response_enabled("select", false);
        dialog.set_close_response("cancel");
        let list = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let choices = Rc::new(RefCell::new(Vec::<String>::new()));
        for server in reg.servers.iter().filter(|s| {
            !s.source.as_deref().unwrap_or("").starts_with("team:")
                && !crate::clients::is_gateway_server(s)
        }) {
            let keys = server
                .env
                .iter()
                .map(|e| e.key.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let check = gtk::CheckButton::new();
            check.set_child(Some(&share_choice_label(
                &server.name,
                (!keys.is_empty()).then(|| {
                    trf(
                        "Local credentials: {keys}",
                        &[("keys", &(keys).to_string())],
                    )
                }),
                crate::teams::personal_share_hint(&reg, server),
            )));
            let ids = choices.clone();
            let id = server.id.clone();
            let d = dialog.clone();
            check.connect_toggled(move |c| {
                let mut ids = ids.borrow_mut();
                ids.retain(|x| x != &id);
                if c.is_active() {
                    ids.push(id.clone());
                }
                d.set_response_enabled("select", !ids.is_empty());
            });
            list.append(&check);
        }
        if list.first_child().is_none() {
            list.append(&l10n::label(
                "Add a working personal server in Servers first.",
            ));
        }
        dialog.set_extra_child(Some(&list));
        let page = self.clone();
        dialog.connect_response(None, move |d, response| {
            if response == "select" {
                page.preview_push(button.clone(), choices.borrow().clone());
            }
            d.close();
        });
        dialog.present();
    }

    fn preview_push(&self, button: gtk::Button, selected: Vec<String>) {
        button.set_sensitive(false);
        self.feedback
            .set_label(&tr("Comparing local and shared team servers…"));
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let ids = selected.clone();
            let result =
                gtk::gio::spawn_blocking(move || crate::teams::preview_push_selected(&ids)).await;
            button.set_sensitive(true);
            match result {
                Ok(Ok(preview)) => page.show_push_review(preview, selected),
                Ok(Err(error)) => page.show_error(&error),
                Err(_) => page.show_error(&tr("the team comparison stopped unexpectedly")),
            }
        });
    }

    fn show_push_review(&self, preview: crate::teams::PushPreview, selected: Vec<String>) {
        let Some(parent) = self.app.active_window() else {
            return;
        };
        let dialog = share_preview_dialog(&parent, &preview);
        let page = self.clone();
        dialog.connect_response(None, move |dialog, response| {
            if response == "push" {
                let base_version = preview.base_version;
                let fingerprint = preview.local_fingerprint.clone();
                let selected = selected.clone();
                page.feedback
                    .set_label(&tr("Updating shared team servers…"));
                let page = page.clone();
                gtk::glib::spawn_future_local(async move {
                    let result = gtk::gio::spawn_blocking(move || {
                        crate::teams::push_selected(&selected, base_version, &fingerprint)
                    })
                    .await;
                    match result {
                        Ok(Ok(result)) => {
                            if let Some(proposal) = &result.proposal {
                                if let Some(parent) = page.app.active_window() {
                                    let confirmation = adw::MessageDialog::new(
                                        Some(&parent),
                                        Some(&tr("Sent for confirmation")),
                                        Some(&tr(
                                            "Finish publishing this update in the Teams dashboard.",
                                        )),
                                    );
                                    l10n::bind_dialog_heading(
                                        &confirmation,
                                        msg("Sent for confirmation"),
                                    );
                                    l10n::bind_dialog_heading(
                                        &confirmation,
                                        msg("Sent for confirmation"),
                                    );
                                    confirmation.add_response("close", &tr("Close"));
                                    l10n::bind_dialog_response(
                                        &confirmation,
                                        "close",
                                        msg("Close"),
                                    );
                                    confirmation.add_response("open", &tr("Open"));
                                    l10n::bind_dialog_response(&confirmation, "open", msg("Open"));
                                    let url = proposal.confirm_url.clone();
                                    let page = page.clone();
                                    confirmation.connect_response(None, move |dialog, response| {
                                        if response == "open" {
                                            if let Err(error) =
                                                crate::teams::open_confirmation(&url)
                                            {
                                                page.show_error(&error);
                                            }
                                        }
                                        dialog.close();
                                    });
                                    confirmation.present();
                                }
                            }
                            *page.sync_notice.borrow_mut() =
                                Some((result.summary.clone(), result.needs_attention()));
                            page.refresh();
                        }
                        Ok(Err(error)) => page.show_error(&error),
                        Err(_) => page.show_error(&tr("the team update stopped unexpectedly")),
                    }
                });
            }
            dialog.close();
        });
        dialog.present();
    }

    fn confirm_leave(&self, button: gtk::Button) {
        let Some(parent) = self.app.active_window() else {
            return;
        };
        let personal = self.personal.get();
        #[allow(deprecated)]
        let dialog = adw::MessageDialog::new(Some(&parent), None, None);
        l10n::bind_dialog_heading(
            &dialog,
            if personal {
                msg("Sign out of sync?")
            } else {
                msg("Disconnect this app from the team?")
            },
        );
        l10n::bind_dialog_body(
            &dialog,
            if personal {
                msg("Synced servers are removed from this app. Your setup stays in Your account. Sign in again to restore it.")
            } else {
                msg("Team servers, instructions and policy are removed from this app. Your personal servers stay saved. Your Team membership and shared setup remain. Reconnect from the Teams website.")
            },
        );
        dialog.add_response("cancel", &tr("Cancel"));
        l10n::bind_dialog_response(&dialog, "cancel", msg("Cancel"));
        dialog.add_response("leave", &tr("Disconnect app"));
        l10n::bind_dialog_response(&dialog, "leave", msg("Disconnect app"));
        dialog.set_close_response("cancel");
        dialog.set_default_response(Some("cancel"));
        dialog.set_response_appearance("leave", adw::ResponseAppearance::Destructive);
        let page = self.clone();
        dialog.connect_response(None, move |dialog, response| {
            if response == "leave" {
                button.set_sensitive(false);
                let page = page.clone();
                gtk::glib::spawn_future_local(async move {
                    match gtk::gio::spawn_blocking(crate::teams::disconnect).await {
                        Ok(Ok(())) => page.refresh(),
                        Ok(Err(error)) => page.show_error(&error),
                        Err(_) => page.show_error(&tr("the team disconnect stopped unexpectedly")),
                    }
                });
            }
            dialog.close();
        });
        dialog.present();
    }

    /// An empty message hides the line. `toolport-feedback` paints a background,
    /// so a banner with nothing to say is just a bar taking up the page. While
    /// disconnected there is nothing to say: the join form below is already the
    /// answer to "am I connected".
    fn set_status(&self, message: &str, error: bool) {
        self.feedback.set_label(message);
        self.feedback.set_visible(!message.is_empty());
        if error {
            self.feedback.remove_css_class("success");
            self.feedback.add_css_class("error");
        } else {
            self.feedback.remove_css_class("error");
        }
    }

    /// Forget the last failure once the action it came from has succeeded.
    fn clear_action_error(&self, from: impl Fn(Option<&ErrorRetry>) -> bool) {
        let mut last = self.action_error.borrow_mut();
        if last
            .as_ref()
            .is_some_and(|error| from(error.retry.as_ref()))
        {
            *last = None;
        }
    }

    fn show_sync_error(&self, error: &str, button: &gtk::Button) {
        self.show_failure(error, Some(ErrorRetry::Sync(button.clone())));
    }

    fn show_error(&self, error: &str) {
        self.show_failure(error, None);
    }

    fn show_failure(&self, error: &str, retry: Option<ErrorRetry>) {
        // A new failure starts its own 30 seconds, even when the message repeats.
        *self.action_error.borrow_mut() = Some(ActionError {
            message: error.to_string(),
            at: std::time::Instant::now(),
            retry: retry.clone(),
        });
        self.present_failure(error, retry);
    }

    /// Show a failure without restarting its deadline, for redraws that replay it.
    fn present_failure(&self, error: &str, retry: Option<ErrorRetry>) {
        while let Some(child) = self.errors.first_child() {
            self.errors.remove(&child);
        }
        self.feedback.remove_css_class("success");
        self.feedback.add_css_class("error");
        let Some(retry) = retry else {
            l10n::bind_formatted_label(
                &self.feedback,
                msg("Sync error: {error}"),
                &[("error", (error))],
            );
            self.feedback.set_visible(true);
            return;
        };
        // The callout carries the error, so the inline line stays hidden.
        self.feedback.set_visible(false);
        self.show_callout(error, retry);
    }

    fn show_callout(&self, error: &str, retry: ErrorRetry) {
        while let Some(child) = self.errors.first_child() {
            self.errors.remove(&child);
        }
        let page = self.clone();
        self.errors.append(&super::error_callout(
            msg("Sync"),
            error,
            move || match &retry {
                ErrorRetry::Refresh => page.refresh(),
                ErrorRetry::Sync(button) => page.sync(button.clone()),
            },
        ));
    }
}

struct TeamsSnapshot {
    registry: crate::registry::Registry,
    servers: super::state::RegistrySnapshot,
    sync_status: crate::team_sync_status::SyncStatus,
    sync_state: crate::personal_sync::SyncState,
}

impl TeamsSnapshot {
    fn from_registry(registry: crate::registry::Registry) -> Self {
        Self {
            servers: super::state::RegistrySnapshot::from_registry(registry.clone()),
            sync_status: crate::team_sync_status::for_registry(&registry),
            sync_state: crate::personal_sync::state(&registry).unwrap_or_default(),
            registry,
        }
    }
}

#[derive(Default)]
struct SyncLoop {
    running: Cell<bool>,
    stopped: Cell<bool>,
    unpaired: Cell<bool>,
    failures: Cell<u32>,
    timer: RefCell<Option<gtk::glib::SourceId>>,
}

fn sync_background_step(page: TeamsPage, runtime: Rc<SyncLoop>) {
    if runtime.stopped.get() || runtime.running.replace(true) {
        return;
    }
    gtk::glib::spawn_future_local(async move {
        let failures = runtime.failures.get();
        let result = gtk::gio::spawn_blocking(move || {
            let registry = crate::registry::load()?;
            if registry.team.is_none() {
                // This slow unpaired poll also stays alive while hidden on
                // filesystems without a registry monitor. Paired installs use
                // only the sync retry schedule, never a second fallback poll.
                return Ok((None, 60));
            }
            let result = crate::teams::sync_wait(25);
            let retry = crate::registry::load()
                .map(|r| crate::teams::sync_retry_seconds(&r, failures.saturating_add(1)))
                .unwrap_or_else(|_| crate::teams::retry_delay_seconds(failures.saturating_add(1)));
            Ok::<_, String>((Some(result), retry))
        })
        .await;
        runtime.running.set(false);
        if runtime.stopped.get() {
            return;
        }
        let unpaired = matches!(&result, Ok(Ok((None, _))));
        if !unpaired {
            page.app.activate_action("hidden-sync-finished", None);
        }
        let delay = match result {
            Ok(Ok((None, delay))) => {
                runtime.unpaired.set(true);
                runtime.failures.set(0);
                delay
            }
            Ok(Ok((Some(Ok(outcome)), _))) => {
                runtime.unpaired.set(false);
                runtime.failures.set(0);
                page.absorb_sync_result(outcome);
                3
            }
            error => {
                runtime.unpaired.set(false);
                runtime.failures.set(failures.saturating_add(1));
                if page.root.is_mapped() {
                    page.refresh();
                }
                match error {
                    Ok(Ok((Some(Err(_)), retry))) => retry,
                    _ => crate::teams::retry_delay_seconds(runtime.failures.get()),
                }
            }
        };
        let next = runtime.clone();
        *runtime.timer.borrow_mut() = Some(gtk::glib::timeout_add_local_once(
            std::time::Duration::from_secs(delay),
            move || {
                next.timer.borrow_mut().take();
                sync_background_step(page, next);
            },
        ));
    });
}

fn render_instructions_status(container: &gtk::Box, status: crate::teams::InstructionsStatusView) {
    let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    heading.append(&source_label(
        msg("Team instructions"),
        gtk::Label::builder()
            .label(tr("Team instructions"))
            .halign(gtk::Align::Start)
            .css_classes(["heading"])
            .build(),
    ));
    heading.append(&source_formatted_label(
        msg("v{value1}"),
        &[("value1", &(status.version).to_string())],
        gtk::Label::builder()
            .label(trf(
                "v{value1}",
                &[("value1", &(status.version).to_string())],
            ))
            .halign(gtk::Align::Start)
            .css_classes(["toolport-muted"])
            .build(),
    ));
    container.append(&heading);
    container.append(
        &gtk::Label::builder()
            .label(tr(
                "Org-managed agent rules, written to your AI clients alongside your own \
                 instructions, never over them. Leaving the team removes them.",
            ))
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-muted"])
            .build(),
    );
    let text = gtk::TextView::new();
    text.set_editable(false);
    text.set_cursor_visible(false);
    text.set_monospace(true);
    text.set_wrap_mode(gtk::WrapMode::WordChar);
    text.set_top_margin(8);
    text.set_bottom_margin(8);
    text.set_left_margin(8);
    text.set_right_margin(8);
    text.buffer().set_text(&status.content);
    let scroller = gtk::ScrolledWindow::builder()
        .child(&text)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(160)
        .build();
    scroller.add_css_class("toolport-text-area");
    container.append(&scroller);
    if status.clients.is_empty() {
        container.append(&source_label(
            msg("No supported AI clients detected on this machine."),
            gtk::Label::builder()
                .label(tr("No supported AI clients detected on this machine."))
                .halign(gtk::Align::Start)
                .xalign(0.0)
                .css_classes(["toolport-muted"])
                .build(),
        ));
        return;
    }
    for client in status.clients {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.append(
            &gtk::Label::builder()
                .label(&client.name)
                .halign(gtk::Align::Start)
                .hexpand(true)
                .xalign(0.0)
                .build(),
        );
        let (state_label, badge_class) = super::rule_apply_state(client.state);
        let badge = l10n::label(state_label);
        badge.add_css_class("toolport-badge");
        badge.add_css_class(badge_class);
        row.append(&badge);
        container.append(&row);
    }
}

/// The member-facing line for a team merge that held servers back: `review`
/// arrived switched off pending member review, `blocked` were refused outright
/// (link-local or cloud-metadata URLs). `None` when there is nothing to say.
fn team_review_line(review: usize, blocked: usize) -> Option<String> {
    let mut parts = Vec::new();
    if review > 0 {
        parts.push(if review == 1 {
            trf("{review} team change is waiting for your review. Held servers stay off; review queued changes above.", &[("review", &review.to_string())])
        } else {
            trf("{review} team changes are waiting for your review. Held servers stay off; review queued changes above.", &[("review", &review.to_string())])
        });
    }
    if blocked > 0 {
        parts.push(if blocked == 1 {
            trf("{blocked} was Blocked because of unsafe definitions or references. env: references are local only, including personal Pro sync. Use a password manager reference instead.", &[("blocked", &blocked.to_string())])
        } else {
            trf("{blocked} were Blocked because of unsafe definitions or references. env: references are local only, including personal Pro sync. Use a password manager reference instead.", &[("blocked", &blocked.to_string())])
        });
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

fn section_label(text: &'static str) -> gtk::Label {
    source_label(
        text,
        gtk::Label::builder()
            .label(text)
            .halign(gtk::Align::Start)
            .margin_top(14)
            .css_classes(["toolport-section-label"])
            .build(),
    )
}

/// One line in the Needs you block: a bold title, a muted detail and at most
/// one action.
fn attention_item(title: &str, detail: Option<&str>, action: Option<gtk::Button>) -> gtk::Box {
    let item = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    item.add_css_class("toolport-attention-item");
    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.set_hexpand(true);
    text.append(
        &gtk::Label::builder()
            .label(title)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["heading"])
            .build(),
    );
    if let Some(detail) = detail {
        text.append(
            &gtk::Label::builder()
                .label(detail)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["toolport-muted", "caption"])
                .build(),
        );
    }
    item.append(&text);
    if let Some(action) = action {
        action.set_valign(gtk::Align::Center);
        item.append(&action);
    }
    item
}

fn list_row(title: &str, detail: Option<&str>, actions: &[gtk::Button]) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    row.add_css_class("toolport-sync-row");
    let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
    text.set_hexpand(true);
    text.append(
        &gtk::Label::builder()
            .label(title)
            .xalign(0.0)
            .css_classes(["heading"])
            .build(),
    );
    if let Some(detail) = detail.filter(|d| !d.is_empty()) {
        text.append(
            &gtk::Label::builder()
                .label(detail)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["toolport-muted", "caption"])
                .build(),
        );
    }
    row.append(&text);
    for action in actions {
        action.set_valign(gtk::Align::Center);
        row.append(action);
    }
    row
}

fn conflict_name(
    registry: &crate::registry::Registry,
    sync: &crate::personal_sync::SyncState,
    id: &str,
    remote: &serde_json::Value,
) -> String {
    crate::personal_sync::visible_text(
        registry
            .servers
            .iter()
            .find(|s| sync.pending.get(id).is_some_and(|m| m.local_id == s.id))
            .map(|s| s.name.as_str())
            .or_else(|| remote["name"].as_str())
            .unwrap_or(id),
    )
}

/// A server and where it lives: on every machine, or only this one.
fn sync_server_row(
    server: &crate::registry::ServerEntry,
    tag: Option<&'static str>,
    page: TeamsPage,
) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.add_css_class("toolport-sync-row");
    row.append(&super::branding::server_logo(
        &server.name,
        &server.transport,
    ));
    let name = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    name.set_hexpand(true);
    name.append(
        &gtk::Label::builder()
            .label(crate::personal_sync::visible_text(&server.name))
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .tooltip_text(&server.name)
            .css_classes(["heading"])
            .build(),
    );
    if let Some(tag) = tag {
        name.append(
            &gtk::Label::builder()
                .label(lookup(tag))
                .css_classes(["toolport-sync-tag", "caption"])
                .build(),
        );
    }
    row.append(&name);
    let choice = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    choice.add_css_class("toolport-segmented");
    choice.add_css_class("toolport-sync-choice");
    choice.set_valign(gtk::Align::Center);
    let every = source_button(
        msg("Every machine"),
        gtk::ToggleButton::with_label("Every machine"),
    );
    let here = source_button(
        msg("This machine"),
        gtk::ToggleButton::with_label("This machine"),
    );
    here.set_group(Some(&every));
    let local = crate::personal_sync::keep_local(server);
    here.set_active(local);
    every.set_active(!local);
    l10n::bind_tooltip(&every, msg("Sync this server to your other machines"));
    l10n::bind_tooltip(&here, msg("Stop syncing. Other machines keep their copy."));
    let id = server.id.clone();
    here.connect_toggled(move |button| {
        let local = button.is_active();
        let id = id.clone();
        let page = page.clone();
        gtk::glib::spawn_future_local(async move {
            match gtk::gio::spawn_blocking(move || crate::personal_sync::set_local_only(&id, local))
                .await
            {
                Ok(Ok(_)) => page.refresh(),
                Ok(Err(e)) => page.show_error(&e),
                Err(_) => page.show_error(&tr("Could not save sync choice")),
            }
        });
    });
    choice.append(&every);
    choice.append(&here);
    row.append(&choice);
    row
}

fn field(label: &'static str, input: &impl IsA<gtk::Widget>) -> gtk::Box {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 4);
    root.append(&source_label(
        label,
        gtk::Label::builder()
            .label(label)
            .halign(gtk::Align::Start)
            .css_classes(["heading"])
            .build(),
    ));
    root.append(input);
    root
}

fn conflict_content(local: Option<&serde_json::Value>, remote: &serde_json::Value) -> gtk::Box {
    let grid = gtk::Box::new(gtk::Orientation::Horizontal, 16);
    let left = crate::personal_sync::conflict_fields(local);
    let right = crate::personal_sync::conflict_fields(Some(remote));
    let keys: std::collections::BTreeSet<_> = left.keys().chain(right.keys()).collect();
    for (title, fields) in [(msg("This machine"), &left), (msg("Other machine"), &right)] {
        let column = gtk::Box::new(gtk::Orientation::Vertical, 8);
        column.set_hexpand(true);
        column.append(
            &gtk::Label::builder()
                .label(lookup(title))
                .xalign(0.0)
                .css_classes(["heading"])
                .build(),
        );
        for key in &keys {
            let changed = left.get(*key) != right.get(*key);
            let label = gtk::Label::builder()
                .label(format!(
                    "{}: {}",
                    crate::personal_sync::review_field_label(key),
                    fields
                        .get(*key)
                        .map(String::as_str)
                        .unwrap_or(&tr("Not set"))
                ))
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::Word)
                .selectable(true)
                .xalign(0.0)
                .build();
            if changed {
                label.add_css_class("warning");
            }
            column.append(&label);
        }
        grid.append(&column);
    }
    grid
}
pub(super) fn execution_review_content(server: &crate::registry::ServerEntry) -> gtk::Box {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let previous = server.unknown_fields.get("syncExecutionReview");
    for text in crate::personal_sync::execution_review_lines(server) {
        let label = gtk::Label::builder()
            .label(&text)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .selectable(true)
            .xalign(0.0)
            .build();
        if previous.is_some() && text != "New server" {
            label.add_css_class("warning");
        }
        if text == "New server" {
            label.add_css_class("heading");
            l10n::bind_label(&label, msg("New server"));
        }
        content.append(&label);
    }
    if previous.is_some() {
        let full = gtk::Box::new(gtk::Orientation::Vertical, 8);
        for (key, value) in crate::personal_sync::execution_review_fields(server) {
            full.append(
                &gtk::Label::builder()
                    .label(crate::personal_sync::review_display(
                        server,
                        &crate::personal_sync::review_field_line(&key, &value),
                    ))
                    .wrap(true)
                    .wrap_mode(gtk::pango::WrapMode::WordChar)
                    .selectable(true)
                    .xalign(0.0)
                    .build(),
            );
        }
        content.append(
            &gtk::Expander::builder()
                .label(tr("Show full definition"))
                .child(&full)
                .build(),
        );
    }
    content
}
pub(super) fn execution_review_scroll(
    server: &crate::registry::ServerEntry,
) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .min_content_height(120)
        .max_content_height(340)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .propagate_natural_height(true)
        .child(&execution_review_content(server))
        .build()
}
fn review_server_row(
    server: crate::registry::ServerEntry,
    registry: &crate::registry::Registry,
    snapshot: &super::state::RegistrySnapshot,
    page: TeamsPage,
) -> gtk::Box {
    if registry.enabled_here(&server.id) {
        if let Some(view) = snapshot.servers.iter().find(|s| s.id == server.id) {
            return super::server_card(view, &snapshot.active_profile_id, page.server_page.clone());
        }
    }

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.add_css_class("toolport-card");
    let copy = gtk::Box::new(gtk::Orientation::Vertical, 3);
    copy.set_hexpand(true);
    copy.append(
        &gtk::Label::builder()
            .label(&server.name)
            .halign(gtk::Align::Start)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .tooltip_text(&server.name)
            .css_classes(["heading"])
            .build(),
    );
    let target = crate::personal_sync::execution_review_lines(&server).join("\n");
    // A server turned off on another machine has nothing to confirm here.
    if server.needs_team_enable_review() {
        copy.append(&execution_review_content(&server));
    }
    row.append(&copy);
    let original = Some(registry).and_then(|r| {
        let id = r.team.as_ref()?.managed_server_ids.get(&server.id)?;
        r.servers
            .iter()
            .find(|s| {
                &s.id == id
                    && !s.source.as_deref().unwrap_or("").starts_with("team:")
                    && r.enabled_here(id)
            })
            .map(|s| s.name.clone())
    });
    if let Some(original) = original {
        let use_managed = l10n::button("Use in this profile");
        use_managed.set_valign(gtk::Align::Center);
        let page = page.clone();
        let id = server.id.clone();
        let detail = target.clone();
        use_managed.connect_clicked(move |_| {
            let Some(parent) = page.app.active_window() else { return; };
            #[allow(deprecated)]
            let d = adw::MessageDialog::new(Some(&parent), Some(&tr("Use team-managed version?")), Some(&trf("{detail}\n\nEnable this managed copy and disable Personal {original} in this profile. The original stays saved. Existing local credentials and sign-in are reused only when the definitions match exactly. Nothing is uploaded. Signing out affects both copies.", &[("detail", &(detail).to_string()), ("original", &(original).to_string())])));
l10n::bind_formatted_dialog_body(&d, msg("{detail}\n\nEnable this managed copy and disable Personal {original} in this profile. The original stays saved. Existing local credentials and sign-in are reused only when the definitions match exactly. Nothing is uploaded. Signing out affects both copies."), &[("detail", &(detail).to_string()), ("original", &(original).to_string())]);
l10n::bind_dialog_heading(&d, msg("Use team-managed version?"));
            d.set_size_request(520, -1);
            d.add_response("cancel", &tr("Cancel"));
 l10n::bind_dialog_response(&d, "cancel", msg("Cancel")); d.add_response("use", &tr("Use managed version"));
 l10n::bind_dialog_response(&d, "use", msg("Use managed version")); d.set_close_response("cancel");
            let page = page.clone(); let id = id.clone();
            d.connect_response(None, move |d, response| {
                if response == "use" { let page = page.clone(); let id = id.clone(); gtk::glib::spawn_future_local(async move {
                    match gtk::gio::spawn_blocking(move || crate::teams::use_managed_server(&id)).await {
                        Ok(Ok(_)) => page.refresh(), Ok(Err(e)) => page.show_error(&e), Err(_) => page.show_error(&tr("Could not switch to managed server")),
                    }
                }); }
                d.close();
            }); d.present();
        });
        row.append(&use_managed);
    }
    let already_enabled = registry.enabled_here(&server.id);
    if already_enabled || !server.needs_team_enable_review() {
        row.append(
            &gtk::Label::builder()
                .label(lookup(if already_enabled {
                    msg("Enabled in this profile")
                } else {
                    msg("Turned off")
                }))
                .css_classes(["toolport-muted"])
                .build(),
        );
        return row;
    }
    let enable = l10n::button("Review and enable");
    enable.set_valign(gtk::Align::Center);
    let held = server.unknown_fields.get("teamHeldChange") == Some(&serde_json::json!(true));
    if held {
        l10n::bind_button(&enable, msg("Held for team change review"));
    }
    enable.set_sensitive(!already_enabled && !held);
    enable.add_css_class("toolport-secondary-action");
    let server_name = server.name.clone();
    let reviewed_entry = server.clone();
    enable.connect_clicked(move |button| {
        let Some(parent) = page.app.active_window() else {
            return;
        };
        #[allow(deprecated)]
        let dialog = adw::MessageDialog::new(
            Some(&parent),
            Some(&trf("Enable {server_name}?", &[("server_name", &(server_name).to_string())])),
            Some(&tr("Enable only after verifying this definition and saved authentication. Credentials remain local.")),
        );
l10n::bind_formatted_dialog_heading(&dialog, msg("Enable {server_name}?"), &[("server_name", &(server_name).to_string())]);
l10n::bind_dialog_body(&dialog, msg("Enable only after verifying this definition and saved authentication. Credentials remain local."));
        dialog.set_size_request(620, -1);
        let scroll = execution_review_scroll(&reviewed_entry);
        dialog.set_extra_child(Some(&scroll));
        dialog.add_response("cancel", &tr("Keep disabled"));
 l10n::bind_dialog_response(&dialog, "cancel", msg("Keep disabled"));
        dialog.add_response("enable", &tr("Enable"));
 l10n::bind_dialog_response(&dialog, "enable", msg("Enable"));
        dialog.set_close_response("cancel");
        dialog.set_default_response(Some("cancel"));
        dialog.set_response_appearance("enable", adw::ResponseAppearance::Suggested);
        let page = page.clone();
        let reviewed_entry = reviewed_entry.clone();
        let button = button.clone();
        dialog.connect_response(None, move |dialog, response| {
            if response == "enable" {
                button.set_sensitive(false);
                let page = page.clone();
                let reviewed_entry = reviewed_entry.clone();
                let button = button.clone();
                gtk::glib::spawn_future_local(async move {
                    let result = gtk::gio::spawn_blocking(move || {
                        let registry = crate::registry::load()?;
                        crate::personal_sync::enable_reviewed(&registry.active_profile_id(), &reviewed_entry)
                    })
                    .await;
                    match result {
                        Ok(Ok(_)) => {
                            page.action_error.replace(None);
                            page.refresh();
                        }
                        // Usually a secret this machine still needs; leave the
                        // button usable for after it has been added.
                        Ok(Err(error)) => {
                            button.set_sensitive(true);
                            page.show_error(&error);
                        }
                        Err(_) => {
                            button.set_sensitive(true);
                            page.show_error(&tr("the review update stopped unexpectedly"));
                        }
                    }
                });
            }
            dialog.close();
        });
        dialog.present();
    });
    row.append(&enable);
    row
}

/// A picker row: the server name, then muted lines for its credential keys and
/// how it relates to the team.
fn share_choice_label(name: &str, keys: Option<String>, hint: Option<&str>) -> gtk::Box {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let title = gtk::Label::builder()
        .label(name)
        .xalign(0.0)
        .wrap(true)
        .build();
    column.append(&title);
    for detail in keys.as_deref().into_iter().chain(hint) {
        let line = gtk::Label::builder()
            .label(detail)
            .xalign(0.0)
            .wrap(true)
            .max_width_chars(48)
            .build();
        line.add_css_class("toolport-muted");
        column.append(&line);
    }
    column
}

/// What the confirm button does for this preview, or `None` when there is
/// nothing to upload or switch. The React preview applies the same rule.
fn share_action(preview: &crate::teams::PushPreview) -> Option<&'static str> {
    if share_uploads(preview) {
        Some(msg("Share selected"))
    } else if preview
        .selections
        .iter()
        .any(|selection| selection.local.outcome == crate::teams::HandoffOutcome::Switched)
    {
        Some(msg("Use Team copies"))
    } else {
        None
    }
}

fn share_uploads(preview: &crate::teams::PushPreview) -> bool {
    !(preview.added.is_empty() && preview.changed.is_empty() && preview.removed.is_empty())
}

#[allow(deprecated)]
fn share_preview_dialog(
    parent: &gtk::Window,
    preview: &crate::teams::PushPreview,
) -> adw::MessageDialog {
    let dialog = adw::MessageDialog::new(
        Some(parent), Some(&tr("Share selected servers?")),
        Some(&tr("Other team servers, instructions and policies stay unchanged. Your personal servers remain saved, and other profiles stay unchanged. Credential values are never uploaded. Each member uses their own credentials locally.")),
    );
    l10n::bind_dialog_heading(&dialog, msg("Share selected servers?"));
    l10n::bind_dialog_body(&dialog, msg("Other team servers, instructions and policies stay unchanged. Your personal servers remain saved, and other profiles stay unchanged. Credential values are never uploaded. Each member uses their own credentials locally."));
    dialog.set_size_request(520, -1);
    dialog.add_response("cancel", &tr("Cancel"));
    l10n::bind_dialog_response(&dialog, "cancel", msg("Cancel"));
    let action = share_action(preview);
    dialog.add_response("push", &lookup(action.unwrap_or(msg("Share selected"))));
    dialog.set_response_enabled("push", action.is_some());
    dialog.set_close_response("cancel");
    dialog.set_default_response(Some("cancel"));
    dialog.set_response_appearance("push", adw::ResponseAppearance::Suggested);
    dialog.set_extra_child(Some(&share_preview_content(preview)));
    dialog
}

/// Bounded, expandable review of the exact display-only preview also used by Tauri.
fn share_preview_content(preview: &crate::teams::PushPreview) -> gtk::ScrolledWindow {
    let changes = gtk::Box::new(gtk::Orientation::Vertical, 8);
    changes.add_css_class("toolport-settings-group");
    let label = |text: &str| {
        gtk::Label::builder()
            .label(text)
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .max_width_chars(52)
            .selectable(true)
            .build()
    };
    for selection in &preview.selections {
        let heading = label(&format!("{} · {}", selection.name, selection.team_change));
        heading.add_css_class("heading");
        changes.append(&heading);
        changes.append(&label(&selection.team_detail));
        for note in &selection.notes {
            let note = label(note);
            note.add_css_class("toolport-muted");
            changes.append(&note);
        }
        let local = label(&selection.local.message);
        if selection.local.outcome == crate::teams::HandoffOutcome::Attention {
            local.add_css_class("error");
        }
        changes.append(&local);
    }
    let uploads = share_uploads(preview);
    if !preview.selections.is_empty() && !uploads {
        let source = if share_action(preview).is_some() {
            msg("Nothing new is uploaded to the team. Only this profile changes.")
        } else {
            msg("Nothing to upload or switch for this selection.")
        };
        let outcome = label(&lookup(source));
        l10n::bind_label(&outcome, source);
        changes.append(&outcome);
    }
    for (kind, names) in [
        (msg("Added"), &preview.added),
        (msg("Changed"), &preview.changed),
        (msg("Removed"), &preview.removed),
    ]
    .into_iter()
    .filter(|_| uploads || preview.selections.is_empty())
    {
        let heading = label(&trf(
            "{kind} ({count})",
            &[("kind", &lookup(kind)), ("count", &names.len().to_string())],
        ));
        heading.add_css_class("heading");
        changes.append(&heading);
        if names.is_empty() {
            changes.append(&label(&tr("None")));
        }
        if kind == "Removed" {
            for name in names {
                changes.append(&label(name));
            }
            continue;
        }
        for definition in preview.definitions.iter().filter(|d| d.change == kind) {
            let expander = gtk::Expander::builder()
                .expanded(preview.definitions.len() == 1)
                .build();
            let summary = label(&format!(
                "{} · {}",
                definition.name,
                super::state::display_transport(&definition.transport)
            ));
            summary.set_selectable(false);
            expander.set_label_widget(Some(&summary));
            let fields = gtk::Box::new(gtk::Orientation::Vertical, 6);
            fields.set_margin_start(16);
            for field in &definition.fields {
                let title = label("");
                let field_key = field.label.clone();
                l10n::bind_label_with(&title, move || {
                    crate::personal_sync::review_field_label(&field_key)
                });
                title.add_css_class("toolport-muted");
                fields.append(&title);
                let value = label(&field.value);
                value.add_css_class("monospace");
                fields.append(&value);
            }
            expander.set_child(Some(&fields));
            changes.append(&expander);
        }
    }
    changes.append(&label(&tr("If the team or your local servers change before saving, Toolport stops and asks you to review again.")));
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_width(440)
        .max_content_height(360)
        .propagate_natural_height(true)
        .child(&changes)
        .build()
}

fn member_label(label: &serde_json::Value) -> String {
    let author = label["author"]["name"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| tr("Unknown author"));
    let time = label["at"]
        .as_i64()
        .and_then(|ms| gtk::glib::DateTime::from_unix_utc(ms / 1000).ok())
        .and_then(|date| date.format("%Y-%m-%d %H:%M UTC").ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| tr("Unknown time"));
    let via = label["via"].as_str().unwrap_or("unknown");
    let approval = label["approvedBy"]["name"]
        .as_str()
        .map(|name| trf(" · approved by {name}", &[("name", (name))]))
        .unwrap_or_default();
    trf(
        "{author} · {time} · via {via}{approval}",
        &[
            ("author", &(author).to_string()),
            ("time", &(time).to_string()),
            ("via", (via)),
            ("approval", &(approval).to_string()),
        ],
    )
}

fn member_review_dialog(
    parent: &impl IsA<gtk::Window>,
    review: &crate::teams::MemberReview,
) -> adw::MessageDialog {
    let dialog = adw::MessageDialog::new(Some(parent), Some(&tr("Review team changes")), Some(&tr("Each decision applies to this member and the exact content shown. Held servers stay off. Safety floors can tighten immediately.")));
    l10n::bind_dialog_heading(&dialog, msg("Review team changes"));
    l10n::bind_dialog_body(&dialog, msg("Each decision applies to this member and the exact content shown. Held servers stay off. Safety floors can tighten immediately."));
    dialog.add_response("close", &tr("Close"));
    l10n::bind_dialog_response(&dialog, "close", msg("Close"));
    dialog.set_extra_child(Some(&member_review_content(review)));
    dialog
}

fn member_review_content(review: &crate::teams::MemberReview) -> gtk::ScrolledWindow {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    for change in review.pending.values() {
        let section = gtk::Box::new(gtk::Orientation::Vertical, 6);
        section.add_css_class("toolport-card");
        section.append(
            &gtk::Label::builder()
                .label(&change.title)
                .xalign(0.0)
                .css_classes(["heading"])
                .build(),
        );
        if change.labels.is_empty() {
            section.append(&source_label(msg("Full diff from your accepted configuration. Change history labels unavailable."), gtk::Label::builder().label(tr("Full diff from your accepted configuration. Change history labels unavailable.")).wrap(true).xalign(0.0).build()));
        } else {
            for label in &change.labels {
                section.append(
                    &gtk::Label::builder()
                        .label(member_label(label))
                        .wrap(true)
                        .xalign(0.0)
                        .build(),
                );
            }
        }
        for field in &change.fields {
            section.append(&source_formatted_label(
                msg("{value1}\nBefore: {value2}\nAfter: {value3}"),
                &[
                    ("value1", &(field.field).to_string()),
                    ("value2", &(field.before).to_string()),
                    ("value3", &(field.after).to_string()),
                ],
                gtk::Label::builder()
                    .label(trf(
                        "{value1}\nBefore: {value2}\nAfter: {value3}",
                        &[
                            ("value1", &(field.field).to_string()),
                            ("value2", &(field.before).to_string()),
                            ("value3", &(field.after).to_string()),
                        ],
                    ))
                    .wrap(true)
                    .selectable(true)
                    .xalign(0.0)
                    .build(),
            ));
        }
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        for (name, accept) in [(msg("Accept"), true), (msg("Reject"), false)] {
            let button = l10n::button(name);
            button.set_widget_name(&format!(
                "{}:{}",
                if accept { "accept" } else { "reject" },
                change.key
            ));
            if accept {
                button.add_css_class("suggested-action");
            }
            buttons.append(&button);
        }
        section.append(&buttons);
        content.append(&section);
    }
    gtk::ScrolledWindow::builder()
        .min_content_width(480)
        .max_content_height(520)
        .propagate_natural_height(true)
        .child(&content)
        .build()
}

fn refresh_member_review_dialog(
    dialog: &adw::MessageDialog,
    review: &crate::teams::MemberReview,
) -> bool {
    if review.pending.is_empty() {
        dialog.close();
        return false;
    }
    dialog.set_extra_child(Some(&member_review_content(review)));
    true
}

fn connect_member_decisions(
    widget: &gtk::Widget,
    review: &crate::teams::MemberReview,
    page: &TeamsPage,
    dialog: &adw::MessageDialog,
) {
    if let Some(button) = widget.downcast_ref::<gtk::Button>() {
        let name = button.widget_name();
        if let Some((action, key)) = name.split_once(':') {
            if let Some(change) = review.pending.get(key) {
                let key = change.key.clone();
                let hash = change.hash.clone();
                let accept = action == "accept";
                let page = page.clone();
                let dialog = dialog.clone();
                button.connect_clicked(move |button| {
                    if page.busy.replace(true) {
                        return;
                    }
                    button.set_sensitive(false);
                    let (key, hash, page, dialog) =
                        (key.clone(), hash.clone(), page.clone(), dialog.clone());
                    gtk::glib::spawn_future_local(async move {
                        let result = gtk::gio::spawn_blocking(move || {
                            crate::teams::review_team_change(&key, &hash, accept)
                        })
                        .await;
                        page.busy.set(false);
                        match result {
                            Ok(Ok(reg)) => {
                                match crate::teams::member_review(&reg) {
                                    Ok(review) => {
                                        if refresh_member_review_dialog(&dialog, &review) {
                                            if let Some(content) = dialog.extra_child() {
                                                connect_member_decisions(
                                                    &content, &review, &page, &dialog,
                                                );
                                            }
                                        }
                                    }
                                    Err(error) => {
                                        dialog.close();
                                        page.show_error(&error);
                                    }
                                }
                                page.refresh();
                            }
                            Ok(Err(error)) => {
                                dialog.close();
                                page.show_error(&error);
                            }
                            Err(_) => {
                                dialog.close();
                                page.show_error(&tr(
                                    "Team review stopped unexpectedly. Review the current queue.",
                                ));
                            }
                        }
                    });
                });
            }
        }
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        connect_member_decisions(&current, review, page, dialog);
        child = current.next_sibling();
    }
}

fn source_label(source: &'static str, label: gtk::Label) -> gtk::Label {
    l10n::bind_label(&label, source);
    label
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
#[cfg(test)]
mod tests {
    #[test]
    fn paired_registry_without_valid_account_status_has_safe_fallback() {
        for status in [
            None,
            Some(serde_json::json!(false)),
            Some(serde_json::json!("bad")),
            Some(serde_json::json!([])),
        ] {
            let mut value = serde_json::json!({"teamId":"fixture", "role":"member", "serverUrl":"https://example.invalid"});
            if let Some(status) = status {
                value["accountStatus"] = status;
            }
            let team = serde_json::from_value(value).unwrap();
            assert!(super::account_status(&team).is_null());
            let _ = crate::personal_sync::status_lines(super::account_status(&team), None);
        }
        let team = serde_json::from_value(serde_json::json!({"teamId":"fixture", "role":"member", "serverUrl":"https://example.invalid", "accountStatus":{"plan":"pro"}})).unwrap();
        assert_eq!(super::account_status(&team)["plan"], "pro");
    }

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn b6_sync_errors_keep_their_retry_and_validation_stays_inline() {
        adw::init().unwrap();
        let _data = crate::registry::DataDirTestEnv::new("gtk-teams-error-retry");
        let app = adw::Application::builder()
            .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(gtk::gio::Cancellable::NONE).unwrap();
        let (_, servers, _) =
            super::super::build_content(&app, crate::approval_broker::start_native());
        let page = super::TeamsPage::new(&app, servers);
        let registry = crate::registry::Registry {
            team: Some(serde_json::from_value(serde_json::json!({"teamId":"solo","role":"admin","serverUrl":"http://127.0.0.1:1","accountStatus":{"personalSync":true,"plan":"pro","canReceiveConfig":true},"personalSyncState":{"initialized":true}})).unwrap()),
            ..Default::default()
        };
        assert!(crate::personal_sync::is_personal(&registry));
        page.render(registry.clone());
        let retry_kind = |page: &super::TeamsPage| {
            page.action_error
                .borrow()
                .as_ref()
                .map(|error| match error.retry {
                    None => "inline",
                    Some(super::ErrorRetry::Refresh) => "refresh",
                    Some(super::ErrorRetry::Sync(_)) => "sync",
                })
        };

        // Validation: plain inline feedback, no Retry callout, also after a redraw.
        page.show_error("Enter the sync service URL and manual code");
        assert!(page.errors.first_child().is_none());
        assert!(page.feedback.is_visible());
        assert!(page.feedback.text().contains("Enter the sync service URL"));
        page.show_sync_status(&registry);
        assert!(page.errors.first_child().is_none());
        assert_eq!(retry_kind(&page), Some("inline"));

        // A sync failure replayed by the watcher or a page switch still retries the sync.
        let button = gtk::Button::new();
        page.show_sync_error("Network offline", &button);
        assert!(page.errors.first_child().is_some());
        assert!(!page.feedback.is_visible());
        page.show_sync_status(&registry);
        assert!(page.errors.first_child().is_some());
        assert_eq!(retry_kind(&page), Some("sync"));

        // The same failure again, after the first expired, gets its own 30 seconds.
        if let Some(error) = page.action_error.borrow_mut().as_mut() {
            error.at -= std::time::Duration::from_secs(40);
        }
        while let Some(child) = page.errors.first_child() {
            page.errors.remove(&child);
        }
        page.show_sync_status(&registry);
        assert!(
            page.errors.first_child().is_none(),
            "an expired failure is not replayed"
        );
        page.show_sync_error("Network offline", &button);
        while let Some(child) = page.errors.first_child() {
            page.errors.remove(&child);
        }
        page.show_sync_status(&registry);
        assert!(
            page.errors.first_child().is_some(),
            "a repeated failure is replayed"
        );

        // A successful reread clears a read failure but not a sync failure.
        page.clear_action_error(|retry| matches!(retry, Some(super::ErrorRetry::Refresh)));
        assert_eq!(retry_kind(&page), Some("sync"));
        page.show_failure("registry unreadable", Some(super::ErrorRetry::Refresh));
        assert_eq!(retry_kind(&page), Some("refresh"));
        page.clear_action_error(|retry| matches!(retry, Some(super::ErrorRetry::Refresh)));
        assert_eq!(retry_kind(&page), None);

        // A shared team keeps its status line and still replays the sync callout.
        let team = crate::registry::Registry {
            team: Some(
                serde_json::from_value(serde_json::json!({
                    "teamId":"fixture", "role":"admin", "serverUrl":"http://127.0.0.1:1"
                }))
                .unwrap(),
            ),
            ..Default::default()
        };
        assert!(!crate::personal_sync::is_personal(&team));
        page.show_sync_error("Team sync failed", &button);
        while let Some(child) = page.errors.first_child() {
            page.errors.remove(&child);
        }
        page.show_sync_status(&team);
        assert!(page.errors.first_child().is_some());
        assert!(page.feedback.is_visible());
        assert_eq!(retry_kind(&page), Some("sync"));
    }

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn review_rows_use_the_supplied_snapshot_even_when_disk_is_unreadable() {
        adw::init().unwrap();
        let data = crate::registry::DataDirTestEnv::new("gtk-review-snapshot");
        let app = adw::Application::builder()
            .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(gtk::gio::Cancellable::NONE).unwrap();
        let (_, servers, _) =
            super::super::build_content(&app, crate::approval_broker::start_native());
        let page = super::TeamsPage::new(&app, servers);
        let registry: crate::registry::Registry = serde_json::from_value(serde_json::json!({
            "version": 3, "profiles": [], "activeProfileId": "default",
            "servers": [
                {"id":"enabled", "name":"Enabled team server", "source":"team:fixture", "transport":"http", "url":"https://example.invalid/mcp", "env":[], "enabled":true},
                {"id":"review", "name":"Review team server", "source":"team:fixture", "transport":"http", "url":"https://example.invalid/mcp", "env":[], "enabled":false}
            ],
            "team": {"teamId":"fixture", "role":"member", "serverUrl":"http://127.0.0.1:1"}
        })).unwrap();
        let snapshot = super::super::state::RegistrySnapshot::from_registry(registry.clone());
        std::fs::write(data.dir.join("registry.json"), "{broken").unwrap();
        for server in &registry.servers {
            let row = super::review_server_row(server.clone(), &registry, &snapshot, page.clone());
            assert!(row.first_child().is_some());
        }
        assert_eq!(
            std::fs::read_to_string(data.dir.join("registry.json")).unwrap(),
            "{broken"
        );
    }

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn personal_review_widgets_have_plain_fields_full_definition_and_scroll() {
        adw::init().unwrap();
        let mut server: crate::registry::ServerEntry = serde_json::from_value(serde_json::json!({
            "id":"review","name":"Review","transport":"stdio","command":"echo","args":["old"],"env":[]
        })).unwrap();
        let previous = crate::personal_sync::execution_review_fields(&server);
        server
            .unknown_fields
            .insert("syncExecutionReview".into(), serde_json::json!(previous));
        server.args = vec!["new".into()];
        let scroll = super::execution_review_scroll(&server);
        assert_eq!(scroll.hscrollbar_policy(), gtk::PolicyType::Never);
        assert_eq!(scroll.vscrollbar_policy(), gtk::PolicyType::Automatic);
        assert_eq!(scroll.max_content_height(), 340);
        let child = scroll.child().unwrap();
        let child = if let Ok(viewport) = child.clone().downcast::<gtk::Viewport>() {
            viewport.child().unwrap()
        } else {
            child
        };
        let content = child.downcast::<gtk::Box>().unwrap();
        let label = content
            .first_child()
            .unwrap()
            .downcast::<gtk::Label>()
            .unwrap();
        assert!(label.label().contains("new"));
        assert!(label.has_css_class("warning"));
        let expander = content
            .last_child()
            .unwrap()
            .downcast::<gtk::Expander>()
            .unwrap();
        assert_eq!(expander.label().as_deref(), Some("Show full definition"));
        assert!(!expander.is_expanded());
        let mut text = String::new();
        collect(expander.child().unwrap().upcast_ref(), &mut text);
        assert!(text.contains("Command: echo"));
        assert!(text.contains("Uses this machine's environment: no"));
        assert!(!text.contains("URL:"));
        assert!(!text.contains("null"));
        assert!(!text.contains("CHANGED"));
    }
    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn personal_sync_lists_what_syncs_and_waits_on_conflicts() {
        adw::init().unwrap();
        let _data = crate::registry::DataDirTestEnv::new("gtk-personal-conflicts");
        let app = adw::Application::builder()
            .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(gtk::gio::Cancellable::NONE).unwrap();
        let (_, servers, _) =
            super::super::build_content(&app, crate::approval_broker::start_native());
        let page = super::TeamsPage::new(&app, servers);
        let mut reg = crate::registry::Registry { team: Some(serde_json::from_value(serde_json::json!({"teamId":"solo","role":"admin","serverUrl":"http://127.0.0.1:1","accountStatus":{"personalSync":true,"plan":"pro","canReceiveConfig":true},"personalSyncState":{"initialized":true,"pending":{"docs-http":{"localId":"docs-http","after":{"name":"Toolport docs","url":"https://example.com/this"}}},"conflicts":{"docs-http":{"name":"Toolport docs","url":"https://gitmcp.io/btsouth/Toolport2026"}}}})).unwrap()), ..Default::default() };
        let server: crate::registry::ServerEntry = serde_json::from_value(serde_json::json!({"id":"docs-http","name":"Toolport docs","transport":"http","url":"https://example.com/this","env":[],"enabled":false,"source":"team:solo","personalSyncEntry":true})).unwrap();
        reg.servers.push(server.clone());
        crate::registry::save(&reg).unwrap();
        *page.sync_notice.borrow_mut() = Some(("Sync is up to date".into(), false));
        page.render(reg);
        assert!(!page.feedback.has_css_class("success"));
        let mut text = String::new();
        collect(page.root.upcast_ref(), &mut text);
        // The conflict waits in Needs you; its versions open in Compare.
        assert!(text.contains("Toolport docs changed on two machines"));
        assert!(text.contains("Compare"));
        assert!(text.contains("Every machine"));
        assert!(text.contains("This machine"));
        assert!(text.contains("changed on two machines"));
        // Sync no longer turns servers on or off.
        assert!(!text.contains("Review and enable"));
        assert!(!text.contains("Turned off"));
        assert!(!page.plan_badge.is_visible());
        // Turned off elsewhere, nothing to confirm: no review text.
        assert!(!text.contains("New server"));
        assert!(!text.contains("Nothing in this definition changed"));
    }

    use super::share_preview_dialog;
    use super::team_review_line;
    use super::{share_action, share_choice_label};
    use crate::teams::{HandoffOutcome, LocalHandoff, PushPreview, ShareSelectionPreview};
    use adw::prelude::*;
    use std::{cell::Cell, rc::Rc};

    #[test]
    fn personal_execution_review_includes_setup_values_and_visible_controls() {
        let server: crate::registry::ServerEntry = serde_json::from_value(serde_json::json!({"id":"review","name":"Review","transport":"stdio","command":"npx\u{202e}","args":["-y","package"],"cwd":"/work\u{200b}","inheritEnv":false,"env":[{"key":"REGION","secret":false,"value":"west"},{"key":"TOKEN","secret":true,"value":"hidden"}],"launch":{"inputs":[{"key":"project","label":"Project","secret":false,"value":"folder"}],"bindings":[{"index":1,"parts":[{"kind":"input","key":"project"}]}]}})).unwrap();
        let text = crate::personal_sync::execution_review_lines(&server).join("\n");
        assert!(text.contains("Environment: REGION = west"));
        assert!(text.contains("Environment: TOKEN = <masked secret>"));
        assert!(text.contains("Input: project = folder"));
        assert!(text.contains("Argument values:"));
        assert!(text.contains("Working folder: /work\\u{200B}"));
        assert!(text.contains("Command: npx\\u{202E}"));
        assert!(text.contains("Uses this machine's environment: no"));
        assert!(!text.contains("hidden"));
    }
    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn member_review_native_shows_diff_labels_and_both_decisions() {
        adw::init().unwrap();
        let mut registry = crate::registry::Registry { team: Some(serde_json::from_value(serde_json::json!({"teamId":"native-review", "serverUrl":"https://teams.toolport.app", "role":"member"})).unwrap()), ..Default::default() };
        crate::teams::stage_team_config(&mut registry, "native-review", &serde_json::json!({"servers":[], "instructions":{"content":"Recognized team instructions"}}), 12, &[serde_json::json!({"author":{"name":"Alice"},"at":1791417600000_i64,"via":"dashboard","approvedBy":{"name":"Bob"},"summary":{"instructions":true}})]).unwrap();
        let review = crate::teams::member_review(&registry).unwrap();
        let parent = adw::ApplicationWindow::builder()
            .title("P14 member review")
            .default_width(700)
            .default_height(800)
            .build();
        let dialog = super::member_review_dialog(&parent, &review);
        let mut text = String::new();
        collect(&dialog.clone().upcast(), &mut text);
        assert!(text.contains("Team instructions"));
        assert!(text.contains("Before: None"));
        assert!(text.contains("After: Recognized team instructions"));
        assert!(
            text.contains("Alice")
                && text.contains("via dashboard")
                && text.contains("approved by Bob")
        );
        assert!(text.contains("Accept") && text.contains("Reject"));
        // Hold this fixture only for an external omabox capture. Frame callbacks
        // observe the completion marker; the timeout bounds the optional capture.
        if let Ok(path) = std::env::var("TOOLPORT_MEMBER_REVIEW_SCREENSHOT") {
            let done = std::path::PathBuf::from(format!("{path}.done"));
            let finished = Rc::new(Cell::new(false));
            let main_loop = gtk::glib::MainLoop::new(None, false);
            let frame_loop = main_loop.clone();
            let captured = finished.clone();
            dialog.add_tick_callback(move |_, _| {
                if done.exists() {
                    captured.set(true);
                    frame_loop.quit();
                    gtk::glib::ControlFlow::Break
                } else {
                    gtk::glib::ControlFlow::Continue
                }
            });
            let timeout_loop = main_loop.clone();
            let timeout =
                gtk::glib::timeout_add_local_once(std::time::Duration::from_secs(30), move || {
                    timeout_loop.quit()
                });
            parent.present();
            dialog.present();
            main_loop.run();
            if finished.get() {
                timeout.remove();
            }
            assert!(
                finished.get(),
                "omabox capture did not finish within 30 seconds"
            );
        }
        dialog.close();
        parent.close();
    }

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn member_review_native_keeps_remaining_decisions_open() {
        adw::init().unwrap();
        let _lock = crate::registry::data_dir_test_lock();
        let scratch =
            std::env::temp_dir().join(format!("toolport-native-queue-{}", std::process::id()));
        std::fs::create_dir_all(&scratch).unwrap();
        let _data = crate::registry::DataDirOverride::set(&scratch);
        let mut reg = crate::registry::Registry { team: Some(serde_json::from_value(serde_json::json!({"teamId":"native-review", "serverUrl":"https://teams.toolport.app", "role":"member"})).unwrap()), ..Default::default() };
        crate::teams::stage_team_config(&mut reg, "native-review", &serde_json::json!({"servers":[], "instructions":{"content":"Pending text"}, "callAuditExport":true}), 1, &[]).unwrap();
        crate::registry::save(&reg).unwrap();
        let review = crate::teams::member_review(&reg).unwrap();
        let parent = adw::ApplicationWindow::builder().build();
        let app = adw::Application::builder()
            .application_id("app.toolport.ReviewFixture")
            .build();
        let (_, server_page, _) =
            super::super::build_content(&app, crate::approval_broker::start_native());
        let page = super::TeamsPage::new(&app, server_page);
        let dialog = super::member_review_dialog(&parent, &review);
        let content = dialog.extra_child().unwrap();
        super::connect_member_decisions(&content, &review, &page, &dialog);
        parent.present();
        dialog.present();
        let mut widgets = vec![content];
        let button = loop {
            let widget = widgets.pop().expect("reject instructions button");
            if widget.widget_name() == "reject:instructions" {
                break widget.downcast::<gtk::Button>().unwrap();
            }
            let mut child = widget.first_child();
            while let Some(current) = child {
                child = current.next_sibling();
                widgets.push(current);
            }
        };
        button.emit_clicked();
        let timed_out = Rc::new(Cell::new(false));
        let timeout_state = timed_out.clone();
        let timeout =
            gtk::glib::timeout_add_local_once(std::time::Duration::from_secs(5), move || {
                timeout_state.set(true)
            });
        let context = gtk::glib::MainContext::default();
        while page.busy.get() && !timed_out.get() {
            context.iteration(true);
        }
        if !timed_out.get() {
            timeout.remove();
        }
        assert!(!timed_out.get(), "native member decision did not finish");
        assert!(dialog.is_visible(), "remaining decisions must stay open");
        let mut text = String::new();
        collect(&dialog.clone().upcast(), &mut text);
        assert!(text.contains("Call-log export"));
        assert!(!text.contains("Pending text"));
        dialog.close();
        parent.close();
        std::fs::remove_dir_all(scratch).unwrap();
    }

    fn selection(
        name: &str,
        change: &str,
        outcome: HandoffOutcome,
        message: &str,
    ) -> ShareSelectionPreview {
        ShareSelectionPreview {
            id: name.to_lowercase(),
            name: name.into(),
            team_change: change.into(),
            team_detail: format!("{change} detail."),
            notes: vec![],
            local: LocalHandoff {
                id: name.to_lowercase(),
                name: name.into(),
                outcome,
                message: message.into(),
            },
        }
    }

    fn selection_preview(selections: Vec<ShareSelectionPreview>) -> PushPreview {
        PushPreview {
            base_version: 3,
            local_fingerprint: "f".into(),
            added: vec![],
            changed: vec![],
            removed: vec![],
            definitions: vec![],
            selections,
        }
    }

    fn collect(widget: &gtk::Widget, text: &mut String) {
        if let Some(expander) = widget.downcast_ref::<gtk::Expander>() {
            if let Some(child) = expander.child() {
                collect(&child, text);
            }
        }
        if let Some(label) = widget.downcast_ref::<gtk::Label>() {
            text.push_str(&label.text());
            text.push('\n');
        }
        let mut child = widget.first_child();
        while let Some(w) = child {
            collect(&w, text);
            child = w.next_sibling();
        }
    }

    #[test]
    fn the_confirm_action_matches_what_the_share_will_do() {
        let switch = selection(
            "Linear",
            "Already shared",
            HandoffOutcome::Switched,
            "switches",
        );
        let kept = selection("Linear", "Already shared", HandoffOutcome::Kept, "keeps");
        let blocked = selection(
            "Vercel",
            "Already shared",
            HandoffOutcome::Attention,
            "needs setup",
        );
        assert_eq!(
            share_action(&selection_preview(vec![switch.clone(), blocked.clone()])),
            Some("Use Team copies")
        );
        assert_eq!(share_action(&selection_preview(vec![kept, blocked])), None);
        let mut update = selection_preview(vec![switch]);
        update.changed = vec!["Linear".into()];
        update.definitions = vec![crate::teams::ShareDefinitionPreview {
            id: "linear".into(),
            name: "Linear".into(),
            change: "Changed".into(),
            transport: "http".into(),
            fields: vec![],
        }];
        assert_eq!(share_action(&update), Some("Share selected"));
    }

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn share_preview_explains_each_selection_and_its_route() {
        adw::init().unwrap();
        let mut same_name = selection("Linear", "New", HandoffOutcome::Switched, "This profile switches to the Team copy. Your personal server stays saved and turns off here.");
        same_name.notes = vec!["The team also has a separate definition named Linear (ID linear-2). It stays separate because sharing matches server IDs, not names.".into()];
        let preview = selection_preview(vec![
            same_name,
            selection("Vercel (Full API)", "Already shared", HandoffOutcome::Attention, "This team copy already has its own local credentials. Keep its existing setup and enable it separately. Your personal server stays on in this profile."),
        ]);
        let parent = gtk::Window::builder()
            .default_width(1000)
            .default_height(760)
            .build();
        parent.present();
        let dialog = share_preview_dialog(&parent, &preview);
        let mut text = String::new();
        collect(dialog.upcast_ref(), &mut text);
        for expected in [
            "Linear · New",
            "(ID linear-2)",
            "Vercel (Full API) · Already shared",
            "Your personal server stays on in this profile.",
            "Nothing new is uploaded to the team. Only this profile changes.",
        ] {
            assert!(text.contains(expected), "missing {expected}:\n{text}");
        }
        assert!(!text.contains("Added (0)"), "{text}");
        assert!(dialog.is_response_enabled("push"));
        assert_eq!(dialog.response_label("push"), "Use Team copies");
        dialog.present();
        if std::env::var_os("TOOLPORT_SHARE_PREVIEW_CAPTURE").is_some() {
            let main_loop = gtk::glib::MainLoop::new(None, false);
            let stop = main_loop.clone();
            gtk::glib::timeout_add_seconds_local_once(180, move || stop.quit());
            main_loop.run();
        }
        dialog.close();

        let picker = share_choice_label(
            "Linear",
            None,
            Some("Shared. The Team copy is in use in this profile."),
        );
        let mut text = String::new();
        collect(picker.upcast_ref(), &mut text);
        assert_eq!(
            text,
            "Linear\nShared. The Team copy is in use in this profile.\n"
        );
        parent.close();
    }

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn share_preview_widgets_show_allowlisted_definitions() {
        adw::init().unwrap();
        let preview = crate::teams::build_push_preview(3, &serde_json::json!([
            {"id":"http", "name":"Internal knowledge", "url":"https://old.test"},
            {"id":"removed", "name":"Retired tools"}
        ]), &serde_json::json!([
            {"id":"stdio", "name":"Project tools", "transport":"stdio", "command":"python3",
             "args":["/home/test/projects/a-long-workspace-name/services/tool-server/server.py", "--workspace", "/home/test/projects/team workspace", "--verbose"],
             "cwd":"/home/test/projects/a-long-workspace-name/services/tool-server",
             "env":[{"key":"GITHUB_TOKEN","value":"SYNTHETIC_ENV_SECRET"},{"key":"WORKSPACE_KEY"},{"key":"SERVICE_ACCOUNT_TOKEN"}]},
            {"id":"http", "name":"Internal knowledge", "transport":"http", "url":"https://example.internal/platform/knowledge/mcp?workspace=engineering&region=us-east",
             "env":[{"key":"API_TOKEN","value":"SYNTHETIC_HTTP_SECRET"}], "oauthToken":"SYNTHETIC_OAUTH_SECRET"}
        ])).unwrap();
        let parent = gtk::Window::builder()
            .title("Phase 4 controlled GTK preview")
            .default_width(1000)
            .default_height(760)
            .build();
        parent.present();
        let dialog = share_preview_dialog(&parent, &preview);
        let mut text = String::new();
        collect(dialog.upcast_ref(), &mut text);
        for expected in [
            "python3",
            "--workspace",
            "Working directory",
            "Endpoint",
            "API_TOKEN",
            "GITHUB_TOKEN",
            "Added (1)",
            "Changed (1)",
            "Removed (1)",
            "personal servers remain saved",
            "Other team servers",
        ] {
            assert!(text.contains(expected), "missing {expected}");
        }
        assert!(!text.contains("SYNTHETIC_"));
        dialog.present();
        if std::env::var_os("TOOLPORT_SHARE_PREVIEW_CAPTURE").is_some() {
            let main_loop = gtk::glib::MainLoop::new(None, false);
            let stop = main_loop.clone();
            gtk::glib::timeout_add_seconds_local_once(180, move || stop.quit());
            main_loop.run();
        }
        dialog.close();
        parent.close();
    }

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn sync_receipt_failure_is_visible_with_success_history() {
        adw::init().unwrap();
        let _data = crate::registry::DataDirTestEnv::new("native-sync-receipt-failure");
        let conn: crate::registry::TeamConnection = serde_json::from_value(serde_json::json!({
            "serverUrl":"https://example.invalid", "teamId":"one", "role":"member"
        }))
        .unwrap();
        let reg = crate::registry::Registry {
            team: Some(conn.clone()),
            ..Default::default()
        };
        crate::registry::save(&reg).unwrap();
        crate::team_sync_status::record(&conn, Ok(())).unwrap();
        let success = crate::team_sync_status::current().last_success_ms;
        let receipt = crate::registry::conduit_dir()
            .unwrap()
            .join("team-sync-status.json");
        std::fs::remove_file(&receipt).unwrap();
        std::fs::create_dir(&receipt).unwrap();
        assert!(crate::team_sync_status::record(&conn, Err("HTTP 500")).is_err());
        let app = adw::Application::builder()
            .application_id("app.toolport.SyncFixture")
            .build();
        let (_, server_page, _) =
            super::super::build_content(&app, crate::approval_broker::start_native());
        let page = super::TeamsPage::new(&app, server_page);
        page.render_sync_failure("HTTP 500. Sync status could not be saved");
        assert!(page.feedback.is_visible());
        assert!(page.feedback.has_css_class("error"));
        let text = page.feedback.label();
        assert!(text.contains("Team sync failed"));
        assert!(text.contains("Last successful sync:"));
        assert!(!text.contains("not recorded yet"));
        assert!(text.contains("HTTP 500. Sync status could not be saved"));
        assert!(!text.contains("Last sync succeeded"));
        assert_eq!(crate::team_sync_status::current().last_success_ms, success);
    }

    #[test]
    fn merge_notices_explain_held_and_blocked_servers() {
        assert_eq!(team_review_line(0, 0), None);
        assert_eq!(
            team_review_line(1, 0).unwrap(),
            "1 team change is waiting for your review. Held servers stay off; review queued changes above."
        );
        assert_eq!(
            team_review_line(2, 1).unwrap(),
            "2 team changes are waiting for your review. Held servers stay off; review queued changes above. \
             1 was Blocked because of unsafe definitions or references. env: references are local only, including personal Pro sync. Use a password manager reference instead."
        );
    }
}
