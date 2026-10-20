//! Skills and rules page: what each installed agent loads besides MCP tools.
//! The scan and every write live in `crate::skills`; this page only draws it, with the same
//! layout as the React view: a list with one status column per agent, and a detail pane.

use super::{branding, i18n as l10n};
use crate::skills::{Agent, Inventory, Issue, RuleFile, Skill, Source, State, Wanted};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;

use adw::prelude::*;

// BreakpointBin intentionally has no child-based minimum size. Issue cards, unlike a whole
// page, must contribute their content height to the scrolling list at every width.
mod issue_frame {
    use adw::prelude::*;
    use gtk::glib;
    use gtk::subclass::prelude::*;
    use std::cell::OnceCell;

    #[derive(Default)]
    pub struct Frame {
        pub bin: OnceCell<adw::BreakpointBin>,
        pub content: OnceCell<gtk::Widget>,
    }

    #[gtk::glib::object_subclass]
    impl ObjectSubclass for Frame {
        const NAME: &'static str = "ToolportSkillIssueFrame";
        type Type = super::IssueFrame;
        type ParentType = gtk::Widget;
    }
    impl ObjectImpl for Frame {
        fn dispose(&self) {
            if let Some(bin) = self.bin.get() {
                bin.unparent();
            }
        }
    }
    impl WidgetImpl for Frame {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let Some(content) = self.content.get() else {
                return (0, 0, -1, -1);
            };
            let (min, nat, _, _) = content.measure(orientation, for_size);
            if orientation == gtk::Orientation::Horizontal {
                (280, nat.max(280), -1, -1)
            } else {
                (min, nat, -1, -1)
            }
        }
        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            if let Some(bin) = self.bin.get() {
                bin.allocate(width, height, baseline, None);
            }
        }
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            if let Some(bin) = self.bin.get() {
                self.obj().snapshot_child(bin, snapshot);
            }
        }
    }
}

