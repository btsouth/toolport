//! Keep tooltips out of the way while one of Toolport's menus is open.
//!
//! GTK 4 moves the current tooltip to whichever surface delivered the latest
//! pointer, key or crossing event (`gtk_tooltip_set_surface`). When a tooltip
//! from the main window is showing and the open menu then gets an event, for
//! example the crossing GTK delivers as the menu closes, GTK maps that tooltip
//! again as a new popup on the menu's surface. Hyprland never configures that
//! popup, and GTK waits for the configure on the main thread, so the whole app
//! stops responding (D01).
//!
//! [`guard_menu`] runs before GTK presents the menu: `GtkMenuButton` calls its
//! create-popup function from `toggled`, ahead of `gtk_popover_popup`, for
//! clicks, keys, `popup()` and the accessibility toggle action alike (GTK 4.10
//! through 4.24). It turns `has-tooltip` off in every window, which also stops
//! custom query handlers since GTK only queries widgets that have it, and asks
//! GTK to re-check the pointer, which hides the tooltip showing. Tooltips inside
//! the menu come back once the pointer really moves inside it. When the menu
//! unmaps, closed or torn down, each widget gets back the tooltip state it
//! wants now, including changes made while the menu was open. Tooltip text and
//! accessible descriptions are never changed.

use gtk::glib;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

struct Muted {
    widget: glib::WeakRef<gtk::Widget>,
    handler: glib::SignalHandlerId,
    /// Whether the widget wants its tooltip on, following changes made while
    /// muted.
    wanted: Rc<Cell<bool>>,
    /// The tooltip comes from a query handler rather than text or markup.
    custom: bool,
}

#[derive(Default)]
struct Quiet {
    /// Guarded menus open now.
    open: Vec<glib::WeakRef<gtk::Popover>>,
    /// The open menu the pointer moved inside, whose own tooltips are back.
    awake: Option<glib::WeakRef<gtk::Popover>>,
    muted: HashMap<usize, Muted>,
    /// Motion watchers on windows and open menus.
    watchers: Vec<(glib::WeakRef<gtk::Widget>, gtk::EventControllerMotion)>,
}

thread_local! {
    static QUIET: RefCell<Quiet> = RefCell::new(Quiet::default());
    /// Set while this module itself turns a tooltip off.
    static MUTING: Cell<bool> = const { Cell::new(false) };
}

/// Hold tooltips back while this menu's popover is open.
pub(super) fn guard_menu(menu: &gtk::MenuButton) {
    let watched = Rc::new(RefCell::new(None::<glib::WeakRef<gtk::Popover>>));
    menu.set_create_popup_func(move |menu| {
        let Some(popover) = menu.popover() else {
            return;
        };
        let known = watched
            .borrow()
            .as_ref()
            .is_some_and(|weak| same(weak, &popover));
        if !known {
            // Unmap covers closing, an unparented menu and a hidden window;
            // `closed` alone misses the last two.
            popover.connect_unmap(menu_closed);
            *watched.borrow_mut() = Some(popover.downgrade());
        }
        menu_opening(&popover);
    });
}

fn same<T: IsA<glib::Object>>(weak: &glib::WeakRef<T>, other: &T) -> bool {
    weak.upgrade().is_some_and(|value| &value == other)
}

fn key(widget: &gtk::Widget) -> usize {
    glib::translate::ToGlibPtr::<*mut gtk::ffi::GtkWidget>::to_glib_none(widget).0 as usize
}

/// About to present a menu: silence every window and hide the tooltip showing.
fn menu_opening(popover: &gtk::Popover) {
    let first = QUIET.with(|quiet| {
        let mut quiet = quiet.borrow_mut();
        quiet
            .open
            .retain(|weak| weak.upgrade().is_some_and(|open| &open != popover));
        let first = quiet.open.is_empty();
        quiet.open.push(popover.downgrade());
        quiet.awake = None;
        first
    });
    let windows = gtk::Window::list_toplevels();
    for window in &windows {
        mute_tree(window);
    }
    if first {
        for window in &windows {
            // A widget that gains a tooltip behind the menu, for example a row
            // rebuilt by a refresh, is muted before its hover delay ends.
            watch(window, |controller, x, y| {
                if let Some(window) = controller.widget() {
                    let mut target = window.pick(x, y, gtk::PickFlags::INSENSITIVE);
                    while let Some(widget) = target {
                        mute(&widget);
                        target = widget.parent();
                    }
                }
            });
        }
    }
    let menu = popover.downgrade();
    watch(popover.upcast_ref(), move |_, _, _| {
        if let Some(popover) = menu.upgrade() {
            wake(&popover);
        }
    });
    // The menu is not presented yet. With every tooltip off, a query at the
    // pointer finds none, so GTK hides the one showing.
    for window in &windows {
        window.trigger_tooltip_query();
    }
}

