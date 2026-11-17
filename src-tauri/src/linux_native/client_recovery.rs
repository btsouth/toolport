//! Clients recovery notices live on the page, never in the transient toast layer.
use super::*;

/// One latest recovery result for the whole shell. A new action replaces it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct RecoveryResult {
    pub(super) client_id: String,
    pub(super) message: String,
    /// False for a neutral listing refresh, which makes no success claim.
    pub(super) success: bool,
}

/// Paths and file names wrap like the React shell: never with a hyphen that is
/// not in them. Pango otherwise hyphenates long words it breaks (".jso-n").
fn no_hyphens() -> gtk::pango::AttrList {
    let attrs = gtk::pango::AttrList::new();
    attrs.insert(gtk::pango::AttrInt::new_insert_hyphens(false));
    attrs
}

fn set_values_text(buffer: &gtk::TextBuffer, text: &str) {
    buffer.set_text(text);
    let table = buffer.tag_table();
    let tag = table.lookup("no-hyphens").unwrap_or_else(|| {
        let tag = gtk::TextTag::builder()
            .name("no-hyphens")
            .insert_hyphens(false)
            .build();
        table.add(&tag);
        tag
    });
    buffer.apply_tag(&tag, &buffer.start_iter(), &buffer.end_iter());
}

fn selectable(value: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(value)
        .attributes(&no_hyphens())
        .xalign(0.0)
        .halign(gtk::Align::Fill)
        .hexpand(true)
        .valign(gtk::Align::Start)
        .vexpand(false)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .selectable(true)
        .build()
}

/// Recovery prose stays selectable for the pointer and readable by assistive
/// technology, but is not a Tab stop. `set_selectable` turns focus on, so this
/// turns it off afterwards; only actions, the value list and Details take Tab.
fn prose(value: &str) -> gtk::Label {
    let label = selectable(value);
    label.set_focusable(false);
    // A selectable label's focus handler reports Tab as handled even when it
    // cannot take focus, which stopped Tab at every card. Skip it entirely.
    label.set_can_focus(false);
    label
}

fn current_text(
    id: &str,
    field: fn(&crate::clients::recovery::ClientRecovery) -> &str,
    fallback: &str,
) -> String {
    crate::clients::recovery::list()
        .ok()
        .and_then(|records| records.into_iter().find(|record| record.id == id))
        .map(|record| field(&record).into())
        .unwrap_or_else(|| fallback.to_string())
}

fn find_named(root: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
    if root.widget_name() == name && root.is_mapped() {
        return Some(root.clone());
    }
    let mut child = root.first_child();
    while let Some(widget) = child {
        if let Some(found) = find_named(&widget, name) {
            return Some(found);
        }
        child = widget.next_sibling();
    }
    None
}

/// Scroll the nearest page scroller so `widget` is fully visible.
fn reveal(widget: &gtk::Widget) {
    let Some(scroller) = widget
        .ancestor(gtk::ScrolledWindow::static_type())
        .and_downcast::<gtk::ScrolledWindow>()
    else {
        return;
    };
    let Some(child) = scroller.child() else {
        return;
    };
    let content = child
        .clone()
        .downcast::<gtk::Viewport>()
        .ok()
        .and_then(|viewport| viewport.child())
        .unwrap_or(child);
    let Some(bounds) = widget.compute_bounds(&content) else {
        return;
    };
    let adjustment = scroller.vadjustment();
    let margin = 12.0;
    let top = f64::from(bounds.y()) - margin;
    let bottom = f64::from(bounds.y() + bounds.height()) + margin;
    let (value, page) = (adjustment.value(), adjustment.page_size());
    if top < value {
        adjustment.set_value(top.max(adjustment.lower()));
    } else if bottom > value + page {
        adjustment.set_value((bottom - page).min(adjustment.upper() - page));
    }
}

/// A rebuilt widget has no allocation until the next layout; reveal it after paint.
fn reveal_later(widget: &gtk::Widget) {
    let Some(clock) = widget.frame_clock() else {
        reveal(widget);
        return;
    };
    let weak = widget.downgrade();
    let handler = std::rc::Rc::new(std::cell::RefCell::new(None::<gtk::glib::SignalHandlerId>));
    let slot = handler.clone();
    let id = clock.connect_after_paint(move |clock| {
        if let Some(id) = slot.borrow_mut().take() {
            clock.disconnect(id);
        }
        if let Some(widget) = weak.upgrade() {
            reveal(&widget);
        }
    });
    *handler.borrow_mut() = Some(id);
    clock.request_phase(gtk::gdk::FrameClockPhase::AFTER_PAINT);
}

pub(super) fn focus_named(root: &impl IsA<gtk::Widget>, name: &str) -> bool {
    let Some(widget) = find_named(root.as_ref(), name) else {
        return false;
    };
    if !widget.grab_focus() {
        return false;
    }
    // Focus alone: a selectable label otherwise selects all of its text,
    // which reads as a highlight bar (and is hard to read in dark themes).
    if let Some(label) = widget.downcast_ref::<gtk::Label>() {
        label.select_region(0, 0);
    }
    // Programmatic focus after a keyboard action must show its ring.
    if let Some(window) = widget.root().and_downcast::<gtk::Window>() {
        window.set_focus_visible(true);
    }
    reveal_later(&widget);
    true
}

/// Focus the first existing candidate once the current rebuild and any closing
/// dialog have settled, so a dialog's own focus restoration cannot win.
pub(super) fn focus_first_later(root: &impl IsA<gtk::Widget>, names: Vec<String>) {
    let root = root.as_ref().downgrade();
    gtk::glib::idle_add_local_once(move || {
        if let Some(root) = root.upgrade() {
            for name in &names {
                if focus_named(&root, name) {
                    break;
                }
            }
        }
    });
}

fn recovery_label(
    record: &crate::clients::recovery::ClientRecovery,
    field: fn(&crate::clients::recovery::ClientRecovery) -> &str,
) -> gtk::Label {
    let label = prose("");
    let id = record.id.clone();
    let fallback = field(record).to_string();
    // Notice metadata is read without opening the vault. Re-fetching also makes
    // visible recovery prose follow a language change, like the action labels.
    l10n::bind_label_with(&label, move || current_text(&id, field, &fallback));
    label
}

fn message_label(record: &crate::clients::recovery::ClientRecovery, heading: bool) -> gtk::Label {
    if !heading {
        return recovery_label(record, |r| &r.description);
    }
    let label = prose("");
    let id = record.id.clone();
    let fallback = record.message.clone();
    l10n::bind_label_with(&label, move || {
        current_text(&id, |r| &r.message, &fallback)
            .split("\n\n")
            .next()
            .unwrap_or("")
            .to_string()
    });
    label
}

