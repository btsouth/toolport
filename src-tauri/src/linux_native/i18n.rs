//! Explicit source-message bindings. Retain the source key, never derive a key
//! from widget text, which may contain a user command, path or credential.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

// A widget can have one binding per text property. Rebinding a changing action
// (Start -> Stop) replaces its original source instead of resurrecting it later.
type BindingKey = (usize, String);
type Refresh = Rc<dyn Fn() -> bool>;
struct Binding {
    refresh: Refresh,
    token: Rc<()>,
    object: glib::WeakRef<glib::Object>,
    observer: Option<glib::SignalHandlerId>,
}
impl Drop for Binding {
    fn drop(&mut self) {
        if let (Some(object), Some(observer)) = (self.object.upgrade(), self.observer.take()) {
            object.disconnect(observer);
        }
    }
}
thread_local! {
    static BINDINGS: RefCell<HashMap<BindingKey, Rc<Binding>>> = RefCell::new(HashMap::new());
    static DROPDOWN_UPDATES: RefCell<HashMap<usize, usize>> = RefCell::new(HashMap::new());
}

fn text_property(object: &glib::Object, property: &str) -> Option<String> {
    object
        .property_value(property)
        .get::<Option<String>>()
        .ok()
        .flatten()
}

fn register(object: &impl IsA<glib::Object>, property: &str, refresh: Refresh) {
    // Retire dead widgets during ordinary rerenders too, even if the language
    // never changes. Drop observers after releasing the registry borrow.
    let retired: Vec<_> = BINDINGS.with(|bindings| {
        let mut bindings = bindings.borrow_mut();
        let dead: Vec<_> = bindings
            .iter()
            .filter(|(_, binding)| binding.object.upgrade().is_none())
            .map(|(key, _)| key.clone())
            .collect();
        dead.into_iter()
            .filter_map(|key| bindings.remove(&key))
            .collect()
    });
    drop(retired);
    let object = object.as_ref();
    let key = (object.as_ptr() as usize, property.to_owned());
    let token = Rc::new(());
    let own_update = Rc::new(Cell::new(false));
    let rendered = Rc::new(RefCell::new(None::<String>));
    // Dialog responses use synthetic keys, while ordinary string properties can
    // relinquish source ownership if another part of the UI writes new text.
    let observable = object.find_property(property).is_some();
    let observer = observable.then(|| {
        let key = key.clone();
        let token = token.clone();
        let own_update = own_update.clone();
        let rendered = rendered.clone();
        let property = property.to_owned();
        object.connect_notify_local(Some(&property.clone()), move |object, _| {
            if own_update.get() || *rendered.borrow() == text_property(object, &property) {
                return;
            }
            let removed = BINDINGS.with(|bindings| {
                let mut bindings = bindings.borrow_mut();
                if bindings
                    .get(&key)
                    .is_some_and(|binding| Rc::ptr_eq(&binding.token, &token))
                {
                    bindings.remove(&key)
                } else {
                    None
                }
            });
            drop(removed);
        })
    });
    let weak = object.downgrade();
    let property = property.to_owned();
    let guarded: Refresh = Rc::new(move || {
        own_update.set(true);
        let alive = refresh();
        if observable {
            if let Some(object) = weak.upgrade() {
                rendered.replace(text_property(&object, &property));
            }
        }
        own_update.set(false);
        alive
    });
    let binding = Rc::new(Binding {
        refresh: guarded,
        token,
        object: object.downgrade(),
        observer,
    });
    let replaced = BINDINGS.with(|bindings| bindings.borrow_mut().insert(key, binding.clone()));
    drop(replaced);
    (binding.refresh)();
}

pub(super) fn label(source: &'static str) -> gtk::Label {
    let widget = gtk::Label::new(None);
    bind_label(&widget, source);
    widget
}

pub(super) fn button(source: &'static str) -> gtk::Button {
    let widget = gtk::Button::new();
    bind_button(&widget, source);
    widget
}

pub(super) fn check_button(source: &'static str) -> gtk::CheckButton {
    let widget = gtk::CheckButton::new();
    bind_check_button(&widget, source);
    widget
}

pub(super) fn bind_label(widget: &gtk::Label, source: &'static str) {
    bind_label_with(widget, move || crate::i18n::lookup(source));
}

