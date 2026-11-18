//! The one page header every native page uses, matching the React `PageHeader`.
//!
//! The header is the page's top bar: the sidebar word as a 22px title, one
//! plain subtitle, then the page's actions in order (search, secondary actions,
//! overflow, refresh) and the window controls. The bar stays the window's drag
//! area. When the title and the actions don't fit on one line, the actions move
//! to their own row under the title instead of squeezing it.

use super::i18n as l10n;
use adw::prelude::*;
use gtk::glib;
use gtk::subclass::prelude::*;

/// Horizontal space between the title and the actions on one line.
const COLUMN_GAP: i32 = 16;
/// Vertical space between the title and the actions once they wrap.
const ROW_GAP: i32 = 12;

mod imp {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    pub struct HeaderWrap {
        pub(super) title: RefCell<Option<gtk::Widget>>,
        pub(super) actions: RefCell<Option<gtk::Widget>>,
        /// When set, the actions stay on the title's line while the title keeps
        /// at least this width, instead of only when it fits at natural width.
        pub(super) min_title: std::cell::Cell<i32>,
        /// When set, wrapped actions start after this widget, as under a logo.
        pub(super) indent_from: RefCell<Option<gtk::Widget>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for HeaderWrap {
        const NAME: &'static str = "ToolportHeaderWrap";
        type Type = super::HeaderWrap;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for HeaderWrap {
        fn dispose(&self) {
            for child in [self.title.take(), self.actions.take()]
                .into_iter()
                .flatten()
            {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for HeaderWrap {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            for child in [self.title.borrow().clone(), self.actions.borrow().clone()]
                .into_iter()
                .flatten()
            {
                if child.should_layout() && child.width() > 0 && child.height() > 0 {
                    self.obj().snapshot_child(&child, snapshot);
                }
            }
        }

        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let (Some(title), Some(actions)) =
                (self.title.borrow().clone(), self.actions.borrow().clone())
            else {
                return (0, 0, -1, -1);
            };
            let (title_min, title_nat, _, _) = title.measure(gtk::Orientation::Horizontal, -1);
            let (actions_min, actions_nat) = action_width(&actions);
            let indent = self.indent();
            if orientation == gtk::Orientation::Horizontal {
                let gap = if actions_nat > 0 { COLUMN_GAP } else { 0 };
                let wrapped_min = if actions_nat > 0 {
                    indent + actions_min
                } else {
                    0
                };
                return (
                    title_min.max(wrapped_min),
                    title_nat.max(self.min_title.get()) + gap + actions_nat,
                    -1,
                    -1,
                );
            }
            let (_, actions_height) = action_height(&actions, -1);
            if for_size < 0 || self.fits(title_min, title_nat, actions_nat, for_size) {
                let title_width = if for_size < 0 {
                    -1
                } else {
                    (for_size - row_gap(actions_nat) - actions_nat).max(title_min)
                };
                let (min, nat, _, _) = title.measure(gtk::Orientation::Vertical, title_width);
                return (min.max(actions_height), nat.max(actions_height), -1, -1);
            }
            let (min, nat, _, _) = title.measure(gtk::Orientation::Vertical, for_size);
            let actions_width = (for_size - indent).min(actions_nat).max(actions_min);
            let (_, actions_height) = action_height(&actions, actions_width);
            (
                min + ROW_GAP + actions_height,
                nat + ROW_GAP + actions_height,
                -1,
                -1,
            )
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let (Some(title), Some(actions)) =
                (self.title.borrow().clone(), self.actions.borrow().clone())
            else {
                return;
            };
            let (title_min, title_nat, _, _) = title.measure(gtk::Orientation::Horizontal, -1);
            let (actions_min, actions_nat) = action_width(&actions);
            // Room the actions can use past their natural width (a label shown
            // whole), only once the line is decided from natural widths.
            let wanted = actions
                .downcast_ref::<super::ActionFlow>()
                .map_or(0, |flow| flow.imp().wanted_extra());
            if self.fits(title_min, title_nat, actions_nat, width) {
                let gap = row_gap(actions_nat);
                // Spare room is what the title can give up without growing
                // taller than it is beside the actions at their natural width.
                let base = (width - gap - actions_nat).max(title_min);
                let extra = if wanted > 0 {
                    let height = |w: i32| title.measure(gtk::Orientation::Vertical, w).1;
                    let base_height = height(base);
                    let keep = title_min.max(self.min_title.get());
                    let (mut lo, mut hi) = (0, wanted.min(base - keep).max(0));
                    while lo < hi {
                        let mid = (lo + hi + 1) / 2;
                        if height(base - mid) <= base_height {
                            lo = mid;
                        } else {
                            hi = mid - 1;
                        }
                    }
                    lo
                } else {
                    0
                };
                let actions_width = actions_nat + extra;
                let title_width = (width - gap - actions_width).max(title_min);
                title.size_allocate(&gtk::Allocation::new(0, 0, title_width, height), -1);
                if actions_nat > 0 {
                    actions.size_allocate(
                        &gtk::Allocation::new(width - actions_width, 0, actions_width, height),
                        -1,
                    );
                }
                return;
            }
            let (_, title_height, _, _) = title.measure(gtk::Orientation::Vertical, width);
            title.size_allocate(&gtk::Allocation::new(0, 0, width, title_height), -1);
            let indent = self.indent();
            let actions_width = (width - indent).min(actions_nat + wanted).max(actions_min);
            let top = title_height + ROW_GAP;
            // Actions that keep to the right edge (server rows, as React's) sit
            // there on their own line too.
            let right = actions
                .downcast_ref::<super::ActionFlow>()
                .is_some_and(|flow| flow.imp().lines_end.get());
            let left = if right {
                (width - actions_width).max(indent)
            } else {
                indent
            };
            actions.size_allocate(
                &gtk::Allocation::new(left, top, actions_width, (height - top).max(0)),
                -1,
            );
        }
    }

    impl HeaderWrap {
        fn fits(&self, title_min: i32, title_nat: i32, actions_nat: i32, width: i32) -> bool {
            match self.min_title.get() {
                0 => fits_one_row(title_nat, actions_nat, width),
                min => fits_one_row(title_min.max(min), actions_nat, width),
            }
        }

        fn indent(&self) -> i32 {
            self.indent_from
                .borrow()
                .as_ref()
                .filter(|widget| widget.should_layout())
                .map_or(0, |widget| {
                    let (_, nat, _, _) = widget.measure(gtk::Orientation::Horizontal, -1);
                    let spacing = widget
                        .parent()
                        .and_then(|parent| parent.downcast::<gtk::Box>().ok())
                        .map_or(0, |parent| parent.spacing());
                    nat + spacing
                })
        }
    }

    fn action_width(actions: &gtk::Widget) -> (i32, i32) {
        if !actions.should_layout() {
            return (0, 0);
        }
        let (min, nat, _, _) = actions.measure(gtk::Orientation::Horizontal, -1);
        (min, nat)
    }

    fn action_height(actions: &gtk::Widget, for_width: i32) -> (i32, i32) {
        if !actions.should_layout() {
            return (0, 0);
        }
        let (min, nat, _, _) = actions.measure(gtk::Orientation::Vertical, for_width);
        (min, nat)
    }

    fn row_gap(actions_nat: i32) -> i32 {
        if actions_nat > 0 {
            COLUMN_GAP
        } else {
            0
        }
    }
}

glib::wrapper! {
    /// Lays out a title and an actions box on one line when both fit at their
    /// natural width, and on two lines (actions left-aligned below) otherwise.
    pub struct HeaderWrap(ObjectSubclass<imp::HeaderWrap>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl HeaderWrap {
    pub(super) fn new(title: &impl IsA<gtk::Widget>, actions: &impl IsA<gtk::Widget>) -> Self {
        let wrap: Self = glib::Object::new();
        title.set_parent(&wrap);
        actions.set_parent(&wrap);
        wrap.imp().title.replace(Some(title.clone().upcast()));
        wrap.imp().actions.replace(Some(actions.clone().upcast()));
        wrap
    }

    /// A list row: the actions share the line while the text keeps `min_title`
    /// pixels, and otherwise move under the text, starting after `indent_from`
    /// (the row's logo), as React's rows do at narrow widths.
    pub(super) fn row(
        title: &impl IsA<gtk::Widget>,
        actions: &impl IsA<gtk::Widget>,
        min_title: i32,
        indent_from: Option<&gtk::Widget>,
    ) -> Self {
        let wrap = Self::new(title, actions);
        wrap.imp().min_title.set(min_title);
        wrap.imp().indent_from.replace(indent_from.cloned());
        wrap
    }
}

mod flow_imp {
    use super::*;

    #[derive(Default)]
    pub struct ActionFlow {
        /// Each line sits at the right edge, as React's row controls do.
        pub(super) lines_end: std::cell::Cell<bool>,
        /// A child that takes room left over on its line, up to what its label
        /// needs to show its whole text (the client access menu).
        pub(super) grow: std::cell::RefCell<Option<(gtk::Widget, gtk::Label)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ActionFlow {
        const NAME: &'static str = "ToolportActionFlow";
        type Type = super::ActionFlow;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for ActionFlow {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for ActionFlow {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let children = self.children();
            if orientation == gtk::Orientation::Horizontal {
                let min = children.iter().map(|(_, min, _)| *min).max().unwrap_or(0);
                let nat = children.iter().map(|(_, _, nat)| *nat).sum::<i32>()
                    + super::ACTION_SPACING * (children.len() as i32 - 1).max(0);
                return (min, nat, -1, -1);
            }
            let lines = lines(&children, for_size, self.lines_end.get());
            let height = lines.iter().map(|line| line.height).sum::<i32>()
                + super::ACTION_SPACING * (lines.len() as i32 - 1).max(0);
            (height, height, -1, -1)
        }

        fn size_allocate(&self, width: i32, _height: i32, _baseline: i32) {
            let children = self.children();
            let grow = self.grow.borrow().clone();
            let wanted = self.wanted_extra();
            let mut top = 0;
            for mut line in lines(&children, width, self.lines_end.get()) {
                let mut used = line.items.iter().map(|(_, w, _)| *w).sum::<i32>()
                    + super::ACTION_SPACING * (line.items.len() as i32 - 1).max(0);
                // Lines are laid out first, then room left on the grow child's
                // line goes to it, so where controls go never changes.
                if let Some((grow_child, _)) = &grow {
                    if let Some(item) = line.items.iter_mut().find(|(c, _, _)| c == grow_child) {
                        let extra = wanted.min(width - used).max(0);
                        item.1 += extra;
                        used += extra;
                    }
                }
                let mut left = 0;
                if self.lines_end.get() {
                    left = (width - used).max(0);
                }
                for (child, child_width, child_height) in line.items {
                    child.size_allocate(
                        &gtk::Allocation::new(
                            left,
                            top + (line.height - child_height) / 2,
                            child_width,
                            child_height,
                        ),
                        -1,
                    );
                    left += child_width + super::ACTION_SPACING;
                }
                top += line.height + super::ACTION_SPACING;
            }
        }
    }

    impl ActionFlow {
        /// How much wider than its natural width the grow child's label needs to
        /// be to show its whole text on one line.
        pub(super) fn wanted_extra(&self) -> i32 {
            let Some((child, label)) = self.grow.borrow().clone() else {
                return 0;
            };
            if !child.should_layout() {
                return 0;
            }
            let whole = label
                .create_pango_layout(Some(&label.text()))
                .pixel_size()
                .0;
            let (_, natural, _, _) = label.measure(gtk::Orientation::Horizontal, -1);
            (whole - natural).max(0)
        }

        /// The visible children with their minimum and natural widths.
        fn children(&self) -> Vec<(gtk::Widget, i32, i32)> {
            let mut out = Vec::new();
            let mut child = self.obj().first_child();
            while let Some(widget) = child {
                child = widget.next_sibling();
                if widget.should_layout() {
                    let (min, nat, _, _) = widget.measure(gtk::Orientation::Horizontal, -1);
                    out.push((widget, min, nat));
                }
            }
            out
        }
    }

    pub(super) struct Line {
        pub(super) items: Vec<(gtk::Widget, i32, i32)>,
        pub(super) height: i32,
    }

    /// Children at natural width, left to right, starting a new line when the
    /// next one would pass `width` (any width when `width` is negative).
    fn lines(children: &[(gtk::Widget, i32, i32)], width: i32, balance: bool) -> Vec<Line> {
        let mut out: Vec<Line> = Vec::new();
        let mut used = 0;
        for (widget, min, nat) in children {
            let child_width = if width < 0 {
                *nat
            } else {
                (*nat).min(width).max(*min)
            };
            let start_new = match out.last() {
                None => true,
                Some(_) => width >= 0 && used + super::ACTION_SPACING + child_width > width,
            };
            if start_new {
                out.push(Line {
                    items: Vec::new(),
                    height: 0,
                });
                used = 0;
            } else {
                used += super::ACTION_SPACING;
            }
            let (_, height, _, _) = widget.measure(gtk::Orientation::Vertical, child_width);
            let line = out.last_mut().expect("a line was just ensured");
            line.items.push((widget.clone(), child_width, height));
            line.height = line.height.max(height);
            used += child_width;
        }
        if balance && width >= 0 && (2..=16).contains(&children.len()) {
            let widths = out
                .iter()
                .flat_map(|line| line.items.iter().map(|(_, w, _)| *w))
                .collect::<Vec<_>>();
            let items = out
                .into_iter()
                .flat_map(|line| line.items)
                .collect::<Vec<_>>();
            let mut items = items.into_iter();
            return super::balanced_lines(&widths, super::ACTION_SPACING, width)
                .into_iter()
                .map(|count| {
                    let items = items.by_ref().take(count).collect::<Vec<_>>();
                    let height = items.iter().map(|(_, _, h)| *h).max().unwrap_or(0);
                    Line { items, height }
                })
                .collect();
        }
        out
    }
}

/// Space between page actions, across and between lines.
const ACTION_SPACING: i32 = 8;

/// How many controls go on each line, in order, so that as few controls as
/// possible sit alone on a line and the last one (a row's switch and menu) is
/// alone only when nothing else fits; then the fewest lines, fullest first.
pub(super) fn balanced_lines(widths: &[i32], spacing: i32, width: i32) -> Vec<usize> {
    let count = widths.len();
    if count <= 1 {
        return vec![count];
    }
    (0u32..(1 << (count - 1)))
        .map(|breaks| {
            let mut lines = vec![1usize];
            for index in 1..count {
                if breaks & (1 << (index - 1)) != 0 {
                    lines.push(1);
                } else {
                    *lines.last_mut().expect("a line") += 1;
                }
            }
            lines
        })
        .filter(|lines| {
            let mut start = 0;
            lines.iter().all(|&len| {
                let used =
                    widths[start..start + len].iter().sum::<i32>() + spacing * (len as i32 - 1);
                start += len;
                len == 1 || used <= width
            })
        })
        .min_by_key(|lines| {
            (
                lines.iter().filter(|&&len| len == 1).count(),
                lines.last() == Some(&1),
                lines.len(),
                std::cmp::Reverse(lines.clone()),
            )
        })
        .unwrap_or_else(|| vec![1; count])
}

glib::wrapper! {
    /// The page actions, left to right at natural width, wrapping onto more lines
    /// when the header is too narrow for them, the way React's header wraps.
    pub struct ActionFlow(ObjectSubclass<flow_imp::ActionFlow>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl ActionFlow {
    pub(super) fn new() -> Self {
        glib::Object::new()
    }

    pub(super) fn append(&self, widget: &impl IsA<gtk::Widget>) {
        widget.insert_before(self, None::<&gtk::Widget>);
    }

    /// Put each line at the right edge instead of the left.
    pub(super) fn set_lines_end(&self, end: bool) {
        self.imp().lines_end.set(end);
        self.queue_allocate();
    }

    /// Let `child` (a direct child) take room left over on its line, up to what
    /// `label` inside it needs to show its whole text. Measuring is unchanged,
    /// so the controls wrap and move at the same widths; only spare room goes
    /// to the label. `child` must pass the room on (an expanding label).
    pub(super) fn set_grow(&self, child: &impl IsA<gtk::Widget>, label: &gtk::Label) {
        self.imp()
            .grow
            .replace(Some((child.clone().upcast(), label.clone())));
        self.queue_allocate();
    }

    /// Give no spare room to the label again, as rows did before `set_grow`.
    #[cfg(test)]
    pub(super) fn clear_grow(&self) {
        self.imp().grow.replace(None);
        self.queue_resize();
    }
}

/// Whether the title and the actions both fit on one line at natural width.
pub(super) fn fits_one_row(title_nat: i32, actions_nat: i32, width: i32) -> bool {
    actions_nat == 0 || title_nat + COLUMN_GAP + actions_nat <= width
}

/// A page header. Append `root` as the first child of the page.
#[derive(Clone)]
pub(super) struct PageHeader {
    pub(super) root: gtk::WindowHandle,
    pub(super) title: gtk::Label,
    pub(super) subtitle: gtk::Label,
    actions: ActionFlow,
    leading: gtk::Box,
}

impl PageHeader {
    /// A header with the sidebar word as its title and one subtitle line.
    pub(super) fn new(title: &'static str, subtitle: &'static str) -> Self {
        let title_label = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["title-2", "toolport-page-title"])
            .accessible_role(gtk::AccessibleRole::Heading)
            .build();
        l10n::bind_label(&title_label, title);
        let subtitle_label = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["toolport-muted", "toolport-page-subtitle"])
            .build();
        l10n::bind_label(&subtitle_label, subtitle);

        let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text.set_valign(gtk::Align::Center);
        text.append(&title_label);
        text.append(&subtitle_label);

        // Back button and logo, when a sub-page has them, sit before the text.
        let leading = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        leading.set_valign(gtk::Align::Center);
        leading.set_visible(false);
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        heading.set_valign(gtk::Align::Center);
        heading.append(&leading);
        heading.append(&text);

        let actions = ActionFlow::new();
        actions.set_valign(gtk::Align::Center);
        actions.add_css_class("toolport-page-actions");
        let wrap = HeaderWrap::new(&heading, &actions);
        wrap.set_hexpand(true);

        // In the collapsed (narrow) layout the sidebar is a page of its own;
        // this button returns to it, as the libadwaita header bar used to.
        let sidebar_back = gtk::Button::builder()
            .icon_name("go-previous-symbolic")
            .valign(gtk::Align::Center)
            .visible(false)
            .css_classes(["flat"])
            .build();
        l10n::bind_tooltip(&sidebar_back, "Back");
        sidebar_back.connect_clicked(|button| {
            if let Some(split) = button.ancestor(adw::NavigationSplitView::static_type()) {
                if let Ok(split) = split.downcast::<adw::NavigationSplitView>() {
                    split.set_show_content(false);
                }
            }
        });
        let controls = gtk::WindowControls::new(gtk::PackType::End);
        controls.set_valign(gtk::Align::Center);

        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        bar.append(&sidebar_back);
        bar.append(&wrap);
        bar.append(&controls);
        let root = gtk::WindowHandle::builder().child(&bar).build();
        root.add_css_class("toolport-header");
        root.add_css_class("toolport-page-header");
        root.connect_realize(move |root| {
            let Some(split) = root
                .ancestor(adw::NavigationSplitView::static_type())
                .and_then(|split| split.downcast::<adw::NavigationSplitView>().ok())
            else {
                return;
            };
            split
                .bind_property("collapsed", &sidebar_back, "visible")
                .sync_create()
                .build();
        });

        Self {
            root,
            title: title_label,
            subtitle: subtitle_label,
            actions,
            leading,
        }
    }

    /// Adds an action at the end. Add them in the order search, secondary
    /// actions, overflow menu, refresh.
    pub(super) fn add_action(&self, widget: &impl IsA<gtk::Widget>) {
        self.actions.append(widget);
    }

    pub(super) fn add_leading(&self, widget: &impl IsA<gtk::Widget>) {
        self.leading.append(widget);
        self.leading.set_visible(true);
    }

    /// Replaces the title with another source message, such as Sync or Team.
    pub(super) fn set_title(&self, source: &'static str) {
        l10n::bind_label(&self.title, source);
    }

    /// Replaces the subtitle with another source message.
    pub(super) fn set_subtitle(&self, source: &'static str) {
        l10n::bind_label(&self.subtitle, source);
    }

    /// Adds a "← label" button before the title for a sub-page, such as
    /// Catalog under Servers, followed by a divider.
    pub(super) fn set_back(&self, label: &'static str, tooltip: &'static str) -> gtk::Button {
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        content.append(
            &gtk::Image::builder()
                .icon_name("go-previous-symbolic")
                .pixel_size(16)
                .build(),
        );
        content.append(&l10n::label(label));
        let back = gtk::Button::builder()
            .child(&content)
            .valign(gtk::Align::Center)
            .css_classes(["flat", "toolport-page-back"])
            .build();
        l10n::bind_tooltip(&back, tooltip);
        self.leading.append(&back);
        let divider = gtk::Separator::new(gtk::Orientation::Vertical);
        divider.add_css_class("toolport-page-back-divider");
        self.leading.append(&divider);
        self.leading.set_visible(true);
        back
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_controls_split_so_no_control_is_left_alone_when_another_split_exists() {
        // CI's German "Service key required" row in a 480 px window, as laid out
        // there (border boxes): chip, Edit service key, View log, then the
        // switch and menu, in 404 px. The chip cannot share its line, and the
        // switch and menu go with View log instead of alone on a third line.
        assert_eq!(
            super::balanced_lines(&[191, 239, 156, 88], 8, 404),
            [1, 1, 2]
        );
        // When the chip and the first action fit together, they share.
        assert_eq!(super::balanced_lines(&[191, 205, 156, 88], 8, 404), [2, 2]);
        // English at 480: chip and Sign in, then View log with the switch.
        assert_eq!(super::balanced_lines(&[105, 70, 80, 88], 8, 338), [2, 2]);
        // Everything fits: one line.
        assert_eq!(super::balanced_lines(&[105, 88], 8, 404), [2]);
        assert_eq!(super::balanced_lines(&[105], 8, 404), [1]);
    }

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn header_preserves_actions_and_wraps_at_narrow_width() {
        adw::init().expect("isolated GTK desktop");
        let header = PageHeader::new("Clients", "Manage Toolport in your installed AI tools");
        let add = super::super::header_button("list-add-symbolic", "Add client location");
        let import = super::super::header_button("document-open-symbolic", "Import");
        header.add_action(&add);
        header.add_action(&import);
        assert_eq!(header.title.label(), "Clients");
        assert_eq!(
            header.subtitle.label(),
            "Manage Toolport in your installed AI tools"
        );
        assert_eq!(header.actions.first_child(), Some(add.clone().upcast()));
        assert_eq!(add.next_sibling(), Some(import.upcast()));
        let bar = header.root.child().unwrap();
        let wrap = bar.first_child().unwrap().next_sibling().unwrap();
        let (_, wide, _, _) = wrap.measure(gtk::Orientation::Vertical, 1100);
        let (_, narrow, _, _) = wrap.measure(gtk::Orientation::Vertical, 420);
        assert!(narrow > wide, "actions should wrap below the title");
        // Too narrow for both actions on one line, they wrap onto a second one
        // instead of running past the window edge.
        let (min, nat, _, _) = header.actions.measure(gtk::Orientation::Horizontal, -1);
        assert!(
            min < nat,
            "actions can be narrower than one line: {min} {nat}"
        );
        let (_, one_line, _, _) = header.actions.measure(gtk::Orientation::Vertical, nat);
        let (_, two_lines, _, _) = header.actions.measure(gtk::Orientation::Vertical, min);
        assert!(two_lines > one_line, "actions wrap: {two_lines} {one_line}");
        let (wrap_min, _, _, _) = wrap.measure(gtk::Orientation::Horizontal, -1);
        assert!(
            wrap_min < nat,
            "the header fits narrower than its actions' one line"
        );
        header.set_title("Catalog");
        header.set_subtitle("Add MCP servers from the registry");
        let back = header.set_back("Servers", "Back to servers");
        assert_eq!(header.title.label(), "Catalog");
        assert_eq!(back.tooltip_text().as_deref(), Some("Back to servers"));
    }

    #[test]
    fn actions_share_the_title_line_only_when_both_fit_at_natural_width() {
        assert!(fits_one_row(300, 400, 716));
        assert!(!fits_one_row(300, 400, 715));
        // With no actions the title always has the line to itself.
        assert!(fits_one_row(900, 0, 200));
    }
}
