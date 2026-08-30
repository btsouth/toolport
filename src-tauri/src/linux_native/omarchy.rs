use std::rc::Rc;

use adw::prelude::*;

pub(super) struct ConnectFeedback {
    pub(super) message: String,
    pub(super) error: bool,
    pub(super) changed: bool,
}

pub(super) fn environment_detected() -> bool {
    crate::omarchy::detect().environment_detected()
}

pub(super) fn show_agent_review(parent: &gtk::Window, finished: Rc<dyn Fn(ConnectFeedback)>) {
    let parent = parent.clone();
    gtk::glib::spawn_future_local(async move {
        let preview =
            gtk::gio::spawn_blocking(crate::registry_controller::preview_omarchy_agent_connections)
                .await;
        match preview {
            Ok(Ok(Some(agents))) => present_review(&parent, agents, finished),
            Ok(Ok(None)) => finished(ConnectFeedback {
                message: "Omarchy was not detected on this machine.".into(),
                error: true,
                changed: false,
            }),
            Ok(Err(error)) => finished(ConnectFeedback {
                message: format!("Could not review Omarchy agents: {error}"),
                error: true,
                changed: false,
            }),
            Err(_) => finished(ConnectFeedback {
                message: "The Omarchy agent review stopped unexpectedly.".into(),
                error: true,
                changed: false,
            }),
        }
    });
}

fn present_review(
    parent: &gtk::Window,
    agents: Vec<crate::omarchy::AgentReview>,
    finished: Rc<dyn Fn(ConnectFeedback)>,
) {
    #[allow(deprecated)]
    let dialog = adw::MessageDialog::new(
        Some(parent),
        Some("Connect installed Omarchy agents"),
        Some("Review every detected agent before Toolport changes its MCP configuration. Connected and unsupported agents will not be modified."),
    );
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("connect", "Connect selected");
    dialog.set_close_response("cancel");
    dialog.set_default_response(Some("connect"));
    dialog.set_response_appearance("connect", adw::ResponseAppearance::Suggested);

    let rows = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let mut selections = Vec::new();
    for agent in agents {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        row.add_css_class("toolport-setting-row");
        let available = agent.state == crate::omarchy::AgentConnectionState::Available;
        let selected = gtk::CheckButton::builder()
            .active(available)
            .sensitive(available)
            .build();
        row.append(&selected);

        let copy = gtk::Box::new(gtk::Orientation::Vertical, 2);
        copy.set_hexpand(true);
        let title = if agent.selected {
            format!("{} · selected in Omarchy", agent.name)
        } else {
            agent.name.clone()
        };
        copy.append(
            &gtk::Label::builder()
                .label(title)
                .halign(gtk::Align::Start)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["heading"])
                .build(),
        );
        copy.append(
            &gtk::Label::builder()
                .label(&agent.detail)
                .halign(gtk::Align::Start)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["toolport-muted"])
                .build(),
        );
        row.append(&copy);

        let status = match agent.state {
            crate::omarchy::AgentConnectionState::Connected => "Connected",
            crate::omarchy::AgentConnectionState::Available => "Available",
            crate::omarchy::AgentConnectionState::Unsupported => "Unsupported",
            crate::omarchy::AgentConnectionState::Blocked => "Needs review",
        };
        let badge = gtk::Label::new(Some(status));
        badge.add_css_class("toolport-badge");
        badge.add_css_class(match agent.state {
            crate::omarchy::AgentConnectionState::Connected => "success",
            crate::omarchy::AgentConnectionState::Available => "review",
            crate::omarchy::AgentConnectionState::Unsupported
            | crate::omarchy::AgentConnectionState::Blocked => "disabled",
        });
        row.append(&badge);
        rows.append(&row);
        if available {
            selections.push((selected, agent.selector));
        }
    }

    dialog.set_response_enabled("connect", !selections.is_empty());
    let scroller = gtk::ScrolledWindow::builder()
        .child(&rows)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_height(180)
        .max_content_height(480)
        .build();
    dialog.set_extra_child(Some(&scroller));

    let selections = Rc::new(selections);
    dialog.connect_response(None, move |dialog, response| {
        if response == "connect" {
            let selected = selections
                .iter()
                .filter(|(check, _)| check.is_active())
                .map(|(_, selector)| selector.clone())
                .collect::<Vec<_>>();
            if selected.is_empty() {
                finished(ConnectFeedback {
                    message: "No available Omarchy agents were selected.".into(),
                    error: true,
                    changed: false,
                });
            } else {
                connect_selected(selected, finished.clone());
            }
        }
        dialog.close();
    });
    dialog.present();
}

fn connect_selected(selectors: Vec<String>, finished: Rc<dyn Fn(ConnectFeedback)>) {
    gtk::glib::spawn_future_local(async move {
        let result = gtk::gio::spawn_blocking(move || {
            crate::registry_controller::connect_omarchy_agents(selectors)
        })
        .await;
        match result {
            Ok(Ok(results)) => {
                let connected = results.iter().filter(|result| result.connected).count();
                let failed = results
                    .iter()
                    .filter(|result| !result.connected)
                    .map(|result| {
                        format!(
                            "{}: {}",
                            result.name,
                            result.error.as_deref().unwrap_or("unknown error")
                        )
                    })
                    .collect::<Vec<_>>();
                let mut message = format!(
                    "Connected {connected} Omarchy agent{}. Restart agents that were already open.",
                    if connected == 1 { "" } else { "s" }
                );
                if !failed.is_empty() {
                    message.push_str(&format!(" Could not connect {}", failed.join("; ")));
                }
                finished(ConnectFeedback {
                    message,
                    error: !failed.is_empty(),
                    changed: connected > 0,
                });
            }
            Ok(Err(error)) => finished(ConnectFeedback {
                message: format!("Could not connect Omarchy agents: {error}"),
                error: true,
                changed: false,
            }),
            Err(_) => finished(ConnectFeedback {
                message: "The Omarchy connection task stopped unexpectedly.".into(),
                error: true,
                changed: false,
            }),
        }
    });
}
