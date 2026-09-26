use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

use adw::prelude::*;

const SUGGESTION_BATCH: usize = 4;

#[derive(Clone, PartialEq, Eq)]
struct CatalogSnapshot {
    entries: Vec<crate::catalog::CatalogEntry>,
    existing: HashSet<String>,
    stacks: Option<Vec<crate::stacks::Stack>>,
}

#[derive(Clone, Default)]
struct SuggestionState {
    entries: Vec<crate::catalog::CatalogEntry>,
    existing: HashSet<String>,
}

#[derive(Clone)]
pub(super) struct CatalogPage {
    pub(super) root: gtk::Box,
    search: gtk::SearchEntry,
    suggestion_popover: gtk::Popover,
    suggestion_list: gtk::Box,
    stack_heading: gtk::Label,
    stack_list: gtk::FlowBox,
    server_count: gtk::Label,
    list: gtk::Box,
    feedback: gtk::Label,
    request_generation: Rc<Cell<u64>>,
    suggestion_generation: Rc<Cell<u64>>,
    suggestion_timer: Rc<RefCell<Option<gtk::glib::SourceId>>>,
    suggestion_state: Rc<RefCell<SuggestionState>>,
    suggestion_limit: Rc<Cell<usize>>,
    rendered: Rc<RefCell<Option<CatalogSnapshot>>>,
    expanded_stacks: Rc<RefCell<HashSet<String>>>,
    pending_notice: Rc<RefCell<Option<String>>>,
    feedback_timer: Rc<RefCell<Option<gtk::glib::SourceId>>>,
    /// Self-hosted entries are configured in the Add server editor rather than
    /// added in one click, so Catalog needs the page that owns it.
    server_page: super::ServerPage,
}

