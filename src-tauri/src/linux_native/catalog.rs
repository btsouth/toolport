use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;

use adw::prelude::*;

#[derive(Clone)]
pub(super) struct CatalogPage {
    pub(super) root: gtk::Box,
    search: gtk::SearchEntry,
    stack_list: gtk::FlowBox,
    list: gtk::Box,
    feedback: gtk::Label,
    loading: Rc<Cell<bool>>,
}

impl CatalogPage {
    pub(super) fn new() -> Self {
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
        page.set_margin_top(28);
        page.set_margin_bottom(28);
        page.set_margin_start(28);
        page.set_margin_end(28);
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
                .halign(gtk::Align::Start)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["toolport-muted"])
                .build(),
        );
        let search = gtk::SearchEntry::builder()
            .placeholder_text("Search servers, categories, or capabilities")
            .hexpand(true)
            .build();
        page.append(&search);
        let feedback = gtk::Label::builder()
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-feedback"])
            .build();
        feedback.set_label("Open Catalog to load curated servers.");
        page.append(&feedback);
        page.append(
            &gtk::Label::builder()
                .label("Starter stacks")
                .halign(gtk::Align::Start)
                .css_classes(["heading"])
                .build(),
        );
        let stack_list = gtk::FlowBox::new();
        stack_list.set_selection_mode(gtk::SelectionMode::None);
        stack_list.set_min_children_per_line(1);
        stack_list.set_max_children_per_line(2);
        stack_list.set_column_spacing(10);
        stack_list.set_row_spacing(10);
        stack_list.set_homogeneous(true);
        page.append(&stack_list);
        page.append(
            &gtk::Label::builder()
                .label("Servers")
                .halign(gtk::Align::Start)
                .css_classes(["heading"])
                .build(),
        );
        let list = gtk::Box::new(gtk::Orientation::Vertical, 10);
        page.append(&list);
        scroller.set_child(Some(&page));
        root.append(&scroller);

        let catalog = Self {
            root,
            search,
            stack_list,
            list,
            feedback,
            loading: Rc::new(Cell::new(false)),
        };
        let page_for_search = catalog.clone();
        catalog
            .search
            .connect_activate(move |entry| page_for_search.search(entry.text().as_str()));
        let page_for_clear = catalog.clone();
        catalog.search.connect_search_changed(move |entry| {
            if entry.text().is_empty() {
                page_for_clear.search("");
            }
        });
        catalog
    }

    pub(super) fn refresh(&self) {
        self.search(self.search.text().as_str());
    }

    fn search(&self, query: &str) {
        if self.loading.replace(true) {
            return;
        }
        self.feedback.set_label(if query.trim().is_empty() {
            "Loading curated servers…"
        } else {
            "Searching the MCP Registry…"
        });
        self.feedback.remove_css_class("error");
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
            page.loading.set(false);
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
        while let Some(child) = self.stack_list.first_child() {
            self.stack_list.remove(&child);
        }
        self.stack_list.set_visible(stacks.is_some());
        if let Some(stacks) = stacks {
            for stack in stacks {
                self.stack_list
                    .insert(&stack_card(stack, &existing, self.clone()), -1);
            }
        }
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        self.feedback
            .set_label(&format!("{} servers available", entries.len()));
        self.feedback.remove_css_class("error");
        self.feedback.add_css_class("success");
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
        self.feedback
            .set_label(&format!("Could not load the catalog: {error}"));
        self.feedback.remove_css_class("success");
        self.feedback.add_css_class("error");
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
            .halign(gtk::Align::Start)
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
        let list = gtk::Box::new(gtk::Orientation::Vertical, 4);
        list.set_margin_top(6);
        for entry in &stack.servers {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            let mut line = entry.name.clone();
            match entry.setup_hint.as_deref() {
                Some(hint) => line.push_str(&format!(" · {hint}")),
                None if entry.env_keys.is_empty() => line.push_str(" · no credential needed"),
                None => line.push_str(&format!(" · needs {}", entry.env_keys.join(", "))),
            }
            row.append(
                &gtk::Label::builder()
                    .label(line)
                    .halign(gtk::Align::Start)
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
                    page.feedback.set_label(&format!(
                        "Added {added} server{} from {name}. Review and enable them in Servers.",
                        if added == 1 { "" } else { "s" }
                    ));
                    page.feedback.remove_css_class("error");
                    page.feedback.add_css_class("success");
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
    let icon = gtk::Image::from_icon_name(if entry.transport == "stdio" {
        "utilities-terminal-symbolic"
    } else {
        "network-server-symbolic"
    });
    icon.add_css_class("toolport-card-icon");
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
            .halign(gtk::Align::Start)
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
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["caption", "toolport-muted"])
            .build(),
    );
    card.append(&copy);

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
        card.append(&docs);
    }

    if existing.contains(&entry.name.to_lowercase()) {
        let added = gtk::Label::new(Some("Added"));
        added.add_css_class("toolport-badge");
        added.add_css_class("success");
        card.append(&added);
    } else {
        let add = gtk::Button::with_label("Add");
        add.add_css_class("suggested-action");
        let entry_for_add = entry.clone();
        add.connect_clicked(move |button| {
            button.set_sensitive(false);
            let entry = entry_for_add.clone();
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
                    Ok(Ok(_)) => {
                        page.feedback.set_label(&format!(
                            "Added {name}. Review credentials and enable it from Servers."
                        ));
                        page.feedback.remove_css_class("error");
                        page.feedback.add_css_class("success");
                        page.refresh();
                    }
                    Ok(Err(error)) => page.show_error(&error),
                    Err(_) => page.show_error("the add operation stopped unexpectedly"),
                }
            });
        });
        card.append(&add);
    }
    card
}