/// The formatter reads retained typed state and must not strongly capture its
/// target widget. This changes text only; it must not rebuild/refetch the view.
pub(super) fn bind_label_with(widget: &gtk::Label, formatter: impl Fn() -> String + 'static) {
    let weak = widget.downgrade();
    register(
        widget,
        "label",
        Rc::new(move || {
            let Some(widget) = weak.upgrade() else {
                return false;
            };
            widget.set_text(&formatter());
            true
        }),
    );
}

pub(super) fn formatted_label(source: &'static str, values: &[(&str, &str)]) -> gtk::Label {
    let widget = gtk::Label::new(None);
    bind_formatted_label(&widget, source, values);
    widget
}

pub(super) fn bind_formatted_label(
    widget: &gtk::Label,
    source: &'static str,
    values: &[(&str, &str)],
) {
    let values: Vec<(String, String)> = values
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect();
    bind_label_with(widget, move || {
        let refs: Vec<_> = values
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        crate::i18n::format_message(source, &refs)
    });
}

pub(super) fn bind_button(widget: &gtk::Button, source: &'static str) {
    bind_button_with(widget, move || crate::i18n::lookup(source));
}

pub(super) fn bind_button_with(widget: &gtk::Button, formatter: impl Fn() -> String + 'static) {
    let weak = widget.downgrade();
    register(
        widget,
        "label",
        Rc::new(move || {
            let Some(widget) = weak.upgrade() else {
                return false;
            };
            widget.set_label(&formatter());
            true
        }),
    );
}

pub(super) fn bind_check_button(widget: &gtk::CheckButton, source: &'static str) {
    let weak = widget.downgrade();
    register(
        widget,
        "label",
        Rc::new(move || {
            let Some(widget) = weak.upgrade() else {
                return false;
            };
            widget.set_label(Some(&crate::i18n::lookup(source)));
            true
        }),
    );
}

/// Bind an explicitly selected text widget; never traverse children or infer a
/// source from current text. Useful when a captured widget's type is aliased.
pub(super) fn bind_text(widget: &impl IsA<gtk::Widget>, source: &'static str) {
    let widget = widget.as_ref();
    if let Some(label) = widget.downcast_ref::<gtk::Label>() {
        bind_label(label, source);
    } else if let Some(button) = widget.downcast_ref::<gtk::Button>() {
        bind_button(button, source);
    } else if let Some(button) = widget.downcast_ref::<gtk::CheckButton>() {
        bind_check_button(button, source);
    } else {
        panic!("bind_text requires a Label, Button or CheckButton");
    }
}

pub(super) fn bind_tooltip(widget: &impl IsA<gtk::Widget>, source: &'static str) {
    bind_tooltip_with(widget, move || crate::i18n::lookup(source));
}

pub(super) fn bind_tooltip_with(
    widget: &impl IsA<gtk::Widget>,
    formatter: impl Fn() -> String + 'static,
) {
    let weak = widget.as_ref().downgrade();
    register(
        widget.as_ref(),
        "tooltip-text",
        Rc::new(move || {
            let Some(widget) = weak.upgrade() else {
                return false;
            };
            // Empty means no tooltip.
            let text = formatter();
            widget.set_tooltip_text((!text.is_empty()).then_some(text.as_str()));
            true
        }),
    );
}

/// The accessible name read by screen readers, kept in the current language.
pub(super) fn bind_accessible_label(widget: &impl IsA<gtk::Accessible>, source: &'static str) {
    let weak = widget.as_ref().downgrade();
    register(
        widget.as_ref(),
        "accessible-label",
        Rc::new(move || {
            let Some(widget) = weak.upgrade() else {
                return false;
            };
            widget.update_property(&[gtk::accessible::Property::Label(&crate::i18n::lookup(
                source,
            ))]);
            true
        }),
    );
}

