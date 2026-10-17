//! Skills and rules page: what each installed agent loads besides MCP tools.
//! The scan and every write live in `crate::skills`; this page only draws it.

use super::{branding, i18n as l10n};
use crate::skills::{Agent, Inventory, Issue, RuleFile, Skill, Source, State, Wanted};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;

use adw::prelude::*;

const SOURCE_ORDER: [Source; 7] = [
    Source::Yours,
    Source::Account,
    Source::Package,
    Source::BuiltIn,
    Source::Library,
    Source::Parked,
    Source::Broken,
];
const CLAUDE_CHOICES: &[&str] = &["On", "Name only", "Off"];
const ISSUE_SKILL_LIMIT: usize = 12;

#[derive(Clone)]
pub(super) struct SkillsPage {
    pub(super) root: gtk::Box,
    app: adw::Application,
    stack: gtk::Stack,
    skills_list: gtk::Box,
    rules_list: gtk::Box,
    issues_list: gtk::Box,
    search: gtk::SearchEntry,
    feedback: gtk::Label,
    project_button: gtk::Button,
    global_button: gtk::Button,
    refresh_button: gtk::Button,
    inventory: Rc<RefCell<Option<Inventory>>>,
    project: Rc<RefCell<Option<String>>>,
    generation: Rc<Cell<u64>>,
    busy: Rc<Cell<bool>>,
    /// A refresh asked for while a write runs; it starts when the write ends so it never reads
    /// the file halfway through a change.
    refresh_pending: Rc<Cell<bool>>,
    /// Cards the user opened, kept open across a rescan.
    expanded: Rc<RefCell<HashSet<String>>>,
}

fn muted(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .halign(gtk::Align::Fill)
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .css_classes(["toolport-muted"])
        .build()
}

fn heading(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .halign(gtk::Align::Start)
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .css_classes(["heading"])
        .build()
}

fn badge(text: &str, class: &str) -> gtk::Label {
    let label = gtk::Label::builder()
        .label(text)
        .valign(gtk::Align::Center)
        .css_classes(["toolport-badge"])
        .build();
    if !class.is_empty() {
        label.add_css_class(class);
    }
    label
}

fn small_logo(id: &str, size: i32) -> gtk::Image {
    let image = branding::client_logo(id);
    image.set_pixel_size(size);
    image.set_valign(gtk::Align::Start);
    image
}

/// The first part of a file, as the React view's Preview block shows it.
fn preview_label(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .halign(gtk::Align::Fill)
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .selectable(true)
        .css_classes(["monospace", "toolport-muted", "toolport-value-card"])
        .build()
}

fn clear(list: &gtk::Box) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
}

/// Decimal units, matching the React view's `fmtBytes`.
fn fmt_bytes(bytes: u64) -> String {
    if bytes >= 999_950 {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    } else if bytes >= 1_000 {
        format!("{:.1} KB", bytes as f64 / 1_000.0)
    } else {
        format!("{bytes} B")
    }
}

fn folder_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

fn source_label(source: Source) -> String {
    crate::i18n::lookup(match source {
        Source::Yours => crate::i18n::msg("Yours"),
        Source::Account => crate::i18n::msg("From your claude.ai account"),
        Source::Package => crate::i18n::msg("System package"),
        Source::BuiltIn => crate::i18n::msg("Built into a client"),
        Source::Library => crate::i18n::msg("Client library"),
        Source::Parked => crate::i18n::msg("Parked"),
        Source::Broken => crate::i18n::msg("Broken links"),
    })
}

fn state_label(state: Option<State>) -> String {
    crate::i18n::lookup(match state {
        Some(State::On) => crate::i18n::msg("On"),
        Some(State::NameOnly) => crate::i18n::msg("Name only"),
        Some(State::Off) => crate::i18n::msg("Off"),
        Some(State::Shadowed) => crate::i18n::msg("Another copy wins"),
        Some(State::Broken) => crate::i18n::msg("Broken"),
        None => crate::i18n::msg("Not loaded"),
    })
}

fn skill_agents(inv: &Inventory) -> Vec<&Agent> {
    inv.agents.iter().filter(|a| a.supports_skills).collect()
}

fn skill_summary(skill: &Skill, agents: &[&Agent]) -> String {
    let count = |want: &[State]| {
        agents
            .iter()
            .filter(|a| {
                skill
                    .states
                    .get(&a.id)
                    .is_some_and(|s| want.contains(&s.state))
            })
            .count()
    };
    let loaded = count(&[State::On, State::NameOnly]);
    let mut text = crate::i18n::trf(
        "{count} of {total} agents load it",
        &[
            ("count", &loaded.to_string()),
            ("total", &agents.len().to_string()),
        ],
    );
    let off = count(&[State::Off]);
    if off > 0 {
        text.push_str(&crate::i18n::trf(
            " · off in {count}",
            &[("count", &off.to_string())],
        ));
    }
    let shadowed = count(&[State::Shadowed]);
    if shadowed > 0 {
        text.push_str(&crate::i18n::trf(
            " · another copy wins in {count}",
            &[("count", &shadowed.to_string())],
        ));
    }
    text
}