fn bullets(lines: &[String]) -> String {
    lines
        .iter()
        .map(|line| format!("• {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// One bounded, keyboard-scrollable list: a read-only TextView keeps standard
/// arrow/Page/Ctrl+Home/End scrolling, selection and Ctrl+C, and passes Tab on.
/// Short content keeps its natural height; longer content is capped at 240px.
fn values_view(text: &str, accessible: &'static str) -> gtk::ScrolledWindow {
    let view: ValuesView = gtk::glib::Object::new();
    view.set_editable(false);
    view.set_cursor_visible(false);
    view.set_accepts_tab(false);
    view.set_wrap_mode(gtk::WrapMode::WordChar);
    view.set_hexpand(true);
    view.set_vexpand(false);
    set_values_text(&view.buffer(), text);
    l10n::bind_accessible_label(&view, accessible);
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(240)
        .vexpand(false)
        .hexpand(true)
        .valign(gtk::Align::Start)
        .css_classes(["toolport-recovery-values"])
        .child(&view)
        .build();
    // An outer page scroller allocates its child's minimum height, so the list
    // must report a correct minimum at the width it is offered. A list that
    // fits uses Never: the scroller then requires the view's own height-for-width
    // minimum (ValuesView), with no scrollbar minimum or gap. A longer list
    // uses Automatic with a fixed 240px minimum and a visible scrollbar.
    fn fit(scroll: &gtk::ScrolledWindow, view: &gtk::TextView) {
        let width = match view.width() {
            width if width > 0 => width,
            _ => scroll.hadjustment().page_size() as i32,
        };
        let long = content_height(view, width) > 240;
        let policy = if long {
            gtk::PolicyType::Automatic
        } else {
            gtk::PolicyType::Never
        };
        if scroll.vscrollbar_policy() != policy {
            scroll.set_vscrollbar_policy(policy);
        }
        // Unset for Never, so only the measured content height applies.
        let height = if long { 240 } else { -1 };
        if scroll.min_content_height() != height {
            scroll.set_min_content_height(height);
        }
    }
    fit(&scroll, view.upcast_ref());
    for adjustment in [scroll.hadjustment(), scroll.vadjustment()] {
        let (weak_scroll, weak_view) = (scroll.downgrade(), view.downgrade());
        adjustment.connect_changed(move |_| {
            if let (Some(scroll), Some(view)) = (weak_scroll.upgrade(), weak_view.upgrade()) {
                fit(&scroll, view.upcast_ref());
            }
        });
    }
    let weak_scroll = scroll.downgrade();
    let weak_view = view.downgrade();
    view.buffer().connect_changed(move |_| {
        if let (Some(scroll), Some(view)) = (weak_scroll.upgrade(), weak_view.upgrade()) {
            fit(&scroll, view.upcast_ref());
        }
    });
    scroll
}

/// Height of the view's text laid out with its own font, margins and wrap
/// mode at `width` (unwrapped when the width is unknown).
fn content_height(view: &gtk::TextView, width: i32) -> i32 {
    let buffer = view.buffer();
    let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
    let layout = view.create_pango_layout(Some(&text));
    if width > 0 {
        let available = (width - view.left_margin() - view.right_margin()).max(1);
        layout.set_width(available * gtk::pango::SCALE);
        layout.set_wrap(gtk::pango::WrapMode::WordChar);
    }
    let paragraphs = text.split('\n').count() as i32;
    let spacing = paragraphs * (view.pixels_above_lines() + view.pixels_below_lines())
        + (layout.line_count() - paragraphs).max(0) * view.pixels_inside_wrap();
    (layout.pixel_size().1 + view.top_margin() + view.bottom_margin() + spacing).max(1)
}

/// A read-only TextView that keeps every standard keyboard, selection and
/// copy behavior, but trades height for width. GtkTextView's own vertical
/// measure ignores the offered width and reports its last, possibly
/// unvalidated layout, so a wrapped list could not reserve its lines at a new
/// width. Its height is capped at 240px; the scroller adds a scrollbar above it.
mod values_view_imp {
    use gtk::glib;
    use gtk::subclass::prelude::*;

    #[derive(Default)]
    pub struct ValuesView;

    #[gtk::glib::object_subclass]
    impl ObjectSubclass for ValuesView {
        const NAME: &'static str = "ToolportRecoveryValuesView";
        type Type = super::ValuesView;
        type ParentType = gtk::TextView;
    }
    impl ObjectImpl for ValuesView {}
    impl WidgetImpl for ValuesView {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let parent = self.parent_measure(orientation, for_size);
            if orientation == gtk::Orientation::Horizontal {
                return parent;
            }
            use gtk::prelude::*;
            let view = self.obj();
            let width = if for_size > 0 { for_size } else { view.width() };
            let height = super::content_height(view.upcast_ref(), width).min(240);
            (height, height, -1, -1)
        }
    }
    impl TextViewImpl for ValuesView {}
}

gtk::glib::wrapper! {
    pub struct ValuesView(ObjectSubclass<values_view_imp::ValuesView>)
        @extends gtk::TextView, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Scrollable;
}

fn values_list(record: &crate::clients::recovery::ClientRecovery) -> gtk::ScrolledWindow {
    let scroll = values_view(&bullets(&record.value_lines), "Missing saved values");
    let view = scroll.child().and_downcast::<gtk::TextView>().unwrap();
    let weak = view.downgrade();
    let id = record.id.clone();
    // Reason lines are translated by the backend; follow a language change.
    l10n::on_language_change(&view, move || {
        let Some(view) = weak.upgrade() else {
            return;
        };
        if let Some(record) = crate::clients::recovery::list()
            .ok()
            .and_then(|rows| rows.into_iter().find(|r| r.id == id))
        {
            set_values_text(&view.buffer(), &bullets(&record.value_lines));
        }
    });
    scroll
}

/// A standard focusable disclosure. GtkExpander trapped Tab while collapsed.
fn details_disclosure(
    record: &crate::clients::recovery::ClientRecovery,
    expanded_set: &std::rc::Rc<std::cell::RefCell<std::collections::HashSet<String>>>,
) -> gtk::Box {
    let expanded = expanded_set.borrow().contains(&record.id);
    let container = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let toggle = gtk::ToggleButton::new();
    toggle.add_css_class("flat");
    toggle.set_halign(gtk::Align::Start);
    toggle.set_widget_name(&format!("client-recovery-details:{}", record.id));
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let arrow = gtk::Image::from_icon_name(if expanded {
        "pan-down-symbolic"
    } else {
        "pan-end-symbolic"
    });
    content.append(&arrow);
    content.append(&l10n::label("Details"));
    toggle.set_child(Some(&content));
    l10n::bind_accessible_label(&toggle, "Details");
    toggle.set_active(expanded);
    toggle.update_state(&[gtk::accessible::State::Expanded(Some(expanded))]);
    // Details may be a keyboard selection stop so its text can be copied.
    let body = recovery_label(record, |record| &record.details);
    body.set_focusable(true);
    body.set_visible(expanded);
    let set = expanded_set.clone();
    let id = record.id.clone();
    let weak_body = body.downgrade();
    toggle.connect_toggled(move |toggle| {
        let open = toggle.is_active();
        if open {
            set.borrow_mut().insert(id.clone());
        } else {
            set.borrow_mut().remove(&id);
        }
        arrow.set_icon_name(Some(if open {
            "pan-down-symbolic"
        } else {
            "pan-end-symbolic"
        }));
        toggle.update_state(&[gtk::accessible::State::Expanded(Some(open))]);
        if let Some(body) = weak_body.upgrade() {
            body.set_visible(open);
        }
    });
    container.append(&toggle);
    container.append(&body);
    container
}

/// Recovery confirmations use the React layout: the heading with a close
/// control, the content, then Cancel and the action side by side. `respond`
/// runs once, with true only for the action; Escape and close mean Cancel.
fn recovery_dialog(
    app: &adw::Application,
    parent: Option<&gtk::Window>,
    heading: &str,
    content: &gtk::Widget,
    action_label: &str,
    destructive: bool,
    respond: impl Fn(bool) + 'static,
) -> gtk::Window {
    let window = gtk::Window::builder()
        .application(app)
        .modal(true)
        .resizable(false)
        .default_width(440)
        .title(heading)
        .build();
    window.set_transient_for(parent);
    window.set_widget_name("client-recovery-dialog");
    // The heading row carries the close control, so no separate title bar.
    window.set_titlebar(Some(&gtk::Box::new(gtk::Orientation::Horizontal, 0)));
    let outer = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(16)
        .margin_top(20)
        .margin_bottom(20)
        .margin_start(20)
        .margin_end(20)
        .build();
    let top = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let title = gtk::Label::builder()
        .label(heading)
        .attributes(&no_hyphens())
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .xalign(0.0)
        .hexpand(true)
        .css_classes(["title-3"])
        .build();
    top.append(&title);
    let close = gtk::Button::builder()
        .icon_name("window-close-symbolic")
        .valign(gtk::Align::Start)
        .css_classes(["flat", "circular"])
        .build();
    close.set_widget_name("client-recovery-dialog-close");
    l10n::bind_tooltip(&close, "Close");
    l10n::bind_accessible_label(&close, "Close");
    top.append(&close);
    outer.append(&top);
    outer.append(content);
    outer.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    let buttons = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::End)
        .build();
    let cancel = l10n::button("Cancel");
    cancel.set_widget_name("client-recovery-dialog-cancel");
    let action = gtk::Button::with_label(action_label);
    action.set_widget_name("client-recovery-dialog-action");
    action.add_css_class(if destructive {
        "destructive-action"
    } else {
        "suggested-action"
    });
    buttons.append(&cancel);
    buttons.append(&action);
    outer.append(&buttons);
    window.set_child(Some(&outer));

    // Every path closes the window first and answers once on idle after it
    // closed: rebuilding the page inside close-request kept the dialog open.
    let choice = std::rc::Rc::new(std::cell::Cell::new(None::<bool>));
    for (button, chosen) in [(&cancel, false), (&close, false), (&action, true)] {
        let window = window.downgrade();
        let choice = choice.clone();
        button.connect_clicked(move |_| {
            if choice.get().is_none() {
                choice.set(Some(chosen));
            }
            if let Some(window) = window.upgrade() {
                window.close();
            }
        });
    }
    let answered = std::cell::Cell::new(false);
    let respond = std::rc::Rc::new(respond);
    window.connect_close_request(move |_| {
        if !answered.replace(true) {
            let chosen = choice.get().unwrap_or(false);
            let respond = respond.clone();
            gtk::glib::idle_add_local_once(move || respond(chosen));
        }
        gtk::glib::Propagation::Proceed
    });
    // Capture phase: the dialog sees Escape before the focused widget, so one
    // press closes it wherever focus is.
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = window.downgrade();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key != gtk::gdk::Key::Escape {
            return gtk::glib::Propagation::Proceed;
        }
        if let Some(window) = weak.upgrade() {
            window.close();
        }
        gtk::glib::Propagation::Stop
    });
    window.add_controller(keys);
    window
}