/// The pointer moved inside the open menu: its own tooltips work again. Only
/// the newest open menu can wake.
fn wake(popover: &gtk::Popover) {
    let woke = QUIET.with(|quiet| {
        let mut quiet = quiet.borrow_mut();
        let newest = quiet.open.last().is_some_and(|weak| same(weak, popover));
        let already = quiet.awake.as_ref().is_some_and(|weak| same(weak, popover));
        if !newest || already {
            return false;
        }
        quiet.awake = Some(popover.downgrade());
        true
    });
    if woke {
        restore_where(|widget| inside(widget, popover));
    }
}

fn menu_closed(popover: &gtk::Popover) {
    let last = QUIET.with(|quiet| {
        let mut quiet = quiet.borrow_mut();
        let before = quiet.open.len();
        quiet
            .open
            .retain(|weak| weak.upgrade().is_some_and(|open| &open != popover));
        if before == quiet.open.len() {
            // Not one of ours, or already handled.
            return false;
        }
        if quiet.awake.as_ref().is_some_and(|weak| same(weak, popover)) {
            quiet.awake = None;
        }
        quiet.open.is_empty()
    });
    unwatch(|widget| widget == popover.upcast_ref::<gtk::Widget>());
    if last {
        unwatch(|_| true);
        restore_where(|_| true);
    }
}

fn inside(widget: &gtk::Widget, popover: &gtk::Popover) -> bool {
    widget == popover.upcast_ref::<gtk::Widget>() || widget.is_ancestor(popover)
}

fn in_awake_menu(widget: &gtk::Widget) -> bool {
    QUIET
        .with(|quiet| {
            quiet
                .borrow()
                .awake
                .as_ref()
                .and_then(glib::WeakRef::upgrade)
        })
        .is_some_and(|awake| inside(widget, &awake))
}

/// GTK's tooltip popup. `gtk_tooltip_window_set_relative_to` parents it to the
/// native it shows on (a window or an open popover) as an ordinary child, from
/// GTK 4.10 through 4.24, so it is found by walking the widget tree.
const TOOLTIP_WINDOW: &str = "GtkTooltipWindow";

fn mute_tree(widget: &gtk::Widget) {
    if widget.type_().name() == TOOLTIP_WINDOW {
        // Hide a tooltip that is showing now, wherever it is, before the menu
        // is presented. GTK treats a hidden tooltip window as no tooltip
        // (`GTK_TOOLTIP_VISIBLE`), so a later event on the menu cannot map it
        // again there. A pointer query only reaches the window under the
        // pointer, so it cannot hide one shown on another open popover.
        widget.set_visible(false);
        return;
    }
    mute(widget);
    let mut child = widget.first_child();
    while let Some(current) = child {
        mute_tree(&current);
        child = current.next_sibling();
    }
}

fn set_tooltip_off(widget: &gtk::Widget) {
    MUTING.with(|muting| muting.set(true));
    widget.set_has_tooltip(false);
    MUTING.with(|muting| muting.set(false));
}

fn has_text(widget: &gtk::Widget) -> bool {
    widget.tooltip_text().is_some() || widget.tooltip_markup().is_some()
}

/// Turn one widget's tooltip off and remember what it wants afterwards.
fn mute(widget: &gtk::Widget) {
    if !widget.has_tooltip() || in_awake_menu(widget) {
        return;
    }
    let key = key(widget);
    let held = QUIET.with(|quiet| {
        quiet
            .borrow()
            .muted
            .get(&key)
            .is_some_and(|muted| same(&muted.widget, widget))
    });
    if held {
        set_tooltip_off(widget);
        return;
    }
    let custom = !has_text(widget);
    let wanted = Rc::new(Cell::new(true));
    let follow = wanted.clone();
    let handler = widget.connect_notify_local(None, move |widget, spec| {
        if MUTING.with(Cell::get) {
            return;
        }
        match spec.name() {
            // `set_tooltip_text` and friends turn it on for new text; keep it
            // off for now and remember it is wanted. A notify reading "off" is
            // our own change: GTK can deliver it after `MUTING` is reset, when
            // the widget's notifications were frozen, so it says nothing about
            // what the widget wants.
            "has-tooltip" if widget.has_tooltip() => {
                follow.set(true);
                set_tooltip_off(widget);
            }
            // Clearing the text leaves `has-tooltip` off without a notify.
            "tooltip-text" | "tooltip-markup" if !custom && !has_text(widget) => {
                follow.set(false);
            }
            _ => {}
        }
    });
    set_tooltip_off(widget);
    QUIET.with(|quiet| {
        quiet.borrow_mut().muted.insert(
            key,
            Muted {
                widget: widget.downgrade(),
                handler,
                wanted,
                custom,
            },
        )
    });
}