gtk::glib::wrapper! {
    pub struct IssueFrame(ObjectSubclass<issue_frame::Frame>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl IssueFrame {
    pub(super) fn new(bin: &adw::BreakpointBin, content: &impl IsA<gtk::Widget>) -> Self {
        use gtk::subclass::prelude::*;
        let frame: Self = gtk::glib::Object::new();
        bin.set_parent(&frame);
        frame.imp().bin.set(bin.clone()).expect("new issue frame");
        frame
            .imp()
            .content
            .set(content.clone().upcast())
            .expect("new issue content");
        frame
    }
}

const SOURCE_ORDER: [Source; 7] = [
    Source::Yours,
    Source::Account,
    Source::Package,
    Source::BuiltIn,
    Source::Library,
    Source::Parked,
    Source::Broken,
];
const ISSUE_SKILL_LIMIT: usize = 12;
/// The collapsed-group key for dismissed Needs a look items.
const DISMISSED_KEY: &str = "issues:dismissed";
/// Width of one agent's status column, shared by the header logos and the row dots: wide
/// enough for a short name under the logo when the list has room, logos only when it doesn't.
const WIDE_COLUMN: i32 = 56;
const NARROW_COLUMN: i32 = 32;
/// What a row keeps for the name and description before the columns widen.
const NAME_ROOM: f64 = 360.0;
const SIZE_COLUMN: i32 = 64;
const DETAIL_WIDTH: i32 = 380;
/// The list column's start and end margins.
const LIST_MARGINS: i32 = 36;
/// Below this page width the detail pane moves under the list, as the React view does.
const NARROW: &str = "max-width: 980px";
/// The least a skill's name and description keep beside the columns.
const NAME_MIN: i32 = 200;
/// A row's own padding and the gaps around its size label.
const ROW_CHROME: i32 = 16 + 2 * 10 + 6;

/// The page width the Skills and Rules lists need beside the detail pane with `count` agent
/// columns at their narrowest. Below it the detail pane moves under the list; the fixed 980px
/// step alone left a list with many agents wider than the window.
fn side_by_side_width(count: usize) -> i32 {
    let columns = NARROW_COLUMN * i32::try_from(count).unwrap_or(i32::MAX / NARROW_COLUMN);
    (LIST_MARGINS + 16 + NAME_MIN + SIZE_COLUMN + columns + DETAIL_WIDTH + 1).max(981)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tab {
    Skills,
    Rules,
    Issues,
}

impl Tab {
    fn name(self) -> &'static str {
        match self {
            Tab::Skills => "skills",
            Tab::Rules => "rules",
            Tab::Issues => "issues",
        }
    }
}

/// What a row in one of the two lists stands for, by position.
#[derive(Clone)]
enum RowKind {
    Group(String),
    Item(String),
}

/// One list-and-detail tab (Skills or Rules).
/// Agent id, display name and optional per-dot detail for the hovered column.
type HoveredColumn = (String, String, Option<String>);

#[derive(Clone)]
struct Pane {
    banner: gtk::Box,
    head: gtk::Box,
    list: gtk::ListBox,
    scroll: gtk::ScrolledWindow,
    detail: gtk::Box,
    detail_scroll: gtk::ScrolledWindow,
    rows: Rc<RefCell<Vec<RowKind>>>,
    layout: gtk::Box,
    separator: gtk::Separator,
    /// Source strings for the two panes' accessible names, read again on a language change.
    names: (&'static str, &'static str),
    /// The column width the list was last drawn with.
    column: Rc<Cell<i32>>,
    /// Whether the rows were last drawn with each name on a line above its dots.
    stacked: Rc<Cell<bool>>,
    /// Every header button and dot by agent, to highlight a hovered or focused column.
    cells: Rc<RefCell<HashMap<String, Vec<gtk::Widget>>>>,
    /// The header row's note naming the hovered column.
    hint: Rc<RefCell<Option<gtk::Label>>>,
    hovered: Rc<RefCell<Option<HoveredColumn>>>,
    focused: Rc<RefCell<Option<HoveredColumn>>>,
    /// The page's breakpoint bin, whose width is what the list really has.
    bin: Rc<RefCell<Option<adw::BreakpointBin>>>,
}

/// A write to run off the main thread, bound to the project that was open when it was asked
/// for. It returns the fresh scan and the message to show.
type Write = Box<dyn FnOnce() -> Result<(Inventory, String), String> + Send>;

#[derive(Clone)]
pub(super) struct SkillsPage {
    pub(super) root: gtk::Box,
    /// Stacks the Skills and Rules lists below the width their columns need.
    grid_narrow: adw::Breakpoint,
    /// `loading`, `error`, `empty` or `content`.
    screens: gtk::Stack,
    failed_body: gtk::Label,
    app: adw::Application,
    stack: gtk::Stack,
    tabs: Rc<Vec<(Tab, gtk::ToggleButton, gtk::Label)>>,
    skills: Pane,
    rules: Pane,
    issues_list: gtk::Box,
    issues_side: gtk::Box,
    search: gtk::SearchEntry,
    agent_filter: gtk::DropDown,
    filter_ids: Rc<RefCell<Vec<String>>>,
    syncing_filter: Rc<Cell<bool>>,
    feedback: gtk::Label,
    /// Hides a success message after a few seconds; errors stay until the next action.
    feedback_timer: Rc<RefCell<Option<gtk::glib::SourceId>>>,
    project_label: gtk::Label,
    project_path: gtk::Label,
    refresh_button: gtk::Button,
    inventory: Rc<RefCell<Option<Inventory>>>,
    project: Rc<RefCell<Option<String>>>,
    generation: Rc<Cell<u64>>,
    busy: Rc<Cell<bool>>,
    /// Writes asked for while one runs. They follow in order, so a quick second click is kept
    /// and no control has to be greyed out (which would take keyboard focus away).
    queued: Rc<RefCell<VecDeque<Write>>>,
    /// The first error from a run of queued writes, shown once the last one finishes.
    queued_error: Rc<RefCell<Option<String>>>,
    /// A refresh asked for while a write runs; it starts when the write ends so it never reads
    /// the file halfway through a change.
    refresh_pending: Rc<Cell<bool>>,
    tab: Rc<Cell<Tab>>,
    /// The agent whose column was clicked: the list shows only what it loads.
    focus: Rc<RefCell<Option<String>>>,
    connected_only: Rc<Cell<bool>>,
    selected_skill: Rc<RefCell<Option<String>>>,
    selected_rule: Rc<RefCell<Option<String>>>,
    collapsed: Rc<RefCell<HashSet<String>>>,
    /// Set while a list is rebuilt, so selecting its first row doesn't count as a click.
    rebuilding: Rc<Cell<bool>>,
    /// The detail pane's state controls by agent, to give focus back after a write redraws them.
    controls: Rc<RefCell<HashMap<String, gtk::Widget>>>,
    last_control: Rc<RefCell<Option<String>>>,
    /// The last activated control slot (`agent` for its switch, `agent#menu` for its menu).
    focused_choice: Rc<RefCell<Option<String>>>,
    /// The control a click is about to redraw (a group header, a column filter), so the redrawn
    /// copy gets keyboard focus back.
    refocus: Rc<RefCell<Option<String>>>,
    refocus_widget: Rc<RefCell<Option<gtk::Widget>>>,
}

/// The folder a skill's file sits in, as shown. The file may be a link to one with another
/// name, so this drops the last part of the path whatever it is called. A broken link has no
/// file; the link is the folder.
fn own_folder(skill: &Skill) -> &str {
    if skill.source == Source::Broken {
        return &skill.path;
    }
    skill
        .path
        .rfind(['/', '\\'])
        .map_or(skill.path.as_str(), |at| &skill.path[..at])
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

fn one_line(text: &str, classes: &[&str]) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .halign(gtk::Align::Fill)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .single_line_mode(true)
        .css_classes(classes.to_vec())
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

fn section(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .halign(gtk::Align::Start)
        .hexpand(true)
        .xalign(0.0)
        .css_classes(["toolport-section-label"])
        .build()
}

/// A section label with a muted note on the right, like "AGENTS … 7 of 7 load it".
fn section_with(text: &str, aside: &str) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_margin_top(6);
    row.append(&section(text));
    let note = muted(aside);
    note.set_wrap(false);
    note.set_hexpand(false);
    note.add_css_class("caption");
    row.append(&note);
    row
}

fn badge(text: &str, class: &str) -> gtk::Label {
    let label = gtk::Label::builder()
        .label(text)
        .valign(gtk::Align::Center)
        .css_classes(["toolport-badge", "caption"])
        .build();
    if !class.is_empty() {
        label.add_css_class(class);
    }
    label
}

fn icon_badge(icon: &str, text: &str, class: &str) -> gtk::Box {
    let chip = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    chip.set_valign(gtk::Align::Center);
    chip.add_css_class("toolport-badge");
    chip.add_css_class("caption");
    if !class.is_empty() {
        chip.add_css_class(class);
    }
    let image = gtk::Image::from_icon_name(icon);
    image.set_pixel_size(12);
    chip.append(&image);
    chip.append(&gtk::Label::new(Some(text)));
    chip
}

/// The status chip for one agent: the legend's word and color, with the same mark as the React
/// view's `SkillStateChip`. Never pressable. `label` overrides the word (rules say "Loaded").
fn state_chip(state: Option<State>, label: Option<&str>) -> gtk::Box {
    let chip = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    chip.set_valign(gtk::Align::Center);
    chip.set_hexpand(false);
    chip.add_css_class("toolport-badge");
    chip.add_css_class("caption");
    chip.add_css_class(match state {
        Some(State::On) => "success",
        Some(State::NameOnly) => "partial",
        Some(State::Shadowed) => "warning",
        Some(State::Broken) => "error",
        _ => "off",
    });
    match state {
        Some(State::Off) | Some(State::Broken) => {
            let image = gtk::Image::from_icon_name(if state == Some(State::Off) {
                "action-unavailable-symbolic"
            } else {
                "dialog-warning-symbolic"
            });
            image.set_pixel_size(12);
            chip.append(&image);
        }
        Some(state) => {
            let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            dot.set_valign(gtk::Align::Center);
            dot.add_css_class("toolport-chip-dot");
            dot.add_css_class(match state {
                State::NameOnly => "name-only",
                State::Shadowed => "ring",
                _ => "on",
            });
            chip.append(&dot);
        }
        None => {
            let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            dot.set_valign(gtk::Align::Center);
            dot.add_css_class("toolport-chip-dot");
            dot.add_css_class("none");
            chip.append(&dot);
        }
    }
    let text = label.map_or_else(|| state_label(state), str::to_string);
    chip.append(&gtk::Label::new(Some(&text)));
    chip
}

/// The labels under the column logos. Profiles of one client share a logo, so they are named
/// by their profile ("claude2", "becca" for `Codex (codex-becca)`) instead of all "Claude".
/// Matches the React view's `columnNames`.
fn column_names(names: &[&str]) -> Vec<String> {
    let split = |name: &str| -> (String, Option<String>) {
        match name
            .trim()
            .strip_suffix(')')
            .and_then(|rest| rest.rsplit_once(" ("))
        {
            Some((base, profile)) => (base.trim().to_string(), Some(profile.trim().to_string())),
            None => (name.trim().to_string(), None),
        }
    };
    let parts: Vec<_> = names.iter().map(|name| split(name)).collect();
    parts
        .iter()
        .map(|(base, profile)| {
            let shared = parts.iter().filter(|(other, _)| other == base).count() > 1;
            match profile.as_deref().filter(|_| shared) {
                Some(profile) => {
                    let client = base.split_whitespace().next().unwrap_or("").to_lowercase();
                    let short = profile
                        .strip_prefix(&format!("{client}-"))
                        .filter(|rest| !rest.is_empty())
                        .unwrap_or(profile);
                    cap(short)
                }
                None => short_agent_name(base),
            }
        })
        .collect()
}

/// At most 8 characters, the last an ellipsis when cut.
fn cap(text: &str) -> String {
    if text.chars().count() > 8 {
        let mut cut: String = text.chars().take(7).collect();
        cut.push('…');
        cut
    } else {
        text.to_string()
    }
}

/// Up to 8 characters of an agent's name for under its column logo ("Claude", "Copilot"),
/// matching the React view's `shortAgentName`.
fn short_agent_name(name: &str) -> String {
    let mut short = name.trim();
    if let Some(stripped) = short
        .strip_suffix(" CLI")
        .or_else(|| short.strip_suffix(" cli"))
    {
        short = stripped.trim_end();
    }
    if short.chars().count() > 8 {
        short = short.split_whitespace().next().unwrap_or(short);
    }
    cap(short)
}

/// What the grid header says about a hovered dot. `via` is a folder for On and a sentence for
/// the other states ("Off in config.toml"), so only On gets "via". Matches React's `dotDetail`.
fn dot_detail(state: Option<&crate::skills::SkillState>) -> String {
    let Some(at) = state else {
        return state_label(None);
    };
    let label = state_label(Some(at.state));
    match at.state {
        State::On => crate::i18n::trf(
            "{state} via {path}",
            &[("state", &label), ("path", &at.via)],
        ),
        State::NameOnly => format!("{label} · {}", at.via),
        _ => at.via.clone(),
    }
}

fn logo(id: &str, size: i32) -> gtk::Image {
    let image = branding::client_logo(id);
    image.set_pixel_size(size);
    image
}

/// An agent's logo on a small rounded square, as in the detail and By agent lists.
fn logo_tile(id: &str) -> gtk::CenterBox {
    let tile = gtk::CenterBox::new();
    tile.add_css_class("toolport-skill-logo");
    tile.set_size_request(28, 28);
    tile.set_hexpand(false);
    tile.set_valign(gtk::Align::Center);
    tile.set_center_widget(Some(&logo(id, 20)));
    tile
}

/// The control slot of an agent Toolport can't change: a lock that says why. Focusable so
/// keyboard users get the tooltip; it is a fact, not a control.
fn lock(reason: &str) -> gtk::Box {
    let lock = gtk::Box::builder()
        .accessible_role(gtk::AccessibleRole::Img)
        .focusable(true)
        .halign(gtk::Align::End)
        .valign(gtk::Align::Center)
        .hexpand(true)
        .css_classes(["toolport-lock"])
        .build();
    lock.set_size_request(28, 28);
    lock.set_tooltip_text(Some(reason));
    lock.update_property(&[gtk::accessible::Property::Label(reason)]);
    let image = gtk::Image::from_icon_name("changes-prevent-symbolic");
    image.set_pixel_size(12);
    image.set_hexpand(true);
    image.set_halign(gtk::Align::Center);
    lock.append(&image);
    lock
}

/// A rounded square holding an icon, the way the mockup heads details and issues.
fn tile(icon: &str, class: &str, size: i32) -> gtk::Box {
    let tile = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    tile.add_css_class("toolport-skill-tile");
    if !class.is_empty() {
        tile.add_css_class(class);
    }
    tile.set_size_request(size, size);
    tile.set_valign(gtk::Align::Start);
    tile.set_halign(gtk::Align::Start);
    // Explicit: a child's expand flag would otherwise stretch the tile across its row.
    tile.set_hexpand(false);
    tile.set_vexpand(false);
    let image = gtk::Image::from_icon_name(icon);
    image.set_halign(gtk::Align::Center);
    image.set_valign(gtk::Align::Center);
    image.set_hexpand(true);
    image.set_pixel_size(size / 2);
    tile.append(&image);
    tile
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

fn clear_box(list: &gtk::Box) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
}

fn clear_list(list: &gtk::ListBox) {
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

fn fmt_date(secs: u64) -> Option<String> {
    let date = gtk::glib::DateTime::from_unix_local(i64::try_from(secs).ok()?).ok()?;
    date.format("%x").ok().map(|text| text.to_string())
}

fn folder_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

fn source_key(source: Source) -> String {
    format!("{source:?}")
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

fn source_icon(source: Source) -> &'static str {
    match source {
        Source::Yours => "avatar-default-symbolic",
        Source::Account => "weather-overcast-symbolic",
        Source::Package => "package-x-generic-symbolic",
        Source::BuiltIn => "changes-prevent-symbolic",
        Source::Library => "folder-symbolic",
        Source::Parked => "action-unavailable-symbolic",
        Source::Broken => "dialog-warning-symbolic",
    }
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

/// One cell of the agent matrix. Shape and color both carry the state, as in the React view.
/// A list row for a window too narrow for names beside the dots: the name (and size) on one
/// line, the dots under it at the same place as the header's logos.
fn stacked_row(
    name: &impl IsA<gtk::Widget>,
    size: Option<&gtk::Label>,
    dots: &gtk::Box,
) -> gtk::ListBoxRow {
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    line.append(name);
    if let Some(size) = size {
        line.append(size);
    }
    dots.set_halign(gtk::Align::End);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 4);
    content.add_css_class("toolport-skill-row");
    content.add_css_class("toolport-skill-row-stacked");
    content.append(&line);
    content.append(dots);
    gtk::ListBoxRow::builder().child(&content).build()
}

fn state_dot(state: Option<State>, tooltip: &str, width: i32) -> gtk::CenterBox {
    // Named like the React view's role="img" dots; the legend's dots sit beside their own label.
    let cell = gtk::CenterBox::builder()
        .accessible_role(if tooltip.is_empty() {
            gtk::AccessibleRole::Presentation
        } else {
            gtk::AccessibleRole::Img
        })
        .build();
    cell.set_size_request(width, 20);
    cell.add_css_class("toolport-skill-cell");
    cell.set_valign(gtk::Align::Fill);
    cell.set_hexpand(false);
    // No tooltip: the header line already names the hovered column and its state, and a
    // tooltip trails the pointer across the grid as a fading second label.
    if !tooltip.is_empty() {
        cell.update_property(&[gtk::accessible::Property::Label(tooltip)]);
    }
    let shape: gtk::Widget = if state == Some(State::Off) {
        let image = gtk::Image::from_icon_name("action-unavailable-symbolic");
        image.set_pixel_size(14);
        image.add_css_class("toolport-skill-off");
        image.upcast()
    } else {
        let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        dot.add_css_class("toolport-skill-dot");
        dot.add_css_class(match state {
            Some(State::On) => "on",
            Some(State::NameOnly) => "name-only",
            Some(State::Shadowed) => "shadowed",
            Some(State::Broken) => "broken",
            _ => "none",
        });
        if state == Some(State::Broken) {
            let inner = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            inner.add_css_class("toolport-skill-dot-core");
            inner.set_halign(gtk::Align::Center);
            inner.set_valign(gtk::Align::Center);
            dot.set_halign(gtk::Align::Center);
            let holder = gtk::CenterBox::new();
            holder.set_center_widget(Some(&inner));
            dot.append(&holder);
        }
        dot.upcast()
    };
    shape.set_halign(gtk::Align::Center);
    shape.set_valign(gtk::Align::Center);
    cell.set_center_widget(Some(&shape));
    cell
}

fn legend() -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    row.add_css_class("toolport-skill-legend");
    for (state, label) in [
        (Some(State::On), l10n::label("On")),
        (Some(State::NameOnly), l10n::label("Name only")),
        (Some(State::Off), l10n::label("Off")),
        (Some(State::Shadowed), l10n::label("Another copy wins")),
        (Some(State::Broken), l10n::label("Broken")),
        (None, l10n::label("Not loaded")),
    ] {
        let item = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let dot = state_dot(state, "", 28);
        dot.set_valign(gtk::Align::Center);
        item.append(&dot);
        label.add_css_class("caption");
        label.add_css_class("toolport-muted");
        item.append(&label);
        row.append(&item);
    }
    row
}

fn project_state_path() -> Option<std::path::PathBuf> {
    Some(crate::registry::conduit_dir()?.join("gtk-skills-project.json"))
}

/// The project folder picked last time, like the React view's saved choice.
fn load_project() -> Option<String> {
    let text = std::fs::read_to_string(project_state_path()?).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value
        .get("project")
        .and_then(|p| p.as_str())
        .map(str::to_string)
}

fn save_project(project: Option<&str>) {
    let Some(path) = project_state_path() else {
        return;
    };
    match project {
        Some(project) => {
            let _ = std::fs::write(path, serde_json::json!({ "project": project }).to_string());
        }
        None => {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn stack_when_narrow(
    narrow: &adw::Breakpoint,
    layout: &gtk::Box,
    separator: &gtk::Separator,
    side: &gtk::ScrolledWindow,
) {
    narrow.add_setter(
        layout,
        "orientation",
        Some(&gtk::Orientation::Vertical.to_value()),
    );
    narrow.add_setter(
        separator,
        "orientation",
        Some(&gtk::Orientation::Horizontal.to_value()),
    );
    narrow.add_setter(side, "width-request", Some(&(-1).to_value()));
}

/// Keep the model object stable during a selection notification. Replacing it on every
/// render queues another selected notification, which redraws again without yielding to input.
fn sync_agent_filter(filter: &gtk::DropDown, names: &[String], selected: u32) {
    let same_model = filter
        .model()
        .and_downcast::<gtk::StringList>()
        .is_some_and(|model| {
            model.n_items() as usize == names.len()
                && names.iter().enumerate().all(|(index, name)| {
                    model.string(index as u32).as_deref() == Some(name.as_str())
                })
        });
    if !same_model {
        filter.set_model(Some(&gtk::StringList::new(
            &names.iter().map(String::as_str).collect::<Vec<_>>(),
        )));
    }
    if filter.selected() != selected {
        filter.set_selected(selected);
    }
}

impl Pane {
    /// The width the list gets from the page, whatever the list itself asks for. Measuring the
    /// list's own scroller instead let wide columns that overflow the page keep themselves wide.
    fn list_width(&self) -> f64 {
        let bin = self.bin.borrow();
        let Some(bin) = bin.as_ref().filter(|bin| bin.width() > 0) else {
            return self.scroll.hadjustment().page_size();
        };
        // Stacked: the list has the whole width. Side by side: less the detail pane.
        let side = if bin.current_breakpoint().is_some() {
            0
        } else {
            DETAIL_WIDTH + 1
        };
        f64::from(bin.width() - side - LIST_MARGINS)
    }

    /// The column width that fits `count` agents in the list's current width.
    fn fitting_column(&self, count: usize) -> i32 {
        let width = self.list_width();
        if width - f64::from(WIDE_COLUMN) * count as f64 >= NAME_ROOM {
            WIDE_COLUMN
        } else {
            NARROW_COLUMN
        }
    }

    /// Whether a row's name keeps `NAME_MIN` beside `count` columns. Below it each row puts its
    /// name on a line above its dots, so many agents in a narrow window never squeeze names out.
    fn names_fit(&self, count: usize, size_column: bool) -> bool {
        let width = self.list_width();
        let size = if size_column { SIZE_COLUMN } else { 0 };
        let columns = NARROW_COLUMN * i32::try_from(count).unwrap_or(i32::MAX / NARROW_COLUMN);
        // Unallocated: draw one-line rows; the first allocation redraws if they don't fit.
        width <= 0.0 || width >= f64::from(ROW_CHROME + NAME_MIN + size) + f64::from(columns)
    }

    /// Pointer hover takes precedence over keyboard focus; leaving it restores the focused
    /// header's name and column instead of erasing a still-active keyboard hint.
    fn hover_column(&self, agent: &str, name: &str, detail: Option<&str>, on: bool) {
        if on {
            self.hovered.replace(Some((
                agent.into(),
                name.into(),
                detail.map(str::to_string),
            )));
        } else if self
            .hovered
            .borrow()
            .as_ref()
            .is_some_and(|(id, _, _)| id == agent)
        {
            self.hovered.replace(None);
        }
        self.update_column();
    }

    fn focus_column(&self, agent: &str, name: &str, detail: Option<&str>, on: bool) {
        if on {
            self.focused
                .replace(Some((agent.into(), name.into(), detail.map(Into::into))));
        } else if self
            .focused
            .borrow()
            .as_ref()
            .is_some_and(|(id, _, _)| id == agent)
        {
            self.focused.replace(None);
        }
        self.update_column();
    }

    fn update_column(&self) {
        let shown = self
            .hovered
            .borrow()
            .clone()
            .or_else(|| self.focused.borrow().clone());
        for (id, cells) in self.cells.borrow().iter() {
            for cell in cells {
                if shown
                    .as_ref()
                    .is_some_and(|(selected, _, _)| selected == id)
                {
                    cell.add_css_class("toolport-skill-col-hl");
                } else {
                    cell.remove_css_class("toolport-skill-col-hl");
                }
            }
        }
        if let Some(hint) = self.hint.borrow().as_ref() {
            if let Some((_, name, detail)) = shown.as_ref() {
                let name = gtk::glib::markup_escape_text(name);
                hint.set_markup(&match detail {
                    Some(detail) => {
                        format!("<b>{name}</b>: {}", gtk::glib::markup_escape_text(detail))
                    }
                    None => format!("<b>{name}</b>"),
                });
            } else {
                // Opacity alone leaves the previous agent name exposed to accessibility.
                hint.set_text("");
            }
            // Keep the single-line hint allocated: hover must not move the cell below it.
            hint.set_opacity(if shown.is_some() { 1.0 } else { 0.0 });
        }
    }

    /// Note `cell` as part of `agent`'s column and highlight the column while the pointer is on it.
    fn track_cell(&self, agent: &Agent, cell: &impl IsA<gtk::Widget>, detail: Option<String>) {
        self.cells
            .borrow_mut()
            .entry(agent.id.clone())
            .or_default()
            .push(cell.clone().upcast());
        let motion = gtk::EventControllerMotion::new();
        let (pane, id, name) = (self.clone(), agent.id.clone(), agent.name.clone());
        let shown = detail.clone();
        motion.connect_enter(move |_, _, _| pane.hover_column(&id, &name, shown.as_deref(), true));
        let (pane, id, name) = (self.clone(), agent.id.clone(), agent.name.clone());
        motion.connect_leave(move |_| pane.hover_column(&id, &name, None, false));
        cell.add_controller(motion);
    }

    /// In a narrow window, stack the detail pane under the list instead of beside it.
    fn stack_when_narrow(&self, narrow: &adw::Breakpoint) {
        stack_when_narrow(narrow, &self.layout, &self.separator, &self.detail_scroll);
    }

    /// The list column (banner, column header, rows, legend) beside a fixed-width detail pane.
    fn new(list_name: &'static str, detail_name: &'static str) -> (Self, gtk::Box) {
        let layout = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        column.set_hexpand(true);
        column.set_margin_start(20);
        column.set_margin_end(16);
        column.set_margin_top(14);
        column.set_margin_bottom(12);
        let banner = gtk::Box::new(gtk::Orientation::Vertical, 0);
        column.append(&banner);
        // The hovered column's name and state, on a line of its own across the whole list so it
        // stays readable with many agents. The line's height comes from a placeholder that never
        // changes, with the hint laid over it: at fractional scales a label measured a few pixels
        // shorter with text than empty, which moved everything below on every hover.
        let hint = one_line("", &["caption", "toolport-muted"]);
        hint.set_max_width_chars(1);
        hint.set_opacity(0.0);
        let placeholder = gtk::Label::builder()
            .label("<b>Ag</b>: Ag")
            .use_markup(true)
            .single_line_mode(true)
            .halign(gtk::Align::Start)
            .opacity(0.0)
            .can_target(false)
            .accessible_role(gtk::AccessibleRole::Presentation)
            .css_classes(["caption"])
            .build();
        let hint_line = gtk::Overlay::builder()
            .child(&placeholder)
            .css_classes(["toolport-skill-hint"])
            .build();
        hint_line.add_overlay(&hint);
        column.append(&hint_line);
        let head = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        head.add_css_class("toolport-skill-head");
        column.append(&head);
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .css_classes(["toolport-skill-list"])
            .build();
        list.update_property(&[gtk::accessible::Property::Label(&crate::i18n::lookup(
            list_name,
        ))]);
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&list)
            .build();
        column.append(&scroll);
        column.append(&legend());
        layout.append(&column);
        let separator = gtk::Separator::new(gtk::Orientation::Vertical);
        layout.append(&separator);
        let detail = gtk::Box::new(gtk::Orientation::Vertical, 12);
        detail.set_margin_start(20);
        detail.set_margin_end(20);
        detail.set_margin_top(18);
        detail.set_margin_bottom(20);
        detail.update_property(&[gtk::accessible::Property::Label(&crate::i18n::lookup(
            detail_name,
        ))]);
        // Never expands: a scroller takes hexpand from its content, and an expanding detail pane
        // split the spare width with the list by content size, moving the divider on every hover.
        let detail_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .width_request(DETAIL_WIDTH)
            .hexpand(false)
            .vexpand(true)
            .child(&detail)
            .build();
        layout.append(&detail_scroll);
        (
            Self {
                banner,
                head,
                list,
                scroll,
                detail,
                detail_scroll,
                rows: Rc::default(),
                layout: layout.clone(),
                separator,
                names: (list_name, detail_name),
                column: Rc::new(Cell::new(NARROW_COLUMN)),
                stacked: Rc::default(),
                cells: Rc::default(),
                hint: Rc::new(RefCell::new(Some(hint))),
                hovered: Rc::default(),
                focused: Rc::default(),
                bin: Rc::default(),
            },
            layout,
        )
    }
}

impl SkillsPage {
    pub(super) fn new(app: &adw::Application) -> Self {
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let header = super::page_header::PageHeader::new(
            crate::i18n::msg("Skills"),
            crate::i18n::msg("Skills and rules each agent loads, and where they live"),
        );
        content.append(&header.root);

        let top = gtk::Box::new(gtk::Orientation::Vertical, 10);
        top.set_margin_top(14);
        top.set_margin_start(24);
        top.set_margin_end(24);

        // Tabs on the left, project/search/refresh on the right, over one bottom rule.
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);

        let tab_box = gtk::Box::new(gtk::Orientation::Horizontal, 18);
        tab_box.set_hexpand(true);
        tab_box.set_valign(gtk::Align::End);
        let mut tabs = Vec::new();
        let mut group: Option<gtk::ToggleButton> = None;
        for (tab, title) in [
            (Tab::Skills, l10n::label("Skills")),
            (Tab::Rules, l10n::label("Rules")),
            (Tab::Issues, l10n::label("Needs a look")),
        ] {
            let content = gtk::Box::new(gtk::Orientation::Horizontal, 7);
            content.append(&title);
            let count = gtk::Label::builder()
                .css_classes(["toolport-skill-count", "caption"])
                .build();
            content.append(&count);
            let button = gtk::ToggleButton::builder()
                .child(&content)
                .css_classes(["flat", "toolport-skill-tab"])
                .build();
            if let Some(first) = &group {
                button.set_group(Some(first));
            } else {
                group = Some(button.clone());
            }
            tab_box.append(&button);
            tabs.push((tab, button, count));
        }

        // Room for "Global only" before a long project name is shortened in the middle.
        let project_label = gtk::Label::builder()
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .width_chars(11)
            .max_width_chars(28)
            .build();
        let project_content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        project_content.append(&gtk::Image::from_icon_name("folder-open-symbolic"));
        project_content.append(&project_label);
        let popover_box = gtk::Box::new(gtk::Orientation::Vertical, 2);
        popover_box.set_margin_top(4);
        popover_box.set_margin_bottom(4);
        let global_item = l10n::button("Global only");
        global_item.add_css_class("flat");
        let add_item = l10n::button("Add a project folder…");
        add_item.add_css_class("flat");
        for item in [&global_item, &add_item] {
            if let Some(label) = item.child().and_downcast::<gtk::Label>() {
                label.set_xalign(0.0);
            }
            popover_box.append(item);
        }
        let project_path = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .max_width_chars(36)
            .css_classes(["monospace", "caption", "toolport-muted"])
            .margin_start(10)
            .margin_end(10)
            .margin_top(6)
            .build();
        popover_box.append(&project_path);
        let popover = gtk::Popover::builder().child(&popover_box).build();
        let project_menu = gtk::MenuButton::builder()
            .child(&project_content)
            .popover(&popover)
            .always_show_arrow(true)
            .valign(gtk::Align::Center)
            .build();
        project_menu.set_tooltip_text(Some(&crate::i18n::tr(
            "Also show the rules and skills a project folder adds",
        )));
        l10n::bind_tooltip(
            &project_menu,
            "Also show the rules and skills a project folder adds",
        );
        bar.append(&project_menu);
        let search = gtk::SearchEntry::builder()
            .placeholder_text(crate::i18n::tr("Search skills"))
            .width_request(220)
            .valign(gtk::Align::Center)
            .css_classes(["toolport-search"])
            .build();
        let agent_filter = gtk::DropDown::from_strings(&[]);
        agent_filter.set_valign(gtk::Align::Center);
        agent_filter.set_tooltip_text(Some(&crate::i18n::tr("Filter by agent")));
        agent_filter.update_property(&[gtk::accessible::Property::Label(&crate::i18n::tr(
            "Filter by agent",
        ))]);
        bar.append(&agent_filter);
        bar.append(&search);
        let refresh_button = gtk::Button::from_icon_name("view-refresh-symbolic");
        refresh_button.set_valign(gtk::Align::Center);
        refresh_button.set_tooltip_text(Some(&crate::i18n::tr("Scan again")));
        l10n::bind_tooltip(&refresh_button, "Scan again");
        bar.append(&refresh_button);
        let toolbar = super::page_header::HeaderWrap::new(&tab_box, &bar);
        toolbar.add_css_class("toolport-skill-tabbar");
        toolbar.set_height_request(46);
        top.append(&toolbar);

        let feedback = gtk::Label::builder()
            .halign(gtk::Align::Fill)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-feedback", "idle"])
            .accessible_role(gtk::AccessibleRole::Status)
            .build();
        feedback.set_visible(false);
        feedback.connect_notify_local(Some("label"), |label, _| {
            label.set_visible(!label.label().is_empty());
        });
        top.append(&feedback);
        content.append(&top);

        // Size to the visible tab only, so a wide tab can't push the window past its tile.
        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(120)
            .hhomogeneous(false)
            .vexpand(true)
            .build();
        use crate::i18n::msg;
        let (skills, skills_layout) = Pane::new(msg("Skills"), msg("Skill details"));
        let (rules, rules_layout) = Pane::new(msg("Rules"), msg("Rules file details"));
        stack.add_named(&skills_layout, Some("skills"));
        stack.add_named(&rules_layout, Some("rules"));

        let issues_layout = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let issues_list = gtk::Box::new(gtk::Orientation::Vertical, 10);
        issues_list.set_margin_start(20);
        issues_list.set_margin_end(16);
        issues_list.set_margin_top(14);
        issues_list.set_margin_bottom(20);
        let issues_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .hexpand(true)
            .vexpand(true)
            .child(&issues_list)
            .build();
        issues_layout.append(&issues_scroll);
        let issues_separator = gtk::Separator::new(gtk::Orientation::Vertical);
        issues_layout.append(&issues_separator);
        let issues_side = gtk::Box::new(gtk::Orientation::Vertical, 10);
        issues_side.set_margin_start(20);
        issues_side.set_margin_end(20);
        issues_side.set_margin_top(18);
        issues_side.set_margin_bottom(20);
        let issues_side_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .width_request(DETAIL_WIDTH)
            .hexpand(false)
            .vexpand(true)
            .child(&issues_side)
            .build();
        issues_layout.append(&issues_side_scroll);
        let issues_page_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .child(&issues_layout)
            .build();
        stack.add_named(&issues_page_scroll, Some("issues"));
        content.append(&stack);

        // A tiled or half-width window gets the stacked layout instead of a clipped one.
        let narrow = adw::Breakpoint::new(
            adw::BreakpointCondition::parse(NARROW)
                .expect("the Skills page breakpoint is a static valid condition"),
        );
        skills.stack_when_narrow(&narrow);
        rules.stack_when_narrow(&narrow);
        // The lists stack sooner when they have many agents; the Needs a look tab keeps 980px.
        let grid_narrow = adw::Breakpoint::new(
            adw::BreakpointCondition::parse(&format!("max-width: {}px", side_by_side_width(0) - 1))
                .expect("a max-width condition is valid"),
        );
        skills.stack_when_narrow(&grid_narrow);
        rules.stack_when_narrow(&grid_narrow);
        stack_when_narrow(
            &narrow,
            &issues_layout,
            &issues_separator,
            &issues_side_scroll,
        );
        // At stacked widths the cards and By agent belong to one scrolling page.
        // Never on the inner scrollers lets the real card measurements reach the page.
        for scroll in [&issues_scroll, &issues_side_scroll] {
            narrow.add_setter(
                scroll,
                "vscrollbar-policy",
                Some(&gtk::PolicyType::Never.to_value()),
            );
        }
        narrow.add_setter(
            &issues_page_scroll,
            "vscrollbar-policy",
            Some(&gtk::PolicyType::Automatic.to_value()),
        );
        narrow.add_setter(&search, "width-request", Some(&150.to_value()));
        let adaptive = adw::BreakpointBin::builder()
            .child(&content)
            .width_request(360)
            .height_request(300)
            .hexpand(true)
            .vexpand(true)
            .build();
        // The bin applies the last matching breakpoint, so the wider grid step goes first and
        // the 980px step, which also stacks the lists, wins below it.
        adaptive.add_breakpoint(grid_narrow.clone());
        adaptive.add_breakpoint(narrow);
        for pane in [&skills, &rules] {
            pane.bin.replace(Some(adaptive.clone()));
        }
        // The first scan, a scan that failed with nothing to show, and no agents at all each get
        // a whole-page screen, as in the React view.
        let screens = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .vexpand(true)
            .build();
        let loading = gtk::Box::new(gtk::Orientation::Vertical, 0);
        loading.set_valign(gtk::Align::Center);
        let spinner = gtk::Spinner::builder().spinning(true).build();
        spinner.set_size_request(32, 32);
        spinner.update_property(&[gtk::accessible::Property::Label(&crate::i18n::tr(
            "Loading…",
        ))]);
        loading.append(&spinner);
        screens.add_named(&loading, Some("loading"));
        let failed = super::state_card(
            "dialog-warning-symbolic",
            "Couldn't read skills and rules",
            "",
            true,
        );
        failed.set_valign(gtk::Align::Center);
        let failed_body = failed
            .last_child()
            .and_downcast::<gtk::Label>()
            .expect("state card body label");
        let try_again = l10n::button("Try again");
        try_again.set_halign(gtk::Align::Center);
        failed.append(&try_again);
        screens.add_named(&failed, Some("error"));
        let empty = super::state_card(
            "system-search-symbolic",
            "No agents found",
            "Install an AI coding agent such as Claude Code, Codex or Cursor, then scan again.",
            false,
        );
        empty.set_valign(gtk::Align::Center);
        let scan_again = l10n::button("Scan again");
        scan_again.set_halign(gtk::Align::Center);
        empty.append(&scan_again);
        screens.add_named(&empty, Some("empty"));
        screens.add_named(&adaptive, Some("content"));
        screens.set_visible_child_name("loading");
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("toolport-content");
        root.append(&screens);

        let page = Self {
            grid_narrow,
            root,
            screens,
            failed_body,
            app: app.clone(),
            stack,
            tabs: Rc::new(tabs),
            skills,
            rules,
            issues_list,
            issues_side,
            search,
            agent_filter,
            filter_ids: Rc::default(),
            syncing_filter: Rc::default(),
            feedback,
            feedback_timer: Rc::default(),
            project_label,
            project_path,
            refresh_button,
            inventory: Rc::default(),
            project: Rc::new(RefCell::new(load_project())),
            generation: Rc::default(),
            busy: Rc::default(),
            refocus: Rc::default(),
            refocus_widget: Rc::default(),
            queued: Rc::default(),
            queued_error: Rc::default(),
            refresh_pending: Rc::default(),
            tab: Rc::new(Cell::new(Tab::Skills)),
            focus: Rc::default(),
            connected_only: Rc::new(Cell::new(true)),
            selected_skill: Rc::default(),
            selected_rule: Rc::default(),
            // Open on your own skills; the rest are a click away.
            collapsed: Rc::new(RefCell::new(
                [
                    Source::Account,
                    Source::Package,
                    Source::BuiltIn,
                    Source::Library,
                    Source::Parked,
                ]
                .into_iter()
                .map(source_key)
                .chain([DISMISSED_KEY.to_string()])
                .collect(),
            )),
            rebuilding: Rc::default(),
            controls: Rc::default(),
            last_control: Rc::default(),
            focused_choice: Rc::default(),
        };
        for (tab, button, _) in page.tabs.iter() {
            let page_ref = page.clone();
            let tab = *tab;
            button.connect_toggled(move |button| {
                if button.is_active() && page_ref.tab.get() != tab {
                    page_ref.select_tab(tab);
                }
            });
        }
        if let Some((_, first, _)) = page.tabs.first() {
            first.set_active(true);
        }
        {
            let page_ref = page.clone();
            page.agent_filter.connect_selected_notify(move |filter| {
                if page_ref.syncing_filter.get() {
                    return;
                }
                let id = page_ref
                    .filter_ids
                    .borrow()
                    .get(filter.selected() as usize)
                    .cloned()
                    .filter(|id| !id.is_empty());
                match id.as_deref() {
                    Some("@connected") => {
                        page_ref.connected_only.set(true);
                        page_ref.focus.replace(None);
                    }
                    None => {
                        page_ref.connected_only.set(false);
                        page_ref.focus.replace(None);
                    }
                    Some(agent) => {
                        let connected = page_ref.inventory.borrow().as_ref().is_some_and(|inv| {
                            inv.agents.iter().any(|a| a.id == agent && a.connected)
                        });
                        if !connected {
                            page_ref.connected_only.set(false);
                        }
                        page_ref.focus.replace(id);
                    }
                }
                page_ref.render();
            });
        }
        for (pane, size_column) in [(&page.skills, true), (&page.rules, false)] {
            let page_ref = page.clone();
            let pane_ref = pane.clone();
            pane.scroll.hadjustment().connect_changed(move |_| {
                let count = pane_ref.cells.borrow().len();
                let column = pane_ref.fitting_column(count);
                let stacked = !pane_ref.names_fit(count, size_column);
                if count > 0
                    && (column != pane_ref.column.get() || stacked != pane_ref.stacked.get())
                {
                    // Set before scheduling, so allocation changes queue only one redraw.
                    pane_ref.column.set(column);
                    pane_ref.stacked.set(stacked);
                    let page = page_ref.clone();
                    gtk::glib::idle_add_local_once(move || page.render());
                }
            });
        }
        page.connect_list(&page.skills, true);
        page.connect_list(&page.rules, false);
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
            let popover = popover.clone();
            add_item.connect_clicked(move |_| {
                popover.popdown();
                page_ref.choose_project();
            });
        }
        {
            let page_ref = page.clone();
            global_item.connect_clicked(move |_| {
                popover.popdown();
                page_ref.set_project(None);
            });
        }
        {
            // Scan whenever the page is shown so it never draws stale state.
            let page_ref = page.clone();
            page.root.connect_map(move |_| page_ref.refresh());
        }
        {
            let page_ref = page.clone();
            l10n::on_language_change(&page.root, move || page_ref.retranslate());
        }
        for button in [&try_again, &scan_again] {
            let page_ref = page.clone();
            button.connect_clicked(move |_| page_ref.refresh());
        }
        page.render_project();
        page
    }

    /// Text set from code rather than bound to one source string, after a language change.
    /// Issue titles, notes and states come from the scan, so a visible page scans again.
    fn retranslate(&self) {
        let filter_label = crate::i18n::tr("Filter by agent");
        self.agent_filter.set_tooltip_text(Some(&filter_label));
        self.agent_filter
            .update_property(&[gtk::accessible::Property::Label(&filter_label)]);
        for pane in [&self.skills, &self.rules] {
            pane.list
                .update_property(&[gtk::accessible::Property::Label(&crate::i18n::lookup(
                    pane.names.0,
                ))]);
            pane.detail
                .update_property(&[gtk::accessible::Property::Label(&crate::i18n::lookup(
                    pane.names.1,
                ))]);
        }
        self.set_search_placeholder();
        self.render_project();
        if self.root.is_mapped() {
            self.refresh();
        }
    }

    /// Ctrl+F: the search box, on the Skills tab when Needs a look (which has none) is open.
    pub(super) fn focus_search(&self) {
        if self.tab.get() == Tab::Issues {
            self.select_tab(Tab::Skills);
        }
        self.search.grab_focus();
    }

    fn set_search_placeholder(&self) {
        self.search
            .set_placeholder_text(Some(&if self.tab.get() == Tab::Rules {
                crate::i18n::tr("Search rules")
            } else {
                crate::i18n::tr("Search skills")
            }));
    }

    fn connect_list(&self, pane: &Pane, skills: bool) {
        {
            let page = self.clone();
            let rows = pane.rows.clone();
            pane.list.connect_row_activated(move |_, row| {
                let kind = usize::try_from(row.index())
                    .ok()
                    .and_then(|i| rows.borrow().get(i).cloned());
                if let Some(RowKind::Group(key)) = kind {
                    {
                        let mut collapsed = page.collapsed.borrow_mut();
                        if !collapsed.remove(&key) {
                            collapsed.insert(key.clone());
                        }
                    }
                    page.render_keeping_focus(format!("group:{key}"));
                }
            });
        }
        let page = self.clone();
        let rows = pane.rows.clone();
        let detail_scroll = pane.detail_scroll.clone();
        pane.list.connect_row_selected(move |_, row| {
            if page.rebuilding.get() {
                return;
            }
            let kind = row
                .and_then(|row| usize::try_from(row.index()).ok())
                .and_then(|i| rows.borrow().get(i).cloned());
            if let Some(RowKind::Item(key)) = kind {
                // A click in the list keeps focus there, whatever a write left to restore.
                page.last_control.replace(None);
                page.focused_choice.replace(None);
                let selected = if skills {
                    &page.selected_skill
                } else {
                    &page.selected_rule
                };
                // Another item opens at its top; a redraw of the same one keeps its place.
                if selected.replace(Some(key.clone())).as_ref() != Some(&key) {
                    detail_scroll.vadjustment().set_value(0.0);
                }
                page.render_details();
            }
        });
    }

    fn select_tab(&self, tab: Tab) {
        // An agent filter from one tab can name an agent the other tab has no column for.
        self.focus.replace(None);
        self.last_control.replace(None);
        self.focused_choice.replace(None);
        self.tab.set(tab);
        self.stack.set_visible_child_name(tab.name());
        for (each, button, _) in self.tabs.iter() {
            if *each == tab && !button.is_active() {
                button.set_active(true);
            }
        }
        self.search.set_visible(tab != Tab::Issues);
        self.set_search_placeholder();
        self.render();
    }

    fn set_project(&self, project: Option<String>) {
        if *self.project.borrow() == project {
            return;
        }
        save_project(project.as_deref());
        self.project.replace(project);
        self.render_project();
        self.refresh();
    }

    fn render_project(&self) {
        let project = self.project.borrow().clone();
        self.project_label.set_label(&match &project {
            Some(path) => crate::i18n::trf("Global + {folder}", &[("folder", &folder_name(path))]),
            None => crate::i18n::tr("Global only"),
        });
        self.project_path
            .set_label(project.as_deref().unwrap_or_default());
        self.project_path.set_visible(project.is_some());
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
            let asked = project.clone();
            let result =
                gtk::gio::spawn_blocking(move || crate::skills::inventory(project.as_deref()))
                    .await;
            if generation != page.generation.get() {
                return;
            }
            page.refresh_button.set_sensitive(true);
            let error = match result {
                Ok(Ok(inv)) => {
                    // A fresh scan supersedes an old error or note.
                    page.show_feedback("", false);
                    page.accept(inv, asked.is_some());
                    return;
                }
                Ok(Err(error)) => error,
                Err(_) => crate::i18n::tr("Couldn't read skills and rules"),
            };
            if page.inventory.borrow().is_some() {
                // Keep what is on screen and say the rescan failed.
                page.show_feedback(&error, true);
            } else {
                page.failed_body.set_text(&error);
                page.screens.set_visible_child_name("error");
            }
        });
    }

    /// Take a scan, following the backend when it dropped a project folder that is gone.
    fn accept(&self, inv: Inventory, asked_project: bool) {
        if asked_project && inv.project.is_none() {
            save_project(None);
            self.project.replace(None);
            self.render_project();
            self.show_feedback(
                &crate::i18n::tr("That project folder is gone. Showing global only."),
                false,
            );
        }
        self.inventory.replace(Some(inv));
        self.render();
    }

    /// Scan again without clearing the message a failed write just showed.
    fn rescan_keeping_feedback(&self) {
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        let project = self.project.borrow().clone();
        let asked = project.is_some();
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let result =
                gtk::gio::spawn_blocking(move || crate::skills::inventory(project.as_deref()))
                    .await;
            if generation != page.generation.get() {
                return;
            }
            match result {
                // Also notices a project folder that went away during the write.
                Ok(Ok(inv)) => page.accept(inv, asked),
                _ => page.render(),
            }
        });
    }

    fn show_feedback(&self, message: &str, error: bool) {
        if let Some(timer) = self.feedback_timer.borrow_mut().take() {
            timer.remove();
        }
        if !error && !message.is_empty() && super::show_toast(&self.feedback, message) {
            // The toast carries it; clear any earlier error so no empty red bar is left.
            self.feedback.set_label("");
            self.feedback.remove_css_class("error");
            self.feedback.add_css_class("idle");
            return;
        }
        self.feedback.set_label(message);
        if message.is_empty() {
            self.feedback.add_css_class("idle");
        } else {
            self.feedback.remove_css_class("idle");
        }
        if error {
            self.feedback.add_css_class("error");
        } else {
            self.feedback.remove_css_class("error");
        }
        if !error && !message.is_empty() {
            let feedback = self.feedback.clone();
            let feedback_timer = self.feedback_timer.clone();
            let timer = gtk::glib::timeout_add_seconds_local_once(5, move || {
                feedback_timer.borrow_mut().take();
                feedback.set_label("");
                feedback.add_css_class("idle");
            });
            *self.feedback_timer.borrow_mut() = Some(timer);
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
            page.set_project(Some(path.to_string_lossy().into_owned()));
        });
    }

    /// Run a write off the main thread, then redraw from the fresh scan it returns.
    fn mutate(
        &self,
        success: String,
        work: impl FnOnce(Option<String>) -> Result<Inventory, String> + Send + 'static,
    ) {
        self.mutate_reporting(move |project| work(project).map(|inv| (inv, success)));
    }

    /// `mutate` for a write whose message depends on what it did.
    fn mutate_reporting(
        &self,
        work: impl FnOnce(Option<String>) -> Result<(Inventory, String), String> + Send + 'static,
    ) {
        // The project now, not when the write gets its turn: a project picked meanwhile must
        // not receive a change clicked in the one before.
        let project = self.project.borrow().clone();
        let work: Write = Box::new(move || work(project));
        if self.busy.replace(true) {
            self.queued.borrow_mut().push_back(work);
            return;
        }
        self.run_write(work);
    }

    fn run_write(&self, work: Write) {
        // Supersede any scan in flight; its result would predate this write.
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(work)
                .await
                .unwrap_or_else(|_| Err(crate::i18n::tr("Couldn't fix this")));
            if let Err(error) = &result {
                page.queued_error.borrow_mut().get_or_insert(error.clone());
            }
            let next = page.queued.borrow_mut().pop_front();
            if let Some(next) = next {
                // Redraw only after the last one, so the controls keep what was clicked.
                page.run_write(next);
                return;
            }
            page.busy.set(false);
            page.refresh_button.set_sensitive(true);
            let error = page.queued_error.borrow_mut().take();
            match (&result, &error) {
                (_, Some(error)) => page.show_feedback(error, true),
                (Ok((_, success)), None) => page.show_feedback(success, false),
                (Err(_), None) => {}
            }
            if page.refresh_pending.replace(false) {
                // A rescan or project change came in during the write: run it now, after the
                // write, and keep this write's message.
                page.rescan_keeping_feedback();
                return;
            }
            if generation != page.generation.get() {
                return;
            }
            match result {
                Ok((inv, _)) if error.is_none() => {
                    page.inventory.replace(Some(inv));
                    page.render();
                }
                // Part of a write may have landed; show what is on disk now.
                _ => page.rescan_keeping_feedback(),
            }
        });
    }

    fn render(&self) {
        let inventory = self.inventory.borrow();
        let Some(inv) = inventory.as_ref() else {
            return;
        };
        if inv.agents.is_empty() {
            self.screens.set_visible_child_name("empty");
            return;
        }
        self.screens.set_visible_child_name("content");
        let reset_focus = self.focus.borrow().as_deref().is_some_and(|id| {
            inv.agents.iter().find(|a| a.id == id).is_none_or(|agent| {
                self.connected_only.get()
                    && inv.agents.iter().any(|a| a.connected)
                    && !agent.connected
            })
        });
        if reset_focus {
            self.focus.replace(None);
        }
        let counts = [
            self.visible_skills(inv, "")
                .iter()
                .filter(|s| s.source != Source::Broken)
                .count(),
            self.visible_rules(inv, "").len(),
            inv.issues.iter().filter(|i| !i.dismissed).count(),
        ];
        for ((tab, _, label), count) in self.tabs.iter().zip(counts) {
            label.set_label(&count.to_string());
            if *tab == Tab::Issues && count > 0 {
                label.add_css_class("warning");
            } else {
                label.remove_css_class("warning");
            }
        }
        self.syncing_filter.set(true);
        let mut ids = vec![String::new()];
        let mut names = vec![crate::i18n::tr("All detected agents")];
        if inv.agents.iter().any(|a| a.connected) {
            ids.insert(0, "@connected".into());
            names.insert(0, crate::i18n::tr("Connected to Toolport"));
        }
        for agent in &inv.agents {
            if self.tab.get() == Tab::Rules || agent.supports_skills {
                ids.push(agent.id.clone());
                names.push(agent.name.clone());
            }
        }
        let aggregate = if self.connected_only.get() && inv.agents.iter().any(|a| a.connected) {
            "@connected"
        } else {
            ""
        };
        let selected = ids
            .iter()
            .position(|id| id == self.focus.borrow().as_deref().unwrap_or(aggregate))
            .unwrap_or_else(|| ids.iter().position(String::is_empty).unwrap_or(0))
            as u32;
        sync_agent_filter(&self.agent_filter, &names, selected);
        self.filter_ids.replace(ids);
        self.agent_filter.set_visible(self.tab.get() != Tab::Issues);
        self.syncing_filter.set(false);
        let needle = self.search.text().trim().to_lowercase();
        match self.tab.get() {
            Tab::Skills => self.render_skills(inv, &needle),
            Tab::Rules => self.render_rules(inv, &needle),
            Tab::Issues => self.render_issues(inv),
        }
        if self.refocus.borrow_mut().take().is_some() {
            if let Some(widget) = self.refocus_widget.borrow_mut().take() {
                // After layout, so the new widget can take focus.
                gtk::glib::idle_add_local_once(move || {
                    widget.grab_focus();
                });
            }
        }
    }

    /// Redraw after a click on the control `key`, then give focus to its redrawn copy.
    fn render_keeping_focus(&self, key: String) {
        self.refocus.replace(Some(key));
        self.refocus_widget.replace(None);
        self.render();
    }

    /// Note `widget` as the redrawn copy of the control `key`, if focus should go back to it.
    fn offer_focus(&self, key: &str, widget: &impl IsA<gtk::Widget>) {
        if self.refocus.borrow().as_deref() == Some(key) {
            self.refocus_widget.replace(Some(widget.clone().upcast()));
        }
    }

    /// Redraw only the detail pane, keeping the list and its scroll position.
    fn render_details(&self) {
        let inventory = self.inventory.borrow();
        let Some(inv) = inventory.as_ref() else {
            return;
        };
        let needle = self.search.text().trim().to_lowercase();
        match self.tab.get() {
            Tab::Skills => {
                let visible = self.visible_skills(inv, &needle);
                self.fill_skill_detail(inv, self.current_skill(&visible));
            }
            Tab::Rules => {
                let visible = self.visible_rules(inv, &needle);
                self.fill_rule_detail(inv, self.current_rule(&visible));
            }
            Tab::Issues => {}
        }
    }

    fn scoped_agents<'a>(&self, inv: &'a Inventory) -> Vec<&'a Agent> {
        let connected = self.connected_only.get() && inv.agents.iter().any(|a| a.connected);
        inv.agents
            .iter()
            .filter(|agent| !connected || agent.connected)
            .collect()
    }

    fn filtering_agents(&self, inv: &Inventory) -> bool {
        self.focus.borrow().is_some()
            || (self.connected_only.get() && inv.agents.iter().any(|a| a.connected))
    }

    fn scoped_skill_agents<'a>(&self, inv: &'a Inventory) -> Vec<&'a Agent> {
        self.scoped_agents(inv)
            .into_iter()
            .filter(|a| a.supports_skills)
            .collect()
    }

    fn visible_skills<'a>(&self, inv: &'a Inventory, needle: &str) -> Vec<&'a Skill> {
        let agents = self.scoped_skill_agents(inv);
        let focus = self.focus.borrow();
        inv.skills
            .iter()
            .filter(|s| {
                !self.filtering_agents(inv)
                    || focus.as_ref().map_or_else(
                        || agents.iter().any(|a| s.states.contains_key(&a.id)),
                        |id| s.states.contains_key(id),
                    )
            })
            .filter(|s| {
                needle.is_empty()
                    || s.name.to_lowercase().contains(needle)
                    || s.description.to_lowercase().contains(needle)
                    || s.path.to_lowercase().contains(needle)
            })
            .collect()
    }

    fn visible_rules<'a>(&self, inv: &'a Inventory, needle: &str) -> Vec<&'a RuleFile> {
        let agents = self.scoped_agents(inv);
        let focus = self.focus.borrow();
        inv.rules
            .iter()
            .filter(|r| {
                !self.filtering_agents(inv)
                    || focus.as_ref().map_or_else(
                        || agents.iter().any(|a| r.agents.contains_key(&a.id)),
                        |id| r.agents.contains_key(id),
                    )
            })
            .filter(|r| {
                needle.is_empty()
                    || r.name.to_lowercase().contains(needle)
                    || r.path.to_lowercase().contains(needle)
                    || r.preview.to_lowercase().contains(needle)
            })
            .collect()
    }

    fn current_skill<'a>(&self, visible: &[&'a Skill]) -> Option<&'a Skill> {
        let selected = self.selected_skill.borrow();
        selected
            .as_ref()
            .and_then(|key| visible.iter().find(|s| &s.key == key).copied())
            .or_else(|| visible.first().copied())
    }

    fn current_rule<'a>(&self, visible: &[&'a RuleFile]) -> Option<&'a RuleFile> {
        let selected = self.selected_rule.borrow();
        selected
            .as_ref()
            .and_then(|key| visible.iter().find(|r| &r.key == key).copied())
            .or_else(|| visible.first().copied())
    }

    fn render_banner(&self, pane: &Pane, inv: &Inventory) {
        clear_box(&pane.banner);
        let active: Vec<&Issue> = inv.issues.iter().filter(|i| !i.dismissed).collect();
        let Some(first) = active.first() else {
            return;
        };
        let card = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        card.add_css_class("toolport-skill-banner");
        card.append(&tile("dialog-warning-symbolic", "warning", 32));
        let copy = gtk::Box::new(gtk::Orientation::Vertical, 2);
        copy.set_hexpand(true);
        copy.set_valign(gtk::Align::Center);
        copy.append(&one_line(
            &if active.len() == 1 {
                crate::i18n::tr("1 thing needs a look")
            } else {
                crate::i18n::trf(
                    "{count} things need a look",
                    &[("count", &active.len().to_string())],
                )
            },
            &["heading"],
        ));
        copy.append(&one_line(&first.title, &["toolport-muted"]));
        card.append(&copy);
        let review = l10n::button("Review");
        review.set_valign(gtk::Align::Center);
        let page = self.clone();
        review.connect_clicked(move |_| page.select_tab(Tab::Issues));
        card.append(&review);
        pane.banner.append(&card);
    }

    /// The "SKILL … logos" row. Clicking a logo shows only what that agent loads.
    /// `all` is how many agents the Rules tab lists, every one the Skills tab can: both tabs
    /// stack at the same width, so switching between them never moves the detail pane.
    fn render_head(
        &self,
        pane: &Pane,
        agents: &[&Agent],
        all: usize,
        label: &str,
        size_column: bool,
    ) {
        clear_box(&pane.head);
        pane.cells.borrow_mut().clear();
        pane.hovered.replace(None);
        pane.focused.replace(None);
        let needed = side_by_side_width(all.max(agents.len()));
        if let Ok(condition) =
            adw::BreakpointCondition::parse(&format!("max-width: {}px", needed - 1))
        {
            if self.grid_narrow.condition().map(|c| c.to_string()) != Some(condition.to_string()) {
                self.grid_narrow.set_condition(Some(&condition));
            }
        }
        pane.column.set(pane.fitting_column(agents.len()));
        pane.stacked.set(!pane.names_fit(agents.len(), size_column));
        if let Some(hint) = pane.hint.borrow().as_ref() {
            hint.set_text("");
            hint.set_opacity(0.0);
        }
        let title_box = gtk::Box::new(gtk::Orientation::Vertical, 4);
        title_box.set_hexpand(true);
        let title = section(label);
        title.set_valign(gtk::Align::End);
        title_box.append(&title);
        pane.head.append(&title_box);
        if size_column {
            let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            spacer.set_size_request(SIZE_COLUMN, 1);
            pane.head.append(&spacer);
        }
        let focus = self.focus.borrow().clone();
        let names = column_names(&agents.iter().map(|a| a.name.as_str()).collect::<Vec<_>>());
        for (agent, short) in agents.iter().zip(names) {
            let overlay = gtk::Overlay::new();
            overlay.set_size_request(pane.column.get(), 22);
            let image = logo(&agent.id, 20);
            image.set_halign(gtk::Align::Center);
            image.set_valign(gtk::Align::Center);
            overlay.set_child(Some(&image));
            if !agent.verified {
                let pip = gtk::Box::new(gtk::Orientation::Horizontal, 0);
                pip.add_css_class("toolport-skill-pip");
                pip.set_halign(gtk::Align::End);
                pip.set_valign(gtk::Align::Start);
                overlay.add_overlay(&pip);
            }
            let column = gtk::Box::new(gtk::Orientation::Vertical, 4);
            column.append(&overlay);
            // The name line is always there, empty when only logos fit, so switching between
            // the two never changes the header's height.
            let wide = pane.column.get() == WIDE_COLUMN;
            column.append(
                &gtk::Label::builder()
                    .label(if wide { short.as_str() } else { "" })
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    // Fills the button the column width sets instead of asking for its own
                    // width. No hexpand: it would spread to the button and pull the logo
                    // off its dots.
                    .max_width_chars(1)
                    .css_classes(["toolport-skill-col-name"])
                    .build(),
            );
            // No tooltip: the line above the header names the hovered column, and a tooltip lags
            // behind the pointer as a fading second name.
            let button = gtk::Button::builder()
                .child(&column)
                .width_request(pane.column.get())
                .css_classes(["flat", "toolport-skill-col"])
                .build();
            // On Skills the filter keeps what the agent finds, switched off or not, so it can be
            // switched back on; on Rules it keeps what the agent reads.
            let values = [("client", agent.name.as_str())];
            button.update_property(&[gtk::accessible::Property::Label(&if size_column {
                crate::i18n::trf("Show only skills {client} finds", &values)
            } else {
                crate::i18n::trf("Show only what {client} loads", &values)
            })]);
            let note = (!agent.verified)
                .then(|| crate::i18n::tr("folders from its docs, not checked on this machine"));
            if let Some(note) = &note {
                button.update_property(&[gtk::accessible::Property::Description(note)]);
            }
            let pressed = focus.as_deref() == Some(agent.id.as_str());
            button.update_state(&[gtk::accessible::State::Pressed(tristate(pressed))]);
            match &focus {
                Some(id) if *id == agent.id => button.add_css_class("selected"),
                Some(_) => {}
                None => {}
            }
            let page = self.clone();
            let id = agent.id.clone();
            button.connect_clicked(move |_| {
                let next = if page.focus.borrow().as_deref() == Some(id.as_str()) {
                    None
                } else {
                    Some(id.clone())
                };
                page.focus.replace(next);
                page.render_keeping_focus(format!("column:{id}"));
            });
            pane.track_cell(agent, &button, note.clone());
            let watch = gtk::EventControllerFocus::new();
            let (p, id, name) = (pane.clone(), agent.id.clone(), agent.name.clone());
            watch.connect_enter(move |_| p.focus_column(&id, &name, note.as_deref(), true));
            let (p, id, name) = (pane.clone(), agent.id.clone(), agent.name.clone());
            watch.connect_leave(move |_| p.focus_column(&id, &name, None, false));
            button.add_controller(watch);
            self.offer_focus(&format!("column:{}", agent.id), &button);
            pane.head.append(&button);
        }
    }

    fn group_row(
        &self,
        key: String,
        icon: &str,
        label: &str,
        count: usize,
        note: &str,
    ) -> gtk::ListBoxRow {
        let open = !self.collapsed.borrow().contains(&key);
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        content.add_css_class("toolport-skill-group");
        let chevron = gtk::Image::from_icon_name(if open {
            "pan-down-symbolic"
        } else {
            "pan-end-symbolic"
        });
        chevron.add_css_class("toolport-muted");
        content.append(&chevron);
        let image = gtk::Image::from_icon_name(icon);
        image.add_css_class("toolport-muted");
        content.append(&image);
        let title = gtk::Label::new(Some(label));
        title.add_css_class("heading");
        content.append(&title);
        content.append(&{
            let pill = gtk::Label::new(Some(&count.to_string()));
            pill.add_css_class("toolport-skill-count");
            pill.add_css_class("caption");
            pill
        });
        if !note.is_empty() {
            content.append(&one_line(note, &["toolport-muted", "caption"]));
        }
        let row = gtk::ListBoxRow::builder()
            .child(&content)
            .selectable(false)
            .css_classes(["toolport-skill-group-row"])
            .build();
        row.update_state(&[gtk::accessible::State::Expanded(Some(open))]);
        self.offer_focus(&format!("group:{key}"), &row);
        row
    }

    fn render_skills(&self, inv: &Inventory, needle: &str) {
        let pane = &self.skills;
        self.render_banner(pane, inv);
        let agents = self.scoped_skill_agents(inv);
        let all = self.scoped_agents(inv).len();
        self.render_head(pane, &agents, all, &crate::i18n::tr("Skill"), true);
        let visible = self.visible_skills(inv, needle);
        // With nothing picked, the first row shown: groups in page order, collapsed ones skipped.
        let open = |source: Source| {
            !self.collapsed.borrow().contains(&source_key(source)) || !needle.is_empty()
        };
        let shown_first = SOURCE_ORDER
            .iter()
            .filter(|source| open(**source))
            .find_map(|source| visible.iter().find(|s| s.source == *source).copied());
        let current_skill = self
            .selected_skill
            .borrow()
            .as_ref()
            .and_then(|key| visible.iter().find(|s| &s.key == key).copied())
            .or(shown_first);
        let current = current_skill.map(|s| s.key.clone());

        let scroll = pane.scroll.vadjustment().value();
        self.rebuilding.set(true);
        clear_list(&pane.list);
        let mut rows = Vec::new();
        let mut selected_row = None;
        for source in SOURCE_ORDER {
            let items: Vec<&&Skill> = visible.iter().filter(|s| s.source == source).collect();
            if items.is_empty() {
                continue;
            }
            let key = source_key(source);
            // A search opens every group so no match hides in a collapsed one.
            let open = open(source);
            pane.list.append(&self.group_row(
                key.clone(),
                source_icon(source),
                &source_label(source),
                items.len(),
                "",
            ));
            rows.push(RowKind::Group(key));
            if !open {
                continue;
            }
            for skill in items {
                let row = self.skill_row(skill, &agents);
                if current.as_deref() == Some(skill.key.as_str()) {
                    selected_row = Some(row.clone());
                }
                pane.list.append(&row);
                rows.push(RowKind::Item(skill.key.clone()));
            }
        }
        if rows.is_empty() {
            let empty = muted(&if needle.is_empty() && self.focus.borrow().is_none() {
                crate::i18n::tr("No skills found in any agent's folders.")
            } else {
                crate::i18n::tr("No skills match.")
            });
            empty.set_halign(gtk::Align::Center);
            empty.set_margin_top(32);
            pane.list.append(
                &gtk::ListBoxRow::builder()
                    .child(&empty)
                    .selectable(false)
                    .activatable(false)
                    .build(),
            );
            rows.push(RowKind::Group(String::new()));
        }
        pane.rows.replace(rows);
        pane.list.select_row(selected_row.as_ref());
        self.rebuilding.set(false);
        restore_scroll(&pane.scroll, scroll);
        self.fill_skill_detail(inv, current_skill);
    }

    fn skill_row(&self, skill: &Skill, agents: &[&Agent]) -> gtk::ListBoxRow {
        let pane = &self.skills;
        let name = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let title = one_line(&skill.name, &["toolport-skill-name"]);
        title.set_hexpand(false);
        name.append(&title);
        if skill.description_chars > crate::skills::DESCRIPTION_CAP {
            let warn = gtk::Image::from_icon_name("dialog-warning-symbolic");
            warn.add_css_class("approval-warning");
            warn.set_tooltip_text(Some(&crate::i18n::tr(
                "Description is cut off in Claude Code",
            )));
            name.append(&warn);
        }
        let size = gtk::Label::builder()
            .label(if skill.source == Source::Broken {
                String::new()
            } else {
                fmt_bytes(skill.size_bytes)
            })
            .xalign(1.0)
            .css_classes(["toolport-muted", "caption", "numeric"])
            .build();
        let dots = self.skill_dots(skill, agents);
        if pane.stacked.get() {
            // The name and size on one line and the dots under it. The description is left to
            // the detail pane: beside the dots it would be cut to a few letters.
            name.set_hexpand(true);
            // Ends where the last dot does.
            size.set_margin_end((pane.column.get() - 12) / 2);
            return stacked_row(&name, Some(&size), &dots);
        }
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        content.add_css_class("toolport-skill-row");
        let copy = gtk::Box::new(gtk::Orientation::Vertical, 2);
        copy.set_hexpand(true);
        copy.add_css_class("toolport-skill-row-copy");
        copy.append(&name);
        copy.append(&one_line(
            if skill.source == Source::Broken {
                &skill.path
            } else {
                &skill.description
            },
            &["toolport-muted", "caption"],
        ));
        content.append(&copy);
        size.set_width_request(SIZE_COLUMN);
        size.set_margin_end(6);
        content.append(&size);
        content.append(&dots);
        gtk::ListBoxRow::builder().child(&content).build()
    }

    fn skill_dots(&self, skill: &Skill, agents: &[&Agent]) -> gtk::Box {
        let pane = &self.skills;
        let dots = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        for agent in agents {
            let saved = skill.states.get(&agent.id);
            let state = saved.map(|s| s.state);
            let detail = dot_detail(saved);
            let dot = state_dot(
                state,
                &crate::i18n::trf(
                    "{client}: {state}",
                    &[("client", &agent.name), ("state", &state_label(state))],
                ),
                pane.column.get(),
            );
            pane.track_cell(agent, &dot, Some(detail));
            dots.append(&dot);
        }
        dots
    }

    /// `target` is the absolute key the backend allows; `path` is what the user sees and copies.
    /// Open uses the app picked from its menu, else the system default.
    fn file_actions(
        &self,
        target: &str,
        path: &str,
        open_label: &'static str,
        can_open: bool,
    ) -> gtk::Box {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let apps = gtk::Box::new(gtk::Orientation::Vertical, 2);
        apps.set_margin_top(4);
        apps.set_margin_bottom(4);
        let popover = gtk::Popover::builder().child(&apps).build();
        let open = adw::SplitButton::builder()
            .child(&l10n::label(open_label))
            .popover(&popover)
            .build();
        open.set_dropdown_tooltip(&crate::i18n::tr("Open with"));
        {
            let page = self.clone();
            let target = target.to_string();
            let shown = path.to_string();
            open.connect_clicked(move |_| page.open_file(&target, &shown, None, false));
        }
        {
            // Listed when the menu opens, so a newly installed editor shows up.
            let page = self.clone();
            let target = target.to_string();
            let shown = path.to_string();
            // The handler gets the popover as its argument: holding a strong copy here would keep
            // every redrawn detail pane's menu alive.
            popover.connect_show(move |popover| {
                clear_box(&apps);
                let title = section(&crate::i18n::tr("Open with"));
                title.set_margin_start(10);
                title.set_margin_bottom(4);
                apps.append(&title);
                let found = crate::skills::open_apps(&target);
                if found.is_empty() {
                    let none = muted(&crate::i18n::tr("No other apps found"));
                    none.set_margin_start(10);
                    none.set_margin_end(10);
                    apps.append(&none);
                }
                let current = found
                    .iter()
                    .find(|app| app.chosen)
                    .or_else(|| found.iter().find(|app| app.default))
                    .map(|app| app.id.clone());
                for app in found {
                    let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                    let check = gtk::Image::from_icon_name("object-select-symbolic");
                    check.set_opacity(if current.as_deref() == Some(app.id.as_str()) {
                        1.0
                    } else {
                        0.0
                    });
                    line.append(&check);
                    line.append(&gtk::Label::new(Some(&if app.default {
                        crate::i18n::trf("{app} (default)", &[("app", &app.name)])
                    } else {
                        app.name.clone()
                    })));
                    let button = gtk::Button::builder()
                        .child(&line)
                        .css_classes(["flat"])
                        .accessible_role(gtk::AccessibleRole::MenuItemRadio)
                        .build();
                    button.update_state(&[gtk::accessible::State::Checked(tristate(
                        current.as_deref() == Some(app.id.as_str()),
                    ))]);
                    let page = page.clone();
                    let target = target.clone();
                    let shown = shown.clone();
                    let popover = popover.downgrade();
                    button.connect_clicked(move |_| {
                        if let Some(popover) = popover.upgrade() {
                            popover.popdown();
                        }
                        page.open_file(&target, &shown, Some(app.id.clone()), false);
                    });
                    apps.append(&button);
                }
            });
        }
        let reveal = l10n::button("Reveal");
        {
            let page = self.clone();
            let target = target.to_string();
            let shown = path.to_string();
            reveal.connect_clicked(move |_| page.open_file(&target, &shown, None, true));
        }
        let copy = gtk::Button::from_icon_name("edit-copy-symbolic");
        copy.set_tooltip_text(Some(&crate::i18n::tr("Copy path")));
        l10n::bind_tooltip(&copy, "Copy path");
        {
            let path = path.to_string();
            let page = self.clone();
            copy.connect_clicked(move |button| {
                button.clipboard().set_text(&path);
                page.show_feedback(&crate::i18n::tr("Path copied"), false);
            });
        }
        if can_open {
            row.append(&open);
        }
        row.append(&reveal);
        row.append(&copy);
        row
    }

    /// Open a file (in `app` when given, which also becomes the app Open uses) or its folder.
    fn open_file(&self, target: &str, shown: &str, app: Option<String>, reveal: bool) {
        let project = self.project.borrow().clone();
        let target = target.to_string();
        let shown = shown.to_string();
        let page = self.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || match app {
                Some(app) => crate::skills::open_path_with(&target, &app, project.as_deref()),
                None => crate::skills::open_path(&target, reveal, project.as_deref()),
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
    }

    /// Ask, then move a skill to the trash and remove the links that reach it.
    fn confirm_remove(&self, skill: &Skill) {
        let Some(parent) = self.app.active_window() else {
            return;
        };
        let heading = crate::i18n::trf("Remove {skill}?", &[("skill", &skill.name)]);
        // Say what will happen: a folder in a skills folder goes to the trash; one that lives
        // elsewhere (a repo) only loses the links to it.
        let folder = own_folder(skill);
        let body = if skill.trash_on_remove {
            crate::i18n::tr(
                "Its folder goes to the trash and links to it are removed, so no client loads it. You can restore the folder from the trash.",
            )
        } else {
            crate::i18n::trf(
                "Only the links to it are removed, so no client loads it. Its folder at {path} stays where it is.",
                &[("path", folder)],
            )
        };
        #[allow(deprecated)]
        let dialog = adw::MessageDialog::new(Some(&parent), Some(&heading), Some(&body));
        dialog.add_response("cancel", &crate::i18n::tr("Cancel"));
        l10n::bind_dialog_response(&dialog, "cancel", "Cancel");
        dialog.add_response("remove", &crate::i18n::tr("Remove"));
        l10n::bind_dialog_response(&dialog, "remove", "Remove");
        dialog.set_close_response("cancel");
        dialog.set_default_response(Some("cancel"));
        dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
        let page = self.clone();
        let key = skill.key.clone();
        let name = skill.name.clone();
        dialog.connect_response(None, move |dialog, response| {
            if response == "remove" {
                let key = key.clone();
                let name = name.clone();
                page.mutate_reporting(move |project| {
                    let removed =
                        crate::skills::remove_skill(&key, project.as_deref()).map_err(titled(
                            crate::i18n::trf("Couldn't remove {skill}", &[("skill", &name)]),
                        ))?;
                    let message = if removed.trashed.is_some() {
                        crate::i18n::trf("Moved {skill} to the trash", &[("skill", &name)])
                    } else {
                        crate::i18n::trf(
                            "Removed the links to {skill}. Its folder stays where it is.",
                            &[("skill", &name)],
                        )
                    };
                    Ok((removed.inventory, message))
                });
            }
            dialog.close();
        });
        dialog.present();
    }

    fn detail_header(&self, icon: &str, name: &str, badges: &[gtk::Widget]) -> gtk::Box {
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        header.append(&tile(icon, "accent", 44));
        let copy = gtk::Box::new(gtk::Orientation::Vertical, 6);
        copy.set_hexpand(true);
        let title = heading(name);
        title.add_css_class("title-3");
        copy.append(&title);
        let chips = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        for badge in badges {
            chips.append(badge);
        }
        copy.append(&chips);
        header.append(&copy);
        header
    }

    fn fill_skill_detail(&self, inv: &Inventory, skill: Option<&Skill>) {
        let pane = &self.skills;
        let scroll = pane.detail_scroll.vadjustment().value();
        clear_box(&pane.detail);
        self.controls.borrow_mut().clear();
        let Some(skill) = skill else {
            let empty = muted(&crate::i18n::tr("Pick a skill to see where it loads."));
            empty.set_halign(gtk::Align::Center);
            empty.set_margin_top(32);
            pane.detail.append(&empty);
            return;
        };
        let broken = skill.source == Source::Broken;
        let mut badges: Vec<gtk::Widget> = vec![icon_badge(
            source_icon(skill.source),
            &source_label(skill.source),
            if broken { "error" } else { "off" },
        )
        .upcast()];
        if let Some(repo) = &skill.git_repo {
            badges.push(
                icon_badge(
                    "media-playlist-shuffle-symbolic",
                    &crate::i18n::trf("git repo: {name}", &[("name", repo)]),
                    "off",
                )
                .upcast(),
            );
        }
        if !skill.editable && !broken {
            badges.push(badge(&crate::i18n::tr("Read only"), "off").upcast());
        }
        pane.detail
            .append(&self.detail_header("starred-symbolic", &skill.name, &badges));
        // A link to a missing folder has nothing to open, only a place to reveal.
        let actions = self.file_actions(&skill.key, &skill.path, "Open SKILL.md", !broken);
        if skill.removable {
            let remove = gtk::Button::from_icon_name("user-trash-symbolic");
            let label = crate::i18n::trf("Remove {skill}", &[("skill", &skill.name)]);
            remove.set_tooltip_text(Some(&label));
            remove.update_property(&[gtk::accessible::Property::Label(&label)]);
            remove.set_sensitive(!self.busy.get());
            let page = self.clone();
            let skill = skill.clone();
            remove.connect_clicked(move |_| page.confirm_remove(&skill));
            actions.append(&remove);
        }
        pane.detail.append(&actions);

        if skill.source != Source::Broken {
            pane.detail
                .append(&section(&crate::i18n::tr("Description")));
            let description = if skill.description.is_empty() {
                muted(&crate::i18n::tr("No description"))
            } else {
                let label = muted(&skill.description);
                label.remove_css_class("toolport-muted");
                // Long descriptions stop after a few lines, like the mockup; Open shows the rest.
                label.set_lines(6);
                label.set_ellipsize(gtk::pango::EllipsizeMode::End);
                label.set_tooltip_text(Some(&skill.description));
                label
            };
            description.set_selectable(true);
            pane.detail.append(&description);
            let cap = crate::skills::DESCRIPTION_CAP;
            let bar = gtk::LevelBar::builder()
                .min_value(0.0)
                .max_value(cap as f64)
                .value(skill.description_chars.min(cap) as f64)
                .css_classes(["toolport-skill-meter"])
                .build();
            bar.remove_offset_value(Some(gtk::LEVEL_BAR_OFFSET_LOW));
            bar.remove_offset_value(Some(gtk::LEVEL_BAR_OFFSET_HIGH));
            bar.remove_offset_value(Some(gtk::LEVEL_BAR_OFFSET_FULL));
            if skill.description_chars > cap {
                bar.add_css_class("over");
            }
            bar.update_property(&[gtk::accessible::Property::Label(&crate::i18n::tr(
                "Description length",
            ))]);
            pane.detail.append(&bar);
            let counts = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let used = muted(&crate::i18n::trf(
                "{count} of {cap} characters",
                &[
                    ("count", &crate::i18n::group_digits(skill.description_chars)),
                    ("cap", &crate::i18n::group_digits(cap)),
                ],
            ));
            used.add_css_class("caption");
            used.set_hexpand(true);
            counts.append(&used);
            let limit = muted(&crate::i18n::tr("Claude Code's limit"));
            limit.add_css_class("caption");
            limit.set_wrap(false);
            limit.set_hexpand(false);
            counts.append(&limit);
            pane.detail.append(&counts);
        }

        pane.detail.append(&section(&crate::i18n::tr("File")));
        let fields = gtk::Grid::builder()
            .column_spacing(16)
            .row_spacing(8)
            .build();
        let mut row = 0;
        let mut field = |label: &str, value: gtk::Widget| {
            let name = muted(label);
            name.set_wrap(false);
            name.set_hexpand(false);
            name.set_valign(gtk::Align::Start);
            fields.attach(&name, 0, row, 1, 1);
            value.set_hexpand(true);
            fields.attach(&value, 1, row, 1, 1);
            row += 1;
        };
        field(&crate::i18n::tr("Path"), path_value(&skill.path).upcast());
        // `links` holds folders, the skill's own among them; only the other ways in are news.
        let own_folder = own_folder(skill);
        let others: Vec<&str> = skill
            .links
            .iter()
            .map(String::as_str)
            .filter(|l| *l != skill.path && *l != own_folder)
            .collect();
        if !others.is_empty() {
            field(
                &crate::i18n::tr("Reached by"),
                path_value(&others.join("\n")).upcast(),
            );
        }
        field(
            &crate::i18n::tr("Source"),
            value_label(&skill.source_note).upcast(),
        );
        if skill.source != Source::Broken {
            field(
                &crate::i18n::tr("Size"),
                value_label(&if skill.lines == 1 {
                    crate::i18n::trf("{size} · 1 line", &[("size", &fmt_bytes(skill.size_bytes))])
                } else {
                    crate::i18n::trf(
                        "{size} · {lines} lines",
                        &[
                            ("size", &fmt_bytes(skill.size_bytes)),
                            ("lines", &crate::i18n::group_digits(skill.lines)),
                        ],
                    )
                })
                .upcast(),
            );
        }
        if let Some(changed) = skill.modified_secs.and_then(fmt_date) {
            field(&crate::i18n::tr("Changed"), value_label(&changed).upcast());
        }
        pane.detail.append(&fields);

        let agents = self.scoped_skill_agents(inv);
        let loaded = agents
            .iter()
            .filter(|a| {
                skill
                    .states
                    .get(&a.id)
                    .is_some_and(|s| matches!(s.state, State::On | State::NameOnly))
            })
            .count();
        pane.detail.append(&section_with(
            &crate::i18n::tr("Agents"),
            &crate::i18n::trf(
                "{count} of {total} load it",
                &[
                    ("count", &loaded.to_string()),
                    ("total", &agents.len().to_string()),
                ],
            ),
        ));
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        list.add_css_class("toolport-skill-agents");
        for agent in agents {
            list.append(&self.agent_row(agent, skill));
        }
        pane.detail.append(&list);
        if !skill.preview.is_empty() {
            pane.detail.append(&section(&crate::i18n::tr("Preview")));
            pane.detail.append(&preview_label(&skill.preview));
        }
        restore_scroll(&pane.detail_scroll, scroll);
        // A write redraws the pane; put focus back on the control the user just used.
        if let Some(agent) = self.last_control.borrow_mut().take() {
            self.restore_agent_control(&agent);
        }
    }

    fn restore_agent_control(&self, agent: &str) {
        let choice = self.focused_choice.borrow();
        let controls = self.controls.borrow();
        if let Some(control) = choice
            .as_ref()
            .filter(|key| *key == agent || key.starts_with(&format!("{agent}#")))
            .and_then(|key| controls.get(key))
            .or_else(|| controls.get(agent))
        {
            control.grab_focus();
        }
    }

    /// One row of a skill's Agents list: logo, where it loads from, the status chip and one
    /// 44px control slot (a switch, a lock, or nothing), plus Claude Code's ⋯ menu for "Name
    /// only". The same layout as the React view's `AgentStateRow`.
    fn agent_row(&self, agent: &Agent, skill: &Skill) -> gtk::Box {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.add_css_class("toolport-skill-agent");
        row.append(&logo_tile(&agent.id));
        let copy = gtk::Box::new(gtk::Orientation::Vertical, 2);
        copy.set_hexpand(true);
        copy.set_valign(gtk::Align::Center);
        let name = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        name.append(&heading(&agent.name));
        if !agent.verified {
            let docs = badge(&crate::i18n::tr("from docs"), "off");
            docs.set_tooltip_text(Some(&crate::i18n::tr(
                "Folders come from this client's docs, not a check on this machine",
            )));
            name.append(&docs);
        }
        copy.append(&name);
        let state = skill.states.get(&agent.id);
        let current = state.map(|s| s.state);
        let switchable = matches!(
            current,
            Some(State::On) | Some(State::NameOnly) | Some(State::Off)
        );
        let reason = state.and_then(|s| s.read_only_reason.as_deref());
        let writable = agent.can_toggle && reason.is_none();
        let read_only = switchable && !writable;
        let mut where_text = state.map_or_else(
            || crate::i18n::tr("Not in any folder it reads"),
            |s| s.via.clone(),
        );
        if read_only {
            where_text = format!(
                "{where_text} · {}",
                crate::i18n::tr("No switch Toolport can write")
            );
        }
        let via = muted(&where_text);
        via.add_css_class("caption");
        copy.append(&via);
        if let Some(reason) = state.and_then(|s| s.read_only_reason.as_deref()) {
            let reason = muted(reason);
            reason.add_css_class("caption");
            copy.append(&reason);
        }
        row.append(&copy);
        row.append(&state_chip(current, None));
        let set = self.state_setter(agent, skill);
        if switchable && writable && agent.name_only {
            row.append(&self.name_only_menu(agent, current == Some(State::NameOnly), set.clone()));
        }
        let slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        slot.set_size_request(44, -1);
        slot.set_hexpand(false);
        slot.set_halign(gtk::Align::End);
        slot.set_valign(gtk::Align::Center);
        if switchable && writable {
            let switch = self.state_switch(agent, skill, current != Some(State::Off), set);
            switch.set_hexpand(true);
            switch.set_halign(gtk::Align::End);
            slot.append(&switch);
        } else if read_only {
            slot.append(&lock(reason.unwrap_or(&agent.switch_note)));
        }
        row.append(&slot);
        row
    }

    /// Writes one agent's state for this skill, reporting the result on the page.
    fn state_setter(&self, agent: &Agent, skill: &Skill) -> Rc<dyn Fn(Wanted)> {
        let page = self.clone();
        let agent_id = agent.id.clone();
        let agent_name = agent.name.clone();
        let key = skill.key.clone();
        let skill_name = skill.name.clone();
        Rc::new(move |wanted: Wanted| {
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
            page.last_control.replace(Some(agent_id.clone()));
            let agent_id = agent_id.clone();
            let key = key.clone();
            let failed = titled(crate::i18n::trf(
                "Couldn't change {skill} in {client}",
                &[("skill", &skill_name), ("client", &agent_name)],
            ));
            page.mutate(success, move |project| {
                crate::skills::set_skill_state(&agent_id, &key, wanted, project.as_deref())
                    .map_err(failed)
            });
        })
    }

    fn state_switch(
        &self,
        agent: &Agent,
        skill: &Skill,
        active: bool,
        set: Rc<dyn Fn(Wanted)>,
    ) -> gtk::Switch {
        let name = crate::i18n::trf(
            "Load {skill} in {client}",
            &[("skill", &skill.name), ("client", &agent.name)],
        );
        let switch = gtk::Switch::builder()
            .active(active)
            .valign(gtk::Align::Center)
            .tooltip_text(&name)
            .build();
        switch.update_property(&[gtk::accessible::Property::Label(&name)]);
        self.controls
            .borrow_mut()
            .insert(agent.id.clone(), switch.clone().upcast());
        let focused = self.focused_choice.clone();
        let slot = agent.id.clone();
        switch.connect_state_set(move |_, active| {
            focused.replace(Some(slot.clone()));
            set(if active { Wanted::On } else { Wanted::Off });
            gtk::glib::Propagation::Proceed
        });
        switch
    }

    /// Claude Code's third state: a small ⋯ menu with one checked item, beside its switch.
    fn name_only_menu(
        &self,
        agent: &Agent,
        name_only: bool,
        set: Rc<dyn Fn(Wanted)>,
    ) -> gtk::MenuButton {
        let content = gtk::Box::new(gtk::Orientation::Vertical, 4);
        content.set_margin_top(6);
        content.set_margin_bottom(6);
        content.set_margin_start(6);
        content.set_margin_end(6);
        content.set_width_request(260);
        let title = section(&agent.name);
        title.set_margin_start(6);
        content.append(&title);
        let check = gtk::CheckButton::builder()
            .label(crate::i18n::tr("Load name and description only"))
            .active(name_only)
            .build();
        content.append(&check);
        let note = muted(&agent.switch_note);
        note.add_css_class("caption");
        note.set_margin_start(6);
        note.set_max_width_chars(36);
        content.append(&note);
        let popover = gtk::Popover::builder().child(&content).build();
        let label = crate::i18n::trf("More options for {client}", &[("client", &agent.name)]);
        let button = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .popover(&popover)
            .valign(gtk::Align::Center)
            .tooltip_text(&label)
            .css_classes(["flat", "toolport-row-more"])
            .build();
        button.update_property(&[gtk::accessible::Property::Label(&label)]);
        let slot = format!("{}#menu", agent.id);
        self.controls
            .borrow_mut()
            .insert(slot.clone(), button.clone().upcast());
        let focused = self.focused_choice.clone();
        let popover_ref = popover.downgrade();
        check.connect_toggled(move |check| {
            // The write redraws the pane; focus goes back to this menu button.
            focused.replace(Some(slot.clone()));
            if let Some(popover) = popover_ref.upgrade() {
                popover.popdown();
            }
            set(if check.is_active() {
                Wanted::NameOnly
            } else {
                Wanted::On
            });
        });
        button
    }

    fn render_rules(&self, inv: &Inventory, needle: &str) {
        let pane = &self.rules;
        self.render_banner(pane, inv);
        let agents = self.scoped_agents(inv);
        self.render_head(
            pane,
            &agents,
            agents.len(),
            &crate::i18n::tr("Rules file"),
            false,
        );
        let visible = self.visible_rules(inv, needle);
        let current = self.current_rule(&visible).map(|r| r.key.clone());

        let scroll = pane.scroll.vadjustment().value();
        self.rebuilding.set(true);
        clear_list(&pane.list);
        let mut rows = Vec::new();
        let mut selected_row = None;
        let project = self.project.borrow().clone();
        for (scope, label, icon) in [
            (
                "global",
                crate::i18n::tr("Global"),
                "avatar-default-symbolic",
            ),
            (
                "project",
                crate::i18n::tr("Project"),
                "folder-open-symbolic",
            ),
        ] {
            let items: Vec<&&RuleFile> = visible.iter().filter(|r| r.scope == scope).collect();
            if items.is_empty() {
                continue;
            }
            let key = format!("rules:{scope}");
            let open = !self.collapsed.borrow().contains(&key) || !needle.is_empty();
            let note = if scope == "global" {
                crate::i18n::tr("every session")
            } else {
                project.as_deref().map(folder_name).unwrap_or_default()
            };
            pane.list
                .append(&self.group_row(key.clone(), icon, &label, items.len(), &note));
            rows.push(RowKind::Group(key));
            if !open {
                continue;
            }
            for rule in items {
                let row = self.rule_row(rule, &agents);
                if current.as_deref() == Some(rule.key.as_str()) {
                    selected_row = Some(row.clone());
                }
                pane.list.append(&row);
                rows.push(RowKind::Item(rule.key.clone()));
            }
        }
        if rows.is_empty() {
            let empty = muted(&if needle.is_empty() && self.focus.borrow().is_none() {
                crate::i18n::tr("No rules files found.")
            } else {
                crate::i18n::tr("No rules files match.")
            });
            empty.set_halign(gtk::Align::Center);
            empty.set_margin_top(32);
            pane.list.append(
                &gtk::ListBoxRow::builder()
                    .child(&empty)
                    .selectable(false)
                    .activatable(false)
                    .build(),
            );
            rows.push(RowKind::Group(String::new()));
        }
        pane.rows.replace(rows);
        pane.list.select_row(selected_row.as_ref());
        self.rebuilding.set(false);
        restore_scroll(&pane.scroll, scroll);
        self.fill_rule_detail(inv, self.current_rule(&visible));
    }

    fn rule_row(&self, rule: &RuleFile, agents: &[&Agent]) -> gtk::ListBoxRow {
        let pane = &self.rules;
        let dots = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        for agent in agents {
            let loaded = rule.agents.contains_key(&agent.id);
            let detail = if loaded {
                crate::i18n::tr("Loaded")
            } else {
                crate::i18n::tr("Not loaded")
            };
            let dot = state_dot(
                loaded.then_some(State::On),
                &crate::i18n::trf(
                    "{client}: {state}",
                    &[
                        ("client", &agent.name),
                        (
                            "state",
                            &if loaded {
                                crate::i18n::tr("Loaded")
                            } else {
                                crate::i18n::tr("Not loaded")
                            },
                        ),
                    ],
                ),
                pane.column.get(),
            );
            pane.track_cell(agent, &dot, Some(detail));
            dots.append(&dot);
        }
        let name = one_line(&rule.name, &["toolport-skill-name"]);
        if pane.stacked.get() {
            // The path is in the detail pane; beside the dots it would be cut to a few letters.
            name.set_hexpand(true);
            return stacked_row(&name, None, &dots);
        }
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        content.add_css_class("toolport-skill-row");
        let copy = gtk::Box::new(gtk::Orientation::Vertical, 2);
        copy.set_hexpand(true);
        copy.add_css_class("toolport-skill-row-copy");
        copy.append(&name);
        copy.append(&one_line(
            &rule.path,
            &["toolport-muted", "caption", "monospace"],
        ));
        content.append(&copy);
        content.append(&dots);
        gtk::ListBoxRow::builder().child(&content).build()
    }

    fn fill_rule_detail(&self, inv: &Inventory, rule: Option<&RuleFile>) {
        let pane = &self.rules;
        let scroll = pane.detail_scroll.vadjustment().value();
        clear_box(&pane.detail);
        let Some(rule) = rule else {
            let empty = muted(&crate::i18n::tr(
                "Pick a rules file to see which agents read it.",
            ));
            empty.set_halign(gtk::Align::Center);
            empty.set_margin_top(32);
            pane.detail.append(&empty);
            return;
        };
        let scope = badge(
            &if rule.scope == "project" {
                crate::i18n::tr("Project")
            } else {
                crate::i18n::tr("Global")
            },
            "off",
        );
        pane.detail.append(&self.detail_header(
            "text-x-generic-symbolic",
            &rule.name,
            &[scope.upcast()],
        ));
        pane.detail
            .append(&self.file_actions(&rule.key, &rule.path, "Open file", true));
        pane.detail.append(&section(&crate::i18n::tr("File")));
        let fields = gtk::Grid::builder()
            .column_spacing(16)
            .row_spacing(8)
            .build();
        for (row, (label, value)) in [
            (
                crate::i18n::tr("Path"),
                path_value(&rule.path).upcast::<gtk::Widget>(),
            ),
            (
                crate::i18n::tr("Size"),
                value_label(&fmt_bytes(rule.size_bytes)).upcast(),
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let name = muted(&label);
            name.set_wrap(false);
            name.set_hexpand(false);
            name.set_valign(gtk::Align::Start);
            fields.attach(&name, 0, row as i32, 1, 1);
            value.set_hexpand(true);
            fields.attach(&value, 1, row as i32, 1, 1);
        }
        pane.detail.append(&fields);

        let agents = self.scoped_agents(inv);
        let loaded = agents
            .iter()
            .filter(|a| rule.agents.contains_key(&a.id))
            .count();
        pane.detail.append(&section_with(
            &crate::i18n::tr("Agents"),
            &crate::i18n::trf(
                "{count} of {total} load it",
                &[
                    ("count", &loaded.to_string()),
                    ("total", &agents.len().to_string()),
                ],
            ),
        ));
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        list.add_css_class("toolport-skill-agents");
        for agent in agents {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            row.add_css_class("toolport-skill-agent");
            row.append(&logo_tile(&agent.id));
            let copy = gtk::Box::new(gtk::Orientation::Vertical, 2);
            copy.set_hexpand(true);
            copy.append(&heading(&agent.name));
            match rule.agents.get(&agent.id) {
                Some(chain) => {
                    for (index, step) in chain.iter().enumerate() {
                        let text = match step.kind.as_str() {
                            "file" => step.path.clone(),
                            "link" => crate::i18n::trf("links to {path}", &[("path", &step.path)]),
                            "import" => {
                                crate::i18n::trf("@imports {path}", &[("path", &step.path)])
                            }
                            _ => crate::i18n::trf("skips @import {path}", &[("path", &step.path)]),
                        };
                        let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                        if index > 0 {
                            let arrow = gtk::Label::new(Some("↳"));
                            arrow.add_css_class("toolport-muted");
                            arrow.set_valign(gtk::Align::Start);
                            line.append(&arrow);
                        }
                        let label = muted(&text);
                        label.add_css_class("caption");
                        if step.kind == "file" {
                            label.add_css_class("monospace");
                        }
                        if step.kind == "skipped-import" {
                            label.add_css_class("warning");
                        }
                        line.append(&label);
                        copy.append(&line);
                    }
                    row.append(&copy);
                    let loaded = state_chip(Some(State::On), Some(&crate::i18n::tr("Loaded")));
                    loaded.set_valign(gtk::Align::Start);
                    row.append(&loaded);
                }
                None => {
                    let note = muted(&agent.rules_note.clone().unwrap_or_else(|| {
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
                    }));
                    note.add_css_class("caption");
                    copy.append(&note);
                    row.append(&copy);
                    let state = state_chip(None, Some(&crate::i18n::tr("Not loaded")));
                    state.set_valign(gtk::Align::Start);
                    row.append(&state);
                }
            }
            let slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            slot.set_size_request(44, -1);
            slot.set_hexpand(false);
            slot.set_halign(gtk::Align::End);
            if rule.agents.contains_key(&agent.id) {
                slot.append(&lock(&crate::i18n::tr("No switch Toolport can write")));
            }
            row.append(&slot);
            list.append(&row);
        }
        pane.detail.append(&list);
        if !rule.preview.is_empty() {
            pane.detail.append(&section(&crate::i18n::tr("Preview")));
            pane.detail.append(&preview_label(&rule.preview));
        }
        restore_scroll(&pane.detail_scroll, scroll);
    }

    fn render_issues(&self, inv: &Inventory) {
        clear_box(&self.issues_list);
        let (dismissed, active): (Vec<&Issue>, Vec<&Issue>) =
            inv.issues.iter().partition(|i| i.dismissed);
        if active.is_empty() {
            let empty = gtk::Box::new(gtk::Orientation::Vertical, 6);
            empty.set_margin_top(40);
            empty.set_halign(gtk::Align::Center);
            let title = heading(&crate::i18n::tr("Nothing needs a look"));
            title.set_halign(gtk::Align::Center);
            empty.append(&title);
            empty.append(&muted(&crate::i18n::tr(
                "Every skill and rules file loads the way it's set.",
            )));
            self.issues_list.append(&empty);
        }
        for issue in active {
            self.issues_list.append(&self.issue_card(inv, issue));
        }
        if !dismissed.is_empty() {
            let open = !self.collapsed.borrow().contains(DISMISSED_KEY);
            let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            content.add_css_class("toolport-skill-group");
            let chevron = gtk::Image::from_icon_name(if open {
                "pan-down-symbolic"
            } else {
                "pan-end-symbolic"
            });
            chevron.add_css_class("toolport-muted");
            content.append(&chevron);
            let title = gtk::Label::new(Some(&crate::i18n::tr("Dismissed")));
            title.add_css_class("heading");
            content.append(&title);
            let pill = gtk::Label::new(Some(&dismissed.len().to_string()));
            pill.add_css_class("toolport-skill-count");
            pill.add_css_class("caption");
            content.append(&pill);
            content.append(&one_line(
                &crate::i18n::tr("Back if anything about them changes"),
                &["toolport-muted", "caption"],
            ));
            let toggle = gtk::Button::builder()
                .child(&content)
                .css_classes(["flat"])
                .build();
            toggle.update_state(&[gtk::accessible::State::Expanded(Some(open))]);
            let page = self.clone();
            toggle.connect_clicked(move |_| {
                {
                    let mut collapsed = page.collapsed.borrow_mut();
                    if !collapsed.remove(DISMISSED_KEY) {
                        collapsed.insert(DISMISSED_KEY.to_string());
                    }
                }
                page.render_keeping_focus(DISMISSED_KEY.to_string());
            });
            self.offer_focus(DISMISSED_KEY, &toggle);
            self.issues_list.append(&toggle);
            if open {
                for issue in dismissed {
                    let card = self.issue_card(inv, issue);
                    self.issues_list.append(&card);
                }
            }
        }
        clear_box(&self.issues_side);
        self.issues_side
            .append(&section(&crate::i18n::tr("By agent")));
        let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
        list.add_css_class("toolport-skill-agents");
        for agent in &inv.agents {
            list.append(&agent_summary(inv, agent));
        }
        self.issues_side.append(&list);
        let note = muted(&crate::i18n::tr(
            "\"Own switch\" means the agent has its own per-skill setting that Toolport can write. Toolport never moves or deletes your skill folders to turn a skill off.",
        ));
        note.add_css_class("caption");
        self.issues_side.append(&note);
    }

    fn issue_card(&self, inv: &Inventory, issue: &Issue) -> IssueFrame {
        let card = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        card.add_css_class("toolport-skill-issue");
        let (icon, class) = match issue.level.as_str() {
            "error" => ("dialog-error-symbolic", "error"),
            "warn" => ("dialog-warning-symbolic", "warning"),
            _ => ("dialog-information-symbolic", "info"),
        };
        card.add_css_class(match class {
            "error" => "toolport-issue-error",
            "warning" => "toolport-issue-warning",
            _ => "toolport-issue-info",
        });
        card.append(&tile(icon, class, 32));
        let copy = gtk::Box::new(gtk::Orientation::Vertical, 6);
        copy.set_hexpand(true);
        let top = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let title = heading(&issue.title);
        title.set_hexpand(true);
        // A wrapping label asks for a short line by default; ask for one line and wrap only
        // when the row is too narrow.
        title.set_natural_wrap_mode(gtk::NaturalWrapMode::None);
        top.append(&title);
        let dismiss = if issue.dismissed {
            l10n::button("Show again")
        } else {
            l10n::button("Dismiss")
        };
        dismiss.add_css_class("flat");
        dismiss.set_valign(gtk::Align::Start);
        dismiss.set_sensitive(!self.busy.get());
        {
            let page = self.clone();
            let fingerprint = issue.fingerprint.clone();
            let hide = !issue.dismissed;
            dismiss.connect_clicked(move |_| {
                let fingerprint = fingerprint.clone();
                // A dismissed card folds into the Dismissed section; one brought back returns to
                // the list with its Dismiss button focused.
                page.refocus.replace(Some(if hide {
                    DISMISSED_KEY.to_string()
                } else {
                    format!("issue:{fingerprint}")
                }));
                let failed = titled(crate::i18n::tr("Couldn't change this"));
                page.mutate(String::new(), move |project| {
                    crate::skills::dismiss_issue(&fingerprint, hide, project.as_deref())
                        .map_err(failed)
                });
            });
        }
        self.offer_focus(&format!("issue:{}", issue.fingerprint), &dismiss);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_valign(gtk::Align::Start);
        actions.append(&dismiss);
        if let Some(label) = issue.fix_label.as_ref().filter(|_| !issue.dismissed) {
            let button = gtk::Button::with_label(label);
            button.add_css_class("toolport-secondary-action");
            button.set_valign(gtk::Align::Start);
            button.set_sensitive(!self.busy.get());
            let page = self.clone();
            let issue = issue.clone();
            let label = label.clone();
            button.connect_clicked(move |_| page.confirm_fix(&issue, &label));
            actions.append(&button);
        }
        top.append(&actions);
        copy.append(&top);
        copy.append(&muted(&issue.detail));
        let logos = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        for agent in issue
            .agents
            .iter()
            .filter_map(|id| inv.agents.iter().find(|a| &a.id == id))
        {
            let chip = gtk::CenterBox::new();
            chip.add_css_class("toolport-skill-chip");
            chip.add_css_class("logo");
            chip.set_size_request(24, 24);
            chip.set_tooltip_text(Some(&agent.name));
            chip.update_property(&[gtk::accessible::Property::Label(&agent.name)]);
            chip.set_center_widget(Some(&logo(&agent.id, 14)));
            logos.append(&chip);
        }
        if logos.first_child().is_some() {
            copy.append(&logos);
        }
        let chips = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .column_spacing(6)
            .row_spacing(6)
            .max_children_per_line(40)
            .homogeneous(false)
            .build();
        for name in issue.skill_names.iter().take(ISSUE_SKILL_LIMIT) {
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            let star = gtk::Image::from_icon_name("starred-symbolic");
            star.set_pixel_size(12);
            star.add_css_class("toolport-muted");
            line.append(&star);
            let label = gtk::Label::new(Some(name));
            label.add_css_class("caption");
            line.append(&label);
            // A chip opens that skill on the Skills tab, where it can be switched or removed.
            let chip = gtk::Button::builder()
                .child(&line)
                .halign(gtk::Align::Start)
                .css_classes(["flat", "toolport-skill-chip"])
                .tooltip_text(crate::i18n::trf("Show {skill}", &[("skill", name)]))
                .build();
            let page = self.clone();
            let keys = issue.skills.clone();
            let name = name.clone();
            chip.connect_clicked(move |_| page.show_skill(&keys, &name));
            chips.insert(&chip, -1);
        }
        if issue.skill_names.len() > ISSUE_SKILL_LIMIT {
            let more = muted(&crate::i18n::trf(
                "+{count} more",
                &[(
                    "count",
                    &(issue.skill_names.len() - ISSUE_SKILL_LIMIT).to_string(),
                )],
            ));
            more.add_css_class("caption");
            chips.insert(&more, -1);
        }
        if chips.first_child().is_some() {
            copy.append(&chips);
        }
        let narrow = adw::Breakpoint::new(
            adw::BreakpointCondition::parse("max-width: 560px").expect("static issue breakpoint"),
        );
        let (a, t, c) = (actions.clone(), top.clone(), copy.clone());
        narrow.connect_apply(move |_| {
            t.remove(&a);
            a.set_halign(gtk::Align::Start);
            c.append(&a);
        });
        let (a, t, c) = (actions.clone(), top.clone(), copy.clone());
        narrow.connect_unapply(move |_| {
            c.remove(&a);
            a.set_halign(gtk::Align::End);
            t.append(&a);
        });
        card.append(&copy);
        let adaptive = adw::BreakpointBin::builder()
            .child(&card)
            .width_request(280)
            // IssueFrame supplies the measured content height; the bin still requires an
            // explicit minimum for its breakpoint allocation contract.
            .height_request(1)
            .build();
        adaptive.add_breakpoint(narrow);
        IssueFrame::new(&adaptive, &card)
    }

    /// Jump from a Needs a look chip to that skill on the Skills tab.
    fn show_skill(&self, keys: &[String], name: &str) {
        let found = self.inventory.borrow().as_ref().and_then(|inv| {
            inv.skills
                .iter()
                .find(|s| s.name == name && keys.contains(&s.key))
                .map(|s| (s.key.clone(), s.source))
        });
        let Some((key, source)) = found else {
            return;
        };
        self.connected_only.set(false);
        self.search.set_text("");
        self.collapsed.borrow_mut().remove(&source_key(source));
        self.selected_skill.replace(Some(key));
        self.skills.detail_scroll.vadjustment().set_value(0.0);
        self.select_tab(Tab::Skills);
    }

    fn confirm_fix(&self, issue: &Issue, label: &str) {
        let Some(parent) = self.app.active_window() else {
            return;
        };
        #[allow(deprecated)]
        let dialog = adw::MessageDialog::new(
            Some(&parent),
            Some(label),
            Some(
                &issue
                    .fix_note
                    .clone()
                    .unwrap_or_else(|| {
                        crate::i18n::tr(
                            "Toolport backs up any config file before it changes it, and never deletes a skill folder.",
                        )
                    }),
            ),
        );
        // A fix's own note arrives already translated by the scan.
        if issue.fix_note.is_none() {
            l10n::bind_dialog_body(
                &dialog,
                "Toolport backs up any config file before it changes it, and never deletes a skill folder.",
            );
        }
        if !issue.fix_paths.is_empty() {
            let paths = path_value(&issue.fix_paths.join("\n"));
            paths.add_css_class("caption");
            paths.add_css_class("toolport-fix-paths");
            dialog.set_extra_child(Some(&paths));
        }
        dialog.add_response("cancel", &crate::i18n::tr("Cancel"));
        l10n::bind_dialog_response(&dialog, "cancel", "Cancel");
        dialog.add_response("fix", label);
        dialog.set_close_response("cancel");
        dialog.set_default_response(Some("cancel"));
        dialog.set_response_appearance("fix", adw::ResponseAppearance::Suggested);
        let page = self.clone();
        let id = issue.id.clone();
        let fingerprint = issue.fingerprint.clone();
        let success = crate::i18n::trf("Fixed: {title}", &[("title", &issue.title)]);
        dialog.connect_response(None, move |dialog, response| {
            if response == "fix" {
                let id = id.clone();
                let fingerprint = fingerprint.clone();
                let failed = titled(crate::i18n::tr("Couldn't fix this"));
                page.mutate(success.clone(), move |project| {
                    crate::skills::fix_issue(&id, &fingerprint, project.as_deref()).map_err(failed)
                });
            }
            dialog.close();
        });
        dialog.present();
    }
}

fn tristate(on: bool) -> gtk::AccessibleTristate {
    if on {
        gtk::AccessibleTristate::True
    } else {
        gtk::AccessibleTristate::False
    }
}

/// An error under a line saying what failed, as the React view's toast shows them.
fn titled(title: String) -> impl FnOnce(String) -> String + Send + 'static {
    move |error| format!("{title}\n{error}")
}