impl CatalogPage {
    pub(super) fn new(server_page: super::ServerPage) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("toolport-content");
        let header = adw::HeaderBar::new();
        header.add_css_class("toolport-header");
        header.set_show_back_button(true);
        header.set_title_widget(Some(
            &gtk::Label::builder()
                .label("Catalog")
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
        page.append(
            &gtk::Label::builder()
                .label("Add trusted MCP servers")
                .halign(gtk::Align::Start)
                .css_classes(["title-2"])
                .build(),
        );
        page.append(
            &gtk::Label::builder()
                .label("Browse Toolport's curated picks or search the official MCP Registry. Added servers start disabled so you can review and authenticate them first.")
                .halign(gtk::Align::Fill)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["toolport-muted"])
                .build(),
        );
        let search = gtk::SearchEntry::builder()
            .placeholder_text("Search servers, categories, or capabilities")
            .hexpand(true)
            .css_classes(["toolport-search"])
            .build();
        page.append(&search);
        let suggestion_list = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let suggestion_popover = gtk::Popover::new();
        suggestion_popover.add_css_class("toolport-catalog-suggestions");
        suggestion_popover.set_has_arrow(false);
        suggestion_popover.set_position(gtk::PositionType::Bottom);
        suggestion_popover.set_halign(gtk::Align::Start);
        // An autohiding GTK popover takes the keyboard grab when it opens.
        // Search suggestions must stay pointer-interactive without stealing
        // typing or Backspace from their entry.
        suggestion_popover.set_autohide(false);
        suggestion_popover.set_child(Some(&suggestion_list));
        suggestion_popover.set_parent(&search);
        let feedback = gtk::Label::builder()
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-feedback"])
            .build();
        feedback.set_visible(false);
        page.append(&feedback);
        let stack_heading = gtk::Label::builder()
            .label("Starter stacks")
            .halign(gtk::Align::Start)
            .css_classes(["heading"])
            .build();
        page.append(&stack_heading);
        let stack_list = gtk::FlowBox::new();
        stack_list.set_selection_mode(gtk::SelectionMode::None);
        stack_list.set_min_children_per_line(1);
        stack_list.set_max_children_per_line(2);
        stack_list.set_column_spacing(10);
        stack_list.set_row_spacing(10);
        stack_list.set_homogeneous(true);
        page.append(&stack_list);
        let server_header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        server_header.append(
            &gtk::Label::builder()
                .label("Servers")
                .halign(gtk::Align::Start)
                .hexpand(true)
                .css_classes(["heading"])
                .build(),
        );
        let server_count = gtk::Label::builder()
            .halign(gtk::Align::End)
            .css_classes(["caption", "toolport-muted"])
            .build();
        server_header.append(&server_count);
        page.append(&server_header);
        let list = gtk::Box::new(gtk::Orientation::Vertical, 10);
        page.append(&list);
        scroller.set_child(Some(&page));
        root.append(&scroller);

        let catalog = Self {
            root,
            search,
            suggestion_popover,
            suggestion_list,
            stack_heading,
            stack_list,
            server_count,
            list,
            feedback,
            request_generation: Rc::new(Cell::new(0)),
            suggestion_generation: Rc::new(Cell::new(0)),
            suggestion_timer: Rc::new(RefCell::new(None)),
            suggestion_state: Rc::new(RefCell::new(SuggestionState::default())),
            suggestion_limit: Rc::new(Cell::new(SUGGESTION_BATCH)),
            rendered: Rc::new(RefCell::new(None)),
            expanded_stacks: Rc::new(RefCell::new(HashSet::new())),
            pending_notice: Rc::new(RefCell::new(None)),
            feedback_timer: Rc::new(RefCell::new(None)),
            server_page,
        };
        let page_for_activate = catalog.clone();
        catalog.search.connect_activate(move |entry| {
            page_for_activate.cancel_suggestion_timer();
            page_for_activate.load_suggestions(entry.text().as_str());
        });
        let page_for_change = catalog.clone();
        catalog.search.connect_search_changed(move |entry| {
            if entry.text().is_empty() {
                page_for_change.close_suggestions();
            } else {
                page_for_change.schedule_suggestions(entry.text().as_str());
            }
        });
        let outside_click = gtk::GestureClick::new();
        outside_click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let page_for_outside = catalog.clone();
        outside_click.connect_pressed(move |_, _, x, y| {
            if !page_for_outside.suggestion_popover.is_visible() {
                return;
            }
            let inside_search = page_for_outside
                .search
                .compute_bounds(&page_for_outside.root)
                .is_some_and(|bounds| {
                    let x = x as f32;
                    let y = y as f32;
                    x >= bounds.x()
                        && x <= bounds.x() + bounds.width()
                        && y >= bounds.y()
                        && y <= bounds.y() + bounds.height()
                });
            if !inside_search {
                page_for_outside.close_suggestions();
            }
        });
        catalog.root.add_controller(outside_click);
        catalog
    }

    pub(super) fn refresh(&self) {
        self.search("");
    }

    fn cancel_suggestion_timer(&self) {
        if let Some(timer) = self.suggestion_timer.borrow_mut().take() {
            timer.remove();
        }
    }

    fn close_suggestions(&self) {
        self.cancel_suggestion_timer();
        self.suggestion_generation
            .set(self.suggestion_generation.get().wrapping_add(1));
        self.suggestion_popover.popdown();
    }

