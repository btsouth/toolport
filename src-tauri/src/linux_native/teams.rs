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
    content: gtk::Box,
    feedback: gtk::Label,
    busy: Rc<Cell<bool>>,
    pending: Rc<RefCell<Option<PendingJoin>>>,
    polling: Rc<Cell<bool>>,
    poll_timer: Rc<RefCell<Option<gtk::glib::SourceId>>>,
    /// A removal or review/blocked notice from the last sync, applied by the
    /// next render so the async refresh cannot overwrite it.
    sync_notice: Rc<RefCell<Option<String>>>,
}

impl TeamsPage {
    pub(super) fn new(app: &adw::Application) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("toolport-content");
        let header = adw::HeaderBar::new();
        header.add_css_class("toolport-header");
        header.set_show_back_button(true);
        header.set_title_widget(Some(
            &gtk::Label::builder()
                .label("Teams")
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
        page.set_margin_top(28);
        page.set_margin_bottom(28);
        page.set_margin_start(28);
        page.set_margin_end(28);
        page.append(
            &gtk::Label::builder()
                .label("Toolport Teams")
                .halign(gtk::Align::Start)
                .css_classes(["title-2"])
                .build(),
        );
        page.append(
            &gtk::Label::builder()
                .label("Share a governed server set with your team. Local commands and private endpoints always require member review before they run.")
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
        feedback.set_label("Open Teams to load connection status.");
        page.append(&feedback);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        page.append(&content);
        scroller.set_child(Some(&page));
        root.append(&scroller);
        Self {
            root,
            app: app.clone(),
            content,
            feedback,
            busy: Rc::new(Cell::new(false)),
            pending: Rc::new(RefCell::new(None)),
            polling: Rc::new(Cell::new(false)),
            poll_timer: Rc::new(RefCell::new(None)),
            sync_notice: Rc::new(RefCell::new(None)),
        }
    }

    pub(super) fn refresh(&self) {
        if self.busy.replace(true) {
            return;
        }
        self.feedback.set_label("Loading team status…");
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
        let window_for_timer = window.clone();
        let running_for_timer = running.clone();
        let next_for_timer = next_allowed.clone();
        let source = gtk::glib::timeout_add_local(std::time::Duration::from_secs(1), move || {
            if !window_for_timer.is_visible()
                || running_for_timer.get()
                || std::time::Instant::now() < *next_for_timer.borrow()
            {
                return gtk::glib::ControlFlow::Continue;
            }
            running_for_timer.set(true);
            let page = page.clone();
            let running = running_for_timer.clone();
            let next_allowed = next_for_timer.clone();
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
                        *next_allowed.borrow_mut() =
                            std::time::Instant::now() + std::time::Duration::from_secs(3);
                        page.absorb_sync_result(outcome);
                    }
                    Ok(Ok(None)) => {
                        *next_allowed.borrow_mut() =
                            std::time::Instant::now() + std::time::Duration::from_secs(10);
                    }
                    Ok(Err(error)) => {
                        *next_allowed.borrow_mut() =
                            std::time::Instant::now() + std::time::Duration::from_secs(15);
                        if page.root.is_mapped() {
                            page.show_error(&format!(
                                "team sync failed; retrying automatically: {error}"
                            ));
                        }
                    }
                    Err(_) => {
                        *next_allowed.borrow_mut() =
                            std::time::Instant::now() + std::time::Duration::from_secs(15);
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

    fn render(&self, registry: crate::registry::Registry) {
        while let Some(child) = self.content.first_child() {
            self.content.remove(&child);
        }
        if let Some(notice) = self.sync_notice.borrow_mut().take() {
            self.feedback.set_label(&notice);
            self.feedback.remove_css_class("success");
            self.feedback.add_css_class("error");
            if let Some(team) = registry.team.clone() {
                self.render_connected(registry, team);
            } else {
                self.render_join();
            }
            return;
        }
        if let Some(team) = registry.team.clone() {
            self.feedback.set_label("Team connection is up to date.");
            self.feedback.remove_css_class("error");
            self.feedback.add_css_class("success");
            self.render_connected(registry, team);
        } else {
            self.feedback.set_label(if self.pending.borrow().is_some() {
                "Join request is waiting for an administrator."
            } else {
                "Not connected to a team."
            });
            self.render_join();
            if self.pending.borrow().is_some() {
                self.schedule_join_poll();
            }
        }
    }

    fn render_join(&self) {
        let group = gtk::Box::new(gtk::Orientation::Vertical, 10);
        group.add_css_class("toolport-settings-group");
        let server_url = gtk::Entry::builder()
            .placeholder_text("https://teams.example.com")
            .build();
        let invite = gtk::PasswordEntry::builder()
            .placeholder_text("Invite or join-link code")
            .show_peek_icon(true)
            .build();
        let member = gtk::Entry::builder()
            .placeholder_text("Your name (optional)")
            .build();
        group.append(&field("Team server", &server_url));
        group.append(&field("Invite code", &invite));
        group.append(&field("Member name", &member));
        let join_actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        join_actions.set_halign(gtk::Align::End);
        if self.pending.borrow().is_some() {
            // A waiting join request must be abandonable: the admin may never
            // answer, and the form is unusable until the request is cleared.
            let cancel = gtk::Button::with_label("Cancel request");
            cancel.add_css_class("toolport-secondary-action");
            let page_for_cancel = self.clone();
            cancel.connect_clicked(move |_| {
                *page_for_cancel.pending.borrow_mut() = None;
                page_for_cancel.cancel_join_poll();
                page_for_cancel.refresh();
            });
            join_actions.append(&cancel);
        }
        let connect = gtk::Button::with_label(if self.pending.borrow().is_some() {
            "Check approval"
        } else {
            "Connect"
        });
        connect.add_css_class("suggested-action");
        let page = self.clone();
        connect.connect_clicked(move |button| {
            if page.pending.borrow().is_some() {
                page.poll_join(button.clone());
            } else {
                page.connect_team(
                    server_url.text().to_string(),
                    invite.text().to_string(),
                    member.text().to_string(),
                    button.clone(),
                );
            }
        });
        join_actions.append(&connect);
        group.append(&join_actions);
        self.content.append(&group);

        // The acquisition and exit paths the shipping tab offers alongside the
        // join form. All external, all through the validated opener.
        let links = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        for (label, url) in [
            (
                "Create a free team",
                "https://teams.toolport.app/?intent=create-team&from=app-teams-tab",
            ),
            ("How Teams works", "https://toolport.app/teams"),
            ("Pricing", "https://toolport.app/teams#pricing"),
            ("Self-host", "https://toolport.app/teams#selfhost"),
        ] {
            let link = gtk::Button::with_label(label);
            link.add_css_class("flat");
            link.connect_clicked(move |_| {
                let _ = crate::oauth::open_web_url(url);
            });
            links.append(&link);
        }
        self.content.append(&links);
    }

    fn render_connected(
        &self,
        registry: crate::registry::Registry,
        team: crate::registry::TeamConnection,
    ) {
        let summary = gtk::Box::new(gtk::Orientation::Vertical, 8);
        summary.add_css_class("toolport-card");
        summary.append(
            &gtk::Label::builder()
                .label(format!("Team {}", team.team_id))
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
            let push = gtk::Button::with_label("Update shared servers");
            push.add_css_class("toolport-secondary-action");
            let page_for_push = self.clone();
            push.connect_clicked(move |button| page_for_push.preview_push(button.clone()));
            actions.append(&push);
        }
        let leave = gtk::Button::with_label("Leave team");
        leave.add_css_class("destructive-action");
        let page_for_leave = self.clone();
        leave.connect_clicked(move |button| page_for_leave.confirm_leave(button.clone()));
        actions.append(&leave);
        summary.append(&actions);
        self.content.append(&summary);

        let active_profile = registry.active_profile_id();
        let review = registry
            .servers
            .iter()
            .filter(|server| {
                server.needs_team_enable_review()
                    && !registry.is_enabled(&active_profile, &server.id)
            })
            .cloned()
            .collect::<Vec<_>>();
        if !review.is_empty() {
            self.content.append(
                &gtk::Label::builder()
                    .label("Member review required")
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

    fn connect_team(&self, url: String, code: String, name: String, button: gtk::Button) {
        if url.trim().is_empty() || code.trim().is_empty() {
            self.show_error("enter the team server and invite code");
            return;
        }
        button.set_sensitive(false);
        self.feedback.set_label("Connecting to team…");
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
                        "Still waiting for administrator approval. Checking again automatically…",
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
                *self.sync_notice.borrow_mut() = Some(message.to_string());
                let notification = gtk::gio::Notification::new("Removed from team");
                notification.set_body(Some(message));
                self.app
                    .send_notification(Some("toolport-team-removed"), &notification);
            }
            crate::teams::SyncResult::Ok { applied, .. } => {
                let outcome = applied.map(|(_, outcome)| outcome).unwrap_or_default();
                *self.sync_notice.borrow_mut() = team_review_line(outcome.review, outcome.blocked);
            }
        }
        self.refresh();
    }

    fn preview_push(&self, button: gtk::Button) {
        button.set_sensitive(false);
        self.feedback
            .set_label("Comparing local and shared team servers…");
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(crate::teams::preview_push_current).await;
            button.set_sensitive(true);
            match result {
                Ok(Ok(preview)) => page.show_push_review(preview),
                Ok(Err(error)) => page.show_error(&error),
                Err(_) => page.show_error("the team comparison stopped unexpectedly"),
            }
        });
    }

    fn show_push_review(&self, preview: crate::teams::PushPreview) {
        let Some(parent) = self.app.active_window() else {
            return;
        };
        #[allow(deprecated)]
        let dialog = adw::MessageDialog::new(
            Some(&parent),
            Some("Replace the team’s shared servers?"),
            Some("Only the shared server list changes. Team instructions, security policies, and other settings stay unchanged. Secrets are never sent."),
        );
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("push", "Replace shared servers");
        dialog.set_close_response("cancel");
        dialog.set_default_response(Some("cancel"));
        dialog.set_response_appearance("push", adw::ResponseAppearance::Destructive);
        let changes = gtk::Box::new(gtk::Orientation::Vertical, 8);
        changes.add_css_class("toolport-settings-group");
        for (label, names) in [
            ("Added", &preview.added),
            ("Changed", &preview.changed),
            ("Removed", &preview.removed),
        ] {
            let names = if names.is_empty() {
                "None".to_string()
            } else {
                names.join(", ")
            };
            changes.append(
                &gtk::Label::builder()
                    .label(format!("{label}: {names}"))
                    .halign(gtk::Align::Start)
                    .xalign(0.0)
                    .wrap(true)
                    .css_classes(["toolport-muted"])
                    .build(),
            );
        }
        changes.append(
            &gtk::Label::builder()
                .label("If either side changes after this preview, Toolport stops instead of overwriting it.")
                .halign(gtk::Align::Start)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["toolport-muted"])
                .build(),
        );
        dialog.set_extra_child(Some(&changes));
        let page = self.clone();
        dialog.connect_response(None, move |dialog, response| {
            if response == "push" {
                let base_version = preview.base_version;
                let fingerprint = preview.local_fingerprint.clone();
                page.feedback.set_label("Updating shared team servers…");
                let page = page.clone();
                gtk::glib::spawn_future_local(async move {
                    let result = gtk::gio::spawn_blocking(move || {
                        crate::teams::push_current(base_version, &fingerprint)
                    })
                    .await;
                    match result {
                        Ok(Ok(version)) => {
                            page.feedback.set_label(&format!(
                                "Shared team servers updated to version {version}."
                            ));
                            page.feedback.remove_css_class("error");
                            page.feedback.add_css_class("success");
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
        #[allow(deprecated)]
        let dialog = adw::MessageDialog::new(
            Some(&parent),
            Some("Leave this Toolport team?"),
            Some("Team-provided servers, instructions, and enforced policy are removed. Your own servers and settings stay intact."),
        );
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("leave", "Leave team");
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

    fn show_error(&self, error: &str) {
        self.feedback.set_label(&format!("Teams error: {error}"));
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
            "{review} team {} a local command or a LAN address, so {} off until you review and enable {} below.",
            if review == 1 { "server runs" } else { "servers run" },
            if review == 1 { "it's" } else { "they're" },
            if review == 1 { "it" } else { "them" },
        ));
    }
    if blocked > 0 {
        parts.push(format!(
            "{blocked} {} blocked as unsafe (link-local or cloud-metadata URLs).",
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
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.add_css_class("toolport-card");
    let copy = gtk::Box::new(gtk::Orientation::Vertical, 3);
    copy.set_hexpand(true);
    copy.append(
        &gtk::Label::builder()
            .label(&server.name)
            .halign(gtk::Align::Start)
            .css_classes(["heading"])
            .build(),
    );
    let target = server
        .command
        .as_deref()
        .or(server.url.as_deref())
        .unwrap_or("Unknown target");
    copy.append(
        &gtk::Label::builder()
            .label(target)
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-muted"])
            .build(),
    );
    row.append(&copy);
    let enable = gtk::Button::with_label("Review and enable");
    enable.add_css_class("toolport-secondary-action");
    let server_name = server.name.clone();
    let server_id = server.id.clone();
    enable.connect_clicked(move |button| {
        let Some(parent) = page.app.active_window() else {
            return;
        };
        #[allow(deprecated)]
        let dialog = adw::MessageDialog::new(
            Some(&parent),
            Some(&format!("Enable {server_name}?")),
            Some("This team server runs a local command or connects to a private address. Enable it only after verifying the target above."),
        );
        dialog.add_response("cancel", "Keep disabled");
        dialog.add_response("enable", "Enable");
        dialog.set_close_response("cancel");
        dialog.set_default_response(Some("cancel"));
        dialog.set_response_appearance("enable", adw::ResponseAppearance::Suggested);
        let page = page.clone();
        let server_id = server_id.clone();
        let button = button.clone();
        dialog.connect_response(None, move |dialog, response| {
            if response == "enable" {
                button.set_sensitive(false);
                let page = page.clone();
                let server_id = server_id.clone();
                gtk::glib::spawn_future_local(async move {
                    let result = gtk::gio::spawn_blocking(move || {
                        let registry = crate::registry::load()?;
                        crate::registry_controller::set_server_enabled(
                            &registry.active_profile_id(),
                            &server_id,
                            true,
                            true,
                        )
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

#[cfg(test)]
mod tests {
    use super::team_review_line;

    #[test]
    fn merge_notices_explain_held_and_blocked_servers() {
        assert_eq!(team_review_line(0, 0), None);
        assert_eq!(
            team_review_line(1, 0).unwrap(),
            "1 team server runs a local command or a LAN address, so it's off until you \
             review and enable it below."
        );
        assert_eq!(
            team_review_line(2, 1).unwrap(),
            "2 team servers run a local command or a LAN address, so they're off until you \
             review and enable them below. 1 was blocked as unsafe (link-local or \
             cloud-metadata URLs)."
        );
    }
}