/// The accessible name and, when there is one, the description read by screen readers,
/// both kept in the current language. `text` is called again after each language change.
pub(super) fn bind_accessible_with(
    widget: &impl IsA<gtk::Accessible>,
    text: impl Fn() -> (String, Option<String>) + 'static,
) {
    let weak = widget.as_ref().downgrade();
    register(
        widget.as_ref(),
        "accessible-label-description",
        Rc::new(move || {
            let Some(widget) = weak.upgrade() else {
                return false;
            };
            let (label, description) = text();
            widget.update_property(&[gtk::accessible::Property::Label(&label)]);
            match &description {
                Some(description) => {
                    widget.update_property(&[gtk::accessible::Property::Description(description)])
                }
                None => widget.reset_property(gtk::AccessibleProperty::Description),
            }
            #[cfg(test)]
            ACCESSIBLE_TEXT.with(|texts| {
                texts
                    .borrow_mut()
                    .insert(widget.as_ptr() as usize, (label, description))
            });
            true
        }),
    );
}

#[cfg(test)]
thread_local! {
    /// What [`bind_accessible_with`] last set, since GTK has no getter for it.
    static ACCESSIBLE_TEXT: RefCell<HashMap<usize, (String, Option<String>)>> =
        RefCell::new(HashMap::new());
}

/// The accessible name and description [`bind_accessible_with`] last set on `widget`.
#[cfg(test)]
pub(super) fn accessible_text(
    widget: &impl IsA<gtk::Accessible>,
) -> Option<(String, Option<String>)> {
    let key = widget.as_ref().as_ptr() as usize;
    ACCESSIBLE_TEXT.with(|texts| texts.borrow().get(&key).cloned())
}

pub(super) fn bind_window_title(window: &impl IsA<gtk::Window>, source: &'static str) {
    let weak = window.as_ref().downgrade();
    register(
        window.as_ref(),
        "title",
        Rc::new(move || {
            let Some(window) = weak.upgrade() else {
                return false;
            };
            window.set_title(Some(&crate::i18n::lookup(source)));
            true
        }),
    );
}

pub(super) fn bind_stack_page_title(page: &gtk::StackPage, source: &'static str) {
    let weak = page.downgrade();
    register(
        page,
        "title",
        Rc::new(move || {
            let Some(page) = weak.upgrade() else {
                return false;
            };
            page.set_title(&crate::i18n::lookup(source));
            true
        }),
    );
}

/// Placeholder text is presentation only; entry contents and cursor stay intact.
pub(super) fn bind_placeholder(widget: &impl IsA<gtk::Widget>, source: &'static str) {
    let weak = widget.as_ref().downgrade();
    register(
        widget.as_ref(),
        "placeholder-text",
        Rc::new(move || {
            let Some(widget) = weak.upgrade() else {
                return false;
            };
            let text = crate::i18n::lookup(source);
            if let Some(entry) = widget.downcast_ref::<gtk::Entry>() {
                entry.set_placeholder_text(Some(&text));
            } else if let Some(entry) = widget.downcast_ref::<gtk::SearchEntry>() {
                entry.set_placeholder_text(Some(&text));
            } else if let Some(entry) = widget.downcast_ref::<gtk::PasswordEntry>() {
                entry.set_placeholder_text(Some(&text));
            } else {
                panic!("bind_placeholder requires an Entry, SearchEntry or PasswordEntry");
            }
            true
        }),
    );
}

pub(super) fn bind_formatted_dialog_heading(
    dialog: &adw::MessageDialog,
    source: &'static str,
    values: &[(&str, &str)],
) {
    let weak = dialog.downgrade();
    let values: Vec<(String, String)> = values
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect();
    register(
        dialog,
        "heading",
        Rc::new(move || {
            let Some(dialog) = weak.upgrade() else {
                return false;
            };
            let refs: Vec<_> = values
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str()))
                .collect();
            dialog.set_heading(Some(&crate::i18n::format_message(source, &refs)));
            true
        }),
    );
}

pub(super) fn bind_formatted_dialog_body(
    dialog: &adw::MessageDialog,
    source: &'static str,
    values: &[(&str, &str)],
) {
    let weak = dialog.downgrade();
    let values: Vec<(String, String)> = values
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect();
    register(
        dialog,
        "body",
        Rc::new(move || {
            let Some(dialog) = weak.upgrade() else {
                return false;
            };
            let refs: Vec<_> = values
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str()))
                .collect();
            dialog.set_body(&crate::i18n::format_message(source, &refs));
            true
        }),
    );
}

