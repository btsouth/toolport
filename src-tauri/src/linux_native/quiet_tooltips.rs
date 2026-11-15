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
//!
//! Popovers no guarded menu button owns, a dropdown's list, a split button's
//! menu or a suggestion list, use [`guard_popover`] instead. It silences the
//! same way when the popover maps: after GTK presents it, before it handles
//! any event that could move a tooltip onto it.

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

/// Hold tooltips back while a popover that no guarded menu button owns is
/// open: a dropdown's list, a split button's menu, a suggestion list. `map`
/// runs once GTK has presented the popover and before it handles any event on
/// it, which is when a showing tooltip would be moved onto it (D01).
pub(super) fn guard_popover(popover: &impl IsA<gtk::Popover>) {
    let popover = popover.upcast_ref::<gtk::Popover>();
    popover.connect_map(menu_opening);
    popover.connect_unmap(menu_closed);
}

/// Guard a dropdown's list, the popover GTK builds inside every `DropDown`.
pub(super) fn guard_dropdown(dropdown: &gtk::DropDown) {
    let mut child = dropdown.first_child();
    while let Some(widget) = child {
        if let Some(popover) = widget.downcast_ref::<gtk::Popover>() {
            guard_popover(popover);
            return;
        }
        child = widget.next_sibling();
    }
    debug_assert!(false, "a GTK DropDown always has a popover child");
}

