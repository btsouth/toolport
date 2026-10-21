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
            if orientation == gtk::Orientation::Horizontal {
                let gap = if actions_nat > 0 { COLUMN_GAP } else { 0 };
                return (
                    title_min.max(actions_min),
                    title_nat + gap + actions_nat,
                    -1,
                    -1,
                );
            }
            let (_, actions_height) = action_height(&actions, -1);
            if for_size < 0 || fits_one_row(title_nat, actions_nat, for_size) {
                let title_width = if for_size < 0 {
                    -1
                } else {
                    (for_size - row_gap(actions_nat) - actions_nat).max(title_min)
                };
                let (min, nat, _, _) = title.measure(gtk::Orientation::Vertical, title_width);
                return (min.max(actions_height), nat.max(actions_height), -1, -1);
            }
            let (min, nat, _, _) = title.measure(gtk::Orientation::Vertical, for_size);
            let (_, actions_height) = action_height(&actions, for_size.min(actions_nat));
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
            if fits_one_row(title_nat, actions_nat, width) {
                let title_width = (width - row_gap(actions_nat) - actions_nat).max(title_min);
                title.size_allocate(&gtk::Allocation::new(0, 0, title_width, height), -1);
                if actions_nat > 0 {
                    actions.size_allocate(
                        &gtk::Allocation::new(width - actions_nat, 0, actions_nat, height),
                        -1,
                    );
                }
                return;
            }
            let (_, title_height, _, _) = title.measure(gtk::Orientation::Vertical, width);
            title.size_allocate(&gtk::Allocation::new(0, 0, width, title_height), -1);
            let actions_width = width.min(actions_nat).max(actions_min);
            let top = title_height + ROW_GAP;
            actions.size_allocate(
                &gtk::Allocation::new(0, top, actions_width, (height - top).max(0)),
                -1,
            );
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
    actions: gtk::Box,
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

        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
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