impl SkillsPage {
    pub(super) fn new(app: &adw::Application) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("toolport-content");
        let header = adw::HeaderBar::new();
        header.add_css_class("toolport-header");
        header.set_show_back_button(true);
        header.set_title_widget(Some(&{
            let widget = gtk::Label::builder()
                .label(crate::i18n::tr("Skills and rules"))
                .css_classes(["title"])
                .build();
            l10n::bind_label(&widget, "Skills and rules");
            widget
        }));
        let refresh_button = gtk::Button::from_icon_name("view-refresh-symbolic");
        refresh_button.set_tooltip_text(Some(&crate::i18n::tr("Scan again")));
        l10n::bind_tooltip(&refresh_button, "Scan again");
        header.pack_end(&refresh_button);
        root.append(&header);

        let top = gtk::Box::new(gtk::Orientation::Vertical, 10);
        top.set_margin_top(16);
        top.set_margin_start(20);
        top.set_margin_end(20);
        top.append(&{
            let widget = muted(&crate::i18n::tr(
                "What each agent loads besides tools, and where it lives",
            ));
            l10n::bind_label(
                &widget,
                "What each agent loads besides tools, and where it lives",
            );
            widget
        });
        let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let search = {
            let widget = gtk::SearchEntry::builder()
                .placeholder_text(crate::i18n::tr("Search skills and rules"))
                .hexpand(true)
                .css_classes(["toolport-search"])
                .build();
            l10n::bind_placeholder(&widget, "Search skills and rules");
            widget
        };
        controls.append(&search);
        let project_button = gtk::Button::with_label(&crate::i18n::tr("Add a project folder…"));
        project_button.set_tooltip_text(Some(&crate::i18n::tr(
            "Also show the rules and skills a project folder adds",
        )));
        l10n::bind_tooltip(
            &project_button,
            "Also show the rules and skills a project folder adds",
        );
        controls.append(&project_button);
        let global_button = l10n::button("Global only");
        global_button.set_visible(false);
        controls.append(&global_button);
        top.append(&controls);
        let feedback = gtk::Label::builder()
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-feedback"])
            .build();
        feedback.set_visible(false);
        top.append(&feedback);
        root.append(&top);

        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(120)
            .vexpand(true)
            .build();
        let tab = || {
            let list = gtk::Box::new(gtk::Orientation::Vertical, 8);
            list.add_css_class("toolport-page");
            list.set_margin_top(12);
            list.set_margin_bottom(20);
            list.set_margin_start(20);
            list.set_margin_end(20);
            let scroller = gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .vexpand(true)
                .child(&list)
                .build();
            (scroller, list)
        };
        let (skills_scroll, skills_list) = tab();
        let (rules_scroll, rules_list) = tab();
        let (issues_scroll, issues_list) = tab();
        stack.add_titled(&skills_scroll, Some("skills"), &crate::i18n::tr("Skills"));
        stack.add_titled(&rules_scroll, Some("rules"), &crate::i18n::tr("Rules"));
        stack.add_titled(
            &issues_scroll,
            Some("issues"),
            &crate::i18n::tr("Needs a look"),
        );
        let switcher = gtk::StackSwitcher::new();
        switcher.set_stack(Some(&stack));
        switcher.set_halign(gtk::Align::Center);
        switcher.set_margin_top(12);
        root.append(&switcher);
        root.append(&stack);