/// Guard a split button's menu.
pub(super) fn guard_split_button(button: &adw::SplitButton) {
    match button.popover() {
        Some(popover) => guard_popover(&popover),
        None => debug_assert!(false, "set the split button's popover before guarding it"),
    }
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
    // With every tooltip off, a query at the pointer finds none, so GTK hides
    // the one showing. A menu button's menu is not presented yet; a guarded
    // popover is, but has not handled an event that could move a tooltip.
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

    /// Popups the shell builds that need no guard of their own, and why.
    /// GTK's own entry context menu and emoji chooser are not built here: they
    /// open only on a click or key press, and GTK hides any showing tooltip on
    /// both (`gtk_tooltip_handle_event`), so they cannot open over one.
    const ALLOWED: [(&str, &str, &str); 1] = [(
        "mod.rs",
        "toolport_menu_popover",
        "builds a menu's popover; each caller hands it to a guarded menu button",
    )];

    /// What each kind of popup must be guarded with. A popover may instead
    /// be handed to a menu button in the same function, which guards it.
    const KINDS: [(&str, &str); 9] = [
        ("MenuButton::builder()", "guard_menu"),
        ("MenuButton::new()", "guard_menu"),
        ("MenuButton::with_", "guard_menu"),
        ("MenuButton::default()", "guard_menu"),
        ("DropDown::", "guard_dropdown"),
        ("SplitButton::", "guard_split_button"),
        ("Popover::new()", "guard_popover"),
        ("Popover::builder()", "guard_popover"),
        ("toolport_menu_popover()", "guard_popover"),
    ];

    /// Popup kinds the shell does not use yet. Adding one needs a guard here
    /// first, or an entry in `ALLOWED` with the reason it cannot hang.
    const UNHANDLED: [&str; 13] = [
        "PopoverMenu",
        "EmojiChooser",
        "EntryCompletion",
        "set_extra_menu",
        "ComboRow",
        // Built without a constructor the checks above recognise.
        "Object::builder::<gtk::MenuButton",
        "Object::builder::<gtk::DropDown",
        "Object::builder::<gtk::Popover",
        "Object::builder::<adw::SplitButton",
        "Object::new::<gtk::MenuButton",
        "Object::new::<gtk::DropDown",
        "Object::new::<gtk::Popover",
        "Object::new::<adw::SplitButton",
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

    /// The shell's code without comments or its test modules.
    fn product_source(path: &std::path::Path) -> String {
        strip_test_modules(
            &std::fs::read_to_string(path).unwrap(),
            &path.display().to_string(),
        )
    }

    /// The source without its `#[cfg(test)]` modules, comment lines blanked.
    /// Panics when a test module does not close: everything after it would
    /// silently drop out of the check.
    fn strip_test_modules(source: &str, file: &str) -> String {
        let mut source = source.to_string();
        while let Some(at) = source.find("#[cfg(test)]\nmod ") {
            let open = at + source[at..].find('{').unwrap();
            let close = matching_brace(&source, open).unwrap_or_else(|| {
                panic!("{file}: the test module at byte {at} does not close before the end")
            });
            source.replace_range(at..=close, "");
        }
        source
            .lines()
            .map(|line| {
                if line.trim_start().starts_with("//") {
                    ""
                } else {
                    line
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The `}` closing the `{` at `open`, skipping braces inside comments and
    /// string, raw string and char literals.
    fn matching_brace(source: &str, open: usize) -> Option<usize> {
        let bytes = source.as_bytes();
        let ident =
            |i: usize| i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
        let mut depth = 0usize;
        let mut i = open;
        while i < bytes.len() {
            match bytes[i] {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                b'/' if bytes.get(i + 1) == Some(&b'/') => {
                    i = source[i..].find('\n').map_or(bytes.len(), |end| i + end);
                    continue;
                }
                b'/' if bytes.get(i + 1) == Some(&b'*') => {
                    let mut nested = 0usize;
                    while i < bytes.len() {
                        if bytes[i..].starts_with(b"/*") {
                            nested += 1;
                            i += 2;
                        } else if bytes[i..].starts_with(b"*/") {
                            nested -= 1;
                            i += 2;
                            if nested == 0 {
                                break;
                            }
                        } else {
                            i += 1;
                        }
                    }
                    continue;
                }
                // r"…", r#"…"#, br"…": no escapes, ends at the quote and hashes.
                b'r' if !ident(i) || (i > 0 && bytes[i - 1] == b'b' && !ident(i - 1)) => {
                    let hashes = bytes[i + 1..].iter().take_while(|&&b| b == b'#').count();
                    if bytes.get(i + 1 + hashes) == Some(&b'"') {
                        let end = format!("\"{}", "#".repeat(hashes));
                        let body = i + 2 + hashes;
                        i = source[body..]
                            .find(&end)
                            .map_or(bytes.len(), |at| body + at + end.len());
                        continue;
                    }
                }
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        i += if bytes[i] == b'\\' { 2 } else { 1 };
                    }
                }
                // A char literal, not a lifetime: 'x', '\n', '\u{7d}', '}'.
                b'\'' => {
                    if bytes.get(i + 1) == Some(&b'\\') {
                        // Skip the escaped character itself, which may be a quote.
                        i = source
                            .get(i + 3..)
                            .and_then(|rest| rest.find('\''))
                            .map_or(bytes.len(), |at| i + 3 + at);
                    } else if let Some(c) = source[i + 1..].chars().next() {
                        if source[i + 1 + c.len_utf8()..].starts_with('\'') {
                            i += c.len_utf8() + 1;
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }
        None
    }

    #[test]
    fn the_test_module_scanner_ignores_braces_in_literals_and_comments() {
        let source = concat!(
            "fn before() {}\n",
            "#[cfg(test)]\nmod tests {\n",
            "    const OPEN: &str = \"{ not a block\";\n",
            "    const RAW: &str = r#\"}\" still raw {\"#;\n",
            "    const BYTES: &[u8] = br\"{\";\n",
            "    const ESCAPED: &str = \"\\\" {\";\n",
            "    const CLOSE: char = '}';\n",
            "    const QUOTE: u8 = b'\\'';\n",
            "    const OPEN_BYTE: u8 = b'{';\n",
            "    const UNICODE: char = '\\u{7b}';\n",
            "    fn borrow<'a>(text: &'a str) -> &'a str { text }\n",
            "    // a comment with {\n",
            "    /* a block { /* nested } */ comment */\n",
            "}\n",
            "fn secret_reference_fields() {}\n",
        );
        let kept = strip_test_modules(source, "fixture.rs");
        assert!(kept.contains("fn before()"));
        assert!(
            kept.contains("fn secret_reference_fields()"),
            "code after the test module stays checked: {kept}"
        );
        assert!(!kept.contains("not a block"));
    }

    #[test]
    #[should_panic(expected = "does not close before the end")]
    fn a_test_module_that_never_closes_fails_the_check() {
        strip_test_modules(
            "#[cfg(test)]\nmod tests {\n    fn open() {\n}\nfn product() {}\n",
            "fixture.rs",
        );
    }

    /// The function a position is in: the nearest `fn NAME` before it.
    fn function_at(source: &str, at: usize) -> &str {
        source[..at]
            .rfind("fn ")
            .map(|start| {
                let rest = &source[start + 3..];
                &rest[..rest
                    .find(|c: char| !c.is_alphanumeric() && c != '_')
                    .unwrap_or(rest.len())]
            })
            .unwrap_or_default()
    }

    /// Whether `name` is the popover of a menu or split button built in
    /// `body`, so that button's guard covers it.
    fn handed_to_menu_button(body: &str, name: &str) -> bool {
        let built = body
            .match_indices(&format!(".popover(&{name})"))
            .any(|(at, _)| {
                let statement = body[..at].rfind(';').map_or(0, |i| i + 1);
                // A split button's own guard is checked as its own popup.
                body[statement..at].contains("MenuButton::builder()")
                    || body[statement..at].contains("SplitButton::builder()")
            });
        let set = body
            .match_indices(&format!(".set_popover(Some(&{name}))"))
            .any(|(at, _)| {
                let line = body[..at].rfind('\n').map_or(0, |i| i + 1);
                let owner = body[line..at].trim();
                body.contains(&format!("let {owner} = gtk::MenuButton::"))
            });
        built || set
    }

    /// Every popup in the shell must be guarded: one opened over a showing
    /// tooltip is enough to hang the app (D01). Menu buttons, dropdowns,
    /// split buttons and bare popovers each have their guard. This reads the
    /// shell's source, so it runs without a desktop.
    #[test]
    fn every_popup_in_the_shell_is_guarded() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/linux_native");
        let mut files = Vec::new();
        sources(&dir, &mut files);
        let mut checked = std::collections::BTreeMap::<&str, usize>::new();
        let mut missing = Vec::new();
        for path in files {
            let file = path.strip_prefix(&dir).unwrap().display().to_string();
            let source = product_source(&path);
            for kind in UNHANDLED {
                if source.contains(kind) {
                    missing.push(format!("{file}: {kind} has no guard yet"));
                }
            }
            let mut builds: Vec<(usize, &str)> = KINDS
                .iter()
                .flat_map(|&(pattern, guard)| {
                    source
                        .match_indices(pattern)
                        // `fn toolport_menu_popover()` defines, not builds.
                        .filter(|(at, _)| !source[..*at].ends_with("fn "))
                        .map(move |(at, _)| (at, guard))
                })
                .collect();
            builds.sort_unstable();
            for (index, &(at, guard)) in builds.iter().enumerate() {
                *checked.entry(guard).or_default() += 1;
                let line_no = source[..at].matches('\n').count() + 1;
                let function = function_at(&source, at);
                if ALLOWED
                    .iter()
                    .any(|&(f, func, _)| f == file && func == function)
                {
                    continue;
                }
                // The binding may start on the line before a long call.
                let line_start = source[..at].rfind('\n').map_or(0, |i| i + 1);
                let previous = source[..line_start.saturating_sub(1)]
                    .rfind('\n')
                    .map_or(0, |i| i + 1);
                let binding = [&source[line_start..at], &source[previous..at]]
                    .into_iter()
                    .find_map(|text| {
                        text.trim_start()
                            .strip_prefix("let ")
                            .and_then(|binding| binding.split_once(" ="))
                            .map(|(name, _)| {
                                name.trim_start_matches("mut ")
                                    .split(':')
                                    .next()
                                    .unwrap_or_default()
                                    .trim()
                            })
                    });
                let Some(name) = binding else {
                    missing.push(format!("{file}:{line_no}: bind the popup to a name"));
                    continue;
                };
                // The guard belongs after the popup is built, in the same
                // function and before the next popup of its kind.
                let body = &source[at..];
                let end = ["\nfn ", "\n    fn ", "\npub", "\n    pub"]
                    .iter()
                    .filter_map(|next| body.find(next))
                    .chain(
                        builds[index + 1..]
                            .iter()
                            .find(|(_, next_guard)| *next_guard == guard)
                            .map(|(next, _)| next - at),
                    )
                    .min()
                    .unwrap_or(body.len());
                let scope = &body[..end];
                let function_end = ["\nfn ", "\n    fn ", "\npub", "\n    pub"]
                    .iter()
                    .filter_map(|next| body.find(next))
                    .min()
                    .unwrap_or(body.len());
                // The menu button that takes the popover may be built first.
                let function_start = ["\nfn ", "\n    fn ", "\npub", "\n    pub"]
                    .iter()
                    .filter_map(|start| source[..at].rfind(start))
                    .max()
                    .unwrap_or(0);
                let owned_by_menu = guard == "guard_popover"
                    && handed_to_menu_button(&source[function_start..at + function_end], name);
                let guarded = scope.lines().any(|line| {
                    let line = line.trim_start();
                    let line = line.strip_prefix("super::").unwrap_or(line);
                    line.starts_with(&format!("quiet_tooltips::{guard}(&{name});"))
                });
                if !guarded && !owned_by_menu {
                    missing.push(format!("{file}:{line_no}: {name} has no {guard}"));
                }
            }
        }
        for guard in [
            "guard_menu",
            "guard_dropdown",
            "guard_split_button",
            "guard_popover",
        ] {
            assert!(
                checked.get(guard).is_some_and(|count| *count > 0),
                "found nothing checked with {guard} in {dir:?}: {checked:?}"
            );
        }
        assert!(
            missing.is_empty(),
            "unguarded popups:\n{}",
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

    type Opener = Box<dyn Fn()>;

    /// A window with a tooltip row and one guarded popup of each other kind,
    /// with how to open each the way keyboard and accessibility users do.
    fn popup_fixture(
        title: &str,
    ) -> (
        gtk::Window,
        gtk::Button,
        Vec<(&'static str, gtk::Popover, Opener)>,
    ) {
        adw::init().unwrap();
        let row = gtk::Button::builder()
            .label("Row")
            .hexpand(true)
            .vexpand(true)
            .build();
        row.set_tooltip_text(Some("Row details"));
        let dropdown = gtk::DropDown::from_strings(&["One", "Two"]);
        guard_dropdown(&dropdown);
        let split_menu = gtk::Popover::builder()
            .child(&gtk::Label::new(Some("Open with")))
            .build();
        let split = adw::SplitButton::builder()
            .label("Open")
            .popover(&split_menu)
            .build();
        guard_split_button(&split);
        let anchor = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let suggestions = gtk::Popover::new();
        suggestions.set_autohide(false);
        suggestions.set_child(Some(&gtk::Label::new(Some("Suggestion"))));
        suggestions.set_parent(&anchor);
        guard_popover(&suggestions);
        let column = gtk::Box::new(gtk::Orientation::Vertical, 6);
        for widget in [
            dropdown.upcast_ref::<gtk::Widget>(),
            split.upcast_ref(),
            anchor.upcast_ref(),
            row.upcast_ref(),
        ] {
            column.append(widget);
        }
        let window = gtk::Window::builder().title(title).child(&column).build();
        window.present();
        pump_until(|| window.is_mapped() && row.width() > 0);
        let mut child = dropdown.first_child();
        let dropdown_list = loop {
            let widget = child.expect("a dropdown has a popover");
            if let Ok(popover) = widget.clone().downcast::<gtk::Popover>() {
                break popover;
            }
            child = widget.next_sibling();
        };
        let cases: Vec<(&'static str, gtk::Popover, Opener)> = vec![
            (
                "dropdown",
                dropdown_list,
                // Enter on the dropdown and its accessibility click take this path.
                Box::new(move || dropdown.emit_by_name::<()>("activate", &[])),
            ),
            ("split button", split_menu, Box::new(move || split.popup())),
            (
                "popover",
                suggestions.clone(),
                Box::new(move || suggestions.popup()),
            ),
        ];
        (window, row, cases)
    }

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn dropdowns_split_buttons_and_popovers_silence_tooltips_while_open() {
        let (_window, row, cases) = popup_fixture("quiet-tooltips-popups");
        for (kind, popover, open) in cases {
            open();
            pump_until(|| popover.is_mapped());
            assert!(popover.is_mapped(), "{kind} opens");
            assert!(is_quiet(), "{kind} is tracked while open");
            assert!(!row.has_tooltip(), "{kind}: tooltips are off while open");
            popover.popdown();
            pump_until(|| !popover.is_mapped());
            assert!(!is_quiet(), "{kind}: nothing left open");
            assert!(row.has_tooltip(), "{kind}: tooltips come back after");
            assert_eq!(row.tooltip_text().as_deref(), Some("Row details"));
        }
    }

    /// Needs a real hover, like the menu test below: the driver moves the box
    /// pointer to the printed row bounds and leaves it there.
    #[test]
    #[ignore = "requires an isolated GTK desktop and an external pointer hover; run in omabox"]
    fn a_tooltip_showing_from_a_real_hover_is_hidden_when_any_popup_opens() {
        let (window, row, cases) = popup_fixture("quiet-tooltips-hover");
        let bounds = row.compute_bounds(&window).expect("row has bounds");
        println!(
            "READY_FOR_HOVER row={},{},{},{}",
            bounds.x() as i32,
            bounds.y() as i32,
            bounds.width() as i32,
            bounds.height() as i32
        );
        let context = glib::MainContext::default();
        let wait_for_tooltip = || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while visible_tooltip_windows(window.upcast_ref()) == 0
                && std::time::Instant::now() < deadline
            {
                context.iteration(false);
            }
        };
        for (kind, popover, open) in cases {
            window.trigger_tooltip_query();
            wait_for_tooltip();
            assert_eq!(
                visible_tooltip_windows(window.upcast_ref()),
                1,
                "{kind}: a real hover must show the row tooltip first"
            );
            println!("TOOLTIP_SHOWN {kind}");
            // No input event: the path an accessibility click takes.
            open();
            pump_until(|| popover.is_mapped());
            assert!(popover.is_mapped(), "{kind} opens without hanging");
            assert_eq!(
                visible_tooltip_windows(window.upcast_ref()),
                0,
                "{kind}: the tooltip is hidden as soon as the popup maps"
            );
            let settle = std::time::Instant::now() + std::time::Duration::from_secs(1);
            while std::time::Instant::now() < settle {
                context.iteration(false);
            }
            assert_eq!(visible_tooltip_windows(window.upcast_ref()), 0);
            popover.popdown();
            pump_until(|| !popover.is_mapped());
            assert!(row.has_tooltip(), "{kind}: tooltips come back after");
        }
        println!("ALL_POPUPS_CLOSED_AND_RESTORED");
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