    fn schedule_suggestions(&self, query: &str) {
        self.cancel_suggestion_timer();
        let Some((query, generation)) = self.begin_suggestions(query) else {
            return;
        };
        let page = self.clone();
        let timer =
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(120), move || {
                page.suggestion_timer.borrow_mut().take();
                page.load_registry_suggestions(query, generation);
            });
        self.suggestion_timer.replace(Some(timer));
    }

    fn load_suggestions(&self, query: &str) {
        let Some((query, generation)) = self.begin_suggestions(query) else {
            return;
        };
        self.load_registry_suggestions(query, generation);
    }

    fn begin_suggestions(&self, query: &str) -> Option<(String, u64)> {
        let query = query.trim().to_string();
        if query.is_empty() {
            self.close_suggestions();
            return None;
        }
        let generation = self.suggestion_generation.get().wrapping_add(1);
        self.suggestion_generation.set(generation);
        let existing = self
            .rendered
            .borrow()
            .as_ref()
            .map(|snapshot| snapshot.existing.clone())
            .unwrap_or_default();
        let entries = crate::catalog::search_curated(&query);
        self.suggestion_state.replace(SuggestionState {
            entries: entries.clone(),
            existing,
        });
        self.suggestion_limit.set(SUGGESTION_BATCH);
        if entries.is_empty() {
            self.show_suggestion_message("Searching the MCP Registry…", false);
        } else {
            self.render_suggestions();
        }
        Some((query, generation))
    }

    fn load_registry_suggestions(&self, query: String, generation: u64) {
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                let entries = crate::catalog::search(&query)?;
                let registry = crate::registry::load()?;
                let existing = registry
                    .servers
                    .into_iter()
                    .map(|server| server.name.to_lowercase())
                    .collect::<HashSet<_>>();
                Ok::<_, String>(SuggestionState { entries, existing })
            })
            .await;
            if generation != page.suggestion_generation.get() {
                return;
            }
            match result {
                Ok(Ok(state)) => {
                    page.suggestion_state.replace(state);
                    page.suggestion_limit.set(SUGGESTION_BATCH);
                    page.render_suggestions();
                }
                Ok(Err(error)) if page.suggestion_state.borrow().entries.is_empty() => {
                    page.show_suggestion_message(&error, true)
                }
                Err(_) if page.suggestion_state.borrow().entries.is_empty() => {
                    page.show_suggestion_message("The catalog search stopped unexpectedly.", true)
                }
                Ok(Err(_)) | Err(_) => {}
            }
        });
    }

    fn show_suggestion_message(&self, message: &str, error: bool) {
        while let Some(child) = self.suggestion_list.first_child() {
            self.suggestion_list.remove(&child);
        }
        let label = gtk::Label::builder()
            .label(message)
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .wrap(true)
            .css_classes(if error {
                vec!["toolport-suggestion-message", "error"]
            } else {
                vec!["toolport-suggestion-message", "toolport-muted"]
            })
            .build();
        self.suggestion_list.append(&label);
        self.open_suggestions();
    }

    fn open_suggestions(&self) {
        self.suggestion_popover
            .set_width_request(self.search.width().clamp(360, 520));
        self.suggestion_popover.popup();
        self.search.grab_focus();
    }

    fn render_suggestions(&self) {
        while let Some(child) = self.suggestion_list.first_child() {
            self.suggestion_list.remove(&child);
        }
        let state = self.suggestion_state.borrow().clone();
        if state.entries.is_empty() {
            self.show_suggestion_message(
                "No trusted matches. Try a vendor, capability, or category.",
                false,
            );
            return;
        }
        let limit = self.suggestion_limit.get().min(state.entries.len());
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        heading.append(
            &gtk::Label::builder()
                .label("Best matches")
                .halign(gtk::Align::Start)
                .hexpand(true)
                .css_classes(["heading"])
                .build(),
        );
        heading.append(
            &gtk::Label::builder()
                .label(format!("Showing {limit} of {}", state.entries.len()))
                .css_classes(["caption", "toolport-muted"])
                .build(),
        );
        heading.add_css_class("toolport-suggestion-heading");
        self.suggestion_list.append(&heading);
        for entry in state.entries.into_iter().take(limit) {
            self.suggestion_list
                .append(&suggestion_row(entry, &state.existing, self.clone()));
        }
        if limit < self.suggestion_state.borrow().entries.len() {
            let more = gtk::Button::with_label("Show 4 more");
            more.add_css_class("toolport-suggestion-more");
            let page = self.clone();
            more.connect_clicked(move |_| {
                page.suggestion_limit
                    .set(page.suggestion_limit.get() + SUGGESTION_BATCH);
                page.render_suggestions();
            });
            self.suggestion_list.append(&more);
        }
        self.open_suggestions();
    }

    fn search(&self, query: &str) {
        let generation = self.request_generation.get().wrapping_add(1);
        self.request_generation.set(generation);
        self.feedback.set_label(if query.trim().is_empty() {
            "Loading curated servers…"
        } else {
            "Searching the MCP Registry…"
        });
        self.feedback.remove_css_class("error");
        self.feedback.remove_css_class("success");
        self.feedback.set_visible(true);
        let query = query.to_string();
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                let entries = if query.trim().is_empty() {
                    crate::catalog::popular()
                } else {
                    crate::catalog::search(&query)?
                };
                let registry = crate::registry::load()?;
                let existing = registry
                    .servers
                    .into_iter()
                    .map(|server| server.name.to_lowercase())
                    .collect::<HashSet<_>>();
                let stacks = query.trim().is_empty().then(crate::stacks::stacks);
                Ok::<_, String>((entries, existing, stacks))
            })
            .await;
            if generation != page.request_generation.get() {
                return;
            }
            match result {
                Ok(Ok((entries, existing, stacks))) => page.render(entries, existing, stacks),
                Ok(Err(error)) => page.show_error(&error),
                Err(_) => page.show_error("the catalog search stopped unexpectedly"),
            }
        });
    }

    fn render(
        &self,
        entries: Vec<crate::catalog::CatalogEntry>,
        existing: HashSet<String>,
        stacks: Option<Vec<crate::stacks::Stack>>,
    ) {
        let snapshot = CatalogSnapshot {
            entries: entries.clone(),
            existing: existing.clone(),
            stacks: stacks.clone(),
        };
        let unchanged = self.rendered.borrow().as_ref() == Some(&snapshot);
        self.rendered.replace(Some(snapshot));
        if let Some(notice) = self.pending_notice.borrow_mut().take() {
            self.show_success(&notice);
        } else {
            self.feedback.set_visible(false);
        }
        self.server_count
            .set_label(&format!("{} available", entries.len()));
        self.stack_heading.set_visible(stacks.is_some());
        self.stack_list.set_visible(stacks.is_some());
        if unchanged {
            return;
        }
        while let Some(child) = self.stack_list.first_child() {
            self.stack_list.remove(&child);
        }
        if let Some(stacks) = stacks {
            for stack in stacks {
                self.stack_list
                    .insert(&stack_card(stack, &existing, self.clone()), -1);
            }
        }
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        if entries.is_empty() {
            self.list.append(&super::state_card(
                "edit-find-symbolic",
                "No matching servers",
                "Try a broader name or capability.",
                false,
            ));
            return;
        }
        for entry in entries {
            self.list
                .append(&catalog_card(entry, &existing, self.clone()));
        }
    }

    fn show_error(&self, error: &str) {
        self.pending_notice.borrow_mut().take();
        // Cancel any success timer first. Otherwise a confirmation shown moments
        // ago fires its four-second hide and takes this error off screen with it.
        if let Some(timer) = self.feedback_timer.borrow_mut().take() {
            timer.remove();
        }
        self.feedback
            .set_label(&format!("Could not load the catalog: {error}"));
        self.feedback.remove_css_class("success");
        self.feedback.add_css_class("error");
        self.feedback.set_visible(true);
    }

    fn show_success(&self, message: &str) {
        if let Some(timer) = self.feedback_timer.borrow_mut().take() {
            timer.remove();
        }
        self.feedback.set_label(message);
        self.feedback.remove_css_class("error");
        self.feedback.add_css_class("success");
        self.feedback.set_visible(true);
        let feedback = self.feedback.clone();
        let timer_slot = self.feedback_timer.clone();
        let timer =
            gtk::glib::timeout_add_local_once(std::time::Duration::from_secs(4), move || {
                feedback.set_visible(false);
                timer_slot.borrow_mut().take();
            });
        self.feedback_timer.replace(Some(timer));
    }
}