pub(super) fn bind_dialog_heading(dialog: &adw::MessageDialog, source: &'static str) {
    let weak = dialog.downgrade();
    register(
        dialog,
        "heading",
        Rc::new(move || {
            let Some(dialog) = weak.upgrade() else {
                return false;
            };
            dialog.set_heading(Some(&crate::i18n::lookup(source)));
            true
        }),
    );
}

pub(super) fn bind_dialog_body(dialog: &adw::MessageDialog, source: &'static str) {
    let weak = dialog.downgrade();
    register(
        dialog,
        "body",
        Rc::new(move || {
            let Some(dialog) = weak.upgrade() else {
                return false;
            };
            dialog.set_body(&crate::i18n::lookup(source));
            true
        }),
    );
}

pub(super) fn bind_dialog_response(
    dialog: &adw::MessageDialog,
    response: &'static str,
    source: &'static str,
) {
    let weak = dialog.downgrade();
    register(
        dialog,
        &format!("response:{response}"),
        Rc::new(move || {
            let Some(dialog) = weak.upgrade() else {
                return false;
            };
            dialog.set_response_label(response, &crate::i18n::lookup(source));
            true
        }),
    );
}

struct DropdownUpdate(usize);
impl DropdownUpdate {
    fn new(widget: &gtk::DropDown) -> Self {
        let key = widget.as_ptr() as usize;
        DROPDOWN_UPDATES.with(|updates| {
            *updates.borrow_mut().entry(key).or_default() += 1;
        });
        Self(key)
    }
}
impl Drop for DropdownUpdate {
    fn drop(&mut self) {
        DROPDOWN_UPDATES.with(|updates| {
            let mut updates = updates.borrow_mut();
            if let Some(depth) = updates.get_mut(&self.0) {
                *depth -= 1;
                if *depth == 0 {
                    updates.remove(&self.0);
                }
            }
        });
    }
}

/// Consumer notify::selected handlers must return while this is true: changing
/// the translated model can notify even after restoring the same selected index.
pub(super) fn dropdown_rebinding(widget: &gtk::DropDown) -> bool {
    DROPDOWN_UPDATES.with(|updates| updates.borrow().contains_key(&(widget.as_ptr() as usize)))
}

/// Sources are a fixed ordered choice list, never user-provided model values.
/// Selection indices remain stable across locale changes. The consumer must use
/// dropdown_rebinding in preference-change handlers to suppress logical writes.
pub(super) fn bind_dropdown_choices(widget: &gtk::DropDown, sources: &'static [&'static str]) {
    let weak = widget.downgrade();
    register(
        widget,
        "localized-choices",
        Rc::new(move || {
            let Some(widget) = weak.upgrade() else {
                return false;
            };
            let selected = widget.selected();
            let _updating = DropdownUpdate::new(&widget);
            let notifications = widget.freeze_notify();
            let values: Vec<_> = sources
                .iter()
                .map(|source| crate::i18n::lookup(source))
                .collect();
            let refs: Vec<_> = values.iter().map(String::as_str).collect();
            let model = gtk::StringList::new(&refs);
            widget.set_model(Some(&model));
            widget.set_selected(selected);
            // Thaw while the update guard is still active.
            drop(notifications);
            true
        }),
    );
}

/// Native language names are endonyms, not source messages. Only System is
/// translated; choice order comes from the shared persisted setting contract.
pub(super) fn bind_language_choices(widget: &gtk::DropDown) {
    let weak = widget.downgrade();
    register(
        widget,
        "localized-choices",
        Rc::new(move || {
            let Some(widget) = weak.upgrade() else {
                return false;
            };
            let choice = crate::i18n::language();
            let selected = crate::ui_settings::LANGUAGES
                .iter()
                .position(|code| *code == choice)
                .unwrap_or(0) as u32;
            let _updating = DropdownUpdate::new(&widget);
            let notifications = widget.freeze_notify();
            let values: Vec<String> = crate::ui_settings::LANGUAGES
                .iter()
                .map(|code| {
                    if *code == "system" {
                        crate::i18n::tr("System")
                    } else {
                        crate::i18n::LANGUAGES
                            .iter()
                            .find(|(locale, _)| locale == code)
                            .map(|(_, name)| *name)
                            .unwrap_or(code)
                            .to_string()
                    }
                })
                .collect();
            let refs: Vec<_> = values.iter().map(String::as_str).collect();
            // Keep the popup's selection model stable while its selected
            // notification is being handled. Only System needs retranslation.
            if let Some(model) = widget.model().and_downcast::<gtk::StringList>() {
                if model.string(0).as_deref() != Some(refs[0]) {
                    model.splice(0, 1, &[refs[0]]);
                }
            } else {
                widget.set_model(Some(&gtk::StringList::new(&refs)));
            }
            if widget.selected() != selected {
                widget.set_selected(selected);
            }
            drop(notifications);
            true
        }),
    );
}