fn working_status() -> gtk::Box {
    let status = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .accessible_role(gtk::AccessibleRole::Status)
        .build();
    status.set_widget_name("client-recovery-working");
    let spinner = gtk::Spinner::builder().spinning(true).build();
    status.append(&spinner);
    let label = l10n::label("Working on this recovery…");
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    status.append(&label);
    status.update_state(&[gtk::accessible::State::Busy(true)]);
    status
}

/// FlowBoxChild is focusable by default, which adds a second Tab stop before
/// each button. Only the button takes focus.
fn insert_action(actions: &gtk::FlowBox, button: &gtk::Button) {
    actions.insert(button, -1);
    if let Some(child) = button.parent() {
        child.set_focusable(false);
    }
}

/// Rescan client rows after a Retry that refreshed waiting client configs,
/// and after a failed one, which may have refreshed them before it failed.
fn rescan_after_retry<E>(
    result: &Result<Result<crate::clients::recovery::RecoveryAttempt, String>, E>,
) -> bool {
    match result {
        Ok(Ok(attempt)) => attempt.refreshed,
        Ok(Err(_)) => true,
        Err(_) => false,
    }
}

impl ClientPage {
    /// Lists from the recovery files only; it never contacts the credential
    /// store, so it is safe on the UI thread.
    pub(super) fn reload_recoveries(&self) {
        match crate::clients::recovery::list() {
            Ok(records) => *self.recoveries.borrow_mut() = records,
            Err(error) => self.show_error(&error),
        }
        self.rebuild_recoveries();
    }

    fn rebuild_recoveries(&self) {
        while let Some(child) = self.recovery_box.first_child() {
            self.recovery_box.remove(&child);
        }
        self.append_recoveries(&self.recovery_box, None);
    }

    pub(super) fn refresh_open_detail(&self) {
        let selected = self.details.selected.borrow().clone();
        if let Some(client) = self
            .detail_clients
            .borrow()
            .iter()
            .find(|client| selected.as_deref() == Some(&client.id))
            .cloned()
        {
            details::client(&client, self);
        }
    }

    fn set_recovery_result(&self, client_id: &str, message: String, success: bool) {
        if success {
            self.clear_client_error(client_id);
        }
        *self.recovery_success.borrow_mut() = Some(RecoveryResult {
            client_id: client_id.into(),
            message,
            success,
        });
    }

    pub(super) fn append_recoveries(&self, parent: &gtk::Box, client: Option<&str>) {
        if let Some(result) = self.recovery_success.borrow().as_ref() {
            if client.is_none_or(|client| client == result.client_id) {
                // The single result line receives focus when an action finishes.
                let label = gtk::Label::builder()
                    .label(&result.message)
                    .xalign(0.0)
                    .halign(gtk::Align::Fill)
                    .hexpand(true)
                    .wrap(true)
                    .wrap_mode(gtk::pango::WrapMode::WordChar)
                    .selectable(true)
                    .accessible_role(gtk::AccessibleRole::Status)
                    .build();
                label.set_focusable(true);
                label.add_css_class("toolport-feedback");
                label.add_css_class(if result.success { "success" } else { "idle" });
                label.set_widget_name("client-recovery-result");
                parent.append(&label);
            }
        }
        let busy = self.recovery_busy.get();
        let active = self.recovery_active.borrow().clone();
        for record in self.recoveries.borrow().iter() {
            if client.is_some_and(|id| id != record.client_id) {
                continue;
            }
            let working = active.as_deref() == Some(record.id.as_str());
            let card = gtk::Box::new(gtk::Orientation::Vertical, 8);
            card.add_css_class("toolport-feedback");
            card.add_css_class("warning");
            card.set_valign(gtk::Align::Start);
            card.set_vexpand(false);
            card.set_widget_name(&format!("client-recovery:{}", record.id));
            card.update_state(&[gtk::accessible::State::Busy(working)]);
            let feedback = self
                .recovery_feedback
                .borrow()
                .iter()
                .find(|(id, _)| id == &record.id)
                .map(|(_, text)| text.clone());
            if record.dismissed {
                let heading = prose("");
                heading.add_css_class("heading");
                l10n::bind_label(&heading, "Recovery notice dismissed");
                card.append(&heading);
                card.append(&recovery_label(record, |record| &record.retention));
                if let Some(message) = &feedback {
                    card.append(&prose(message));
                }
                if working {
                    card.append(&working_status());
                }
                let show = l10n::button("Show recovery");
                show.set_halign(gtk::Align::Start);
                show.set_sensitive(!busy);
                show.set_widget_name(&format!("client-recovery-show:{}", record.id));
                let page = self.clone();
                let id = record.id.clone();
                show.connect_clicked(move |button| {
                    page.set_recovery_dismissed(&id, false, button);
                });
                card.append(&show);
                parent.append(&card);
                continue;
            }
            let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            heading.set_valign(gtk::Align::Start);
            let icon = gtk::Image::from_icon_name("dialog-warning-symbolic");
            icon.add_css_class("warning");
            icon.set_valign(gtk::Align::Start);
            icon.set_pixel_size(16);
            heading.append(&icon);
            let title = message_label(record, true);
            title.add_css_class("heading");
            heading.append(&title);
            card.append(&heading);
            card.append(&message_label(record, false));
            if !record.value_lines.is_empty() {
                card.append(&values_list(record));
            }
            card.append(&recovery_label(record, |record| &record.instructions));
            card.append(&recovery_label(record, |record| &record.retention));
            if let Some(feedback) = feedback.or_else(|| {
                record
                    .error
                    .clone()
                    .filter(|error| error != &record.description)
            }) {
                card.append(&prose(&feedback));
            }
            if working {
                card.append(&working_status());
            }
            let actions = gtk::FlowBox::builder()
                .selection_mode(gtk::SelectionMode::None)
                .column_spacing(8)
                .row_spacing(8)
                .halign(gtk::Align::Start)
                .min_children_per_line(1)
                .max_children_per_line(3)
                .build();
            if record.can_retry {
                let retry = l10n::button("Retry");
                retry.add_css_class("suggested-action");
                retry.set_sensitive(!busy);
                retry.set_widget_name(&format!("client-recovery-retry:{}", record.id));
                let page = self.clone();
                let record_for_retry = record.clone();
                retry.connect_clicked(move |button| {
                    page.retry_recovery(&record_for_retry, button);
                });
                insert_action(&actions, &retry);
            }
            if record.can_discard || record.can_discard_file {
                if record.can_discard && !record.discard_started {
                    let dismiss = l10n::button("Dismiss");
                    dismiss.set_sensitive(!busy);
                    dismiss.set_widget_name(&format!("client-recovery-dismiss:{}", record.id));
                    let page = self.clone();
                    let record_for_dismiss = record.clone();
                    dismiss.connect_clicked(move |_| {
                        page.confirm_dismiss_recovery(&record_for_dismiss)
                    });
                    insert_action(&actions, &dismiss);
                }
                let discard = gtk::Button::new();
                let id = record.id.clone();
                let fallback = record.discard_action.clone();
                // Backend label: "Discard these saved values" or "Finish discarding".
                l10n::bind_button_with(&discard, move || {
                    current_text(&id, |r| &r.discard_action, &fallback)
                });
                if record.discard_started {
                    discard.add_css_class("destructive-action");
                }
                discard.set_sensitive(!busy);
                discard.set_widget_name(&format!("client-recovery-discard:{}", record.id));
                let page = self.clone();
                let record = record.clone();
                discard.connect_clicked(move |_| page.prepare_discard_recovery(&record));
                insert_action(&actions, &discard);
            }
            card.append(&actions);
            card.append(&details_disclosure(record, &self.recovery_expanded));
            parent.append(&card);
        }
    }