/// Put a scroller back where it was once the rebuilt content has been measured.
fn restore_scroll(scroll: &gtk::ScrolledWindow, value: f64) {
    if value <= 0.0 {
        return;
    }
    let adjustment = scroll.vadjustment();
    adjustment.set_value(value);
    gtk::glib::idle_add_local_once(move || adjustment.set_value(value));
}

fn path_value(path: &str) -> gtk::Label {
    // A path broken across lines must not gain a hyphen that isn't in it, and breaks at its
    // slashes and dashes before splitting a word.
    gtk::Label::builder()
        .label(format!(
            "<span insert_hyphens=\"false\">{}</span>",
            gtk::glib::markup_escape_text(path)
        ))
        .use_markup(true)
        .halign(gtk::Align::Fill)
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .selectable(true)
        .css_classes(["monospace"])
        .build()
}

fn value_label(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .halign(gtk::Align::Fill)
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .build()
}

fn agent_summary(inv: &Inventory, agent: &Agent) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.add_css_class("toolport-skill-agent");
    row.append(&logo_tile(&agent.id));
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
    let summary = muted(&parts.join(" · "));
    summary.add_css_class("caption");
    copy.append(&summary);
    row.append(&copy);
    let broken = count(State::Broken);
    let status = if broken > 0 {
        badge(
            &crate::i18n::trf("{count} broken", &[("count", &broken.to_string())]),
            "error",
        )
    } else if agent.supports_skills {
        // A fact about the agent, not a button: plain secondary text.
        let label = muted(&if agent.can_toggle {
            crate::i18n::tr("Own switch")
        } else {
            crate::i18n::tr("No switch")
        });
        label.set_wrap(false);
        label.set_hexpand(false);
        label.add_css_class("caption");
        label.set_tooltip_text(Some(&agent.switch_note));
        label
    } else {
        return row;
    };
    status.set_valign(gtk::Align::Center);
    row.append(&status);
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_column_names_match_the_react_labels() {
        for (name, short) in [
            ("Claude Code", "Claude"),
            ("Copilot CLI", "Copilot"),
            ("OpenCode", "OpenCode"),
            ("LongAgentName", "LongAge…"),
            ("代理智能体名称很长测试", "代理智能体名称…"),
        ] {
            assert_eq!(short_agent_name(name), short);
        }
    }

    #[test]
    fn dot_hint_keeps_non_path_explanations() {
        let state = crate::skills::SkillState {
            state: State::Off,
            via: "Off in config.toml".into(),
            switch_name: None,
            read_only_reason: None,
            by_pattern: false,
        };
        assert_eq!(dot_detail(Some(&state)), "Off in config.toml");
    }

    fn regression_agent() -> Agent {
        Agent {
            connected: false,
            id: "claude-code".into(),
            name: "Claude Code".into(),
            supports_skills: true,
            verified: true,
            can_toggle: true,
            name_only: true,
            switch_note: String::new(),
            roots: vec![],
            rules_note: None,
            rule_locations: vec![],
        }
    }

    #[test]
    #[ignore = "requires GTK; explicitly run in the isolated omabox session"]
    fn connected_profile_scope_filters_columns_rows_and_rules() {
        adw::init().unwrap();
        let _data = crate::registry::DataDirTestEnv::new("skills-connected-profiles");
        let app = adw::Application::builder().build();
        app.register(None::<&gtk::gio::Cancellable>).unwrap();
        let agents: Vec<Agent> = (0..8)
            .map(|i| Agent {
                id: format!("{}@{i}", if i < 4 { "claude-code" } else { "codex" }),
                name: format!("Profile {i}"),
                connected: matches!(i, 0 | 1 | 4),
                ..regression_agent()
            })
            .collect();
        let skill: Skill = serde_json::from_value(serde_json::json!({
            "key": "private", "name": "Private work skill", "description": "", "descriptionChars": 0,
            "path": "", "folder": "", "source": "yours", "sourceNote": "", "editable": false,
            "sizeBytes": 0, "lines": 0, "modifiedSecs": null, "gitRepo": null,
            "links": [], "states": {"codex@5": {"state": "on", "via": "Work profile"}}, "preview": ""
        })).unwrap();
        let rule = RuleFile {
            key: "work-rule".into(),
            name: "AGENTS.md".into(),
            path: "Work/AGENTS.md".into(),
            scope: "global".into(),
            size_bytes: 10,
            preview: "Work rules".into(),
            agents: [("codex@5".into(), vec![])].into(),
        };
        let inv = Inventory {
            agents,
            skills: vec![skill],
            rules: vec![rule],
            issues: vec![],
            project: None,
        };
        let page = SkillsPage::new(&app);
        page.inventory.replace(Some(inv));
        page.render();
        let columns = |pane: &Pane| {
            let mut count = 0;
            let mut child = pane.head.first_child();
            while let Some(widget) = child {
                if widget.has_css_class("toolport-skill-col") {
                    count += 1;
                }
                child = widget.next_sibling();
            }
            count
        };
        assert_eq!(columns(&page.skills), 3);
        assert_eq!(page.agent_filter.selected(), 0);
        assert_eq!(page.filter_ids.borrow()[0], "@connected");
        assert!(page
            .visible_skills(page.inventory.borrow().as_ref().unwrap(), "")
            .is_empty());
        page.agent_filter.set_selected(1); // All detected agents, via the real callback.
        assert_eq!(columns(&page.skills), 8);
        assert_eq!(
            page.visible_skills(page.inventory.borrow().as_ref().unwrap(), "")
                .len(),
            1
        );
        page.select_tab(Tab::Rules);
        assert_eq!(columns(&page.rules), 8);
        assert_eq!(
            page.visible_rules(page.inventory.borrow().as_ref().unwrap(), "")
                .len(),
            1
        );
        page.agent_filter.set_selected(0);
        assert_eq!(columns(&page.rules), 3);
        assert!(page
            .visible_rules(page.inventory.borrow().as_ref().unwrap(), "")
            .is_empty());
        page.select_tab(Tab::Skills);
        assert_eq!(columns(&page.skills), 3);
        page.show_skill(&["private".into()], "Private work skill");
        assert!(
            !page.connected_only.get(),
            "issue jump must reveal an unconnected profile's skill"
        );
        assert_eq!(
            page.current_skill(&page.visible_skills(page.inventory.borrow().as_ref().unwrap(), ""))
                .unwrap()
                .key,
            "private"
        );
        for agent in &mut page.inventory.borrow_mut().as_mut().unwrap().agents {
            agent.connected = false;
        }
        page.render();
        assert_eq!(columns(&page.skills), 8);
        assert_eq!(
            page.filter_ids.borrow()[page.agent_filter.selected() as usize],
            ""
        );
    }

    // Separate processes keep GTK window/frame-clock state independent at each size.
    #[test]
    #[ignore = "requires GTK; explicitly run in the isolated omabox session"]
    fn issues_page_scroll_at_1024() {
        assert_issues_page_scroll(768);
    }

    #[test]
    #[ignore = "requires GTK; explicitly run in the isolated omabox session"]
    fn issues_page_scroll_at_1120() {
        assert_issues_page_scroll(864);
    }

    #[test]
    #[ignore = "requires GTK; explicitly run in the isolated omabox session"]
    fn issues_panes_scroll_at_1440() {
        assert_issues_page_scroll(1184);
    }

    fn assert_issues_page_scroll(width: i32) {
        adw::init().unwrap();
        let _data = crate::registry::DataDirTestEnv::new("issues-page-scroll");
        let app = adw::Application::builder().build();
        app.register(None::<&gtk::gio::Cancellable>).unwrap();
        let issues = (0..6).map(|i| serde_json::json!({
            "id": format!("issue-{i}"), "level": "warn",
            "title": "Skills you turned off still load in 3 other clients",
            "detail": "Review the client configuration and the skill location before changing what each agent loads. ".repeat(3),
            "agents": ["claude-code"], "skills": [], "skillNames": [],
            "fixLabel": "Turn off where possible"
        })).collect::<Vec<_>>();
        let inv: Inventory = serde_json::from_value(serde_json::json!({
            "agents": [regression_agent()], "skills": [], "rules": [], "issues": issues, "project": null
        })).unwrap();
        let settle = || {
            let loop_ = gtk::glib::MainLoop::new(None, false);
            let stop = loop_.clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(250), move || {
                stop.quit()
            });
            loop_.run();
        };
        let page = SkillsPage::new(&app);
        page.inventory.replace(Some(inv));
        page.select_tab(Tab::Issues);
        let outer = page
            .stack
            .child_by_name("issues")
            .unwrap()
            .downcast::<gtk::ScrolledWindow>()
            .unwrap();
        let inner = page
            .issues_list
            .ancestor(gtk::ScrolledWindow::static_type())
            .unwrap()
            .downcast::<gtk::ScrolledWindow>()
            .unwrap();
        let side = page
            .issues_side
            .ancestor(gtk::ScrolledWindow::static_type())
            .unwrap()
            .downcast::<gtk::ScrolledWindow>()
            .unwrap();
        let window = adw::ApplicationWindow::builder()
            .application(&app)
            .default_width(width)
            .default_height(700)
            .resizable(false)
            .content(&page.root)
            .build();
        let theme = super::super::theme::ThemeController::new();
        theme.attach(&window);
        window.present();
        // Mapping the page starts a scan of the real HOME; drop its result so these cards stay.
        page.generation.set(page.generation.get().wrapping_add(1));
        page.tabs[2].1.grab_focus();
        settle();
        // Adjustment changes queue viewport allocation. Drive it before reading
        // bounds rather than depending on the compositor's next frame callback.
        let allocate = || {
            outer.queue_allocate();
            outer.allocate(outer.width(), outer.height(), -1, None);
        };
        allocate();
        assert_eq!(
            page.root.width(),
            width,
            "exercise the requested page width"
        );
        let narrow = width < 980;
        assert_eq!(
            inner.vscrollbar_policy(),
            if narrow {
                gtk::PolicyType::Never
            } else {
                gtk::PolicyType::Automatic
            }
        );
        assert_eq!(side.vscrollbar_policy(), inner.vscrollbar_policy());
        assert_eq!(
            outer.vscrollbar_policy(),
            if narrow {
                gtk::PolicyType::Automatic
            } else {
                gtk::PolicyType::Never
            }
        );
        let last = page.issues_list.last_child().unwrap();
        assert!(last.is::<IssueFrame>(), "exercise real issue cards");
        assert!(last.height() >= last.measure(gtk::Orientation::Vertical, last.width()).0);
        if narrow {
            let adj = outer.vadjustment();
            assert!(
                adj.upper() > adj.page_size() * 2.0,
                "cards retain their full measured height"
            );
            assert_eq!(
                inner.vadjustment().upper(),
                inner.vadjustment().page_size(),
                "no short inner scroller"
            );
            let bounds = last.compute_bounds(&outer).unwrap();
            adj.set_value(adj.value() + f64::from(bounds.y()));
            settle();
            allocate();
            let bounds = last.compute_bounds(&outer).unwrap();
            assert!(
                bounds.y() >= -1.0 && bounds.y() + bounds.height() <= outer.height() as f32 + 1.0,
                "last card reachable in page scroll: {bounds:?}"
            );
            adj.set_value(adj.upper() - adj.page_size());
            settle();
            allocate();
            let bounds = page.issues_side.compute_bounds(&outer).unwrap();
            assert!(
                bounds.y() >= 0.0 && bounds.y() + bounds.height() <= outer.height() as f32 + 1.0,
                "By agent reachable in same scroll"
            );
        } else {
            assert!(
                inner.vadjustment().upper() > inner.vadjustment().page_size(),
                "wide cards keep independent scrolling"
            );
        }
        window.destroy();
    }

    #[test]
    #[ignore = "requires GTK; explicitly run in the isolated omabox session"]
    fn b6_toast_confirmation_clears_an_earlier_error_bar() {
        adw::init().expect("this regression runs inside omabox");
        let _lock = crate::registry::data_dir_test_lock();
        let scratch = std::env::temp_dir().join(format!(
            "toolport-b6_toast_confirmation_clears_an_earlier_error_bar-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&scratch).unwrap();
        let _data = crate::registry::DataDirOverride::set(&scratch);
        let app = adw::Application::builder()
            .application_id("com.tsout.Toolport.SkillsToastTest")
            .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(gtk::gio::Cancellable::NONE).unwrap();
        let page = SkillsPage::new(&app);
        let overlay = adw::ToastOverlay::new();
        overlay.set_child(Some(&page.root));
        let window = adw::ApplicationWindow::builder()
            .application(&app)
            .content(&overlay)
            .build();
        page.show_feedback("Could not write the skill", true);
        assert!(page.feedback.has_css_class("error"));
        page.show_feedback("Skill turned on", false);
        assert_eq!(page.feedback.text(), "");
        assert!(!page.feedback.has_css_class("error"), "no empty red bar");
        assert!(page.feedback.has_css_class("idle"));
        assert!(page.feedback_timer.borrow().is_none());
        window.destroy();
        std::fs::remove_dir_all(&scratch).unwrap();
    }

    #[test]
    #[ignore = "requires GTK; explicitly run in the isolated omabox session"]
    fn column_hover_and_focus_preserve_header_height() {
        gtk::init().expect("this regression runs inside omabox");
        let _lock = crate::registry::data_dir_test_lock();
        let scratch = std::env::temp_dir().join(format!(
            "toolport-column_hover_and_focus_preserve_header_height-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&scratch).unwrap();
        let _data = crate::registry::DataDirOverride::set(&scratch);
        let app = adw::Application::builder().build();
        let page = SkillsPage::new(&app);
        let agent = regression_agent();
        for pane in [&page.skills, &page.rules] {
            page.render_head(pane, &[&agent], 1, "SKILL", false);
            for column in [NARROW_COLUMN, WIDE_COLUMN] {
                for cell in pane.cells.borrow().values().flatten() {
                    cell.set_width_request(column);
                }
                // The list column holds the hover line and the header, so both count.
                let list = pane
                    .head
                    .parent()
                    .expect("the header sits in the list column");
                let height = || list.measure(gtk::Orientation::Vertical, 640);
                let width = || list.measure(gtk::Orientation::Horizontal, -1);
                let before = height();
                let wide_before = width();
                for _ in 0..10 {
                    pane.hover_column(
                        &agent.id,
                        &agent.name,
                        Some(
                            "On via ~/.claude/skills/a/very/long/folder/that/would/widen/the/list",
                        ),
                        true,
                    );
                    assert_eq!(
                        width(),
                        wide_before,
                        "a long hover note must not widen the list"
                    );
                    pane.hover_column(&agent.id, &agent.name, Some("Off in settings.json"), true);
                    assert_eq!(height(), before, "hover must not resize the header");
                    assert_eq!(width(), wide_before, "hover must not resize the header");
                    let hint = pane.hint.borrow().clone().unwrap();
                    // Measuring at scale 1 can't catch a few pixels a fractional scale adds, so
                    // check the hint can't size its line at all: it is laid over a placeholder.
                    let line = hint
                        .parent()
                        .and_downcast::<gtk::Overlay>()
                        .expect("the hint is laid over its line");
                    assert_ne!(
                        line.child().as_ref(),
                        Some(hint.upcast_ref::<gtk::Widget>()),
                        "only the placeholder sizes the hover line"
                    );
                    assert_eq!(hint.text(), "Claude Code: Off in settings.json");
                    assert_eq!(hint.opacity(), 1.0);
                    assert!(pane.cells.borrow()[&agent.id]
                        .iter()
                        .all(|cell| cell.has_css_class("toolport-skill-col-hl")));
                    pane.focus_column(&agent.id, &agent.name, None, true);
                    pane.hover_column(&agent.id, &agent.name, None, false);
                    assert_eq!(height(), before, "keyboard hint must not resize the header");
                    assert_eq!(hint.text(), "Claude Code");
                    assert_eq!(hint.opacity(), 1.0);
                    pane.focus_column(&agent.id, &agent.name, Some("not checked"), true);
                    assert_eq!(
                        hint.text(),
                        "Claude Code: not checked",
                        "keyboard focus shows the same note as hover"
                    );
                    pane.focus_column(&agent.id, &agent.name, None, false);
                    assert_eq!(height(), before, "leaving must not resize the header");
                    assert_eq!(hint.opacity(), 0.0);
                    assert!(
                        hint.text().is_empty(),
                        "hidden label must expose no stale accessible name"
                    );
                    assert!(
                        hint.label().is_empty(),
                        "hidden markup must also be cleared"
                    );
                    assert!(
                        hint.is_visible(),
                        "reserve the row without visibility relayout"
                    );
                    assert!(pane.cells.borrow()[&agent.id]
                        .iter()
                        .all(|cell| !cell.has_css_class("toolport-skill-col-hl")));
                }
            }
        }
        std::fs::remove_dir_all(&scratch).unwrap();
    }

    #[test]
    #[ignore = "requires GTK; explicitly run in the isolated omabox session"]
    fn many_agents_put_names_above_their_dots_when_narrow() {
        adw::init().unwrap();
        let _data = crate::registry::DataDirTestEnv::new("skills-many-agents");
        let app = adw::Application::builder().build();
        app.register(None::<&gtk::gio::Cancellable>).unwrap();
        // Stacked: names above the dots, every name whole.
        assert_eq!(names_with_twenty_agents(&app, 760), (true, false));
        // Wide enough beside the detail pane: one line per row, names still whole.
        assert_eq!(names_with_twenty_agents(&app, 1600), (false, false));
    }

    /// Whether the Skills rows are stacked at `width` with 20 agents, and whether the skill's
    /// name is cut short.
    fn names_with_twenty_agents(app: &adw::Application, width: i32) -> (bool, bool) {
        let agents: Vec<Agent> = (0..20)
            .map(|i| Agent {
                id: format!("claude-code@{i}"),
                name: format!("Profile {i}"),
                connected: true,
                ..regression_agent()
            })
            .collect();
        let states: serde_json::Map<_, _> = agents
            .iter()
            .map(|a| {
                (
                    a.id.clone(),
                    serde_json::json!({"state": "on", "via": "~/.claude"}),
                )
            })
            .collect();
        let skill: Skill = serde_json::from_value(serde_json::json!({
            "key": "notes", "name": "release-notes-writer", "description": "Writes release notes",
            "descriptionChars": 20, "path": "", "folder": "", "source": "yours", "sourceNote": "",
            "editable": false, "sizeBytes": 2048, "lines": 40, "modifiedSecs": null,
            "gitRepo": null, "links": [], "states": states, "preview": ""
        }))
        .unwrap();
        let inv = Inventory {
            agents,
            skills: vec![skill],
            rules: vec![],
            issues: vec![],
            project: None,
        };
        let settle = || {
            let loop_ = gtk::glib::MainLoop::new(None, false);
            let stop = loop_.clone();
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(250), move || {
                stop.quit()
            });
            loop_.run();
        };
        let page = SkillsPage::new(app);
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .default_width(width)
            .default_height(800)
            .resizable(false)
            .content(&page.root)
            .build();
        window.present();
        // Mapping the page starts a scan of the real HOME; drop its result.
        page.generation.set(page.generation.get().wrapping_add(1));
        page.inventory.replace(Some(inv));
        page.render();
        settle();
        assert_eq!(
            page.root.width(),
            width,
            "exercise the requested page width"
        );
        let mut name = None;
        let mut stack = vec![page.skills.list.clone().upcast::<gtk::Widget>()];
        while let Some(widget) = stack.pop() {
            if let Some(label) = widget.downcast_ref::<gtk::Label>() {
                if label.text() == "release-notes-writer" {
                    name = Some(label.clone());
                }
            }
            let mut child = widget.first_child();
            while let Some(next) = child {
                stack.push(next.clone());
                child = next.next_sibling();
            }
        }
        let name = name.expect("the skill's row is drawn");
        let result = (page.skills.stacked.get(), name.layout().is_ellipsized());
        window.destroy();
        result
    }

    #[test]
    #[ignore = "requires GTK; explicitly run in the isolated omabox session"]
    fn detail_panes_keep_their_width_whatever_they_show() {
        gtk::init().expect("this regression runs inside omabox");
        let _lock = crate::registry::data_dir_test_lock();
        let scratch = std::env::temp_dir().join(format!(
            "toolport-detail_panes_keep_their_width-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&scratch).unwrap();
        let _data = crate::registry::DataDirOverride::set(&scratch);
        let app = adw::Application::builder().build();
        let page = SkillsPage::new(&app);
        for pane in [&page.skills, &page.rules] {
            // Detail content that expands must not make the pane share the list's spare width.
            let wide = gtk::Label::new(Some("~/.agents/skills/find-skills/SKILL.md"));
            wide.set_hexpand(true);
            pane.detail.append(&wide);
            assert!(!pane
                .detail_scroll
                .compute_expand(gtk::Orientation::Horizontal));
            assert!(
                pane.bin.borrow().is_some(),
                "the list measures the page, not itself"
            );
            let agent = regression_agent();
            page.render_head(pane, &[&agent], 1, "SKILL", true);
            for cell in pane.cells.borrow().values().flatten() {
                assert!(
                    !cell.compute_expand(gtk::Orientation::Horizontal),
                    "a header logo must stay over its dots"
                );
            }
        }
        std::fs::remove_dir_all(&scratch).unwrap();
    }

    #[test]
    fn profiles_of_one_client_are_named_by_profile() {
        assert_eq!(
            column_names(&[
                "Claude Code (claude)",
                "Claude Code (claude2)",
                "Codex (codex)",
                "Codex (codex-becca)",
                "Cursor",
                "OpenCode",
                "Command Code",
            ]),
            ["claude", "claude2", "codex", "becca", "Cursor", "OpenCode", "Command"]
        );
        // One profile keeps the client's name.
        assert_eq!(
            column_names(&["Claude Code (claude)", "Codex (codex-work)", "Copilot CLI"]),
            ["Claude", "Codex", "Copilot"]
        );
        assert_eq!(
            column_names(&["Codex (codex-second-account)", "Codex (codex)"]),
            ["second-…", "codex"]
        );
    }

    #[test]
    #[ignore = "requires GTK; explicitly run in the isolated omabox session"]
    fn menu_then_switch_then_switch_restores_latest_control() {
        gtk::init().expect("this regression runs inside omabox");
        let _lock = crate::registry::data_dir_test_lock();
        let scratch = std::env::temp_dir().join(format!(
            "toolport-menu_then_switch_then_switch_restores_latest_control-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&scratch).unwrap();
        let _data = crate::registry::DataDirOverride::set(&scratch);
        let app = adw::Application::builder().build();
        let page = SkillsPage::new(&app);
        let agent = regression_agent();
        let skill: Skill = serde_json::from_value(serde_json::json!({
            "key": "synthetic", "name": "synthetic", "description": "", "descriptionChars": 0,
            "path": "", "folder": "", "source": "yours", "sourceNote": "", "editable": false,
            "sizeBytes": 0, "lines": 0, "modifiedSecs": null, "gitRepo": null,
            "links": [], "states": {}, "preview": ""
        }))
        .unwrap();
        let writes = Rc::new(RefCell::new(Vec::new()));
        let seen = writes.clone();
        let set: Rc<dyn Fn(Wanted)> = Rc::new(move |wanted| seen.borrow_mut().push(wanted));
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let window = gtk::Window::builder().child(&row).build();
        let menu = page.name_only_menu(&agent, false, set.clone());
        row.append(&menu);
        let switch = page.state_switch(&agent, &skill, true, set.clone());
        row.append(&switch);
        // Exercise the actual menu callback, without writing to any real client config.
        let content = menu
            .popover()
            .unwrap()
            .child()
            .unwrap()
            .downcast::<gtk::Box>()
            .unwrap();
        let check = content
            .first_child()
            .unwrap()
            .next_sibling()
            .unwrap()
            .downcast::<gtk::CheckButton>()
            .unwrap();
        check.set_active(true);
        assert_eq!(
            page.focused_choice.borrow().as_deref(),
            Some("claude-code#menu")
        );
        page.restore_agent_control(&agent.id);
        switch.grab_focus();
        // Each write rebuilds the switch, as render_skill_detail does after a scan.
        let mut current = switch;
        for active in [false, true] {
            current.emit_by_name::<bool>("state-set", &[&active]);
            assert_eq!(
                page.focused_choice.borrow().as_deref(),
                Some(agent.id.as_str())
            );
            row.remove(&current);
            current = page.state_switch(&agent, &skill, active, set.clone());
            row.append(&current);
            page.restore_agent_control(&agent.id);
            assert!(
                gtk::prelude::RootExt::focus(&window).as_ref() == Some(current.upcast_ref()),
                "redraw must restore the switch, not the old menu"
            );
            assert!(
                !menu.is_active(),
                "the second activation must not open the menu"
            );
        }
        assert_eq!(
            *writes.borrow(),
            vec![Wanted::NameOnly, Wanted::Off, Wanted::On]
        );
        window.close();
        std::fs::remove_dir_all(&scratch).unwrap();
    }

    #[test]
    #[ignore = "requires GTK; explicitly run in the isolated omabox session"]
    fn agent_filter_redraw_does_not_requeue_selection() {
        gtk::init().expect("this regression runs inside omabox");
        let names = vec![
            "All agents".to_string(),
            "Claude".to_string(),
            "Codex".to_string(),
        ];
        let filter = gtk::DropDown::from_strings(&[]);
        sync_agent_filter(&filter, &names, 0);
        let original = filter.model().unwrap();
        let calls = Rc::new(Cell::new(0));
        let seen = calls.clone();
        filter.connect_selected_notify(move |filter| {
            seen.set(seen.get() + 1);
            // Bound the failure case instead of letting a regression hang the test runner.
            if seen.get() < 4 {
                sync_agent_filter(filter, &names, filter.selected());
            }
        });
        filter.set_selected(2);
        let context = gtk::glib::MainContext::default();
        while context.pending() {
            context.iteration(false);
        }
        assert_eq!(filter.selected(), 2);
        assert_eq!(filter.model().unwrap(), original);
        assert_eq!(
            calls.get(),
            1,
            "one user selection must produce one notification"
        );
    }
}