fn suggestion_row(
    entry: crate::catalog::CatalogEntry,
    existing: &HashSet<String>,
    page: CatalogPage,
) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    row.add_css_class("toolport-suggestion-row");
    row.append(&super::branding::server_logo(&entry.name, &entry.transport));
    let copy = gtk::Box::new(gtk::Orientation::Vertical, 2);
    copy.set_hexpand(true);
    copy.append(
        &gtk::Label::builder()
            .label(&entry.name)
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .max_width_chars(32)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["heading"])
            .build(),
    );
    let detail = if entry.source == "curated" {
        format!("Toolport verified · {}", entry.description)
    } else if let Some(publisher) = entry.publisher.as_deref() {
        format!("MCP Registry · {publisher} · {}", entry.description)
    } else {
        format!("MCP Registry · {}", entry.description)
    };
    copy.append(
        &gtk::Label::builder()
            .label(detail)
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .max_width_chars(52)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .lines(1)
            .css_classes(["caption", "toolport-muted"])
            .build(),
    );
    row.append(&copy);

    if existing.contains(&entry.name.to_lowercase()) {
        let added = gtk::Label::new(Some("Added"));
        added.set_size_request(72, -1);
        added.set_valign(gtk::Align::Center);
        added.set_xalign(0.5);
        added.add_css_class("toolport-catalog-added");
        row.append(&added);
    } else {
        let add = gtk::Button::with_label("Add");
        add.set_size_request(72, -1);
        add.set_valign(gtk::Align::Center);
        add.add_css_class("suggested-action");
        add.add_css_class("toolport-catalog-action");
        let entry_for_add = entry.clone();
        add.connect_clicked(move |button| {
            let entry = entry_for_add.clone();
            if let Some(hint) = entry.url_hint.as_deref() {
                configure_self_hosted(&entry, hint, &page);
                return;
            }
            button.set_sensitive(false);
            let name = entry.name.clone();
            let page = page.clone();
            gtk::glib::spawn_future_local(async move {
                let result = gtk::gio::spawn_blocking(move || {
                    crate::registry_controller::add_catalog_entry(entry)
                })
                .await;
                match result {
                    Ok(Ok(registry)) => {
                        open_added_setup(&registry, &page);
                        page.pending_notice.replace(Some(format!(
                            "Added {name}. Review credentials and enable it from Servers."
                        )));
                        page.search.set_text("");
                        page.refresh();
                    }
                    Ok(Err(error)) => page.show_suggestion_message(&error, true),
                    Err(_) => page
                        .show_suggestion_message("The add operation stopped unexpectedly.", true),
                }
            });
        });
        row.append(&add);
    }
    row
}