    /// Every new action supersedes the shown result and this row's old feedback,
    /// never other rows' errors. All rows' actions are disabled while it runs.
    fn begin_recovery_action(&self, id: &str) {
        self.recovery_feedback
            .borrow_mut()
            .retain(|(row, _)| row != id);
        self.recovery_success.borrow_mut().take();
        self.recovery_busy.set(true);
        *self.recovery_active.borrow_mut() = Some(id.to_string());
        // Rebuild from the current rows only: the action itself relists after it runs.
        self.rebuild_recoveries();
        self.refresh_open_detail();
    }

    fn end_recovery_action(&self) {
        self.recovery_busy.set(false);
        self.recovery_active.borrow_mut().take();
    }

    fn retry_recovery(
        &self,
        record: &crate::clients::recovery::ClientRecovery,
        button: &gtk::Button,
    ) {
        button.set_sensitive(false);
        self.recovery_expanded.borrow_mut().remove(&record.id);
        if !record.can_discard {
            self.refresh_error_recovery(record);
            return;
        }
        self.begin_recovery_action(&record.id);
        let page = self.clone();
        let record = record.clone();
        gtk::glib::spawn_future_local(async move {
            let id = record.id.clone();
            let result = gtk::gio::spawn_blocking(move || {
                crate::clients::recovery::retry_with_report(&id)
                    .map_err(|e| crate::clients::recovery::safe_action_error(&id, &e))
            })
            .await;
            page.end_recovery_action();
            page.recovery_feedback
                .borrow_mut()
                .retain(|(id, _)| id != &record.id);
            let rescan = rescan_after_retry(&result);
            match result {
                Ok(Ok(result)) if result.pending.is_none() => {
                    page.set_recovery_result(&record.client_id, result.message, true);
                }
                Ok(Ok(result)) => page
                    .recovery_feedback
                    .borrow_mut()
                    .push((record.id.clone(), result.message)),
                Ok(Err(error)) => page
                    .recovery_feedback
                    .borrow_mut()
                    .push((record.id.clone(), error)),
                Err(_) => page
                    .recovery_feedback
                    .borrow_mut()
                    .push((record.id.clone(), crate::clients::recovery::worker_error())),
            }
            page.reload_recoveries();
            page.refresh_open_detail();
            if rescan {
                page.refresh_with(false);
            }
            focus_first_later(
                &page.root,
                vec![
                    format!("client-recovery-retry:{}", record.id),
                    "client-recovery-result".into(),
                ],
            );
        });
    }

    /// An unreadable or untrusted record. The backend reclassifies the whole
    /// read-only inventory: a replacement error keeps focus and never reports
    /// success, and a truly removed record gets a neutral refreshed message.
    fn refresh_error_recovery(&self, record: &crate::clients::recovery::ClientRecovery) {
        self.begin_recovery_action(&record.id);
        let page = self.clone();
        let record = record.clone();
        gtk::glib::spawn_future_local(async move {
            let (id, client, backup) = (
                record.id.clone(),
                record.client_id.clone(),
                record.backup_path.clone(),
            );
            let update = gtk::gio::spawn_blocking(move || {
                crate::clients::recovery::refresh_notice(&id, &client, &backup)
            })
            .await;
            page.end_recovery_action();
            let mut focus = Vec::new();
            match update {
                Ok(Ok(update)) => {
                    *page.recoveries.borrow_mut() = update.rows;
                    if let Some(message) = update.message {
                        page.set_recovery_result(&record.client_id, message, false);
                        focus.push("client-recovery-result".to_string());
                    } else if let Some(id) = update.focus_id {
                        focus.push(format!("client-recovery-retry:{id}"));
                    }
                    page.rebuild_recoveries();
                }
                Ok(Err(error)) => {
                    page.recovery_feedback
                        .borrow_mut()
                        .push((record.id.clone(), error));
                    page.reload_recoveries();
                }
                Err(_) => {
                    page.recovery_feedback
                        .borrow_mut()
                        .push((record.id.clone(), crate::clients::recovery::worker_error()));
                    page.reload_recoveries();
                }
            }
            page.refresh_open_detail();
            focus.push(format!("client-recovery-retry:{}", record.id));
            focus_first_later(&page.root, focus);
        });
    }

    fn confirm_dismiss_recovery(&self, record: &crate::clients::recovery::ClientRecovery) {
        let parent = self
            .app
            .active_window()
            .or_else(|| self.root.root().and_downcast::<gtk::Window>());
        let page = self.clone();
        let id = record.id.clone();
        recovery_dialog(
            &self.app,
            parent.as_ref(),
            &crate::i18n::tr("Dismiss recovery notice?"),
            recovery_label(record, |record| &record.retention).upcast_ref(),
            &crate::i18n::tr("Dismiss"),
            false,
            move |dismiss| {
                if dismiss {
                    // A temporary button supplies the same busy guard as Show.
                    page.set_recovery_dismissed(&id, true, &gtk::Button::new());
                } else {
                    focus_first_later(&page.root, vec![format!("client-recovery-dismiss:{id}")]);
                }
            },
        )
        .present();
    }

