//! A row of widgets that wraps onto more lines instead of squeezing them.
//!
//! Each child gets its natural width on the current line; a child that does not fit
//! starts the next line. Only a child that alone is wider than the whole line is
//! narrowed (a label then ellipsizes), so the first child, such as a row's name, is
//! never squeezed by the chips after it. libadwaita's `WrapBox` needs 1.7; this keeps
//! the shell on the libadwaita it already requires.

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

/// Where each child goes: `(child index, line, x, width)`, for children of the given
/// `(minimum, natural)` widths placed in `width` with `spacing` between neighbours.
pub(super) fn wrap_layout(
    sizes: &[(i32, i32)],
    spacing: i32,
    width: i32,
) -> Vec<(usize, usize, i32, i32)> {
    let mut placed = Vec::with_capacity(sizes.len());
    let mut line = 0;
    let mut x = 0;
    let mut line_empty = true;
    for (index, &(min, nat)) in sizes.iter().enumerate() {
        let child_width = nat.min(width).max(min);
        if !line_empty && x + spacing + child_width > width {
            line += 1;
            x = 0;
            line_empty = true;
        }
        if !line_empty {
            x += spacing;
        }
        placed.push((index, line, x, child_width));
        x += child_width;
        line_empty = false;
    }
    placed
}

mod imp {
    use super::*;
    use std::cell::Cell;

    /// A child with its line, x and width.
    type Placed = (gtk::Widget, usize, i32, i32);

