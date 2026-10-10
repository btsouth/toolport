use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;

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
    feedback: gtk::Label,
    busy: Rc<Cell<bool>>,
    pending: Rc<RefCell<Option<PendingJoin>>>,
    polling: Rc<Cell<bool>>,
    poll_timer: Rc<RefCell<Option<gtk::glib::SourceId>>>,
    /// A removal or review/blocked notice from the last sync, applied by the
    /// next render so the async refresh cannot overwrite it.
    sync_notice: Rc<RefCell<Option<(String, bool)>>>,
    rendered_state: Rc<RefCell<Option<(String, bool)>>>,
}

impl TeamsPage {
    pub(super) fn new(app: &adw::Application, server_page: super::ServerPage) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("toolport-content");
        let header = adw::HeaderBar::new();
        header.add_css_class("toolport-header");
        header.set_show_back_button(true);
        header.set_title_widget(Some(
            &gtk::Label::builder()
                .label("Sync")
                .css_classes(["title"])
                .build(),
        ));
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
        let title_row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        title_row.append(
            &gtk::Label::builder()
                .label("Sync")
                .halign(gtk::Align::Start)
                .css_classes(["title-2"])
                .build(),
        );
        // Seat count and wording come from `teams_plan`, which is checked against
        // the React shell's `teamsPlan.ts`. Quoting a price the other shell does
        // not quote is how two surfaces end up making two different claims.
        title_row.append(
            &gtk::Label::builder()
                .label("Free: 1 person, 1 device")
                .valign(gtk::Align::Center)
                .css_classes(["toolport-badge", "success", "caption"])
                .build(),
        );
        page.append(&title_row);
        page.append(
            &gtk::Label::builder()
                .label("Set up once. Your servers follow you to every machine. Secret values and approvals stay on this machine.")
                .halign(gtk::Align::Fill)
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
        feedback.set_visible(false);
        page.append(&feedback);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        page.append(&content);
        scroller.set_child(Some(&page));
        root.append(&scroller);
        Self {
            root,
            app: app.clone(),
            server_page,
            content,
            feedback,
            busy: Rc::new(Cell::new(false)),
            pending: Rc::new(RefCell::new(None)),
            polling: Rc::new(Cell::new(false)),
            poll_timer: Rc::new(RefCell::new(None)),
            sync_notice: Rc::new(RefCell::new(None)),
            rendered_state: Rc::new(RefCell::new(None)),
        }
    }