fn restore_where(matches: impl Fn(&gtk::Widget) -> bool) {
    let mut restore = Vec::new();
    QUIET.with(|quiet| {
        let mut quiet = quiet.borrow_mut();
        let keys: Vec<usize> = quiet
            .muted
            .iter()
            .filter(|(_, muted)| muted.widget.upgrade().is_none_or(|widget| matches(&widget)))
            .map(|(key, _)| *key)
            .collect();
        for key in keys {
            if let Some(muted) = quiet.muted.remove(&key) {
                restore.push(muted);
            }
        }
    });
    // Outside the borrow: turning a tooltip on notifies GTK.
    for muted in restore {
        let Some(widget) = muted.widget.upgrade() else {
            continue;
        };
        widget.disconnect(muted.handler);
        if muted.wanted.get() && (muted.custom || has_text(&widget)) {
            widget.set_has_tooltip(true);
        }
    }
}

fn watch(
    widget: &gtk::Widget,
    on_motion: impl Fn(&gtk::EventControllerMotion, f64, f64) + 'static,
) {
    let controller = gtk::EventControllerMotion::new();
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    controller.connect_motion(on_motion);
    widget.add_controller(controller.clone());
    QUIET.with(|quiet| {
        quiet
            .borrow_mut()
            .watchers
            .push((widget.downgrade(), controller))
    });
}