    fn set_recovery_dismissed(&self, id: &str, dismissed: bool, button: &gtk::Button) {
        button.set_sensitive(false);
        self.begin_recovery_action(id);
        let page = self.clone();
        let id = id.to_string();
        let focus_id = id.clone();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                crate::clients::recovery::dismiss(&id, dismissed)
                    .map_err(|e| crate::clients::recovery::safe_action_error(&id, &e))
            })
            .await;
            match result {
                Ok(Ok(())) => {
                    page.end_recovery_action();
                    if dismissed {
                        page.recovery_expanded.borrow_mut().remove(&focus_id);
                    }
                    page.reload_recoveries();
                    page.refresh_open_detail();
                    focus_first_later(
                        &page.root,
                        vec![format!(
                            "client-recovery-{}:{focus_id}",
                            if dismissed { "show" } else { "retry" }
                        )],
                    );
                }
                Ok(Err(error)) => page.recovery_action_error(&focus_id, error, "retry"),
                Err(_) => page.recovery_action_error(
                    &focus_id,
                    crate::clients::recovery::worker_error(),
                    "retry",
                ),
            }
        });
    }

    /// Show a failed action on its own row, relisted so a changed phase (for
    /// example a started Discard) is current, and focus that row's action.
    fn recovery_action_error(&self, id: &str, error: String, action: &str) {
        self.end_recovery_action();
        self.recovery_feedback
            .borrow_mut()
            .retain(|(row, _)| row != id);
        self.recovery_feedback.borrow_mut().push((id.into(), error));
        self.reload_recoveries();
        self.refresh_open_detail();
        focus_first_later(
            &self.root,
            [action, "retry", "discard", "show"]
                .into_iter()
                .map(|action| format!("client-recovery-{action}:{id}"))
                .collect(),
        );
    }

    /// Discard is prepared lazily from the recovery files (never the vault), so
    /// the confirmation lists the exact inventory the manifest token covers.
    fn prepare_discard_recovery(&self, record: &crate::clients::recovery::ClientRecovery) {
        self.begin_recovery_action(&record.id);
        let page = self.clone();
        let record = record.clone();
        gtk::glib::spawn_future_local(async move {
            let id = record.id.clone();
            let result = gtk::gio::spawn_blocking(move || {
                crate::clients::recovery::prepare_discard(&id)
                    .map_err(|e| crate::clients::recovery::safe_discard_error(&id, &e))
            })
            .await;
            match result {
                Ok(Ok(prepared)) => {
                    page.confirm_discard_recovery(&record, prepared);
                }
                Ok(Err(error)) => page.recovery_action_error(&record.id, error, "discard"),
                Err(_) => page.recovery_action_error(
                    &record.id,
                    crate::clients::recovery::discard_worker_error(&record.id),
                    "discard",
                ),
            }
        });
    }

    /// The row stays busy ("Working on this recovery…", actions greyed) until
    /// the confirmation closes, as in the React shell.
    fn confirm_discard_recovery(
        &self,
        record: &crate::clients::recovery::ClientRecovery,
        prepared: crate::clients::recovery::DiscardConfirmation,
    ) -> gtk::Window {
        let parent = self
            .app
            .active_window()
            .or_else(|| self.root.root().and_downcast::<gtk::Window>());
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.append(&prose(&prepared.description));
        // One bounded list holds both the values and the exact backup files.
        let list = (!prepared.lines.is_empty()).then(|| {
            values_view(
                &bullets(&prepared.lines),
                "Saved values and files to delete",
            )
        });
        if let Some(list) = &list {
            content.append(list);
        }
        if !prepared.location.is_empty() {
            content.append(&prose(&prepared.location));
        }
        let page = self.clone();
        let record = record.clone();
        let manifest = prepared.manifest;
        let dialog = recovery_dialog(
            &self.app,
            parent.as_ref(),
            &prepared.heading,
            content.upcast_ref(),
            &prepared.action_label,
            true,
            move |discard| {
                if discard {
                    page.discard_recovery(&record, manifest.clone());
                    return;
                }
                page.end_recovery_action();
                page.rebuild_recoveries();
                page.refresh_open_detail();
                focus_first_later(
                    &page.root,
                    vec![format!("client-recovery-discard:{}", record.id)],
                );
            },
        );
        dialog.present();
        // First focus is the list being deleted, as in React.
        if let Some(view) = list.and_then(|list| list.child()) {
            view.grab_focus();
        }
        dialog
    }

    fn discard_recovery(
        &self,
        record: &crate::clients::recovery::ClientRecovery,
        manifest: String,
    ) {
        self.begin_recovery_action(&record.id);
        let page = self.clone();
        let record = record.clone();
        gtk::glib::spawn_future_local(async move {
            let id = record.id.clone();
            let result = gtk::gio::spawn_blocking(move || {
                crate::clients::recovery::discard_confirmed(&id, &manifest)
                    .map_err(|e| crate::clients::recovery::safe_discard_error(&id, &e))
            })
            .await;
            match result {
                Ok(Ok(message)) => {
                    page.end_recovery_action();
                    page.set_recovery_result(&record.client_id, message, true);
                    page.reload_recoveries();
                    page.refresh_open_detail();
                    focus_first_later(&page.root, vec!["client-recovery-result".into()]);
                }
                Ok(Err(error)) => page.recovery_action_error(&record.id, error, "discard"),
                Err(_) => page.recovery_action_error(
                    &record.id,
                    crate::clients::recovery::discard_worker_error(&record.id),
                    "discard",
                ),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    /// A failed Retry may already have refreshed waiting client configs, so
    /// the client rows are rescanned then too.
    #[test]
    fn a_failed_retry_still_rescans_client_rows() {
        let attempt = |refreshed| crate::clients::recovery::RecoveryAttempt {
            id: "r".into(),
            client_id: "cursor".into(),
            client_name: "Cursor".into(),
            restored: 0,
            accounted: 0,
            remaining: 1,
            message: String::new(),
            pending: None,
            refreshed,
        };
        let ok = |a| -> Result<Result<_, String>, ()> { Ok(Ok(a)) };
        assert!(rescan_after_retry(&ok(attempt(true))));
        assert!(!rescan_after_retry(&ok(attempt(false))));
        assert!(rescan_after_retry::<()>(&Ok(Err("restore failed".into()))));
        assert!(!rescan_after_retry::<()>(&Err(())));
    }

    /// F5: the recovery card's labels re-read their text on the GTK main
    /// thread. With a locked keyring that must make no credential-store call,
    /// so it can never wait on (or raise) an unlock prompt.
    #[test]
    fn card_labels_read_their_text_without_the_credential_store() {
        crate::secrets::tests::with_isolated_vault(|| {
            let data = crate::registry::conduit_dir().unwrap();
            let card =
                crate::clients::recovery::locked_card_fixture("cursor", &data.join("gtk.json"));
            let mut texts = Vec::new();
            let (calls, prompts) = crate::clients::backup_vault::fake::locked_everywhere(|| {
                texts.push(current_text(&card.id, |r| &r.message, "fallback"));
                texts.push(current_text(&card.id, |r| &r.instructions, "fallback"));
            });
            assert_eq!(calls, Vec::<String>::new(), "card text made store calls");
            assert_eq!(prompts, Vec::<String>::new(), "card text would prompt");
            assert_eq!(texts, vec![card.message.clone(), card.instructions.clone()]);
        });
    }

    #[test]
    #[ignore = "requires an isolated GTK desktop"]
    fn nested_recovery_lists_keep_their_bounded_content_height() {
        let _data = crate::registry::DataDirTestEnv::new("gtk-recovery-list-allocation");
        adw::init().unwrap();
        let mut record = crate::clients::recovery::ClientRecovery {
            id: "allocation".into(),
            client_id: "cursor".into(),
            client_name: "Cursor".into(),
            path: "/fixture/config.json".into(),
            backup_path: "/fixture/original.json".into(),
            missing: Vec::new(),
            dismissed: false,
            message: String::new(),
            description: String::new(),
            value_lines: vec!["API_KEY for server: keyring locked".into()],
            discard_values: Vec::new(),
            instructions: String::new(),
            retention: String::new(),
            details: String::new(),
            error: None,
            can_discard: true,
            can_discard_file: false,
            can_retry: true,
            discard_started: false,
            discard_action: "Discard these saved values".into(),
            cleanup: false,
        };
        let short = values_list(&record);
        record.value_lines = (0..20)
            .map(|index| format!("API_KEY_{index:02} for server: keyring locked"))
            .collect();
        let long = values_list(&record);
        record.value_lines = vec![format!(
            "API_KEY for {}: keyring locked",
            "long-server-name-".repeat(24)
        )];
        let wrapped = values_list(&record);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
        content.append(&short);
        content.append(&long);
        content.append(&wrapped);
        let remaining_clients = gtk::Label::new(Some("Other client rows"));
        remaining_clients.set_height_request(600);
        content.append(&remaining_clients);
        let outer = gtk::ScrolledWindow::builder().child(&content).build();
        let window = gtk::Window::builder().child(&outer).build();
        window.present();
        let context = gtk::glib::MainContext::default();
        let mut narrow_height = 0;
        // Everything the fit depends on, so a failure says which value was stale.
        let state = |scroll: &gtk::ScrolledWindow| {
            let view = scroll.child().and_downcast::<gtk::TextView>().unwrap();
            let buffer = view.buffer();
            let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
            format!(
                "height={} scroll_width={} view_width={} hadj_page={} vadj_upper={} \
                 min_content={} max_content={} policy={:?} view_measure={:?} view_measure_at_width={:?} \
                 scroll_measure_at_width={:?} scroll_request_mode={:?} \
                 window_width={} text_chars={}",
                scroll.height(),
                scroll.width(),
                view.width(),
                scroll.hadjustment().page_size(),
                scroll.vadjustment().upper(),
                scroll.min_content_height(),
                scroll.max_content_height(),
                scroll.vscrollbar_policy(),
                view.measure(gtk::Orientation::Vertical, -1),
                view.measure(gtk::Orientation::Vertical, view.width()),
                scroll.measure(gtk::Orientation::Vertical, scroll.width()),
                scroll.request_mode(),
                window.width(),
                text.chars().count(),
            )
        };
        for width in [480, 1200] {
            // The toplevel's own frame layout allocates `outer` too. Give it the
            // same width so both layouts agree and the list can settle.
            window.set_default_size(width, 400);
            for _ in 0..8 {
                while context.pending() {
                    context.iteration(false);
                }
                outer.allocate(width, 400, -1, None);
            }
            assert_eq!(
                long.height(),
                240,
                "long list at {width}px must use the shared 240px cap: {}",
                state(&long)
            );
            assert!(
                short.height() < 40,
                "one value at {width}px must not leave a scrollbar-sized gap: {}",
                state(&short)
            );
            if width == 480 {
                narrow_height = wrapped.height();
                assert!(
                    (40..=240).contains(&narrow_height),
                    "wrapped narrow list must reserve its lines: {}",
                    state(&wrapped)
                );
            } else {
                assert!(
                    wrapped.height() < narrow_height,
                    "widening must remove the space for unneeded wrapped lines \
                     (narrow {narrow_height}): {}",
                    state(&wrapped)
                );
            }
        }
        let short_view = short.child().unwrap().downcast::<gtk::TextView>().unwrap();
        for (text, expected_height) in [
            (vec!["Translated value"; 20].join("\n"), 240),
            ("One value".into(), 15),
        ] {
            short_view.buffer().set_text(&text);
            for _ in 0..8 {
                while context.pending() {
                    context.iteration(false);
                }
                outer.allocate(1200, 400, -1, None);
            }
            if expected_height == 240 {
                assert_eq!(
                    short.height(),
                    240,
                    "changed text must enable bounded scrolling: {}",
                    state(&short)
                );
            } else {
                assert!(
                    short.height() < 40,
                    "changed text must remove the scrollbar gap: {}",
                    state(&short)
                );
            }
        }
        window.close();
    }

    use super::*;

    #[test]
    #[ignore = "requires an isolated GTK desktop; run in omabox"]
    fn recovery_notice_is_selectable_wrapped_and_has_explicit_actions() {
        let _data = crate::registry::DataDirTestEnv::new("gtk-recovery-notice");
        adw::init().unwrap();
        let app = adw::Application::builder()
            .application_id("com.tsout.Toolport.RecoveryNoticeTest")
            .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(gtk::gio::Cancellable::NONE).unwrap();
        let (_, servers, _) = build_content(&app, crate::approval_broker::start_native());
        let page = ClientPage::new(&app, servers);
        let values = (0..20)
            .map(|index| format!("• API_KEY_{index:02} for server-{index:02}"))
            .collect::<Vec<_>>()
            .join("\n");
        let body = format!("Its original config is back, except for values Toolport could not read because your keyring is locked or unavailable:\n\n{values}\n\nThe config has placeholders for these values for now.");
        let message = format!("Cursor is disconnected.\n\n{body}");
        let record = crate::clients::recovery::ClientRecovery {
            id: "fixture-receipt".into(),
            client_id: "cursor".into(),
            client_name: "Cursor".into(),
            path: "/fixture/cursor.json".into(),
            backup_path: "/fixture/original.json".into(),
            missing: (0..20)
                .map(|index| crate::clients::backup_storage::Reference {
                    server: format!("server-{index:02}"),
                    key: format!("API_KEY_{index:02}"),
                })
                .collect(),
            dismissed: false,
            message: message.clone(),
            description: "Its original config is back. These values still have placeholders:"
                .into(),
            value_lines: values
                .lines()
                .map(|s| s.trim_start_matches("• ").to_string())
                .collect(),
            discard_values: values.lines().map(str::to_string).collect(),
            error: None,
            can_discard: true,
            can_discard_file: false,
            can_retry: true,
            discard_started: false,
            discard_action: "Discard these saved values".into(),
            cleanup: false,
            instructions: "Unlock your keyring, then choose Retry.".into(),
            retention: "The recovery backup does not expire automatically.".into(),
            details: "TOOLPORT_MISSING_CREDENTIAL_0".into(),
        };
        page.recoveries.borrow_mut().push(record.clone());
        page.append_recoveries(&page.recovery_box, None);
        let card = page.recovery_box.first_child().unwrap();
        let heading = card.first_child().unwrap().downcast::<gtk::Box>().unwrap();
        let icon = heading
            .first_child()
            .unwrap()
            .downcast::<gtk::Image>()
            .unwrap();
        assert_eq!(icon.icon_name().as_deref(), Some("dialog-warning-symbolic"));
        let title = heading
            .last_child()
            .unwrap()
            .downcast::<gtk::Label>()
            .unwrap();
        assert_eq!(title.text(), "Cursor is disconnected.");
        assert!(title.has_css_class("heading"));
        assert_eq!(title.xalign(), 0.0);
        assert!(title.is_selectable());
        let label = heading
            .next_sibling()
            .unwrap()
            .downcast::<gtk::Label>()
            .unwrap();
        assert_eq!(label.text(), record.description);
        let scroll = label
            .next_sibling()
            .unwrap()
            .downcast::<gtk::ScrolledWindow>()
            .unwrap();
        assert_eq!(scroll.max_content_height(), 240);
        assert!(scroll.propagates_natural_height());
        assert!(scroll.has_css_class("toolport-recovery-values"));
        // A read-only TextView is the one keyboard stop for the bounded list:
        // standard scrolling, selection and Ctrl+C, and Tab moves on.
        let values = scroll.child().unwrap().downcast::<gtk::TextView>().unwrap();
        let buffer = values.buffer();
        let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
        for index in 0..20 {
            assert!(text.contains(&format!("API_KEY_{index:02}")));
        }
        assert!(!values.is_editable());
        assert!(!values.accepts_tab());
        assert!(!values.is_cursor_visible());
        assert!(values.is_focusable());
        assert_eq!(values.wrap_mode(), gtk::WrapMode::WordChar);
        assert!(label.is_selectable());
        assert!(label.wraps());
        assert_eq!(label.wrap_mode(), gtk::pango::WrapMode::WordChar);
        assert_eq!(label.valign(), gtk::Align::Start);
        assert_eq!(label.ellipsize(), gtk::pango::EllipsizeMode::None);
        assert!(!label.text().contains("TOOLPORT_MISSING_CREDENTIAL"));
        // Prose is selectable but never a Tab stop.
        fn focusable_labels(widget: &gtk::Widget, found: &mut Vec<String>) {
            if let Ok(label) = widget.clone().downcast::<gtk::Label>() {
                if label.is_focusable() && label.is_visible() {
                    found.push(label.text().to_string());
                }
            }
            let mut child = widget.first_child();
            while let Some(next) = child {
                focusable_labels(&next, found);
                child = next.next_sibling();
            }
        }
        let mut stops = Vec::new();
        focusable_labels(&card, &mut stops);
        assert!(
            stops.is_empty(),
            "collapsed notice has no prose Tab stops: {stops:?}"
        );
        let mut child = card.first_child();
        while let Some(widget) = child {
            if let Ok(scroll) = widget.clone().downcast::<gtk::ScrolledWindow>() {
                assert_eq!(scroll.max_content_height(), 240);
            }
            child = widget.next_sibling();
        }
        // Details is a standard toggle button, not a GtkExpander that traps Tab.
        let details = card.last_child().unwrap().downcast::<gtk::Box>().unwrap();
        let toggle = details
            .first_child()
            .unwrap()
            .downcast::<gtk::ToggleButton>()
            .unwrap();
        let body = details
            .last_child()
            .unwrap()
            .downcast::<gtk::Label>()
            .unwrap();
        assert!(toggle.is_focusable());
        assert!(!toggle.is_active());
        assert!(!body.is_visible());
        assert!(body.text().contains("TOOLPORT_MISSING_CREDENTIAL_0"));
        toggle.set_active(true);
        assert!(body.is_visible());
        assert!(
            body.is_focusable(),
            "Details text may be a keyboard selection stop"
        );
        assert!(page.recovery_expanded.borrow().contains(&record.id));
        toggle.set_active(false);
        assert!(!body.is_visible());
        assert!(!page.recovery_expanded.borrow().contains(&record.id));
        let actions = details
            .prev_sibling()
            .unwrap()
            .downcast::<gtk::FlowBox>()
            .unwrap();
        let button = |index| {
            let child = actions.child_at_index(index).unwrap();
            assert!(!child.is_focusable(), "FlowBox wrappers are not Tab stops");
            child.child().unwrap().downcast::<gtk::Button>().unwrap()
        };
        assert_eq!(button(0).label().as_deref(), Some("Retry"));
        assert_eq!(button(1).label().as_deref(), Some("Dismiss"));
        assert_eq!(
            button(2).label().as_deref(),
            Some("Discard these saved values")
        );
        while let Some(child) = page.recovery_box.first_child() {
            page.recovery_box.remove(&child);
        }
        page.recoveries.borrow_mut()[0].dismissed = true;
        page.append_recoveries(&page.recovery_box, None);
        let dismissed = page.recovery_box.first_child().unwrap();
        assert_eq!(
            dismissed
                .last_child()
                .unwrap()
                .downcast::<gtk::Button>()
                .unwrap()
                .label()
                .as_deref(),
            Some("Show recovery")
        );
        assert!(dismissed
            .first_child()
            .unwrap()
            .downcast::<gtk::Label>()
            .unwrap()
            .text()
            .contains("Recovery notice dismissed"));

        // A started Discard offers only Finish discarding: no restoration Retry
        // and no plain Dismiss.
        while let Some(child) = page.recovery_box.first_child() {
            page.recovery_box.remove(&child);
        }
        {
            let mut rows = page.recoveries.borrow_mut();
            rows[0].dismissed = false;
            rows[0].can_retry = false;
            rows[0].discard_started = true;
            rows[0].discard_action = "Finish discarding".into();
        }
        page.append_recoveries(&page.recovery_box, None);
        let started = page.recovery_box.first_child().unwrap();
        let actions = started
            .last_child()
            .unwrap()
            .prev_sibling()
            .unwrap()
            .downcast::<gtk::FlowBox>()
            .unwrap();
        assert!(actions.child_at_index(1).is_none());
        assert_eq!(
            actions
                .child_at_index(0)
                .unwrap()
                .child()
                .unwrap()
                .downcast::<gtk::Button>()
                .unwrap()
                .label()
                .as_deref(),
            Some("Finish discarding")
        );
        {
            let mut rows = page.recoveries.borrow_mut();
            rows[0].can_retry = true;
            rows[0].discard_started = false;
            rows[0].discard_action = "Discard these saved values".into();
        }

        // One latest result in total: a newer result replaces the older one.
        page.set_recovery_result("cursor", "Cursor: values put back: 1.".into(), true);
        page.set_recovery_result("codex", "Codex: values put back: 1.".into(), true);
        while let Some(child) = page.recovery_box.first_child() {
            page.recovery_box.remove(&child);
        }
        page.append_recoveries(&page.recovery_box, None);
        let mut results = Vec::new();
        let mut child = page.recovery_box.first_child();
        while let Some(widget) = child {
            if widget.widget_name() == "client-recovery-result" {
                results.push(widget.clone().downcast::<gtk::Label>().unwrap().text());
            }
            child = widget.next_sibling();
        }
        assert_eq!(results, vec!["Codex: values put back: 1."]);

        // A tray/background action has no active parent window. Confirmation
        // must still open, show the exact prepared inventory in one bounded
        // list, and confirmed deletion must guard every other action.
        assert!(app.active_window().is_none());
        page.recovery_feedback.borrow_mut().push((
            "other-receipt".into(),
            "Keep another client's feedback".into(),
        ));
        let prepared = crate::clients::recovery::DiscardConfirmation {
            manifest: "manifest-1".into(),
            heading: "Discard these saved values?".into(),
            description: "Toolport will permanently delete the recovery copies of 20 values and 2 backup files for Cursor.".into(),
            lines: vec![
                "Value: API_KEY_00 for server-00".into(),
                "Backup file: original-1111.json".into(),
                "Backup file: 1794746461499-mcp.json".into(),
            ],
            location: "Recovery files to delete:\n/fixture".into(),
            action_label: "Discard these saved values".into(),
        };
        fn find(widget: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
            if widget.widget_name() == name {
                return Some(widget.clone());
            }
            let mut child = widget.first_child();
            while let Some(next) = child {
                if let Some(found) = find(&next, name) {
                    return Some(found);
                }
                child = next.next_sibling();
            }
            None
        }
        // Prepare marks the row busy; it stays busy while the dialog is open
        // and Cancel releases it, as in React.
        page.begin_recovery_action(&record.id);
        let dialog = page.confirm_discard_recovery(&record, prepared.clone());
        assert!(
            page.recovery_busy.get(),
            "busy while the confirmation is open"
        );
        let root = dialog.child().unwrap();
        find(&root, "client-recovery-dialog-cancel")
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap()
            .emit_clicked();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while page.recovery_busy.get() && std::time::Instant::now() < deadline {
            gtk::glib::MainContext::default().iteration(false);
        }
        assert!(!page.recovery_busy.get(), "Cancel releases the row");
        assert!(!dialog.is_visible());

        // A reload that empties the list under an open dialog keeps the dialog,
        // and closing it still releases the row: nothing stays "Working".
        let saved = page.recoveries.borrow().clone();
        page.begin_recovery_action(&record.id);
        let dialog = page.confirm_discard_recovery(&record, prepared.clone());
        page.recoveries.borrow_mut().clear();
        page.rebuild_recoveries();
        assert!(dialog.is_visible() && page.recovery_busy.get());
        dialog.close();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while page.recovery_busy.get() && std::time::Instant::now() < deadline {
            gtk::glib::MainContext::default().iteration(false);
        }
        assert!(!page.recovery_busy.get(), "closing releases an emptied row");
        *page.recoveries.borrow_mut() = saved;
        page.rebuild_recoveries();
        page.recovery_feedback
            .borrow_mut()
            .push((record.id.clone(), "Previous Retry failure".into()));

        let dialog = page.confirm_discard_recovery(&record, prepared.clone());
        assert!(dialog.transient_for().is_none());
        assert_eq!(
            dialog.title().as_deref(),
            Some("Discard these saved values?")
        );
        let root = dialog.child().unwrap();
        // React's layout: a close control, then Cancel and the action side by side.
        assert!(find(&root, "client-recovery-dialog-close").is_some());
        let cancel = find(&root, "client-recovery-dialog-cancel").unwrap();
        let action = find(&root, "client-recovery-dialog-action").unwrap();
        assert_eq!(cancel.next_sibling().as_ref(), Some(&action));
        assert_eq!(
            cancel
                .parent()
                .unwrap()
                .downcast::<gtk::Box>()
                .unwrap()
                .orientation(),
            gtk::Orientation::Horizontal
        );
        let extra = root.first_child().unwrap().next_sibling().unwrap();
        let mut lists = Vec::new();
        let mut texts = Vec::new();
        let mut child = extra.first_child();
        while let Some(widget) = child {
            if let Ok(scroll) = widget.clone().downcast::<gtk::ScrolledWindow>() {
                assert_eq!(scroll.max_content_height(), 240);
                let view = scroll.child().unwrap().downcast::<gtk::TextView>().unwrap();
                let buffer = view.buffer();
                assert!(
                    buffer.tag_table().lookup("no-hyphens").is_some(),
                    "file names wrap without inserted hyphens"
                );
                lists.push(
                    buffer
                        .text(&buffer.start_iter(), &buffer.end_iter(), false)
                        .to_string(),
                );
            } else if let Ok(label) = widget.clone().downcast::<gtk::Label>() {
                assert!(
                    label
                        .attributes()
                        .is_some_and(|attrs| attrs.to_str().contains("insert-hyphens false")),
                    "paths wrap without inserted hyphens"
                );
                texts.push(label.text().to_string());
            }
            child = widget.next_sibling();
        }
        assert_eq!(lists, vec![bullets(&prepared.lines)]);
        assert_eq!(
            texts,
            vec![prepared.description.clone(), prepared.location.clone()]
        );
        assert!(
            !texts.iter().any(|text| text.contains("expire")),
            "Discard must not reuse the Dismiss retention promise"
        );
        action.downcast::<gtk::Button>().unwrap().emit_clicked();
        assert!(
            !dialog.is_visible(),
            "the dialog closes before Discard runs"
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while page.recovery_active.borrow().is_none() && std::time::Instant::now() < deadline {
            gtk::glib::MainContext::default().iteration(false);
        }
        assert!(
            page.recovery_busy.get(),
            "Discard must be busy before its worker starts"
        );
        assert_eq!(
            page.recovery_active.borrow().as_deref(),
            Some("fixture-receipt")
        );
        assert!(
            page.recovery_success.borrow().is_none(),
            "a new action clears the result"
        );
        fn named(widget: &gtk::Widget, name: &str) -> bool {
            if widget.widget_name() == name {
                return true;
            }
            let mut child = widget.first_child();
            while let Some(next) = child {
                if named(&next, name) {
                    return true;
                }
                child = next.next_sibling();
            }
            false
        }
        assert!(
            named(page.recovery_box.upcast_ref(), "client-recovery-working"),
            "the active row shows its working status"
        );
        assert!(
            !page
                .recovery_feedback
                .borrow()
                .iter()
                .any(|(id, _)| id == &record.id),
            "a new Discard must clear this record's stale Retry failure"
        );
        assert!(
            page.recovery_feedback
                .borrow()
                .iter()
                .any(|(id, _)| id == "other-receipt"),
            "other records keep their feedback"
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while page.recovery_busy.get() && std::time::Instant::now() < deadline {
            gtk::glib::MainContext::default().iteration(false);
        }
        assert!(
            !page.recovery_busy.get(),
            "failed Discard must release the busy guard"
        );
        assert!(page.recovery_active.borrow().is_none());
        assert!(page
            .recovery_feedback
            .borrow()
            .iter()
            .any(|(id, message)| id == &record.id && !message.is_empty()));
        assert!(
            page.recovery_success.borrow().is_none(),
            "a failed Discard reports no success"
        );
        page.set_recovery_dismissed(&record.id, true, &gtk::Button::new());
        assert!(page.recovery_busy.get());
        assert!(
            !page
                .recovery_feedback
                .borrow()
                .iter()
                .any(|(id, _)| id == &record.id),
            "Dismiss must clear the preceding action's feedback too"
        );
        assert!(page
            .recovery_feedback
            .borrow()
            .iter()
            .any(|(id, _)| id == "other-receipt"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while page.recovery_busy.get() && std::time::Instant::now() < deadline {
            gtk::glib::MainContext::default().iteration(false);
        }
        assert!(!page.recovery_busy.get());
    }
}

#[cfg(test)]
mod tab_order_tests {
    use super::*;

    fn record(
        id: &str,
        name: &str,
        dismissed: bool,
        error: bool,
    ) -> crate::clients::recovery::ClientRecovery {
        crate::clients::recovery::ClientRecovery {
            id: id.into(),
            client_id: id.into(),
            client_name: name.into(),
            path: format!("/fixture/{id}.json"),
            backup_path: format!("/fixture/{id}-original.json"),
            missing: Vec::new(),
            dismissed,
            message: format!("{name} is disconnected.\n\nIts original config is back."),
            description: "Its original config is back.".into(),
            value_lines: if error {
                Vec::new()
            } else {
                vec!["API_KEY for server: keyring locked".into()]
            },
            discard_values: Vec::new(),
            error: error.then(|| "This recovery file uses an unsupported version.".into()),
            can_discard: !error,
            can_discard_file: false,
            can_retry: true,
            discard_started: false,
            discard_action: "Discard these saved values".into(),
            cleanup: false,
            instructions: "Unlock your keyring, then choose Retry.".into(),
            retention: "The recovery backup does not expire automatically.".into(),
            details: "TOOLPORT_MISSING_CREDENTIAL_0".into(),
        }
    }

    fn identity(widget: &gtk::Widget) -> String {
        let name = widget.widget_name();
        if name.starts_with("client") {
            name.to_string()
        } else {
            widget.type_().name().to_string()
        }
    }

    /// Tab from "Scan clients again" reaches every card's controls in order,
    /// including an unreadable file's Discard and a dismissed card, and Shift+Tab walks back out to the scan button.
    #[test]
    #[ignore = "requires an isolated GTK desktop"]
    fn tab_reaches_every_recovery_control_and_shift_tab_returns() {
        let _data = crate::registry::DataDirTestEnv::new("gtk-recovery-tab-order");
        adw::init().unwrap();
        let app = adw::Application::builder()
            .application_id("com.tsout.Toolport.RecoveryTabTest")
            .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(gtk::gio::Cancellable::NONE).unwrap();
        let (_, servers, _) = build_content(&app, crate::approval_broker::start_native());
        let page = ClientPage::new(&app, servers);
        page.recoveries.borrow_mut().extend([
            record("codex", "Codex", false, false),
            record("windsurf", "Windsurf", false, true),
            // An unreadable receipt: Retry and Discard recovery file, no Dismiss.
            crate::clients::recovery::ClientRecovery {
                can_discard_file: true,
                discard_action: "Discard recovery file".into(),
                ..record("cursor", "Cursor", false, true)
            },
            record("claude-code", "Claude Code", true, false),
        ]);
        page.rebuild_recoveries();
        let window = gtk::Window::builder()
            .application(&app)
            .default_width(1200)
            .default_height(900)
            .child(&page.root)
            .build();
        window.present();
        while gtk::glib::MainContext::default().iteration(false) {}
        let focused = || {
            gtk::prelude::GtkWindowExt::focus(&window)
                .map(|widget| identity(&widget))
                .unwrap_or_default()
        };
        let expected = [
            "ToolportRecoveryValuesView",
            "client-recovery-retry:codex",
            "client-recovery-dismiss:codex",
            "client-recovery-discard:codex",
            "client-recovery-details:codex",
            "client-recovery-retry:windsurf",
            "client-recovery-details:windsurf",
            "client-recovery-retry:cursor",
            "client-recovery-discard:cursor",
            "client-recovery-details:cursor",
            "client-recovery-show:claude-code",
        ];
        assert!(page.refresh_button.grab_focus());
        assert_eq!(focused(), "clients-refresh");
        let mut forward = Vec::new();
        for _ in expected {
            assert!(window.child_focus(gtk::DirectionType::TabForward));
            forward.push(focused());
        }
        assert_eq!(forward, expected);
        let mut backward = Vec::new();
        for _ in expected {
            assert!(window.child_focus(gtk::DirectionType::TabBackward));
            backward.push(focused());
        }
        let mut back_expected: Vec<_> = expected[..expected.len() - 1]
            .iter()
            .rev()
            .map(|id| id.to_string())
            .collect();
        back_expected.push("clients-refresh".into());
        assert_eq!(backward, back_expected);

        // The result line takes focus without selecting its text.
        page.set_recovery_result("codex", "Codex: values put back: 1.".into(), true);
        page.rebuild_recoveries();
        assert!(focus_named(&page.root, "client-recovery-result"));
        let result = find_named(page.root.upcast_ref(), "client-recovery-result")
            .unwrap()
            .downcast::<gtk::Label>()
            .unwrap();
        assert!(result.has_focus());
        assert!(
            result
                .selection_bounds()
                .is_none_or(|(start, end)| start == end),
            "focus must not select the result text"
        );

        // A refused Disconnect stays until that client's recovery changes.
        page.show_client_error("cursor", "Cursor: could not read its move record");
        assert!(page.feedback.is_visible());
        page.set_recovery_result("codex", "Codex: values put back: 1.".into(), true);
        assert!(
            page.feedback.is_visible(),
            "another client's result keeps it"
        );
        page.set_recovery_result("cursor", "Cursor: record deleted.".into(), true);
        assert!(!page.feedback.is_visible());

        // The notice is keyed to the client the action was for, even when the
        // user opened another client before it failed.
        *page.details.selected.borrow_mut() = Some("codex".into());
        page.show_client_error("cursor", "Cursor: could not read its move record");
        assert_eq!(
            page.detail_notice
                .borrow()
                .as_ref()
                .map(|(id, _, _)| id.clone()),
            Some("cursor".to_string())
        );
        page.set_recovery_result("cursor", "Cursor: record deleted.".into(), true);
        assert!(page.detail_notice.borrow().is_none());
        page.details.selected.borrow_mut().take();
        window.destroy();
    }
}