    pub(super) fn refresh(&self) {
        if self.busy.replace(true) {
            return;
        }
        self.feedback.set_label("Loading sync status…");
        self.server_page.reprobe_if_stale();
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(crate::registry::load).await;
            page.busy.set(false);
            match result {
                Ok(Ok(registry)) => page.render(registry),
                Ok(Err(error)) => page.show_error(&error),
                Err(_) => page.show_error("the team status read stopped unexpectedly"),
            }
        });
    }

    pub(super) fn attach_background_sync(&self, window: &adw::ApplicationWindow) {
        let running = Rc::new(Cell::new(false));
        let next_allowed = Rc::new(RefCell::new(std::time::Instant::now()));
        let page = self.clone();
        let failures = Rc::new(Cell::new(0u32));
        let running_for_timer = running.clone();
        let next_for_timer = next_allowed.clone();
        let source = gtk::glib::timeout_add_local(std::time::Duration::from_secs(1), move || {
            if running_for_timer.get() || std::time::Instant::now() < *next_for_timer.borrow() {
                return gtk::glib::ControlFlow::Continue;
            }
            running_for_timer.set(true);
            let page = page.clone();
            let running = running_for_timer.clone();
            let next_allowed = next_for_timer.clone();
            let failures = failures.clone();
            gtk::glib::spawn_future_local(async move {
                let result = gtk::gio::spawn_blocking(|| {
                    if crate::registry::load()?.team.is_none() {
                        return Ok::<_, String>(None);
                    }
                    crate::teams::sync_wait(25).map(Some)
                })
                .await;
                running.set(false);
                match result {
                    Ok(Ok(Some(outcome))) => {
                        failures.set(0);
                        *next_allowed.borrow_mut() =
                            std::time::Instant::now() + std::time::Duration::from_secs(3);
                        page.absorb_sync_result(outcome);
                    }
                    Ok(Ok(None)) => {
                        failures.set(0);
                        *next_allowed.borrow_mut() =
                            std::time::Instant::now() + std::time::Duration::from_secs(10);
                    }
                    Ok(Err(error)) => {
                        failures.set(failures.get().saturating_add(1));
                        *next_allowed.borrow_mut() = std::time::Instant::now()
                            + std::time::Duration::from_secs(crate::teams::retry_delay_seconds(
                                failures.get(),
                            ));
                        if page.root.is_mapped() {
                            page.render_sync_failure(&error);
                        }
                    }
                    Err(_) => {
                        failures.set(failures.get().saturating_add(1));
                        *next_allowed.borrow_mut() = std::time::Instant::now()
                            + std::time::Duration::from_secs(crate::teams::retry_delay_seconds(
                                failures.get(),
                            ));
                        if page.root.is_mapped() {
                            page.show_error(
                                "team sync stopped unexpectedly; retrying automatically",
                            );
                        }
                    }
                }
            });
            gtk::glib::ControlFlow::Continue
        });
        let source = Rc::new(RefCell::new(Some(source)));
        window.connect_destroy(move |_| {
            if let Some(source) = source.borrow_mut().take() {
                source.remove();
            }
        });
    }

    fn render_sync_failure(&self, error: &str) {
        self.render_sync_status();
        self.set_status(&format!("{} {error}", self.feedback.label()), true);
    }

    fn render_sync_status(&self) {
        let status = crate::team_sync_status::current();
        let last = status
            .last_success_ms
            .and_then(|ms| gtk::glib::DateTime::from_unix_local(ms as i64 / 1000).ok())
            .and_then(|date| date.format("%b %d, %Y at %H:%M").ok())
            .map(|date| date.to_string())
            .unwrap_or_else(|| "not recorded yet".into());
        self.set_status(
            &format!(
                "{}. Last successful sync: {last}.",
                crate::team_sync_status::summary(&status)
            ),
            matches!(status.state.as_str(), "offline" | "error"),
        );
        self.feedback.remove_css_class("success");
    }

    fn render(&self, registry: crate::registry::Registry) {
        let notice = self.sync_notice.borrow_mut().take();
        let render_state = (
            serde_json::to_string(&registry).unwrap_or_default(),
            self.pending.borrow().is_some(),
        );
        if notice.is_none() && self.rendered_state.borrow().as_ref() == Some(&render_state) {
            if let Some(error) = crate::personal_sync::state(&registry)
                .ok()
                .and_then(|s| s.error)
            {
                self.set_status(&error, true);
                return;
            }
            if registry.team.is_some() {
                self.render_sync_status();
            } else if self.pending.borrow().is_some() {
                self.set_status("Waiting for invitation approval.", false);
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
            self.set_status(&notice, is_error);
            if !is_error {
                self.feedback.add_css_class("success");
            }
            if let Some(team) = registry.team.clone() {
                self.render_connected(registry, team);
            } else {
                self.render_join();
            }
            return;
        }
        if let Some(team) = registry.team.clone() {
            self.render_sync_status();
            self.render_connected(registry, team);
        } else {
            self.feedback.remove_css_class("success");
            if self.pending.borrow().is_some() {
                self.set_status("Waiting for invitation approval.", false);
            } else {
                self.set_status("", false);
            }
            self.render_join();
            if self.pending.borrow().is_some() {
                self.schedule_join_poll();
            }
        }
    }

    /// What Teams actually buys you. Only rendered while disconnected: someone
    /// who has already joined does not need the pitch, they need their team.
    fn render_value_props(&self) {
        let cards = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .min_children_per_line(1)
            .max_children_per_line(2)
            .column_spacing(10)
            .row_spacing(10)
            .homogeneous(true)
            .build();
        for (title, detail) in [
            (
                "One shared server set",
                "Everyone connects to the same servers. No copying config between machines.",
            ),
            (
                "Rules travel with it",
                "Team instructions land in each member's agent files, alongside their own.",
            ),
            (
                "Nothing runs unreviewed",
                "Local commands and private endpoints wait for each member to approve them.",
            ),
            (
                "Published, not copy-pasted",
                "Compare your local servers against the team's and publish only the differences you choose.",
            ),
        ] {
            let card = gtk::Box::new(gtk::Orientation::Vertical, 5);
            card.add_css_class("toolport-value-card");
            card.append(
                &gtk::Label::builder()
                    .label(title)
                    .halign(gtk::Align::Start)
                    .xalign(0.0)
                    .wrap(true)
                    .max_width_chars(22)
                    .css_classes(["heading"])
                    .build(),
            );
            card.append(
                &gtk::Label::builder()
                    .label(detail)
                    .halign(gtk::Align::Start)
                    .xalign(0.0)
                    .wrap(true)
                    // Without a cap the natural width of a full sentence is wide
                    // enough that three cards cannot share a line, and the
                    // FlowBox drops them to one per row.
                    .max_width_chars(30)
                    .css_classes(["caption", "toolport-muted"])
                    .build(),
            );
            cards.append(&card);
        }
        self.content.append(&cards);
    }

    /// The three-step version, because "Sync service URL" and "Manual code" mean
    /// nothing to someone who has not been told how a team gets made.
    fn render_how_it_works(&self) {
        let group = gtk::Box::new(gtk::Orientation::Vertical, 9);
        group.add_css_class("toolport-settings-group");
        group.add_css_class("toolport-padded-group");
        group.append(
            &gtk::Label::builder()
                .label("How it works")
                .halign(gtk::Align::Start)
                .css_classes(["heading"])
                .build(),
        );
        for (number, text) in [
            (
                "1",
                "One person creates the team and adds the servers everyone should have.",
            ),
            ("2", "You join with the invite code they send you."),
            (
                "3",
                "Your agents pick up the team's servers and rules. Anything that runs on your own machine still waits for you to approve it.",
            ),
        ] {
            let step = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            step.append(
                &gtk::Label::builder()
                    .label(number)
                    .valign(gtk::Align::Start)
                    .css_classes(["toolport-badge", "caption"])
                    .build(),
            );
            step.append(
                &gtk::Label::builder()
                    .label(text)
                    .halign(gtk::Align::Fill)
                    .xalign(0.0)
                    .wrap(true)
                    .hexpand(true)
                    .css_classes(["toolport-muted"])
                    .build(),
            );
            group.append(&step);
        }
        self.content.append(&group);
    }

    fn render_join(&self) {
        if self.pending.borrow().is_some() {
            let cancel = gtk::Button::with_label("Cancel request");
            let page = self.clone();
            cancel.connect_clicked(move |_| {
                page.cancel_join_poll();
                *page.pending.borrow_mut() = None;
                page.refresh();
            });
            self.content.append(&cancel);
        }
        let sign_in = gtk::Button::with_label("Sign in to sync");
        sign_in.add_css_class("suggested-action");
        sign_in.connect_clicked(|_| {
            let _ =
                crate::oauth::open_web_url("https://teams.toolport.app/?intent=pro&from=app-sync");
        });
        self.content.append(&sign_in);
        self.content.append(
            &gtk::Label::builder()
                .label(crate::teams_plan::pro_line())
                .wrap(true)
                .xalign(0.0)
                .build(),
        );
        let group = gtk::Box::new(gtk::Orientation::Vertical, 10);
        group.add_css_class("toolport-settings-group");
        group.append(&super::section_heading("Use a manual code", "If your browser cannot open Toolport, copy the manual code shown after approving this device."));
        let url = gtk::Entry::builder()
            .text(crate::teams::HOSTED_TEAMS_URL)
            .build();
        let code = gtk::PasswordEntry::builder()
            .placeholder_text("Manual code")
            .show_peek_icon(true)
            .build();
        let name = gtk::Entry::builder()
            .placeholder_text("Your name (optional)")
            .build();
        group.append(&field("Sync service URL", &url));
        group.append(&field("Manual code", &code));
        group.append(&field("Your name", &name));
        let button = gtk::Button::with_label("Sign in with code");
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
        self.content.append(&group);
    }

    fn render_connected(
        &self,
        registry: crate::registry::Registry,
        team: crate::registry::TeamConnection,
    ) {
        if crate::personal_sync::is_personal(&registry) {
            self.render_personal(registry, team);
            return;
        }
        let summary = gtk::Box::new(gtk::Orientation::Vertical, 8);
        summary.add_css_class("toolport-card");
        summary.append(
            &gtk::Label::builder()
                .label(
                    team.team_name
                        .clone()
                        .unwrap_or_else(|| format!("Team {}", team.team_id)),
                )
                .halign(gtk::Align::Start)
                .css_classes(["heading"])
                .build(),
        );
        summary.append(
            &gtk::Label::builder()
                .label(format!(
                    "{} · {} · config version {}",
                    team.server_url, team.role, team.last_version
                ))
                .halign(gtk::Align::Start)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["toolport-muted"])
                .build(),
        );
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let sync = gtk::Button::with_label("Sync now");
        sync.add_css_class("suggested-action");
        let page_for_sync = self.clone();
        sync.connect_clicked(move |button| page_for_sync.sync(button.clone()));
        actions.append(&sync);
        if team.role == "admin" {
            let push = gtk::Button::with_label("Share selected servers");
            push.add_css_class("toolport-secondary-action");
            let page_for_push = self.clone();
            push.connect_clicked(move |button| page_for_push.select_servers(button.clone()));
            actions.append(&push);
        }
        let account_link = gtk::Button::with_label("Link portal account");
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
                        .set_text("Could not prepare account link. Please try again."),
                }
                button.set_sensitive(true);
            });
        });
        if team.account_linked != Some(true) {
            actions.append(&account_link);
        }
        let leave = gtk::Button::with_label("Disconnect app");
        leave.add_css_class("destructive-action");
        let page_for_leave = self.clone();
        leave.connect_clicked(move |button| page_for_leave.confirm_leave(button.clone()));
        actions.append(&leave);
        summary.append(&actions);
        self.content.append(&summary);
        self.content.append(&gtk::Label::builder().label("Open Clients to use your enabled team servers from an AI client. Review any remaining servers below before enabling them. Successful managed calls are reported automatically.").wrap(true).xalign(0.0).build());

        match crate::teams::member_review(&registry) {
            Ok(review) if !review.pending.is_empty() => {
                let button = gtk::Button::with_label(&format!(
                    "Review {} team changes",
                    review.pending.len()
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
                self.content.append(&gtk::Label::builder().label("Held servers stay off and instructions stay unchanged. Safety floors can tighten immediately.").wrap(true).xalign(0.0).build());
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
            self.content.append(
                &gtk::Label::builder()
                    .label("Team-managed servers · review and local setup")
                    .halign(gtk::Align::Start)
                    .css_classes(["heading"])
                    .build(),
            );
            for server in review {
                self.content
                    .append(&review_server_row(server, self.clone()));
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
        let status = team
            .unknown_fields
            .get("accountStatus")
            .cloned()
            .unwrap_or_default();
        let sync = crate::personal_sync::state(&registry).unwrap_or_default();
        let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
        card.add_css_class("toolport-card");
        card.append(
            &gtk::Label::builder()
                .label("Your account")
                .xalign(0.0)
                .css_classes(["heading"])
                .build(),
        );
        for text in crate::personal_sync::status_lines(&status, sync.last_synced_at) {
            card.append(
                &gtk::Label::builder()
                    .label(text)
                    .wrap(true)
                    .xalign(0.0)
                    .build(),
            );
        }
        if let Some(error) = &sync.error {
            self.set_status(error, true);
        }
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let button = gtk::Button::with_label("Sync now");
        let page = self.clone();
        button.connect_clicked(move |b| page.sync(b.clone()));
        actions.append(&button);
        let button = gtk::Button::with_label("Your account");
        let origin = team.server_url.clone();
        button.connect_clicked(move |_| {
            let _ = crate::oauth::open_web_url(&origin);
        });
        actions.append(&button);
        let button = gtk::Button::with_label("Sign out");
        let page = self.clone();
        button.connect_clicked(move |b| page.confirm_leave(b.clone()));
        actions.append(&button);
        card.append(&actions);
        self.content.append(&card);
        for (id, remote) in &sync.conflicts {
            let row = gtk::Box::new(gtk::Orientation::Vertical, 8);
            row.add_css_class("toolport-card");
            row.append(
                &gtk::Label::builder()
                    .label(format!(
                        "{id} changed on both machines. Choose which version to keep."
                    ))
                    .wrap(true)
                    .xalign(0.0)
                    .build(),
            );
            row.append(
                &gtk::Label::builder()
                    .label(serde_json::to_string_pretty(remote).unwrap_or_default())
                    .selectable(true)
                    .wrap(true)
                    .xalign(0.0)
                    .build(),
            );
            for (keep, label) in [
                (true, "Keep this machine's version"),
                (false, "Use synced version"),
            ] {
                let button = gtk::Button::with_label(label);
                let id = id.clone();
                let expected = remote.clone();
                let page = self.clone();
                button.connect_clicked(move |_| {
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
                            Err(_) => page.show_error("Could not resolve sync conflict"),
                        }
                    });
                });
                row.append(&button);
            }
            self.content.append(&row);
        }
        self.content.append(&gtk::Label::builder().label("Changes in Servers sync automatically. Use Sync settings for values that are the same on every machine or a setup kept only here.").wrap(true).xalign(0.0).build());
        for server in registry
            .servers
            .iter()
            .filter(|s| s.source.as_deref() == Some(&format!("team:{}", team.team_id)))
        {
            self.content
                .append(&review_server_row(server.clone(), self.clone()));
        }
    }

    fn connect_team(&self, url: String, code: String, name: String, button: gtk::Button) {
        if url.trim().is_empty() || code.trim().is_empty() {
            self.show_error("Enter the sync service URL and manual code");
            return;
        }
        button.set_sensitive(false);
        self.feedback.set_label("Signing in to sync…");
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
                Err(_) => page.show_error("the team connection stopped unexpectedly"),
            }
        });
    }

    fn poll_join(&self, button: gtk::Button) {
        self.run_join_poll(Some(button));
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
        self.feedback.set_label("Checking join approval…");
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
                    page.feedback.set_label(
                        "Still waiting for invitation approval. Checking again automatically…",
                    );
                    page.schedule_join_poll();
                }
                Ok(Ok(crate::teams::JoinPoll::Denied)) => {
                    page.cancel_join_poll();
                    *page.pending.borrow_mut() = None;
                    page.show_error("the team administrator denied this join request");
                }
                Ok(Ok(crate::teams::JoinPoll::Unknown)) => {
                    page.cancel_join_poll();
                    *page.pending.borrow_mut() = None;
                    page.show_error("the join request expired or is no longer available");
                }
                Ok(Err(error)) => {
                    page.show_error(&format!("{error}. Retrying the join request automatically"));
                    page.schedule_join_poll();
                }
                Err(_) => {
                    page.show_error(
                        "the approval check stopped unexpectedly; retrying automatically",
                    );
                    page.schedule_join_poll();
                }
            }
        });
    }

    fn sync(&self, button: gtk::Button) {
        button.set_sensitive(false);
        self.feedback.set_label("Syncing team configuration…");
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(crate::teams::sync_now).await;
            button.set_sensitive(true);
            match result {
                Ok(Ok(outcome)) => page.absorb_sync_result(outcome),
                Ok(Err(error)) => page.show_error(&error),
                Err(_) => page.show_error("the team sync stopped unexpectedly"),
            }
        });
    }

    /// Route a finished sync into the UI. Removal and safety-blocked servers
    /// must be said out loud: servers vanishing (or silently never arriving)
    /// with no explanation reads as data loss. The notice is parked in page
    /// state because `refresh` renders asynchronously and would otherwise
    /// overwrite whatever is set here.
    fn absorb_sync_result(&self, result: crate::teams::SyncResult) {
        match result {
            crate::teams::SyncResult::Removed => {
                let message = "You were removed from the team. Its shared servers \
                               and this machine's team token have been cleared.";
                *self.sync_notice.borrow_mut() = Some((message.to_string(), true));
                let notification = gtk::gio::Notification::new("Removed from team");
                notification.set_body(Some(message));
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
        let Some(parent) = self.app.active_window() else {
            return;
        };
        let reg = match crate::registry::load() {
            Ok(r) => r,
            Err(e) => {
                self.show_error(&e);
                return;
            }
        };
        #[allow(deprecated)]
        let dialog = adw::MessageDialog::new(Some(&parent), Some("Share with your team"), Some("Choose one working server to begin. Other team servers and your personal originals stay in place. Each member supplies credentials locally. The preview shows how each choice relates to the team before anything is uploaded."));
        dialog.set_size_request(480, -1);
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("select", "Preview selected");
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
                (!keys.is_empty()).then(|| format!("Local credentials: {keys}")),
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
            list.append(&gtk::Label::new(Some(
                "Add a working personal server in Servers first.",
            )));
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
            .set_label("Comparing local and shared team servers…");
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let ids = selected.clone();
            let result =
                gtk::gio::spawn_blocking(move || crate::teams::preview_push_selected(&ids)).await;
            button.set_sensitive(true);
            match result {
                Ok(Ok(preview)) => page.show_push_review(preview, selected),
                Ok(Err(error)) => page.show_error(&error),
                Err(_) => page.show_error("the team comparison stopped unexpectedly"),
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
                page.feedback.set_label("Updating shared team servers…");
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
                                        Some("Sent for confirmation"),
                                        Some(
                                            "Finish publishing this update in the Teams dashboard.",
                                        ),
                                    );
                                    confirmation.add_response("close", "Close");
                                    confirmation.add_response("open", "Open");
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
                        Err(_) => page.show_error("the team update stopped unexpectedly"),
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
        let personal = crate::registry::load().is_ok_and(|r| crate::personal_sync::is_personal(&r));
        #[allow(deprecated)]
        let dialog = adw::MessageDialog::new(
            Some(&parent),
            Some(if personal {
                "Sign out of sync?"
            } else {
                "Disconnect this app from the team?"
            }),
            Some(if personal {
                "Synced servers are removed from this app. Your setup stays in Your account. Sign in again to restore it."
            } else {
                "Team servers, instructions and policy are removed from this app. Your personal servers stay saved. Your Team membership and shared setup remain. Reconnect from the Teams website."
            }),
        );
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("leave", "Disconnect app");
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
                        Err(_) => page.show_error("the team disconnect stopped unexpectedly"),
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

    fn show_error(&self, error: &str) {
        self.feedback.set_label(&format!("Sync error: {error}"));
        self.feedback.set_visible(true);
        self.feedback.remove_css_class("success");
        self.feedback.add_css_class("error");
    }
}

fn render_instructions_status(container: &gtk::Box, status: crate::teams::InstructionsStatusView) {
    let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    heading.append(
        &gtk::Label::builder()
            .label("Team instructions")
            .halign(gtk::Align::Start)
            .css_classes(["heading"])
            .build(),
    );
    heading.append(
        &gtk::Label::builder()
            .label(format!("v{}", status.version))
            .halign(gtk::Align::Start)
            .css_classes(["toolport-muted"])
            .build(),
    );
    container.append(&heading);
    container.append(
        &gtk::Label::builder()
            .label(
                "Org-managed agent rules, written to your AI clients alongside your own \
                 instructions, never over them. Leaving the team removes them.",
            )
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
        container.append(
            &gtk::Label::builder()
                .label("No supported AI clients detected on this machine.")
                .halign(gtk::Align::Start)
                .xalign(0.0)
                .css_classes(["toolport-muted"])
                .build(),
        );
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
        let badge = gtk::Label::new(Some(state_label));
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
    if review == 0 && blocked == 0 {
        return None;
    }
    let mut parts = Vec::new();
    if review > 0 {
        parts.push(format!(
            "{review} team {} waiting for your review. Held servers stay off; review queued changes above.",
            if review == 1 { "change is" } else { "changes are" },
        ));
    }
    if blocked > 0 {
        parts.push(format!(
            "{blocked} {} Blocked because of unsafe definitions or references. env: references are local only, including personal Pro sync. Use a password manager reference instead.",
            if blocked == 1 { "was" } else { "were" }
        ));
    }
    Some(parts.join(" "))
}

fn field(label: &str, input: &impl IsA<gtk::Widget>) -> gtk::Box {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 4);
    root.append(
        &gtk::Label::builder()
            .label(label)
            .halign(gtk::Align::Start)
            .css_classes(["heading"])
            .build(),
    );
    root.append(input);
    root
}

fn review_server_row(server: crate::registry::ServerEntry, page: TeamsPage) -> gtk::Box {
    if let Ok(registry) = crate::registry::load() {
        if registry.is_enabled(&registry.active_profile_id(), &server.id) {
            let snapshot = super::state::RegistrySnapshot::from_registry(registry);
            if let Some(view) = snapshot.servers.iter().find(|s| s.id == server.id) {
                return super::server_card(
                    view,
                    &snapshot.active_profile_id,
                    page.server_page.clone(),
                );
            }
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
    let credentials = server
        .env
        .iter()
        .map(|e| e.key.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let target = if let Some(command) = &server.command {
        format!(
            "Command: {command}\nArguments: {}\nWorking directory: {}\nLocal keys: {}",
            serde_json::to_string(&server.args).unwrap_or_default(),
            server.cwd.as_deref().unwrap_or("Inherit from client"),
            if credentials.is_empty() {
                "Sign-in requirements are checked when this server connects"
            } else {
                &credentials
            }
        )
    } else {
        format!(
            "URL: {}\nLocal keys: {}",
            server.url.as_deref().unwrap_or("Unknown target"),
            if credentials.is_empty() {
                "Sign-in requirements are checked when this server connects"
            } else {
                &credentials
            }
        )
    };
    let target = format!(
        "{target}\n{}",
        crate::secret_refs::review_lines(&server).join("\n")
    );
    copy.append(
        &gtk::Label::builder()
            .label(&target)
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-muted"])
            .build(),
    );
    row.append(&copy);
    let original = crate::registry::load().ok().and_then(|r| {
        let id = r.team.as_ref()?.managed_server_ids.get(&server.id)?;
        r.servers
            .iter()
            .find(|s| {
                &s.id == id
                    && !s.source.as_deref().unwrap_or("").starts_with("team:")
                    && r.is_enabled(&r.active_profile_id(), id)
            })
            .map(|s| s.name.clone())
    });
    if let Some(original) = original {
        let use_managed = gtk::Button::with_label("Use in this profile");
        use_managed.set_valign(gtk::Align::Center);
        let page = page.clone();
        let id = server.id.clone();
        let detail = target.clone();
        use_managed.connect_clicked(move |_| {
            let Some(parent) = page.app.active_window() else { return; };
            #[allow(deprecated)]
            let d = adw::MessageDialog::new(Some(&parent), Some("Use team-managed version?"), Some(&format!("{detail}\n\nEnable this managed copy and disable Personal {original} in this profile. The original stays saved. Existing local credentials and sign-in are reused only when the definitions match exactly. Nothing is uploaded. Signing out affects both copies.")));
            d.set_size_request(520, -1);
            d.add_response("cancel", "Cancel"); d.add_response("use", "Use managed version"); d.set_close_response("cancel");
            let page = page.clone(); let id = id.clone();
            d.connect_response(None, move |d, response| {
                if response == "use" { let page = page.clone(); let id = id.clone(); gtk::glib::spawn_future_local(async move {
                    match gtk::gio::spawn_blocking(move || crate::teams::use_managed_server(&id)).await {
                        Ok(Ok(_)) => page.refresh(), Ok(Err(e)) => page.show_error(&e), Err(_) => page.show_error("Could not switch to managed server"),
                    }
                }); }
                d.close();
            }); d.present();
        });
        row.append(&use_managed);
    }
    let already_enabled =
        crate::registry::load().is_ok_and(|r| r.is_enabled(&r.active_profile_id(), &server.id));
    let enable = gtk::Button::with_label(if already_enabled {
        "Enabled in this profile"
    } else {
        "Review and enable"
    });
    enable.set_valign(gtk::Align::Center);
    let held = server.unknown_fields.get("teamHeldChange") == Some(&serde_json::json!(true));
    if held {
        enable.set_label("Held for team change review");
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
            Some(&format!("Enable {server_name}?")),
            Some(&format!("{target}\n\nEnable only after verifying this definition and saved authentication. Credentials remain local.")),
        );
        dialog.add_response("cancel", "Keep disabled");
        dialog.add_response("enable", "Enable");
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
                gtk::glib::spawn_future_local(async move {
                    let result = gtk::gio::spawn_blocking(move || {
                        let registry = crate::registry::load()?;
                        crate::personal_sync::enable_reviewed(&registry.active_profile_id(), &reviewed_entry)
                    })
                    .await;
                    match result {
                        Ok(Ok(_)) => page.refresh(),
                        Ok(Err(error)) => page.show_error(&error),
                        Err(_) => page.show_error("the review update stopped unexpectedly"),
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
        Some("Share selected")
    } else if preview
        .selections
        .iter()
        .any(|selection| selection.local.outcome == crate::teams::HandoffOutcome::Switched)
    {
        Some("Use Team copies")
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
        Some(parent), Some("Share selected servers?"),
        Some("Other team servers, instructions and policies stay unchanged. Your personal servers remain saved, and other profiles stay unchanged. Credential values are never uploaded. Each member uses their own credentials locally."),
    );
    dialog.set_size_request(520, -1);
    dialog.add_response("cancel", "Cancel");
    let action = share_action(preview);
    dialog.add_response("push", action.unwrap_or("Share selected"));
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
        changes.append(&label(if share_action(preview).is_some() {
            "Nothing new is uploaded to the team. Only this profile changes."
        } else {
            "Nothing to upload or switch for this selection."
        }));
    }
    for (kind, names) in [
        ("Added", &preview.added),
        ("Changed", &preview.changed),
        ("Removed", &preview.removed),
    ]
    .into_iter()
    .filter(|_| uploads || preview.selections.is_empty())
    {
        let heading = label(&format!("{kind} ({})", names.len()));
        heading.add_css_class("heading");
        changes.append(&heading);
        if names.is_empty() {
            changes.append(&label("None"));
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
            let summary = label(&format!("{} · {}", definition.name, definition.transport));
            summary.set_selectable(false);
            expander.set_label_widget(Some(&summary));
            let fields = gtk::Box::new(gtk::Orientation::Vertical, 6);
            fields.set_margin_start(16);
            for field in &definition.fields {
                let title = label(&field.label);
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
    changes.append(&label("If the team or your local servers change before saving, Toolport stops and asks you to review again."));
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_width(440)
        .max_content_height(360)
        .propagate_natural_height(true)
        .child(&changes)
        .build()
}

fn member_label(label: &serde_json::Value) -> String {
    let author = label["author"]["name"].as_str().unwrap_or("Unknown author");
    let time = label["at"]
        .as_i64()
        .and_then(|ms| gtk::glib::DateTime::from_unix_utc(ms / 1000).ok())
        .and_then(|date| date.format("%Y-%m-%d %H:%M UTC").ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "Unknown time".into());
    let via = label["via"].as_str().unwrap_or("unknown");
    let approval = label["approvedBy"]["name"]
        .as_str()
        .map(|name| format!(" · approved by {name}"))
        .unwrap_or_default();
    format!("{author} · {time} · via {via}{approval}")
}

fn member_review_dialog(
    parent: &impl IsA<gtk::Window>,
    review: &crate::teams::MemberReview,
) -> adw::MessageDialog {
    let dialog = adw::MessageDialog::new(Some(parent), Some("Review team changes"), Some("Each decision applies to this member and the exact content shown. Held servers stay off. Safety floors can tighten immediately."));
    dialog.add_response("close", "Close");
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
            section.append(&gtk::Label::builder().label("Full diff from your accepted configuration. Change history labels unavailable.").wrap(true).xalign(0.0).build());
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
            section.append(
                &gtk::Label::builder()
                    .label(format!(
                        "{}\nBefore: {}\nAfter: {}",
                        field.field, field.before, field.after
                    ))
                    .wrap(true)
                    .selectable(true)
                    .xalign(0.0)
                    .build(),
            );
        }
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        for (name, accept) in [("Accept", true), ("Reject", false)] {
            let button = gtk::Button::with_label(name);
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
                                page.show_error(
                                    "Team review stopped unexpectedly. Review the current queue.",
                                );
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

#[cfg(test)]
mod tests {
    use super::share_preview_dialog;
    use super::team_review_line;
    use super::{share_action, share_choice_label};
    use crate::teams::{HandoffOutcome, LocalHandoff, PushPreview, ShareSelectionPreview};
    use adw::prelude::*;
    use std::{cell::Cell, rc::Rc};

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn member_review_native_shows_diff_labels_and_both_decisions() {
        adw::init().unwrap();
        let mut registry = crate::registry::Registry::default();
        registry.team = Some(serde_json::from_value(serde_json::json!({"teamId":"native-review", "serverUrl":"https://teams.toolport.app", "role":"member"})).unwrap());
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
        let mut reg = crate::registry::Registry::default();
        reg.team = Some(serde_json::from_value(serde_json::json!({"teamId":"native-review", "serverUrl":"https://teams.toolport.app", "role":"member"})).unwrap());
        crate::teams::stage_team_config(&mut reg, "native-review", &serde_json::json!({"servers":[], "instructions":{"content":"Pending text"}, "callAuditExport":true}), 1, &[]).unwrap();
        crate::registry::save(&reg).unwrap();
        let review = crate::teams::member_review(&reg).unwrap();
        let parent = adw::ApplicationWindow::builder().build();
        let app = adw::Application::builder().application_id("app.toolport.ReviewFixture").build();
        let (_, server_page, _) = super::super::build_content(&app, crate::approval_broker::start_native());
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
        let mut reg = crate::registry::Registry::default();
        reg.team = Some(conn.clone());
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