fn unwatch(matches: impl Fn(&gtk::Widget) -> bool) {
    let mut remove = Vec::new();
    QUIET.with(|quiet| {
        let mut quiet = quiet.borrow_mut();
        let mut kept = Vec::new();
        for (weak, controller) in quiet.watchers.drain(..) {
            match weak.upgrade() {
                Some(widget) if matches(&widget) => remove.push((widget, controller)),
                Some(_) => kept.push((weak, controller)),
                None => {}
            }
        }
        quiet.watchers = kept;
    });
    for (widget, controller) in remove {
        widget.remove_controller(&controller);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every menu button in the shell must be guarded: one unguarded menu
    /// opened over a showing tooltip is enough to hang the app (D01). This
    /// reads the shell's source, so it runs without a desktop.
    #[test]
    fn every_menu_button_in_the_shell_is_guarded() {
        const CONSTRUCTORS: [&str; 5] = [
            "MenuButton::builder()",
            "MenuButton::new()",
            "MenuButton::with_",
            "MenuButton::default()",
            "Object::builder::<gtk::MenuButton>()",
        ];
        fn sources(dir: &std::path::Path, found: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    sources(&path, found);
                } else if path.extension().is_some_and(|ext| ext == "rs")
                    && !path.ends_with("quiet_tooltips.rs")
                {
                    found.push(path);
                }
            }
        }
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/linux_native");
        let mut files = Vec::new();
        sources(&dir, &mut files);
        let mut checked = 0;
        let mut missing = Vec::new();
        for path in files {
            let file = path.strip_prefix(&dir).unwrap().display().to_string();
            // Comments neither build menus nor guard them.
            let source: String = std::fs::read_to_string(&path)
                .unwrap()
                .lines()
                .map(|line| {
                    if line.trim_start().starts_with("//") {
                        ""
                    } else {
                        line
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            let mut builds: Vec<usize> = CONSTRUCTORS
                .iter()
                .flat_map(|constructor| source.match_indices(constructor).map(|(at, _)| at))
                .collect();
            builds.sort_unstable();
            for (index, &at) in builds.iter().enumerate() {
                checked += 1;
                let line_no = source[..at].matches('\n').count() + 1;
                let line_start = source[..at].rfind('\n').map_or(0, |i| i + 1);
                let Some(name) = source[line_start..at]
                    .trim_start()
                    .strip_prefix("let ")
                    .and_then(|binding| binding.split_once(" = "))
                    .map(|(name, _)| name.trim_start_matches("mut ").trim())
                else {
                    missing.push(format!("{file}:{line_no}: bind the menu button to a name"));
                    continue;
                };
                // The guard belongs after this menu is built, in the same
                // function and before the next menu button.
                let body = &source[at..];
                let end = ["\nfn ", "\n    fn ", "\npub", "\n    pub"]
                    .iter()
                    .filter_map(|next| body.find(next))
                    .chain(builds.get(index + 1).map(|next| next - at))
                    .min()
                    .unwrap_or(body.len());
                if !body[..end].contains(&format!("guard_menu(&{name})")) {
                    missing.push(format!("{file}:{line_no}: {name} has no guard_menu"));
                }
            }
        }
        assert!(checked > 0, "found no menu buttons to check in {dir:?}");
        assert!(
            missing.is_empty(),
            "unguarded menu buttons:\n{}",
            missing.join("\n")
        );
    }

    fn pump_until(done: impl Fn() -> bool) {
        let context = glib::MainContext::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !done() && std::time::Instant::now() < deadline {
            context.iteration(false);
        }
    }

    fn is_quiet() -> bool {
        QUIET.with(|quiet| !quiet.borrow().open.is_empty())
    }

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn a_guarded_menu_silences_tooltips_before_it_is_presented() {
        adw::init().unwrap();
        let row = gtk::Button::with_label("Row");
        row.set_tooltip_text(Some("Row details"));
        let cleared = gtk::Button::with_label("Cleared");
        cleared.set_tooltip_text(Some("Goes away while open"));
        let plain = gtk::Button::with_label("No tooltip");
        let queried = Rc::new(Cell::new(0));
        let custom = gtk::Label::new(Some("Custom"));
        custom.set_has_tooltip(true);
        let count = queried.clone();
        // Connected before the guard exists: it still cannot run, because GTK
        // only queries widgets with `has-tooltip`.
        custom.connect_query_tooltip(move |_, _, _, _, tooltip| {
            count.set(count.get() + 1);
            tooltip.set_text(Some("From a query handler"));
            true
        });
        let item = gtk::Button::with_label("Disabled item");
        item.set_sensitive(false);
        item.set_tooltip_text(Some("Why it is off"));
        let popover = gtk::Popover::builder().child(&item).build();
        let menu = gtk::MenuButton::builder()
            .label("Actions")
            .popover(&popover)
            .build();
        menu.set_tooltip_text(Some("Server actions"));
        guard_menu(&menu);
        // Proof the guard runs before GTK presents the menu: by the time the
        // popover realizes (it has no surface yet), tooltips are already off.
        let off_at_realize = Rc::new(Cell::new(None));
        let seen = off_at_realize.clone();
        let row_for_realize = row.clone();
        popover.connect_realize(move |_| seen.set(Some(!row_for_realize.has_tooltip())));
        let column = gtk::Box::new(gtk::Orientation::Vertical, 6);
        for widget in [
            row.upcast_ref::<gtk::Widget>(),
            cleared.upcast_ref(),
            plain.upcast_ref(),
            custom.upcast_ref(),
            menu.upcast_ref(),
        ] {
            column.append(widget);
        }
        let window = gtk::Window::builder().child(&column).build();
        window.present();
        pump_until(|| window.is_mapped() && menu.width() > 0);

        // The accessibility toggle action takes this same path.
        menu.popup();
        pump_until(|| popover.is_mapped());
        assert!(popover.is_mapped());
        assert!(is_quiet());
        assert_eq!(off_at_realize.get(), Some(true));
        for widget in [
            row.upcast_ref::<gtk::Widget>(),
            cleared.upcast_ref(),
            custom.upcast_ref(),
            menu.upcast_ref(),
            item.upcast_ref(),
        ] {
            assert!(
                !widget.has_tooltip(),
                "{widget:?} must be off while the menu is open"
            );
        }
        // Changes while the menu is open are kept, but stay off for now.
        row.set_tooltip_text(Some("Row details, updated"));
        assert!(!row.has_tooltip());
        cleared.set_tooltip_text(None);
        plain.set_tooltip_text(Some("Gained while open"));
        // Real pointer motion inside the menu brings its own tooltips back.
        wake(&popover);
        assert!(item.has_tooltip());
        assert!(!row.has_tooltip());

        menu.popdown();
        pump_until(|| !popover.is_visible());
        assert!(!is_quiet());
        assert!(row.has_tooltip());
        assert_eq!(row.tooltip_text().as_deref(), Some("Row details, updated"));
        assert!(
            !cleared.has_tooltip() && cleared.tooltip_text().is_none(),
            "a tooltip cleared while the menu was open must stay cleared"
        );
        assert!(
            plain.has_tooltip(),
            "a tooltip added while open shows after"
        );
        assert!(custom.has_tooltip(), "custom query tooltips come back");
        assert_eq!(
            queried.get(),
            0,
            "no query reached the custom handler while open"
        );
        assert!(menu.has_tooltip() && item.has_tooltip());
        assert_eq!(menu.tooltip_text().as_deref(), Some("Server actions"));
        assert_eq!(item.tooltip_text().as_deref(), Some("Why it is off"));
        // Ordinary tooltip changes behave normally again.
        row.set_tooltip_text(None);
        assert!(!row.has_tooltip());
        row.set_tooltip_text(Some("Back"));
        assert!(row.has_tooltip());

        // A second open works the same way: the handlers were removed.
        menu.popup();
        pump_until(|| popover.is_mapped());
        assert!(!row.has_tooltip() && !item.has_tooltip());
        // Tearing the menu down while open still gives tooltips back.
        menu.set_popover(None::<&gtk::Popover>);
        pump_until(|| !is_quiet());
        assert!(!is_quiet());
        assert!(row.has_tooltip() && custom.has_tooltip());
    }

    fn visible_tooltip_windows(widget: &gtk::Widget) -> usize {
        let mut count = usize::from(widget.type_().name() == TOOLTIP_WINDOW && widget.is_visible());
        let mut child = widget.first_child();
        while let Some(current) = child {
            count += visible_tooltip_windows(&current);
            child = current.next_sibling();
        }
        count
    }

    /// Needs a real hover: while it waits, a driver moves the box pointer to
    /// the center of the printed row bounds, offset by the window's position
    /// (`gtk-bugs/scripts/hover-quiet-tooltips.sh`).
    #[test]
    #[ignore = "requires an isolated GTK desktop and an external pointer hover; run in omabox"]
    fn a_tooltip_showing_from_a_real_hover_is_hidden_before_a_menu_opens() {
        adw::init().unwrap();
        let row = gtk::Button::builder()
            .label("Row")
            .hexpand(true)
            .vexpand(true)
            .build();
        row.set_tooltip_text(Some("Row details"));
        let item = gtk::Button::with_label("Item");
        let popover = gtk::Popover::builder().child(&item).build();
        let menu = gtk::MenuButton::builder()
            .label("Actions")
            .popover(&popover)
            .build();
        guard_menu(&menu);
        let column = gtk::Box::new(gtk::Orientation::Vertical, 6);
        column.append(&menu);
        column.append(&row);
        let window = gtk::Window::builder()
            .title("quiet-tooltips-hover")
            .child(&column)
            .build();
        window.present();
        pump_until(|| window.is_mapped() && row.width() > 0);
        // The driver aims at the row's center: these bounds plus the window's
        // position from the compositor.
        let bounds = row.compute_bounds(&window).expect("row has bounds");
        println!(
            "READY_FOR_HOVER row={},{},{},{}",
            bounds.x() as i32,
            bounds.y() as i32,
            bounds.width() as i32,
            bounds.height() as i32
        );
        let context = glib::MainContext::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while visible_tooltip_windows(window.upcast_ref()) == 0
            && std::time::Instant::now() < deadline
        {
            context.iteration(false);
        }
        assert_eq!(
            visible_tooltip_windows(window.upcast_ref()),
            1,
            "a real hover must show the row tooltip first"
        );
        println!("TOOLTIP_SHOWN");
        let at_realize = Rc::new(Cell::new(None));
        let seen = at_realize.clone();
        let root = window.clone();
        popover
            .connect_realize(move |_| seen.set(Some(visible_tooltip_windows(root.upcast_ref()))));
        // No input event: the path the accessibility toggle action takes.
        menu.popup();
        pump_until(|| popover.is_mapped());
        assert!(popover.is_mapped(), "the menu opens without hanging");
        assert_eq!(
            at_realize.get(),
            Some(0),
            "the tooltip is hidden before the menu has a surface"
        );
        // Keep the loop running with the pointer still over the row: nothing
        // comes back while the menu is open.
        let settle = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < settle {
            context.iteration(false);
        }
        assert_eq!(visible_tooltip_windows(window.upcast_ref()), 0);
        menu.popdown();
        pump_until(|| !popover.is_visible());
        assert!(!is_quiet());
        assert!(row.has_tooltip());
        println!("CLOSED_AND_RESTORED");
    }
}