    #[derive(Default)]
    pub struct WrapBox {
        pub(super) spacing: Cell<i32>,
        pub(super) row_spacing: Cell<i32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for WrapBox {
        const NAME: &'static str = "ToolportWrapBox";
        type Type = super::WrapBox;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for WrapBox {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WrapBox {
        fn children(&self) -> Vec<gtk::Widget> {
            let mut children = Vec::new();
            let mut child = self.obj().first_child();
            while let Some(widget) = child {
                child = widget.next_sibling();
                if widget.should_layout() {
                    children.push(widget);
                }
            }
            children
        }

        /// The children with their place, and the height of every line.
        fn layout(&self, width: i32) -> (Vec<Placed>, Vec<i32>) {
            let children = self.children();
            let sizes = children
                .iter()
                .map(|child| {
                    let (min, nat, _, _) = child.measure(gtk::Orientation::Horizontal, -1);
                    (min, nat)
                })
                .collect::<Vec<_>>();
            let width = if width < 0 {
                sizes.iter().map(|(_, nat)| nat).sum::<i32>()
                    + self.spacing.get() * (sizes.len() as i32 - 1).max(0)
            } else {
                width
            };
            let mut heights = Vec::new();
            let placed = wrap_layout(&sizes, self.spacing.get(), width)
                .into_iter()
                .map(|(index, line, x, child_width)| {
                    let child = children[index].clone();
                    let (_, height, _, _) = child.measure(gtk::Orientation::Vertical, child_width);
                    if heights.len() <= line {
                        heights.push(0);
                    }
                    heights[line] = heights[line].max(height);
                    (child, line, x, child_width)
                })
                .collect();
            (placed, heights)
        }

        fn total_height(&self, heights: &[i32]) -> i32 {
            heights.iter().sum::<i32>() + self.row_spacing.get() * (heights.len() as i32 - 1).max(0)
        }
    }

    impl WidgetImpl for WrapBox {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            if orientation == gtk::Orientation::Horizontal {
                let children = self.children();
                let mut min = 0;
                let mut nat = 0;
                for child in &children {
                    let (child_min, child_nat, _, _) =
                        child.measure(gtk::Orientation::Horizontal, -1);
                    min = min.max(child_min);
                    nat += child_nat;
                }
                nat += self.spacing.get() * (children.len() as i32 - 1).max(0);
                return (min, nat.max(min), -1, -1);
            }
            let (_, heights) = self.layout(for_size);
            let height = self.total_height(&heights);
            (height, height, -1, -1)
        }

        fn size_allocate(&self, width: i32, _height: i32, _baseline: i32) {
            let (placed, heights) = self.layout(width);
            let mut tops = Vec::with_capacity(heights.len());
            let mut top = 0;
            for height in &heights {
                tops.push(top);
                top += height + self.row_spacing.get();
            }
            for (child, line, x, child_width) in placed {
                child.size_allocate(
                    &gtk::Allocation::new(x, tops[line], child_width, heights[line]),
                    -1,
                );
            }
        }
    }
}

glib::wrapper! {
    /// Lays out its children left to right, wrapping to a new line when the next one
    /// does not fit at its natural width.
    pub struct WrapBox(ObjectSubclass<imp::WrapBox>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl WrapBox {
    pub(super) fn new(spacing: i32, row_spacing: i32) -> Self {
        let wrap: Self = glib::Object::new();
        wrap.imp().spacing.set(spacing);
        wrap.imp().row_spacing.set(row_spacing);
        wrap
    }

    pub(super) fn append(&self, child: &impl IsA<gtk::Widget>) {
        child.set_parent(self);
        self.queue_resize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn everything_shares_one_line_when_it_fits() {
        assert_eq!(
            wrap_layout(&[(10, 120), (10, 80), (10, 90)], 8, 400),
            [(0, 0, 0, 120), (1, 0, 128, 80), (2, 0, 216, 90)]
        );
    }

    #[test]
    fn a_chip_that_does_not_fit_moves_to_the_next_line_and_the_name_keeps_its_width() {
        // "~/.claude_acct_07", "No claude.ai sign-in", "Used by T3 Code: Claude Work"
        // in a 250 px title line.
        assert_eq!(
            wrap_layout(&[(12, 160), (12, 140), (12, 200)], 8, 250),
            [(0, 0, 0, 160), (1, 1, 0, 140), (2, 2, 0, 200)]
        );
        assert_eq!(
            wrap_layout(&[(12, 100), (12, 60), (12, 120)], 8, 250),
            [(0, 0, 0, 100), (1, 0, 108, 60), (2, 1, 0, 120)]
        );
    }

    #[test]
    fn only_a_child_wider_than_the_whole_line_is_narrowed() {
        assert_eq!(
            wrap_layout(&[(12, 100), (12, 400)], 8, 250),
            [(0, 0, 0, 100), (1, 1, 0, 250)]
        );
        // Never below its own minimum.
        assert_eq!(wrap_layout(&[(40, 400)], 8, 20), [(0, 0, 0, 40)]);
    }

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn chips_wrap_below_the_name_instead_of_squeezing_it() {
        gtk::init().expect("isolated GTK desktop");
        let wrap = WrapBox::new(8, 4);
        let name = gtk::Label::builder()
            .label("~/.claude_acct_07")
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .single_line_mode(true)
            .build();
        let account = gtk::Label::builder()
            .label("No claude.ai sign-in")
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        let used = gtk::Label::builder()
            .label("Used by T3 Code: Claude account number 07 for the long running client work")
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        wrap.append(&name);
        wrap.append(&account);
        wrap.append(&used);
        let natural = |label: &gtk::Label| label.measure(gtk::Orientation::Horizontal, -1).1;
        let width = natural(&name) + 20;
        let (_, one_line, _, _) = wrap.measure(gtk::Orientation::Vertical, 4000);
        let (_, height, _, _) = wrap.measure(gtk::Orientation::Vertical, width);
        assert!(height > one_line * 2, "three lines: {height} vs {one_line}");
        wrap.size_allocate(&gtk::Allocation::new(0, 0, width, height), -1);
        // The name keeps its whole width; only the chip wider than the line is narrowed.
        assert_eq!(name.width(), natural(&name));
        assert!(used.width() <= width);
        assert!(account.allocation().y() > name.allocation().y());
        assert!(used.allocation().y() > account.allocation().y());
    }
}