        let page = Self {
            root,
            app: app.clone(),
            stack,
            skills_list,
            rules_list,
            issues_list,
            search,
            feedback,
            project_button,
            global_button,
            refresh_button,
            inventory: Rc::default(),
            project: Rc::default(),
            generation: Rc::default(),
            busy: Rc::default(),
            refresh_pending: Rc::default(),
            expanded: Rc::default(),
        };
        {
            let page_ref = page.clone();
            page.refresh_button
                .connect_clicked(move |_| page_ref.refresh());
        }
        {
            let page_ref = page.clone();
            page.search
                .connect_search_changed(move |_| page_ref.render());
        }
        {
            let page_ref = page.clone();
            page.project_button
                .connect_clicked(move |_| page_ref.choose_project());
        }
        {
            let page_ref = page.clone();
            page.global_button.connect_clicked(move |_| {
                page_ref.project.replace(None);
                page_ref.refresh();
            });
        }
        {
            // Scan whenever the page is shown so it never draws stale state.
            let page_ref = page.clone();
            page.root.connect_map(move |_| page_ref.refresh());
        }
        page
    }

    pub(super) fn refresh(&self) {
        if self.busy.get() {
            self.refresh_pending.set(true);
            return;
        }
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        self.refresh_button.set_sensitive(false);
        let project = self.project.borrow().clone();
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let result =
                gtk::gio::spawn_blocking(move || crate::skills::inventory(project.as_deref()))
                    .await;
            if generation != page.generation.get() {
                return;
            }
            page.refresh_button.set_sensitive(true);
            match result {
                Ok(Ok(inv)) => {
                    // A fresh scan supersedes an old error or note.
                    page.show_feedback("", false);
                    page.inventory.replace(Some(inv));
                    page.render();
                }
                Ok(Err(error)) => page.show_feedback(&error, true),
                Err(_) => {
                    page.show_feedback(&crate::i18n::tr("Couldn't read skills and rules"), true)
                }
            }
        });
    }

    /// Scan again without clearing the message a failed write just showed.
    fn rescan_keeping_feedback(&self) {
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        let project = self.project.borrow().clone();
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let result =
                gtk::gio::spawn_blocking(move || crate::skills::inventory(project.as_deref()))
                    .await;
            if generation != page.generation.get() {
                return;
            }
            if let Ok(Ok(inv)) = result {
                page.inventory.replace(Some(inv));
            }
            page.render();
        });
    }

    fn show_feedback(&self, message: &str, error: bool) {
        self.feedback.set_label(message);
        self.feedback.set_visible(!message.is_empty());
        if error {
            self.feedback.add_css_class("error");
        } else {
            self.feedback.remove_css_class("error");
        }
    }

    fn choose_project(&self) {
        let Some(parent) = self.app.active_window() else {
            return;
        };
        let dialog = gtk::FileDialog::builder()
            .title(crate::i18n::tr("Choose a project folder"))
            .modal(true)
            .build();
        if let Some(home) = dirs::home_dir() {
            dialog.set_initial_folder(Some(&gtk::gio::File::for_path(home)));
        }
        let page = self.clone();
        dialog.select_folder(Some(&parent), gtk::gio::Cancellable::NONE, move |result| {
            let Some(path) = result.ok().and_then(|file| file.path()) else {
                return;
            };
            page.project
                .replace(Some(path.to_string_lossy().into_owned()));
            page.refresh();
        });
    }

    /// Run a write off the main thread, then redraw from the fresh scan it returns.
    fn mutate(
        &self,
        success: String,
        work: impl FnOnce(Option<String>) -> Result<Inventory, String> + Send + 'static,
    ) {
        if self.busy.replace(true) {
            // Snap the control that fired back to what is saved, after this signal returns.
            self.show_feedback(
                &crate::i18n::tr("Wait for the last change to finish."),
                false,
            );
            let page = self.clone();
            gtk::glib::idle_add_local_once(move || page.render());
            return;
        }
        // Supersede any scan in flight; its result would predate this write.
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        let project = self.project.borrow().clone();
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || work(project)).await;
            page.busy.set(false);
            page.refresh_button.set_sensitive(true);
            if page.refresh_pending.replace(false) {
                // A rescan or project change came in during the write: run it now, after the
                // write, and keep this write's message.
                match &result {
                    Ok(Ok(_)) => page.show_feedback(&success, false),
                    Ok(Err(error)) => page.show_feedback(error, true),
                    Err(_) => page.show_feedback(&crate::i18n::tr("Couldn't fix this"), true),
                }
                page.rescan_keeping_feedback();
                return;
            }
            if generation != page.generation.get() {
                return;
            }
            match result {
                Ok(Ok(inv)) => {
                    page.inventory.replace(Some(inv));
                    page.show_feedback(&success, false);
                    page.render();
                }
                Ok(Err(error)) => {
                    page.show_feedback(&error, true);
                    // Part of a write may have landed; show what is on disk now.
                    page.rescan_keeping_feedback();
                }
                Err(_) => {
                    page.show_feedback(&crate::i18n::tr("Couldn't fix this"), true);
                    page.rescan_keeping_feedback();
                }
            }
        });
    }

    fn render(&self) {
        let project = self.project.borrow().clone();
        match &project {
            Some(path) => self.project_button.set_label(&crate::i18n::trf(
                "Global + {folder}",
                &[("folder", &folder_name(path))],
            )),
            None => self
                .project_button
                .set_label(&crate::i18n::tr("Add a project folder…")),
        }
        self.global_button.set_visible(project.is_some());
        let inventory = self.inventory.borrow();
        let Some(inv) = inventory.as_ref() else {
            return;
        };
        let needle = self.search.text().trim().to_lowercase();
        let skills_count = inv
            .skills
            .iter()
            .filter(|s| s.source != Source::Broken)
            .count();
        for (name, title) in [
            (
                "skills",
                crate::i18n::trf("Skills ({count})", &[("count", &skills_count.to_string())]),
            ),
            (
                "rules",
                crate::i18n::trf(
                    "Rules ({count})",
                    &[("count", &inv.rules.len().to_string())],
                ),
            ),
            (
                "issues",
                crate::i18n::trf(
                    "Needs a look ({count})",
                    &[("count", &inv.issues.len().to_string())],
                ),
            ),
        ] {
            if let Some(child) = self.stack.child_by_name(name) {
                self.stack.page(&child).set_title(&title);
            }
        }
        self.render_skills(inv, &needle);
        self.render_rules(inv, &needle);
        self.render_issues(inv);
    }

    fn empty(list: &gtk::Box, text: &str) {
        let label = muted(text);
        label.set_halign(gtk::Align::Center);
        label.set_margin_top(32);
        list.append(&label);
    }

    fn render_skills(&self, inv: &Inventory, needle: &str) {
        clear(&self.skills_list);
        if inv.agents.is_empty() {
            Self::empty(
                &self.skills_list,
                &crate::i18n::tr(
                    "Install an AI coding agent such as Claude Code, Codex or Cursor, then scan again.",
                ),
            );
            return;
        }
        let agents = skill_agents(inv);
        let matches = |s: &&Skill| {
            needle.is_empty()
                || s.name.to_lowercase().contains(needle)
                || s.description.to_lowercase().contains(needle)
                || s.path.to_lowercase().contains(needle)
        };
        let mut any = false;
        for source in SOURCE_ORDER {
            let items: Vec<&Skill> = inv
                .skills
                .iter()
                .filter(|s| s.source == source)
                .filter(matches)
                .collect();
            if items.is_empty() {
                continue;
            }
            any = true;
            let group = heading(&crate::i18n::trf(
                "{label} ({count})",
                &[
                    ("label", &source_label(source)),
                    ("count", &items.len().to_string()),
                ],
            ));
            group.set_margin_top(10);
            self.skills_list.append(&group);
            for skill in items {
                self.skills_list.append(&self.skill_card(skill, &agents));
            }
        }
        if !any {
            Self::empty(
                &self.skills_list,
                &if needle.is_empty() {
                    crate::i18n::tr("No skills found in any agent's folders.")
                } else {
                    crate::i18n::tr("No skills match.")
                },
            );
        }
    }

    /// A collapsed card per skill; the detail is built the first time it opens.
    fn skill_card(&self, skill: &Skill, agents: &[&Agent]) -> gtk::Box {
        let card = gtk::Box::new(gtk::Orientation::Vertical, 6);
        card.add_css_class("toolport-card");
        let title = gtk::Box::new(gtk::Orientation::Vertical, 2);
        title.set_hexpand(true);
        let name_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        name_row.append(&heading(&skill.name));
        if skill.description_chars > crate::skills::DESCRIPTION_CAP {
            let warn = gtk::Image::from_icon_name("dialog-warning-symbolic");
            warn.add_css_class("approval-warning");
            warn.set_tooltip_text(Some(&crate::i18n::tr(
                "Description is cut off in Claude Code",
            )));
            name_row.append(&warn);
        }
        title.append(&name_row);
        let subtitle = muted(if skill.source == Source::Broken {
            &skill.path
        } else {
            &skill.description
        });
        subtitle.set_wrap(false);
        subtitle.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.append(&subtitle);
        if skill.source != Source::Broken {
            title.append(&muted(&skill_summary(skill, agents)));
        }
        let expander = gtk::Expander::builder()
            .label_widget(&title)
            .css_classes(["toolport-details-expander"])
            .build();
        card.append(&expander);

        let key = skill.key.clone();
        let page = self.clone();
        let build = move |expander: &gtk::Expander| {
            if expander.child().is_some() {
                return;
            }
            let inventory = page.inventory.borrow();
            let Some(inv) = inventory.as_ref() else {
                return;
            };
            let Some(skill) = inv.skills.iter().find(|s| s.key == key) else {
                return;
            };
            expander.set_child(Some(&page.skill_detail(inv, skill)));
        };
        if self.expanded.borrow().contains(&skill.key) {
            expander.set_expanded(true);
            build(&expander);
        }
        let expanded = self.expanded.clone();
        let key = skill.key.clone();
        expander.connect_expanded_notify(move |expander| {
            if expander.is_expanded() {
                expanded.borrow_mut().insert(key.clone());
                build(expander);
            } else {
                expanded.borrow_mut().remove(&key);
            }
        });
        card
    }

    fn file_actions(&self, path: &str, folder: &str, open_label: &'static str) -> gtk::Box {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let open = l10n::button(open_label);
        let reveal = l10n::button("Reveal");
        let copy = gtk::Button::from_icon_name("edit-copy-symbolic");
        copy.set_tooltip_text(Some(&crate::i18n::tr("Copy path")));
        l10n::bind_tooltip(&copy, "Copy path");
        for (button, target, reveal_folder) in [
            (&open, path.to_string(), false),
            (&reveal, folder.to_string(), true),
        ] {
            let page = self.clone();
            button.connect_clicked(move |_| {
                let project = page.project.borrow().clone();
                let target = target.clone();
                let page = page.clone();
                gtk::glib::spawn_future_local(async move {
                    let shown = target.clone();
                    let result = gtk::gio::spawn_blocking(move || {
                        crate::skills::open_path(&target, reveal_folder, project.as_deref())
                    })
                    .await;
                    if let Ok(Err(error)) = result {
                        page.show_feedback(
                            &crate::i18n::trf(
                                "Couldn't open {path}: {error}",
                                &[("path", &shown), ("error", &error)],
                            ),
                            true,
                        );
                    }
                });
            });
        }
        {
            let path = path.to_string();
            let page = self.clone();
            copy.connect_clicked(move |button| {
                button.clipboard().set_text(&path);
                page.show_feedback(&crate::i18n::tr("Path copied"), false);
            });
        }
        row.append(&open);
        row.append(&reveal);
        row.append(&copy);
        row
    }

    fn skill_detail(&self, inv: &Inventory, skill: &Skill) -> gtk::Box {
        let detail = gtk::Box::new(gtk::Orientation::Vertical, 8);
        detail.set_margin_top(8);
        if skill.source != Source::Broken && !skill.description.is_empty() {
            let description = muted(&skill.description);
            description.set_selectable(true);
            detail.append(&description);
            detail.append(&muted(&crate::i18n::trf(
                "{count} of {cap} characters (Claude Code's limit)",
                &[
                    ("count", &skill.description_chars.to_string()),
                    ("cap", &crate::skills::DESCRIPTION_CAP.to_string()),
                ],
            )));
        }
        let path = gtk::Label::builder()
            .label(&skill.path)
            .halign(gtk::Align::Start)
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::Char)
            .selectable(true)
            .css_classes(["monospace", "toolport-muted"])
            .build();
        detail.append(&path);
        let mut facts = vec![skill.source_note.clone()];
        if let Some(repo) = &skill.git_repo {
            facts.push(crate::i18n::trf("git repo: {name}", &[("name", repo)]));
        }
        if skill.source != Source::Broken {
            facts.push(if skill.lines == 1 {
                crate::i18n::trf("{size} · 1 line", &[("size", &fmt_bytes(skill.size_bytes))])
            } else {
                crate::i18n::trf(
                    "{size} · {lines} lines",
                    &[
                        ("size", &fmt_bytes(skill.size_bytes)),
                        ("lines", &skill.lines.to_string()),
                    ],
                )
            });
        }
        detail.append(&muted(&facts.join(" · ")));
        let others: Vec<&str> = skill
            .links
            .iter()
            .map(String::as_str)
            .filter(|l| *l != skill.path)
            .collect();
        if !others.is_empty() {
            detail.append(&muted(&crate::i18n::trf(
                "Reached by {paths}",
                &[("paths", &others.join(", "))],
            )));
        }
        detail.append(&self.file_actions(&skill.path, &skill.folder, "Open SKILL.md"));

        let agents = gtk::Box::new(gtk::Orientation::Vertical, 0);
        agents.add_css_class("toolport-project-files");
        for agent in skill_agents(inv) {
            agents.append(&self.agent_row(agent, skill));
        }
        detail.append(&agents);
        if !skill.preview.is_empty() {
            detail.append(&preview_label(&skill.preview));
        }
        detail
    }

    fn agent_row(&self, agent: &Agent, skill: &Skill) -> gtk::Box {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        row.add_css_class("toolport-project-file-row");
        row.append(&small_logo(&agent.id, 20));
        let copy = gtk::Box::new(gtk::Orientation::Vertical, 2);
        copy.set_hexpand(true);
        let name = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        name.append(&heading(&agent.name));
        if !agent.verified {
            let docs = badge(&crate::i18n::tr("from docs"), "disabled");
            docs.set_tooltip_text(Some(&crate::i18n::tr(
                "Folders come from this client's docs, not a check on this machine",
            )));
            name.append(&docs);
        }
        copy.append(&name);
        let state = skill.states.get(&agent.id);
        copy.append(&muted(&state.map_or_else(
            || crate::i18n::tr("Not in any folder it reads"),
            |s| s.via.clone(),
        )));
        row.append(&copy);
        row.append(&self.state_control(agent, skill));
        if state.is_none() {
            row.set_opacity(0.6);
        }
        row
    }

    fn state_control(&self, agent: &Agent, skill: &Skill) -> gtk::Widget {
        let current = skill.states.get(&agent.id).map(|s| s.state);
        let Some(current) = current else {
            let label = muted(&state_label(None));
            label.set_wrap(false);
            label.set_halign(gtk::Align::End);
            return label.upcast();
        };
        match current {
            State::Broken => return badge(&state_label(Some(current)), "error").upcast(),
            State::Shadowed => return badge(&state_label(Some(current)), "review").upcast(),
            _ => {}
        }
        if !agent.can_toggle {
            let label = badge(
                &state_label(Some(current)),
                if current == State::Off {
                    "disabled"
                } else {
                    "success"
                },
            );
            label.set_tooltip_text(Some(&agent.switch_note));
            return label.upcast();
        }
        let set = {
            let page = self.clone();
            let agent_id = agent.id.clone();
            let agent_name = agent.name.clone();
            let key = skill.key.clone();
            let skill_name = skill.name.clone();
            move |wanted: Wanted| {
                let success = match wanted {
                    Wanted::Off => crate::i18n::trf(
                        "Turned off {skill} in {client}",
                        &[("skill", &skill_name), ("client", &agent_name)],
                    ),
                    Wanted::NameOnly => crate::i18n::trf(
                        "{client} now lists {skill} by name only",
                        &[("skill", &skill_name), ("client", &agent_name)],
                    ),
                    Wanted::On => crate::i18n::trf(
                        "Turned on {skill} in {client}",
                        &[("skill", &skill_name), ("client", &agent_name)],
                    ),
                };
                let agent_id = agent_id.clone();
                let key = key.clone();
                page.mutate(success, move |project| {
                    crate::skills::set_skill_state(&agent_id, &key, wanted, project.as_deref())
                });
            }
        };
        if agent.id == "claude-code" {
            let dropdown = gtk::DropDown::from_strings(&[] as &[&str]);
            l10n::bind_dropdown_choices(&dropdown, CLAUDE_CHOICES);
            dropdown.set_valign(gtk::Align::Center);
            dropdown.set_selected(match current {
                State::NameOnly => 1,
                State::Off => 2,
                _ => 0,
            });
            dropdown.set_tooltip_text(Some(&crate::i18n::trf(
                "{skill} in {client}",
                &[("skill", &skill.name), ("client", &agent.name)],
            )));
            dropdown.connect_selected_notify(move |dropdown| {
                if l10n::dropdown_rebinding(dropdown) {
                    return;
                }
                set(match dropdown.selected() {
                    1 => Wanted::NameOnly,
                    2 => Wanted::Off,
                    _ => Wanted::On,
                });
            });
            return dropdown.upcast();
        }
        let switch = gtk::Switch::builder()
            .active(current != State::Off)
            .valign(gtk::Align::Center)
            .tooltip_text(crate::i18n::trf(
                "Load {skill} in {client}",
                &[("skill", &skill.name), ("client", &agent.name)],
            ))
            .build();
        let busy = self.busy.clone();
        switch.connect_state_set(move |_, active| {
            if busy.get() {
                return gtk::glib::Propagation::Stop;
            }
            set(if active { Wanted::On } else { Wanted::Off });
            gtk::glib::Propagation::Proceed
        });
        switch.upcast()
    }

    fn render_rules(&self, inv: &Inventory, needle: &str) {
        clear(&self.rules_list);
        let matches = |r: &&RuleFile| {
            needle.is_empty()
                || r.name.to_lowercase().contains(needle)
                || r.path.to_lowercase().contains(needle)
                || r.preview.to_lowercase().contains(needle)
        };
        let mut any = false;
        for (scope, label) in [
            ("global", crate::i18n::tr("Global")),
            ("project", crate::i18n::tr("Project")),
        ] {
            let items: Vec<&RuleFile> = inv
                .rules
                .iter()
                .filter(|r| r.scope == scope)
                .filter(matches)
                .collect();
            if items.is_empty() {
                continue;
            }
            any = true;
            let group = heading(&crate::i18n::trf(
                "{label} ({count})",
                &[("label", &label), ("count", &items.len().to_string())],
            ));
            group.set_margin_top(10);
            self.rules_list.append(&group);
            for rule in items {
                self.rules_list.append(&self.rule_card(inv, rule));
            }
        }
        if !any {
            Self::empty(
                &self.rules_list,
                &if needle.is_empty() {
                    crate::i18n::tr("No rules files found.")
                } else {
                    crate::i18n::tr("No rules files match.")
                },
            );
        }
    }

    fn rule_card(&self, inv: &Inventory, rule: &RuleFile) -> gtk::Box {
        let card = gtk::Box::new(gtk::Orientation::Vertical, 6);
        card.add_css_class("toolport-card");
        let title = gtk::Box::new(gtk::Orientation::Vertical, 2);
        title.append(&heading(&rule.name));
        let path = muted(&rule.path);
        path.add_css_class("monospace");
        title.append(&path);
        let readers = inv
            .agents
            .iter()
            .filter(|a| rule.agents.contains_key(&a.id))
            .count();
        title.append(&muted(&crate::i18n::trf(
            "{count} of {total} agents read it",
            &[
                ("count", &readers.to_string()),
                ("total", &inv.agents.len().to_string()),
            ],
        )));
        let expander = gtk::Expander::builder()
            .label_widget(&title)
            .css_classes(["toolport-details-expander"])
            .build();
        let open_key = format!("rule:{}", rule.key);
        expander.set_expanded(self.expanded.borrow().contains(&open_key));
        {
            let expanded = self.expanded.clone();
            expander.connect_expanded_notify(move |expander| {
                if expander.is_expanded() {
                    expanded.borrow_mut().insert(open_key.clone());
                } else {
                    expanded.borrow_mut().remove(&open_key);
                }
            });
        }
        let detail = gtk::Box::new(gtk::Orientation::Vertical, 8);
        detail.set_margin_top(8);
        let folder = std::path::Path::new(&rule.path)
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        detail.append(&self.file_actions(&rule.path, &folder, "Open file"));
        let agents = gtk::Box::new(gtk::Orientation::Vertical, 0);
        agents.add_css_class("toolport-project-files");
        for agent in &inv.agents {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            row.add_css_class("toolport-project-file-row");
            row.append(&small_logo(&agent.id, 20));
            let copy = gtk::Box::new(gtk::Orientation::Vertical, 2);
            copy.set_hexpand(true);
            copy.append(&heading(&agent.name));
            match rule.agents.get(&agent.id) {
                Some(chain) => {
                    for step in chain {
                        let text = match step.kind.as_str() {
                            "file" => step.path.clone(),
                            "link" => crate::i18n::trf("links to {path}", &[("path", &step.path)]),
                            "import" => {
                                crate::i18n::trf("@imports {path}", &[("path", &step.path)])
                            }
                            _ => crate::i18n::trf("skips @import {path}", &[("path", &step.path)]),
                        };
                        let line = muted(&text);
                        if step.kind == "skipped-import" {
                            line.add_css_class("warning");
                        }
                        copy.append(&line);
                    }
                    row.append(&copy);
                    row.append(&badge(&crate::i18n::tr("Loaded"), "success"));
                }
                None => {
                    copy.append(&muted(&agent.rules_note.clone().unwrap_or_else(|| {
                        let others: Vec<&str> = agent
                            .rule_locations
                            .iter()
                            .map(String::as_str)
                            .filter(|path| *path != rule.path)
                            .collect();
                        if others.is_empty() {
                            crate::i18n::tr("No global rules file")
                        } else {
                            crate::i18n::trf(
                                "Reads {paths}, which doesn't lead here",
                                &[("paths", &others.join(", "))],
                            )
                        }
                    })));
                    row.append(&copy);
                    let state = muted(&crate::i18n::tr("Not loaded"));
                    state.set_wrap(false);
                    row.append(&state);
                    row.set_opacity(0.6);
                }
            }
            agents.append(&row);
        }
        detail.append(&agents);
        if !rule.preview.is_empty() {
            detail.append(&preview_label(&rule.preview));
        }
        expander.set_child(Some(&detail));
        card.append(&expander);
        card
    }

    fn render_issues(&self, inv: &Inventory) {
        clear(&self.issues_list);
        if inv.issues.is_empty() {
            Self::empty(
                &self.issues_list,
                &crate::i18n::tr("Every skill and rules file loads the way it's set."),
            );
        }
        for issue in &inv.issues {
            self.issues_list.append(&self.issue_card(inv, issue));
        }
        let by_agent = heading(&crate::i18n::tr("By agent"));
        by_agent.set_margin_top(16);
        self.issues_list.append(&by_agent);
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        list.add_css_class("toolport-project-files");
        for agent in &inv.agents {
            list.append(&agent_summary(inv, agent));
        }
        self.issues_list.append(&list);
        self.issues_list.append(&muted(&crate::i18n::tr(
            "\"Own switch\" means the agent has its own per-skill setting that Toolport can write. Toolport never moves or deletes your skill folders to turn a skill off.",
        )));
    }

    fn issue_card(&self, inv: &Inventory, issue: &Issue) -> gtk::Box {
        let card = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        card.add_css_class("toolport-card");
        let icon = gtk::Image::from_icon_name(match issue.level.as_str() {
            "error" => "dialog-error-symbolic",
            "warn" => "dialog-warning-symbolic",
            _ => "dialog-information-symbolic",
        });
        icon.add_css_class(match issue.level.as_str() {
            "error" => "approval-denied",
            "warn" => "approval-warning",
            _ => "toolport-muted",
        });
        icon.set_valign(gtk::Align::Start);
        card.append(&icon);
        let copy = gtk::Box::new(gtk::Orientation::Vertical, 4);
        copy.set_hexpand(true);
        copy.append(&heading(&issue.title));
        copy.append(&muted(&issue.detail));
        let names: Vec<&str> = issue
            .agents
            .iter()
            .filter_map(|id| inv.agents.iter().find(|a| &a.id == id))
            .map(|a| a.name.as_str())
            .collect();
        if !names.is_empty() {
            copy.append(&muted(&crate::i18n::trf(
                "Agents: {names}",
                &[("names", &names.join(", "))],
            )));
        }
        if !issue.skill_names.is_empty() {
            let mut shown = issue
                .skill_names
                .iter()
                .take(ISSUE_SKILL_LIMIT)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            if issue.skill_names.len() > ISSUE_SKILL_LIMIT {
                shown.push_str(&crate::i18n::trf(
                    " +{count} more",
                    &[(
                        "count",
                        &(issue.skill_names.len() - ISSUE_SKILL_LIMIT).to_string(),
                    )],
                ));
            }
            copy.append(&muted(&crate::i18n::trf(
                "Skills: {names}",
                &[("names", &shown)],
            )));
        }
        card.append(&copy);
        if let Some(label) = &issue.fix_label {
            let button = gtk::Button::with_label(label);
            button.add_css_class("suggested-action");
            button.set_valign(gtk::Align::Start);
            let page = self.clone();
            let id = issue.id.clone();
            let title = issue.title.clone();
            let label = label.clone();
            button.connect_clicked(move |_| page.confirm_fix(&id, &title, &label));
            card.append(&button);
        }
        card
    }

    fn confirm_fix(&self, id: &str, title: &str, label: &str) {
        let Some(parent) = self.app.active_window() else {
            return;
        };
        #[allow(deprecated)]
        let dialog = adw::MessageDialog::new(
            Some(&parent),
            Some(label),
            Some(&crate::i18n::tr(
                "Toolport backs up any config file before it changes it, and never deletes a skill folder.",
            )),
        );
        l10n::bind_dialog_body(
            &dialog,
            "Toolport backs up any config file before it changes it, and never deletes a skill folder.",
        );
        dialog.add_response("cancel", &crate::i18n::tr("Cancel"));
        l10n::bind_dialog_response(&dialog, "cancel", "Cancel");
        dialog.add_response("fix", label);
        dialog.set_close_response("cancel");
        dialog.set_default_response(Some("cancel"));
        dialog.set_response_appearance("fix", adw::ResponseAppearance::Suggested);
        let page = self.clone();
        let id = id.to_string();
        let success = crate::i18n::trf("Fixed: {title}", &[("title", title)]);
        dialog.connect_response(None, move |dialog, response| {
            if response == "fix" {
                let id = id.clone();
                page.mutate(success.clone(), move |project| {
                    crate::skills::fix_issue(&id, project.as_deref())
                });
            }
            dialog.close();
        });
        dialog.present();
    }
}