/// Drop a binding before a property begins displaying raw user data.
pub(super) fn unbind(object: &impl IsA<glib::Object>, property: &str) {
    let key = (object.as_ref().as_ptr() as usize, property.to_owned());
    BINDINGS.with(|bindings| {
        bindings.borrow_mut().remove(&key);
    });
}

/// Call `callback` after each language change for as long as `object` lives, for views
/// whose text comes from data (a rescan) rather than one source string.
pub(super) fn on_language_change(object: &impl IsA<glib::Object>, callback: impl Fn() + 'static) {
    let weak = object.as_ref().downgrade();
    // `register` runs a binding once right away; this one waits for a real change.
    let first = Cell::new(true);
    register(
        object,
        "language-change",
        Rc::new(move || {
            if weak.upgrade().is_none() {
                return false;
            }
            if !first.replace(false) {
                callback();
            }
            true
        }),
    );
}

/// Run on the GTK main thread after the shared language revision changes.
pub(super) fn refresh_bindings() {
    // Invoke outside RefCell borrows: GTK property notifications may rebind text.
    let bindings: Vec<_> = BINDINGS.with(|bindings| {
        bindings
            .borrow()
            .iter()
            .map(|(key, callback)| (key.clone(), callback.clone()))
            .collect()
    });
    for (key, callback) in bindings {
        let current = BINDINGS.with(|bindings| {
            bindings
                .borrow()
                .get(&key)
                .is_some_and(|entry| Rc::ptr_eq(entry, &callback))
        });
        if !current {
            continue;
        }
        if !(callback.refresh)() {
            BINDINGS.with(|bindings| {
                let mut bindings = bindings.borrow_mut();
                if bindings
                    .get(&key)
                    .is_some_and(|current| Rc::ptr_eq(current, &callback))
                {
                    bindings.remove(&key);
                }
            });
        }
    }
}

/// Watch the directory because atomic saves replace the settings inode. The
/// caller retains this monitor for the application lifetime. No polling service.
pub(super) fn install_language_watch(
    on_change: impl Fn() + 'static,
) -> Result<gio::FileMonitor, String> {
    let directory =
        crate::registry::conduit_dir().ok_or("could not resolve UI settings directory")?;
    let monitor = gio::File::for_path(directory)
        .monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
        .map_err(|error| error.to_string())?;
    let queued = Rc::new(Cell::new(false));
    let on_change = Rc::new(on_change);
    monitor.connect_changed(move |_, file, other, _| {
        let matches = |file: &gio::File| {
            file.basename().as_deref() == Some(std::path::Path::new("ui-settings.json"))
        };
        if !(matches(file) || other.is_some_and(matches)) || queued.replace(true) {
            return;
        }
        let queued = queued.clone();
        let on_change = on_change.clone();
        glib::idle_add_local_once(move || {
            queued.set(false);
            if crate::i18n::reload_language().unwrap_or(false) {
                refresh_bindings();
                on_change();
            }
        });
    });
    Ok(monitor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registering_a_binding_prunes_destroyed_objects_without_a_language_change() {
        let object = glib::Object::new::<glib::Object>();
        let key = (object.as_ptr() as usize, "test-old".to_owned());
        let weak = object.downgrade();
        register(
            &object,
            "test-old",
            Rc::new(move || weak.upgrade().is_some()),
        );
        assert!(BINDINGS.with(|bindings| bindings.borrow().contains_key(&key)));
        drop(object);
        let next = glib::Object::new::<glib::Object>();
        let weak = next.downgrade();
        register(&next, "test-new", Rc::new(move || weak.upgrade().is_some()));
        assert!(!BINDINGS.with(|bindings| bindings.borrow().contains_key(&key)));
        unbind(&next, "test-new");
    }
}