/// Self-hosted catalog entries carry a `url_hint` and no URL, because the
/// endpoint is the user's own instance. Adding one directly would write an http
/// server with neither a command nor a URL, which can never connect, so the Add
/// button opens the prefilled Add server editor instead and the hint becomes
/// the URL placeholder. Catalog refreshes when the editor closes so a saved
/// server flips the row to Added.
fn configure_self_hosted(entry: &crate::catalog::CatalogEntry, hint: &str, page: &CatalogPage) {
    let view = super::state::ServerView {
        id: String::new(),
        name: entry.name.clone(),
        transport: super::state::transport_label(&entry.transport).to_string(),
        transport_id: entry.transport.clone(),
        command: entry.command.clone(),
        args: entry.args.clone(),
        launch: entry.launch.clone(),
        url: None,
        cwd: None,
        secret_keys: entry.env_keys.clone(),
        client_credentials: None,
        enabled: false,
        requires_review: false,
    };
    let editor =
        super::open_server_editor_prefilled(Some(view), None, page.server_page.clone(), Some(hint));
    if let Some(editor) = editor {
        let page = page.clone();
        editor.connect_destroy(move |_| page.refresh());
    }
}

fn stack_card(
    stack: crate::stacks::Stack,
    existing: &HashSet<String>,
    page: CatalogPage,
) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
    card.add_css_class("toolport-card");
    card.append(
        &gtk::Label::builder()
            .label(&stack.name)
            .halign(gtk::Align::Start)
            .css_classes(["heading"])
            .build(),
    );
    card.append(
        &gtk::Label::builder()
            .label(&stack.description)
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .lines(2)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["toolport-muted"])
            .build(),
    );
    let missing = stack
        .servers
        .iter()
        .filter(|entry| !existing.contains(&entry.name.to_lowercase()))
        .count();
    // Setup steps up front: which credential each server needs and where to
    // create it, so "Add stack" is not a leap of faith.
    {
        let steps = gtk::Expander::new(Some("Setup steps"));
        steps.set_expanded(page.expanded_stacks.borrow().contains(&stack.id));
        let stack_id = stack.id.clone();
        let expanded_stacks = page.expanded_stacks.clone();
        steps.connect_expanded_notify(move |steps| {
            if steps.is_expanded() {
                expanded_stacks.borrow_mut().insert(stack_id.clone());
            } else {
                expanded_stacks.borrow_mut().remove(&stack_id);
            }
        });
        let list = gtk::Box::new(gtk::Orientation::Vertical, 4);
        list.set_margin_top(6);
        for entry in &stack.servers {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            let mut line = entry.name.clone();
            let launch_labels = entry
                .launch
                .as_ref()
                .map(|launch| {
                    launch
                        .inputs
                        .iter()
                        .filter(|input| input.required)
                        .map(|input| input.label.as_str())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            match entry.setup_hint.as_deref() {
                Some(hint) => line.push_str(&format!(" · {hint}")),
                None if entry.env_keys.is_empty() && launch_labels.is_empty() => {
                    line.push_str(" · no credential needed")
                }
                None if !entry.env_keys.is_empty() => {
                    line.push_str(&format!(" · needs {}", entry.env_keys.join(", ")))
                }
                None => {}
            }
            if !launch_labels.is_empty() {
                line.push_str(&format!(" · launch setup: {}", launch_labels.join(", ")));
            }
            row.append(
                &gtk::Label::builder()
                    .label(line)
                    .halign(gtk::Align::Fill)
                    .hexpand(true)
                    .xalign(0.0)
                    .wrap(true)
                    .css_classes(["caption", "toolport-muted"])
                    .build(),
            );
            if let Some(credentials_url) = entry.credentials_url.clone() {
                let get = gtk::Button::with_label("Get credential");
                get.add_css_class("flat");
                get.set_tooltip_text(Some(&credentials_url));
                get.connect_clicked(move |_| {
                    let _ = crate::oauth::open_web_url(&credentials_url);
                });
                row.append(&get);
            }
            list.append(&row);
        }
        steps.set_child(Some(&list));
        card.append(&steps);
    }
    let footer = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    footer.append(
        &gtk::Label::builder()
            .label(format!("{} servers · {} new", stack.servers.len(), missing))
            .halign(gtk::Align::Start)
            .hexpand(true)
            .css_classes(["caption", "toolport-muted"])
            .build(),
    );
    let add = gtk::Button::with_label(if missing == 0 { "Added" } else { "Add stack" });
    add.set_sensitive(missing > 0);
    add.add_css_class(if missing == 0 {
        "toolport-secondary-action"
    } else {
        "suggested-action"
    });
    let name = stack.name.clone();
    add.connect_clicked(move |button| {
        button.set_sensitive(false);
        let entries = stack.servers.clone();
        let name = name.clone();
        let button = button.clone();
        let page = page.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                crate::registry_controller::add_catalog_stack(entries)
            })
            .await;
            button.set_sensitive(true);
            match result {
                Ok(Ok((_, added))) => {
                    page.pending_notice.replace(Some(format!(
                        "Added {added} server{} from {name}. Review and enable them in Servers.",
                        if added == 1 { "" } else { "s" }
                    )));
                    page.refresh();
                }
                Ok(Err(error)) => page.show_error(&error),
                Err(_) => page.show_error("the stack setup stopped unexpectedly"),
            }
        });
    });
    footer.append(&add);
    card.append(&footer);
    card
}