fn agent_summary(inv: &Inventory, agent: &Agent) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    row.add_css_class("toolport-project-file-row");
    row.append(&small_logo(&agent.id, 20));
    let copy = gtk::Box::new(gtk::Orientation::Vertical, 2);
    copy.set_hexpand(true);
    copy.append(&heading(&agent.name));
    let count = |state: State| {
        inv.skills
            .iter()
            .filter(|s| s.states.get(&agent.id).is_some_and(|x| x.state == state))
            .count()
    };
    let mut parts = Vec::new();
    if agent.supports_skills {
        parts.push(crate::i18n::trf(
            "{count} on",
            &[("count", &count(State::On).to_string())],
        ));
        let name_only = count(State::NameOnly);
        if name_only > 0 {
            parts.push(crate::i18n::trf(
                "{count} name only",
                &[("count", &name_only.to_string())],
            ));
        }
        let off = count(State::Off);
        if off > 0 {
            parts.push(crate::i18n::trf(
                "{count} off",
                &[("count", &off.to_string())],
            ));
        }
    } else {
        parts.push(crate::i18n::tr("No file-based skills"));
    }
    let rules = inv
        .rules
        .iter()
        .filter(|r| r.agents.contains_key(&agent.id))
        .count();
    parts.push(match rules {
        0 => crate::i18n::tr("no rules files"),
        1 => crate::i18n::tr("1 rules file"),
        _ => crate::i18n::trf("{count} rules files", &[("count", &rules.to_string())]),
    });
    copy.append(&muted(&parts.join(" · ")));
    row.append(&copy);
    let broken = count(State::Broken);
    if broken > 0 {
        row.append(&badge(
            &crate::i18n::trf("{count} broken", &[("count", &broken.to_string())]),
            "error",
        ));
    } else if agent.supports_skills {
        let label = badge(
            &if agent.can_toggle {
                crate::i18n::tr("Own switch")
            } else {
                crate::i18n::tr("No switch")
            },
            "",
        );
        label.set_tooltip_text(Some(&agent.switch_note));
        row.append(&label);
    }
    row
}