fn catalog_card(
    entry: crate::catalog::CatalogEntry,
    existing: &HashSet<String>,
    page: CatalogPage,
) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    card.add_css_class("toolport-card");
    let icon = super::branding::server_logo(&entry.name, &entry.transport);
    card.append(&icon);
    let copy = gtk::Box::new(gtk::Orientation::Vertical, 3);
    copy.set_hexpand(true);
    copy.append(
        &gtk::Label::builder()
            .label(&entry.name)
            .halign(gtk::Align::Start)
            .css_classes(["heading"])
            .build(),
    );
    copy.append(
        &gtk::Label::builder()
            .label(&entry.description)
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .lines(2)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["toolport-muted"])
            .build(),
    );
    // Provenance tier, like the shipping catalog: who stands behind the entry.
    let provenance = match entry.source.as_str() {
        "curated" => "Toolport verified".to_string(),
        "registry" => match entry.publisher.as_deref() {
            Some(publisher) => format!("MCP Registry · {publisher}"),
            None => "MCP Registry".to_string(),
        },
        _ => "Your pick".to_string(),
    };
    let metadata = [
        Some(provenance.as_str()),
        (!entry.category.is_empty()).then_some(entry.category.as_str()),
        Some(entry.transport.as_str()),
        (!entry.env_keys.is_empty()).then_some("credentials"),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ");
    copy.append(
        &gtk::Label::builder()
            .label(metadata)
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["caption", "toolport-muted"])
            .build(),
    );
    card.append(&copy);

    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    actions.set_valign(gtk::Align::Center);
    actions.set_halign(gtk::Align::End);
    if let Some(homepage) = entry.homepage.clone() {
        let docs = gtk::Button::builder()
            .icon_name("help-browser-symbolic")
            .tooltip_text(format!("Open docs: {homepage}"))
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        docs.connect_clicked(move |_| {
            // Catalog homepages come from registry data; only validated web
            // URLs reach the browser.
            let _ = crate::oauth::open_web_url(&homepage);
        });
        actions.append(&docs);
    } else {
        let docs_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        docs_spacer.set_size_request(32, -1);
        actions.append(&docs_spacer);
    }

    let action_slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    action_slot.set_size_request(72, -1);
    if existing.contains(&entry.name.to_lowercase()) {
        let added = gtk::Label::new(Some("Added"));
        added.set_valign(gtk::Align::Center);
        added.set_size_request(72, -1);
        added.set_xalign(0.5);
        added.add_css_class("toolport-catalog-added");
        action_slot.append(&added);
    } else {
        let add = gtk::Button::with_label("Add");
        add.add_css_class("suggested-action");
        add.add_css_class("toolport-catalog-action");
        add.set_valign(gtk::Align::Center);
        add.set_size_request(72, -1);
        let entry_for_add = entry.clone();
        add.connect_clicked(move |button| {
            let entry = entry_for_add.clone();
            if let Some(hint) = entry.url_hint.as_deref() {
                // The panel would otherwise stay open behind the modal editor.
                page.close_suggestions();
                configure_self_hosted(&entry, hint, &page);
                return;
            }
            button.set_sensitive(false);
            let name = entry.name.clone();
            let button = button.clone();
            let page = page.clone();
            gtk::glib::spawn_future_local(async move {
                let result = gtk::gio::spawn_blocking(move || {
                    crate::registry_controller::add_catalog_entry(entry)
                })
                .await;
                button.set_sensitive(true);
                match result {
                    Ok(Ok(registry)) => {
                        open_added_setup(&registry, &page);
                        page.pending_notice.replace(Some(format!(
                            "Added {name}. Review credentials and enable it from Servers."
                        )));
                        page.refresh();
                    }
                    Ok(Err(error)) => page.show_error(&error),
                    Err(_) => page.show_error("the add operation stopped unexpectedly"),
                }
            });
        });
        action_slot.append(&add);
    }
    actions.append(&action_slot);
    card.append(&actions);
    card
}

/// Catalog entries with argument inputs open the saved editor immediately so
/// their vaulted and plain values can be entered before enabling the server.
fn open_added_setup(registry: &crate::registry::Registry, page: &CatalogPage) {
    let Some(last) = registry.servers.last() else {
        return;
    };
    if !last
        .launch
        .as_ref()
        .is_some_and(|launch| !launch.inputs.is_empty())
    {
        return;
    }
    let id = last.id.clone();
    let view = super::state::RegistrySnapshot::from_registry(registry.clone())
        .servers
        .into_iter()
        .find(|server| server.id == id);
    if let Some(view) = view {
        let _ = super::open_server_editor_prefilled(
            Some(view),
            Some(id),
            page.server_page.clone(),
            None,
        );
    }
}
