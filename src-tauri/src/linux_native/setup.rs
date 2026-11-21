//! Shared review window for client cutover and multi-server paste.
use super::i18n as l10n;
use crate::registry_controller::SetupItem;
use adw::prelude::*;

#[derive(Default)]
pub(super) struct Completion {
    pub message: String,
    pub servers: Vec<crate::registry_controller::SetupServerResult>,
    pub tools: Vec<serde_json::Value>,
    pub backup: Option<String>,
    /// What the connect could not finish tidying up (an old token left in the keychain),
    /// shown under the servers as React shows them.
    pub warnings: Vec<String>,
    /// Set for a client connect: its result page then shows what React's does (the
    /// restart line naming the client, "What your agent sees" even when nothing
    /// moved, and Details with the config and backup).
    pub connect: Option<ConnectResult>,
}

/// What a client connect's result page needs beyond the servers and tools.
#[derive(Default)]
pub(super) struct ConnectResult {
    pub client: String,
    pub config_path: String,
}

/// The connect review's lede, React's sentence: what happens to the selected entries,
/// and that a customized Toolport entry is replaced when it is.
pub(super) fn connect_review_lede(force: bool) -> &'static str {
    if force {
        crate::i18n::msg(
            "Selected entries move into Toolport after verification. This replaces the customized Toolport entry.",
        )
    } else {
        crate::i18n::msg("Selected entries move into Toolport after verification.")
    }
}
impl From<String> for Completion {
    fn from(message: String) -> Self {
        Self {
            message,
            ..Self::default()
        }
    }
}

impl From<&str> for Completion {
    fn from(message: &str) -> Self {
        message.to_string().into()
    }
}

/// The inside of a review entry's toggle, laid out as React's `ImportRow`: a 12px square
/// dot, the name over its command, then the labels on the right.
/// React's dialog width (`sm:max-w-lg`), for the review, its result and its prompts.
const REVIEW_WIDTH: i32 = 512;

/// The room a dialog leaves around it in the window it opens over, 16px each side, as
/// React's `max-w-[calc(100%-2rem)]` and `max-h-[calc(100dvh-2rem)]`.
const DIALOG_MARGIN: i32 = 32;

/// A dialog's width as React sizes it: `width`, but never wider than the window it
/// opens over less 16px each side (`max-w-[calc(100%-2rem)]`), so it never runs off a
/// narrow window. An unrealized parent has no width yet and keeps `width`.
fn dialog_width(parent: &impl IsA<gtk::Window>, width: i32) -> i32 {
    match parent.as_ref().width() {
        available if available > 0 => width.min(available - DIALOG_MARGIN).max(240),
        _ => width,
    }
}
/// The tallest the review's body grows before it scrolls when the window it opens over
/// has no size yet.
const REVIEW_MAX_BODY_HEIGHT: i32 = 600;
/// The least the review's body keeps in a window too short for it, about one server
/// row; the dialog then runs past the window rather than show no body at all.
const REVIEW_MIN_BODY_HEIGHT: i32 = 72;

/// Keeps the review inside the window it opens over, as React's dialog is with
/// `max-h-[calc(100dvh-2rem)]`, and follows that window as it is resized. GTK has no
/// such cap: a dialog window takes its content's natural height, however tall.
///
/// The cap goes on the body's scroller: the window's height less the margin, header
/// and footer. While the server list shows, the viewport gives the body only that
/// height and the body's box takes the shortfall from the list, the one child that
/// can shrink (labels and folds need their full height), so only the list scrolls. Without the list (a review that could not be read, the result page) the
/// viewport gives the body its natural height and the whole body scrolls: at minimum
/// heights a result page taller than the window would squeeze its rows.
fn fit_review_to_parent(
    parent: &gtk::Window,
    content: &adw::Clamp,
    scroller: &gtk::ScrolledWindow,
    list: &gtk::ScrolledWindow,
) {
    let width = dialog_width(parent, REVIEW_WIDTH);
    content.set_maximum_size(width);
    content.set_tightening_threshold(width);
    if let Some(root) = content.child() {
        root.set_width_request(width);
    }
    if let Some(viewport) = scroller.child().and_downcast::<gtk::Viewport>() {
        viewport.set_vscroll_policy(if list.get_visible() {
            gtk::ScrollablePolicy::Minimum
        } else {
            gtk::ScrollablePolicy::Natural
        });
    }
    let natural = |widget: &gtk::Widget| widget.measure(gtk::Orientation::Vertical, width).1;
    let height = match parent.height() {
        height if height > 0 => height,
        _ => parent.default_height(),
    };
    let body_height = match content.child() {
        Some(root) if height > 0 => {
            let header_and_footer = natural(&root) - natural(scroller.upcast_ref());
            height - DIALOG_MARGIN - header_and_footer
        }
        _ => REVIEW_MAX_BODY_HEIGHT,
    };
    scroller.set_max_content_height(body_height.max(REVIEW_MIN_BODY_HEIGHT));
}

/// Refits the review with [`fit_review_to_parent`] whenever the window it opens over
/// changes size, until the review closes. The returned function refits it at once,
/// for changes to the review itself such as its result page.
fn track_parent_size(
    parent: &gtk::Window,
    dialog: &adw::Window,
    content: &adw::Clamp,
    scroller: &gtk::ScrolledWindow,
    list: &gtk::ScrolledWindow,
) -> std::rc::Rc<dyn Fn()> {
    let (window, content, scroller, list) = (
        parent.downgrade(),
        content.downgrade(),
        scroller.downgrade(),
        list.downgrade(),
    );
    let fit: std::rc::Rc<dyn Fn()> = std::rc::Rc::new(move || {
        if let (Some(parent), Some(content), Some(scroller), Some(list)) = (
            window.upgrade(),
            content.upgrade(),
            scroller.upgrade(),
            list.upgrade(),
        ) {
            fit_review_to_parent(&parent, &content, &scroller, &list);
        }
    });
    fit();
    let mut sizes: Vec<(gtk::glib::Object, &str)> = vec![
        (parent.clone().upcast(), "default-width"),
        (parent.clone().upcast(), "default-height"),
    ];
    // A tiled or user-resized window changes its surface, not its default size.
    if let Some(surface) = parent.surface() {
        sizes.push((surface.clone().upcast(), "width"));
        sizes.push((surface.upcast(), "height"));
    }
    let handlers: Vec<_> = sizes
        .into_iter()
        .map(|(object, property)| {
            let fit = fit.clone();
            // The new size reaches the window's allocation on its next layout.
            let id = object.connect_notify_local(Some(property), move |_, _| {
                let fit = fit.clone();
                gtk::glib::idle_add_local_once(move || fit());
            });
            (object, id)
        })
        .collect();
    let handlers = std::cell::RefCell::new(handlers);
    dialog.connect_destroy(move |_| {
        for (object, id) in handlers.borrow_mut().drain(..) {
            object.disconnect(id);
        }
    });
    fit
}

/// A credential counts once it has more than spaces, as React's prompt and the review's
/// own check (`refresh_confirm`) decide. The value itself is used as typed.
fn credential_entered(value: &str) -> bool {
    !value.trim().is_empty()
}

/// Asks for one credential value in a sheet with the spec §5 footer (Cancel, then "Use for
/// connection"), in place of a message dialog with a response strip. Esc cancels.
fn credential_prompt(
    owner: &impl IsA<gtk::Window>,
    name: &str,
    key: &str,
    on_save: impl Fn(String) + 'static,
) -> adw::Window {
    // React's default dialog width (`sm:max-w-sm`), narrower than the review.
    let prompt = adw::Window::builder()
        .transient_for(owner)
        .modal(true)
        .default_width(dialog_width(owner, 384))
        .default_height(-1)
        .build();
    prompt.set_widget_name("toolport-credential-prompt");
    let heading = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .build();
    l10n::bind_formatted_label(&heading, "{value0} credentials", &[("value0", name)]);
    let title = crate::i18n::trf("{value0} credentials", &[("value0", name)]);
    prompt.set_title(Some(&title));
    let (header, _) = dialog_header(&prompt, &heading);
    let text = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .build();
    text.add_css_class("toolport-dialog-lede");
    l10n::bind_text(
        &text,
        crate::i18n::msg(
            "This value is used only for the selected server. It is saved only if connection succeeds.",
        ),
    );
    // The key names the field above it, as React's label does, and wraps as React's does
    // (a long key would otherwise widen the sheet past a narrow window).
    let field = gtk::Label::builder()
        .label(key)
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .build();
    field.add_css_class("toolport-dialog-field");
    // Masked with no show/hide control, as React's password field (the spec is silent).
    let entry = gtk::PasswordEntry::builder()
        .show_peek_icon(false)
        .activates_default(true)
        .build();
    entry.update_property(&[gtk::accessible::Property::Label(key)]);
    // Spec §13: the shared 32px input with a surface fill and accent focus ring.
    entry.add_css_class("toolport-input");
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.set_margin_top(12);
    body.set_margin_bottom(24);
    body.set_margin_start(24);
    body.set_margin_end(24);
    body.append(&text);
    let labelled = gtk::Box::new(gtk::Orientation::Vertical, 8);
    labelled.append(&field);
    labelled.append(&entry);
    body.append(&labelled);
    let cancel = l10n::button("Cancel");
    cancel.add_css_class("flat");
    let save = l10n::button("Use for connection");
    save.set_widget_name("toolport-credential-save");
    save.add_css_class("suggested-action");
    save.set_sensitive(false);
    {
        // Weak: the save handler holds the entry, so a strong reference here would keep
        // both, and the typed value, alive after the sheet closes.
        let save = save.downgrade();
        entry.connect_changed(move |entry| {
            if let Some(save) = save.upgrade() {
                save.set_sensitive(credential_entered(&entry.text()));
            }
        });
    }
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&header);
    root.append(&body);
    root.append(&super::dialog_footer(&[&cancel, &save]));
    prompt.set_content(Some(&root));
    prompt.set_default_widget(Some(&save));
    super::close_on_escape(&prompt);
    {
        let prompt = prompt.downgrade();
        cancel.connect_clicked(move |_| {
            if let Some(prompt) = prompt.upgrade() {
                prompt.close();
            }
        });
    }
    {
        let prompt = prompt.downgrade();
        let entry = entry.clone();
        save.connect_clicked(move |_| {
            if !credential_entered(&entry.text()) {
                return;
            }
            on_save(entry.text().to_string());
            if let Some(prompt) = prompt.upgrade() {
                prompt.close();
            }
        });
    }
    prompt.present();
    entry.grab_focus();
    prompt
}

/// Commands up to this length wrap in a review entry; longer ones keep both ends, as
/// React's ImportRow does.
const REVIEW_COMMAND_WRAP_CHARS: usize = 72;

struct ReviewItem {
    hint: Option<gtk::Label>,
    subtitle: gtk::Label,
    suffixes: gtk::Box,
    check: gtk::ToggleButton,
    name: String,
}

impl ReviewItem {
    fn new(check: &gtk::ToggleButton, name: &str, hint: Option<&str>, command: &str) -> Self {
        // React's ImportRow: the dot, the name and the status on one line, then where it
        // lands when that differs ("Import as github (2)"), then the command on its own
        // line across the whole entry.
        let entry = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let dot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        dot.add_css_class("toolport-select-dot");
        dot.set_valign(gtk::Align::Center);
        dot.set_widget_name("toolport-select-dot");
        line.append(&dot);
        let title = gtk::Label::builder()
            .label(name)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        title.add_css_class("toolport-review-name");
        line.append(&title);
        let suffixes = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        suffixes.set_valign(gtk::Align::Center);
        line.append(&suffixes);
        entry.append(&line);
        let hint = hint.map(|text| {
            let label = gtk::Label::builder()
                .label(text)
                .xalign(0.0)
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .build();
            label.add_css_class("dim-label");
            label.add_css_class("toolport-review-hint");
            entry.append(&label);
            label
        });
        let subtitle = gtk::Label::builder().label(command).xalign(0.0).build();
        if command.chars().count() > REVIEW_COMMAND_WRAP_CHARS {
            // Like React, a long command keeps both ends; hover shows it in full.
            subtitle.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            subtitle.set_single_line_mode(true);
        } else {
            subtitle.set_wrap(true);
            subtitle.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        }
        subtitle.set_tooltip_text(Some(command));
        subtitle.add_css_class("monospace");
        subtitle.add_css_class("dim-label");
        subtitle.add_css_class("toolport-review-command");
        entry.append(&subtitle);
        check.set_child(Some(&entry));
        let item = Self {
            hint,
            subtitle,
            suffixes,
            check: check.clone(),
            name: name.to_string(),
        };
        item.describe();
        item
    }

    fn add_suffix(&self, widget: &impl IsA<gtk::Widget>) {
        self.suffixes.append(widget);
        // A status label that changes or shows later (the credential state, the Error
        // chip) updates what a screen reader hears, as React's button text does.
        if let Some(label) = widget.dynamic_cast_ref::<gtk::Label>() {
            let (check, name, hint, subtitle, suffixes) = (
                self.check.clone(),
                self.name.clone(),
                self.hint.clone(),
                self.subtitle.clone(),
                self.suffixes.clone(),
            );
            let describe =
                move || describe_toggle(&check, &name, hint.as_ref(), &subtitle, &suffixes);
            let again = describe.clone();
            label.connect_label_notify(move |_| describe());
            label.connect_visible_notify(move |_| again());
        }
        self.describe();
    }

    fn set_subtitle(&self, text: &str) {
        // A sentence (why the entry can't move) reads in full.
        self.subtitle.set_label(text);
        self.subtitle.set_ellipsize(gtk::pango::EllipsizeMode::None);
        self.subtitle.set_single_line_mode(false);
        self.subtitle.set_wrap(true);
        self.subtitle.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        self.describe();
    }

    fn describe(&self) {
        describe_toggle(
            &self.check,
            &self.name,
            self.hint.as_ref(),
            &self.subtitle,
            &self.suffixes,
        );
    }
}

/// The name is the toggle's accessible name. Its description is where it lands when that
/// differs, the command, then each status label shown beside the name (New, the credential
/// state, Error), as a screen reader reads React's button text. The toggle itself reports
/// pressed.
fn describe_toggle(
    check: &gtk::ToggleButton,
    name: &str,
    hint: Option<&gtk::Label>,
    subtitle: &gtk::Label,
    suffixes: &gtk::Box,
) {
    let mut parts: Vec<String> = hint.map(|h| h.label().to_string()).into_iter().collect();
    parts.push(subtitle.label().to_string());
    let mut child = suffixes.first_child();
    while let Some(widget) = child {
        if let Some(label) = widget.downcast_ref::<gtk::Label>() {
            if label.is_visible() && !label.label().is_empty() {
                parts.push(label.label().to_string());
            }
        }
        child = widget.next_sibling();
    }
    check.update_property(&[
        gtk::accessible::Property::Label(name),
        gtk::accessible::Property::Description(&parts.join(", ")),
    ]);
}

/// When the backup file was written, in local time and the app language, as React writes
/// it (`fmtTs(ms, "dateTime")`).
fn backup_date(backup: &str) -> Option<String> {
    std::fs::metadata(backup)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|t| gtk::glib::DateTime::from_unix_local(t.as_secs() as i64).ok())
        .map(|t| {
            crate::i18n::format_date_time(
                t.year(),
                t.month() as u32,
                t.day_of_month() as u32,
                t.hour() as u32,
                t.minute() as u32,
            )
        })
}

/// "Import as github (2)" when an import saves under another name than the client's.
/// An unsupported server is not imported, so it gets no hint.
fn import_hint(item: &SetupItem) -> Option<String> {
    item.source_name
        .as_deref()
        .filter(|source| *source != item.name && item.unsupported.is_none())
        .map(|_| {
            crate::i18n::trf(
                crate::i18n::msg("Import as {value0}"),
                &[("value0", item.name.as_str())],
            )
        })
}

/// React's plain `<details>` disclosure in the connect dialogs: the title is a toggle with
/// no border or fill, and the content sits under it. The spec has no disclosure style, so
/// React's decides.
fn details_expander(title: &str) -> (gtk::Expander, gtk::Box) {
    let expander = gtk::Expander::new(Some(title));
    expander.add_css_class("toolport-disclosure");
    let content = gtk::Box::new(gtk::Orientation::Vertical, 2);
    content.set_margin_top(8);
    expander.set_child(Some(&content));
    (expander, content)
}

/// How one step of a batch ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StepEnd {
    /// The action completed and the user went on with Next or Done.
    Completed,
    /// The action completed, then Escape or closing the window stopped the batch.
    CompletedAndStopped,
    /// The step failed and the user chose Skip: the batch goes on without it.
    Skipped,
    /// Cancel, close or Escape before the action completed: the batch stops.
    Stopped,
}

/// Extra parts of a review run as one step of a batch, such as T3 Code's Connect all.
#[derive(Clone, Default)]
pub(super) struct Step {
    /// A line under the title, such as "Step 1 of 2: Claude Work", in the current language.
    pub line: Option<std::rc::Rc<dyn Fn() -> String>>,
    /// More steps follow this one: its success button says Next instead of Done.
    pub more: bool,
    /// Called once when the window closes, with how the step ended and whether the
    /// keyboard closed it (so the control that gets focus next shows its ring). A step
    /// with this callback is part of a batch, so a failure also offers Skip.
    pub closed: Option<std::rc::Rc<dyn Fn(StepEnd, bool)>>,
    /// Set by [`connect_step`] when the review could not be read: the window offers
    /// Retry, which closes it without ending the step and reads the review again.
    pub retry: Option<std::rc::Rc<dyn Fn()>>,
    /// The batch's keyring flag: set once a step fails with the keyring still locked, so
    /// later steps don't ask to unlock it (`registry_controller::connect_step`). React's
    /// batch keeps the same flag.
    pub keyring_quiet: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// This try is a Retry the user chose: it may ask to unlock the keyring.
    pub retried: bool,
}

/// Whether this try of a batch step must not ask to unlock the keyring.
fn quiet_try(flag: &Option<std::sync::Arc<std::sync::atomic::AtomicBool>>, retried: bool) -> bool {
    !retried
        && flag
            .as_ref()
            .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::SeqCst))
}

/// After a failed try: a keyring left locked makes the rest of the batch quiet.
fn note_keyring(flag: &Option<std::sync::Arc<std::sync::atomic::AtomicBool>>, error: &str) {
    if let Some(flag) = flag {
        if crate::registry_controller::keyring_locked_after(error) {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

/// Whether a step's first Connect follows the batch's keyring flag (and so may be quiet)
/// rather than being a Retry the user chose. A review that asked to unlock and was
/// dismissed was the run's one ask: the flag is set, so this step's Connect is quiet too,
/// even on a step the user retried. Outside a batch nothing changes.
fn first_connect_follows_batch(
    flag: &Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    retried: bool,
    review_asked: bool,
) -> bool {
    match flag {
        Some(flag) if review_asked => {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            true
        }
        _ => !retried,
    }
}

/// How a batch step ended from what happened in its window.
pub(super) fn step_end(completed: bool, advanced: bool, skipped: bool) -> StepEnd {
    match (completed, advanced, skipped) {
        (true, true, _) => StepEnd::Completed,
        (true, false, _) => StepEnd::CompletedAndStopped,
        (false, _, true) => StepEnd::Skipped,
        (false, _, false) => StepEnd::Stopped,
    }
}

/// Focus `button` in `window`, with the focus ring when the keyboard got here.
fn focus_with_ring(window: &impl IsA<gtk::Window>, button: &gtk::Button, keyboard: bool) {
    button.grab_focus();
    if keyboard {
        window.as_ref().set_focus_visible(true);
    }
}

/// The connect review's title names the client, as React's dialog does.
pub(super) fn review_title(client_name: &str) -> String {
    crate::i18n::trf("Review and connect {value0}", &[("value0", client_name)])
}

/// The title of the window shown when a client's review can't be read: "Connect NAME",
/// as React's dialog titles it.
pub(super) fn unreadable_review_title(client_name: &str) -> String {
    crate::i18n::trf("Connect {value0}", &[("value0", client_name)])
}

/// React's dialog header: the title on the left and a close button on the right, in
/// place of a centred header bar title. The row still moves the window.
fn dialog_header(window: &adw::Window, heading: &gtk::Label) -> (gtk::WindowHandle, gtk::Button) {
    heading.set_hexpand(true);
    heading.add_css_class("toolport-dialog-title");
    heading.set_accessible_role(gtk::AccessibleRole::Heading);
    let close = super::header_icon_button("window-close-symbolic");
    close.set_valign(gtk::Align::Start);
    l10n::bind_tooltip(&close, "Close");
    close.update_property(&[gtk::accessible::Property::Label(&crate::i18n::tr("Close"))]);
    let title_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    title_row.set_margin_top(16);
    title_row.set_margin_start(24);
    title_row.set_margin_end(12);
    title_row.append(heading);
    title_row.append(&close);
    let header = gtk::WindowHandle::new();
    header.set_child(Some(&title_row));
    let window = window.downgrade();
    close.connect_clicked(move |_| {
        if let Some(window) = window.upgrade() {
            window.close();
        }
    });
    (header, close)
}

// Keep all reviewed dialog fields and its confirmation callback together.
#[allow(clippy::too_many_arguments)]
pub(super) fn review(
    parent: &gtk::Window,
    title: &str,
    items: Vec<SetupItem>,
    disclosure: &str,
    confirm_label: &'static str,
    require_credentials: bool,
    action: impl Fn(
            Vec<String>,
            std::collections::BTreeMap<String, std::collections::BTreeMap<String, bool>>,
            std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
        ) -> Result<Completion, String>
        + Send
        + Sync
        + 'static,
    finished: impl Fn() + 'static,
    credential_page: Option<super::ServerPage>,
) {
    review_with_notice(
        parent,
        title,
        items,
        disclosure,
        None,
        None,
        confirm_label,
        require_credentials,
        action,
        finished,
        credential_page,
        Step::default(),
    );
}

/// The sentence and TOML lines a connect review shows for the client settings
/// Toolport adds to its own entry, or `None` when it adds none.
pub(super) fn gateway_settings_notice(
    client_name: &str,
    settings: &[String],
) -> Option<(String, String)> {
    (!settings.is_empty()).then(|| {
        (
            crate::i18n::trf(
                "Toolport also adds these settings to its {client} entry, each only if it is not set. A setting you delete stays deleted until you connect again.",
                &[("client", client_name)],
            ),
            settings.join("\n"),
        )
    })
}

/// The card that shows the gateway settings notice. Its text stays selectable
/// by mouse but is not a Tab stop, so tabbing never selects the whole sentence.
pub(super) fn gateway_settings_card(sentence: &str, lines: &str) -> gtk::Box {
    let card = gtk::Box::new(gtk::Orientation::Vertical, 6);
    card.set_widget_name("toolport-setup-gateway-settings");
    card.add_css_class("card");
    for (text, monospace) in [(sentence, false), (lines, true)] {
        let label = gtk::Label::builder()
            .label(text)
            .xalign(0.0)
            .wrap(true)
            .selectable(true)
            .focusable(false)
            .margin_start(12)
            .margin_end(12)
            .build();
        if monospace {
            // A long setting has no spaces to break at; let it break anywhere.
            label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
            // React's notice is 13px throughout, its settings in mono (spec §2).
            label.add_css_class("toolport-path-text");
            label.set_margin_bottom(12);
        } else {
            label.add_css_class("toolport-dialog-field");
            label.set_margin_top(12);
        }
        card.append(&label);
    }
    card
}

/// [`review`] with its own lede (a source string; `None` keeps the generic one) and a
/// notice shown under it, before the user confirms.
#[allow(clippy::too_many_arguments)]
pub(super) fn review_with_notice(
    parent: &gtk::Window,
    title: &str,
    items: Vec<SetupItem>,
    disclosure: &str,
    lede: Option<&'static str>,
    notice: Option<(String, String)>,
    confirm_label: &'static str,
    require_credentials: bool,
    action: impl Fn(
            Vec<String>,
            std::collections::BTreeMap<String, std::collections::BTreeMap<String, bool>>,
            std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
        ) -> Result<Completion, String>
        + Send
        + Sync
        + 'static,
    finished: impl Fn() + 'static,
    _credential_page: Option<super::ServerPage>,
    step: Step,
) {
    let dialog = adw::Window::builder()
        .transient_for(parent)
        .modal(true)
        .title(title)
        // React's dialog: the height follows the content as folds open and close, so no
        // empty band sits above the footer (spec §8). A fixed-size window is sized to its
        // content by GTK on every change, where a resizable one keeps its first size.
        .resizable(false)
        .build();
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let heading = gtk::Label::builder()
        .label(title)
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .build();
    heading.set_widget_name("toolport-setup-title");
    let (header, close) = dialog_header(&dialog, &heading);
    close.set_widget_name("toolport-setup-close");
    let cancel = l10n::button("Cancel");
    // Close is off while a check runs, as Cancel is and as React's close button is.
    cancel
        .bind_property("sensitive", &close, "sensitive")
        .sync_create()
        .build();
    // Keep action dispatch on the source key, independent of its translated label.
    let confirm = l10n::button(confirm_label);
    confirm.set_widget_name("toolport-setup-confirm");
    confirm.add_css_class("suggested-action");
    cancel.add_css_class("flat");
    // Only a batch step offers Skip, and only once the step has failed.
    let in_batch = step.closed.is_some();
    let skip = l10n::button("Skip");
    skip.set_widget_name("toolport-setup-skip");
    skip.set_visible(false);
    let skipped = std::rc::Rc::new(std::cell::Cell::new(false));
    root.append(&header);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 16);
    body.set_margin_top(12);
    body.set_margin_bottom(24);
    body.set_margin_start(24);
    body.set_margin_end(24);
    let step_line = step.line.clone().map(|line| {
        let widget = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["toolport-muted", "caption"])
            .build();
        l10n::bind_label_with(&widget, move || line());
        body.append(&widget);
        widget
    });
    let completed = std::rc::Rc::new(std::cell::Cell::new(false));
    // Set when Next or Done closes a completed step; Escape there stops the batch.
    let advanced = std::rc::Rc::new(std::cell::Cell::new(false));
    // Set when Retry reopens a review that could not be read; the step goes on.
    let retrying = std::rc::Rc::new(std::cell::Cell::new(false));
    let keyboard = super::keyboard_tracker(&dialog, parent.gets_focus_visible());
    // Set while the action runs. The window stays open until it finishes, as Cancel and
    // Escape do and React's dialog does: closing it would end a batch step as stopped
    // while the config is still being written. Connected first, so a batch step's handler
    // below never sees this close.
    let busy = std::rc::Rc::new(std::cell::Cell::new(false));
    {
        let busy = busy.clone();
        dialog.connect_close_request(move |_| {
            if busy.get() {
                gtk::glib::Propagation::Stop
            } else {
                gtk::glib::Propagation::Proceed
            }
        });
    }
    if let Some(closed) = step.closed.clone() {
        let (completed, advanced, skipped, retrying, keyboard) = (
            completed.clone(),
            advanced.clone(),
            skipped.clone(),
            retrying.clone(),
            keyboard.clone(),
        );
        let closed = std::cell::Cell::new(Some(closed));
        dialog.connect_close_request(move |_| {
            if retrying.get() {
                return gtk::glib::Propagation::Proceed;
            }
            if let Some(closed) = closed.take() {
                let end = step_end(completed.get(), advanced.get(), skipped.get());
                let keyboard = keyboard.get();
                // Let this window finish closing before the next one opens.
                gtk::glib::idle_add_local_once(move || closed(end, keyboard));
            }
            gtk::glib::Propagation::Proceed
        });
    }
    // A review that could not be read shows its error as the body, as React's dialog
    // does, with no lede, server list or Details.
    let unreadable = step.retry.is_some();
    let lede = if unreadable {
        let widget = gtk::Label::builder()
            .label(disclosure)
            .xalign(0.0)
            .wrap(true)
            .selectable(true)
            .focusable(false)
            .build();
        widget.add_css_class("warning");
        widget
    } else {
        let source = lede.unwrap_or(crate::i18n::msg(
            "Review the selected servers before continuing.",
        ));
        let widget = gtk::Label::builder()
            .label(crate::i18n::lookup(source))
            .xalign(0.0)
            .wrap(true)
            .build();
        // A description: 13px secondary text (spec §2).
        widget.add_css_class("toolport-dialog-lede");
        widget.set_widget_name("toolport-setup-lede");
        l10n::bind_label(&widget, source);
        widget
    };
    body.append(&lede);
    let notice = notice.map(|(sentence, lines)| gateway_settings_card(&sentence, &lines));
    if let Some(card) = &notice {
        body.append(card);
    }
    // React's review: one bordered toggle per entry, stacked with a small gap.
    let rows = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let mut selected = Vec::new();
    let mut credential_choices = Vec::new();
    let credential_inputs =
        std::rc::Rc::new(std::cell::RefCell::new(std::collections::BTreeMap::<
            String,
            std::collections::BTreeMap<String, String>,
        >::new()));
    let required_fields = std::rc::Rc::new(std::cell::RefCell::new(Vec::<(
        gtk::glib::WeakRef<gtk::ToggleButton>,
        String,
        Vec<crate::registry_controller::CredentialReview>,
    )>::new()));
    // Names the key, so a long one wraps anywhere rather than widening the dialog.
    let missing_reason = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .visible(false)
        .build();
    missing_reason.add_css_class("warning");
    let refresh_confirm: std::rc::Rc<dyn Fn()> = std::rc::Rc::new({
        let (button, fields, inputs, reason, busy) = (
            confirm.downgrade(),
            required_fields.clone(),
            credential_inputs.clone(),
            missing_reason.downgrade(),
            busy.clone(),
        );
        move || {
            let missing = if require_credentials {
                fields
                    .borrow()
                    .iter()
                    .filter(|(check, _, _)| check.upgrade().is_some_and(|check| check.is_active()))
                    .flat_map(|(_, name, fields)| {
                        fields
                            .iter()
                            .filter(|field| {
                                field.required
                                    && !field.present
                                    && !inputs
                                        .borrow()
                                        .get(name)
                                        .and_then(|values| values.get(&field.key))
                                        .is_some_and(|v| !v.trim().is_empty())
                            })
                            .map(|field| {
                                crate::i18n::trf(
                                    "Enter {value0} or deselect {value1}",
                                    &[
                                        ("value0", &field.key.to_string()),
                                        ("value1", &name.to_string()),
                                    ],
                                )
                            })
                            .collect::<Vec<_>>()
                    })
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            if let Some(button) = button.upgrade() {
                button.set_sensitive(!busy.get() && missing.is_empty());
            }
            if let Some(reason) = reason.upgrade() {
                reason.set_label(&missing.join("\n"));
                reason.set_visible(!missing.is_empty());
            }
        }
    });
    for item in items {
        let command = item
            .command
            .as_ref()
            .map(|c| {
                format!("{c} {}", item.args.join(" "))
                    .trim_end()
                    .to_string()
            })
            .or(item.url.clone())
            .unwrap_or_else(|| crate::i18n::tr("Needs an endpoint URL"));
        // React's review item: the whole entry is one toggle button with a square dot,
        // the name and its command, and the labels on the right. Space and Enter toggle
        // it; screen readers hear its name, command and pressed state.
        let unsupported = item.unsupported.is_some();
        let check = gtk::ToggleButton::builder()
            .active(!unsupported)
            .hexpand(true)
            .build();
        check.add_css_class("toolport-review-item");
        let row = ReviewItem::new(
            &check,
            item.source_name.as_deref().unwrap_or(&item.name),
            import_hint(&item).as_deref(),
            &command,
        );
        required_fields.borrow_mut().push((
            check.downgrade(),
            item.name.clone(),
            item.credentials.clone(),
        ));
        let refresh = refresh_confirm.clone();
        check.connect_toggled(move |_| refresh());
        check.set_sensitive(!unsupported);
        let (settings, content) = details_expander(&crate::i18n::trf(
            "{value0} credentials and settings",
            &[("value0", &item.name.to_string())],
        ));
        // Small text inside the entry, as React's `text-meta` credentials fold.
        settings.add_css_class("small");
        // React's plain list of credential lines, no row fill.
        let credential_rows = gtk::Box::new(gtk::Orientation::Vertical, 8);
        content.append(&credential_rows);
        let tag = l10n::label(if item.unsupported.is_some() {
            crate::i18n::msg("Needs review")
        } else if !item.updates.is_empty() {
            crate::i18n::msg("Updates existing server")
        } else if item.is_new {
            crate::i18n::msg("New")
        } else {
            crate::i18n::msg("In Toolport")
        });
        if !item.updates.is_empty() {
            let changes = adw::ActionRow::builder()
                .use_markup(false)
                .title(crate::i18n::tr("Changes"))
                .subtitle(item.updates.join(", "))
                .build();
            credential_rows.append(&changes);
        }
        tag.add_css_class("dim-label");
        row.add_suffix(&tag);
        let spinner = gtk::Spinner::new();
        spinner.set_visible(false);
        row.add_suffix(&spinner);
        let state = if !item.credentials.is_empty() {
            let found = item.credentials.iter().all(|env| env.present);
            let required_found = item
                .credentials
                .iter()
                .all(|env| !env.required || env.present);
            let secret = item.credentials.iter().any(|env| env.secret);
            let state = l10n::label(if !found && required_found {
                crate::i18n::msg("Optional")
            } else if !found {
                crate::i18n::msg("Missing")
            } else if secret {
                crate::i18n::msg("Found, goes to keychain")
            } else {
                crate::i18n::msg("Found")
            });
            state.add_css_class(if required_found {
                "dim-label"
            } else {
                "warning"
            });
            row.add_suffix(&state);
            Some(state)
        } else {
            None
        };
        let row_choices = std::rc::Rc::new(std::cell::RefCell::new(Vec::<
            gtk::glib::WeakRef<gtk::CheckButton>,
        >::new()));
        let fields = std::rc::Rc::new(item.credentials.clone());
        let refresh_state: std::rc::Rc<dyn Fn()> = std::rc::Rc::new({
            let refresh_confirm = refresh_confirm.clone();
            let (state, fields, choices, inputs, name) = (
                state.clone(),
                fields.clone(),
                row_choices.clone(),
                credential_inputs.clone(),
                item.name.clone(),
            );
            move || {
                if let Some(state) = &state {
                    let inputs = inputs.borrow();
                    let found = fields.iter().all(|env| {
                        env.present
                            || inputs
                                .get(&name)
                                .and_then(|values| values.get(&env.key))
                                .is_some_and(|value| !value.is_empty())
                    });
                    let required_found = fields.iter().all(|env| {
                        !env.required
                            || env.present
                            || inputs
                                .get(&name)
                                .and_then(|values| values.get(&env.key))
                                .is_some_and(|value| !value.is_empty())
                    });
                    l10n::bind_text(
                        state,
                        if !found && required_found {
                            crate::i18n::msg("Optional")
                        } else if !found {
                            crate::i18n::msg("Missing")
                        } else if choices
                            .borrow()
                            .iter()
                            .filter_map(|choice| choice.upgrade())
                            .any(|choice| choice.is_active())
                        {
                            crate::i18n::msg("Found, goes to keychain")
                        } else {
                            crate::i18n::msg("Found")
                        },
                    );
                    state.remove_css_class("warning");
                    state.remove_css_class("dim-label");
                    state.add_css_class(if required_found {
                        "dim-label"
                    } else {
                        "warning"
                    });
                }
                refresh_confirm();
            }
        });
        if let Some(reason) = &item.unsupported {
            row.set_subtitle(&crate::i18n::trf(
                "Unsupported: {value0}",
                &[("value0", &reason.to_string())],
            ));
        }
        for env in item.credentials {
            let choice = gtk::CheckButton::builder()
                .label(crate::i18n::trf(
                    "Keep {value0} in keychain",
                    &[("value0", &env.key.to_string())],
                ))
                .active(env.secret)
                .valign(gtk::Align::Center)
                .build();
            // The choice text wraps before the line runs out of room.
            if let Some(text) = choice.last_child().and_downcast::<gtk::Label>() {
                text.set_wrap(true);
                text.set_wrap_mode(gtk::pango::WrapMode::WordChar);
                text.set_xalign(0.0);
            }
            row_choices.borrow_mut().push(choice.downgrade());
            let refresh = refresh_state.clone();
            choice.connect_toggled(move |_| refresh());
            choice.set_sensitive(
                item.unsupported.is_none()
                    && !env.key.starts_with("__")
                    && env.key != crate::secrets::IMPORTED_URL_KEY,
            );
            // One line as in React: the keychain choice, "Optional" when it is, the
            // value's state on the right, then Enter value when it is missing.
            let value_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            value_row.set_widget_name("toolport-setup-credential");
            value_row.append(&choice);
            if !env.required {
                let optional = l10n::label(crate::i18n::msg("Optional"));
                optional.add_css_class("toolport-muted");
                optional.set_valign(gtk::Align::Center);
                value_row.append(&optional);
            }
            let value_state = gtk::Label::builder()
                .label(if env.present {
                    crate::i18n::tr("Found")
                } else if env.required {
                    crate::i18n::tr("Missing")
                } else {
                    String::new()
                })
                .hexpand(true)
                .xalign(1.0)
                .valign(gtk::Align::Center)
                .css_classes(["toolport-muted"])
                .build();
            value_row.append(&value_state);
            credential_rows.append(&value_row);
            if !env.present {
                let open = l10n::button("Enter value");
                open.set_valign(gtk::Align::Center);
                let owner = dialog.clone();
                let inputs = credential_inputs.clone();
                let name = item.name.clone();
                let key = env.key.clone();
                let refresh_state = refresh_state.clone();
                let value_row_for_input = value_state.clone();
                open.connect_clicked(move |_| {
                    let (inputs, name_key, key_key, value_row, refresh) = (
                        inputs.clone(),
                        name.clone(),
                        key.clone(),
                        value_row_for_input.clone(),
                        refresh_state.clone(),
                    );
                    credential_prompt(&owner, &name, &key, move |value| {
                        inputs
                            .borrow_mut()
                            .entry(name_key.clone())
                            .or_default()
                            .insert(key_key.clone(), value);
                        value_row.set_label(&crate::i18n::tr("Ready for connection"));
                        refresh();
                    });
                });
                value_row.append(&open);
            }
            credential_choices.push((item.name.clone(), env.key, choice));
        }
        let error_chip = l10n::label(crate::i18n::msg("Error"));
        error_chip.add_css_class("toolport-badge");
        error_chip.add_css_class("error");
        error_chip.set_valign(gtk::Align::Center);
        error_chip.set_visible(false);
        row.add_suffix(&error_chip);
        rows.append(&check);
        let row_error = gtk::Label::builder()
            .wrap(true)
            .xalign(0.0)
            .visible(false)
            .margin_start(16)
            .margin_end(16)
            .margin_bottom(8)
            .css_classes(["toolport-inline-error"])
            .build();
        rows.append(&row_error);
        if credential_rows.first_child().is_some() {
            rows.append(&settings);
        }
        selected.push((
            check,
            unsupported,
            spinner,
            item.key,
            item.name,
            row_error,
            error_chip,
        ));
    }
    let entries = gtk::ScrolledWindow::builder()
        .child(&rows)
        .focusable(false)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .visible(!unreadable)
        .build();
    if selected.len() > 1 {
        entries.set_min_content_height(96);
    }
    entries.set_widget_name("toolport-setup-review-entries");
    body.append(&entries);
    body.append(&missing_reason);
    refresh_confirm();
    let (details, content) = details_expander(&crate::i18n::tr("Details"));
    // The review's Details is small text, as React's `text-meta` summary.
    details.add_css_class("small");
    let path = gtk::Label::builder()
        .label(disclosure)
        .wrap(true)
        .selectable(true)
        .xalign(0.0)
        .build();
    path.add_css_class("toolport-disclosure-text");
    path.add_css_class("toolport-path-text");
    path.add_css_class("toolport-muted");
    content.append(&path);
    details.set_visible(!unreadable);
    rows.set_visible(!unreadable);
    details.set_widget_name("toolport-setup-review-details");
    // Under the lede and any notice, above the entries, where React's review has it.
    let above = notice
        .as_ref()
        .map(|card| card.clone().upcast::<gtk::Widget>())
        .unwrap_or_else(|| lede.clone().upcast());
    body.insert_child_after(&details, Some(&above));
    let feedback = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .selectable(true)
        .visible(false)
        .build();
    body.append(&feedback);
    let scroller = gtk::ScrolledWindow::builder()
        .child(&body)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .build();
    scroller.set_widget_name("toolport-setup-scroller");
    root.append(&scroller);
    root.append(&super::dialog_footer(&[&cancel, &skip, &confirm]));
    // React's 512px width, capped to a narrow window: never narrower than that, and
    // long lines wrap instead of widening it.
    let width = dialog_width(parent, REVIEW_WIDTH);
    root.set_width_request(width);
    let clamp = adw::Clamp::builder()
        .maximum_size(width)
        .tightening_threshold(width)
        .child(&root)
        .build();
    dialog.set_content(Some(&clamp));
    let refit = track_parent_size(parent, &dialog, &clamp, &scroller, &entries);
    {
        let (closing, skipped) = (dialog.clone(), skipped.clone());
        skip.connect_clicked(move |_| {
            skipped.set(true);
            closing.close();
        });
    }
    let closing = dialog.clone();
    let busy_cancel = busy.clone();
    cancel.connect_clicked(move |_| {
        if !busy_cancel.get() {
            closing.close();
        }
    });
    let keys = gtk::EventControllerKey::new();
    let closing = dialog.clone();
    let busy_keys = busy.clone();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gtk::gdk::Key::Escape && !busy_keys.get() {
            closing.close();
            return gtk::glib::Propagation::Stop;
        }
        gtk::glib::Propagation::Proceed
    });
    dialog.add_controller(keys);
    if confirm_label == "Close" {
        let closing = dialog.clone();
        confirm.connect_clicked(move |_| closing.close());
        cancel.set_visible(false);
        dialog.present();
        return;
    }
    if let Some(retry) = step.retry.clone() {
        // The review itself could not be read: Retry reads it again, and a batch can go
        // on to the next step.
        let (closing, retrying) = (dialog.clone(), retrying.clone());
        confirm.connect_clicked(move |_| {
            retrying.set(true);
            closing.close();
            retry();
        });
        skip.set_visible(in_batch);
        dialog.present();
        focus_with_ring(&dialog, &confirm, keyboard.get());
        return;
    }
    let selected = std::rc::Rc::new(selected);
    let credential_choices = std::rc::Rc::new(credential_choices);
    let action = std::sync::Arc::new(action);
    let finished = std::rc::Rc::new(finished);
    let closing = dialog.clone();
    let shown = dialog.clone();
    let more = step.more;
    let titled = dialog.clone();
    confirm.connect_clicked(move |button| {
        if completed.get() {
            advanced.set(true);
            closing.close();
            return;
        }
        let chosen = selected
            .iter()
            .filter(|(check, _, _, _, _, _, _)| check.is_active())
            .map(|(_, _, _, key, _, _, _)| key.clone())
            .collect::<Vec<_>>();
        let mut choices =
            std::collections::BTreeMap::<String, std::collections::BTreeMap<String, bool>>::new();
        for (name, key, choice) in credential_choices.iter() {
            choices
                .entry(name.clone())
                .or_default()
                .insert(key.clone(), choice.is_active());
            choice.set_sensitive(false);
        }
        let inputs = credential_inputs.borrow().clone();
        busy.set(true);
        button.set_sensitive(false);
        cancel.set_sensitive(false);
        skip.set_sensitive(false);
        for (check, _row, spinner, _, _, row_error, error_chip) in selected.iter() {
            row_error.set_visible(false);
            error_chip.set_visible(false);
            check.set_sensitive(false);
            spinner.set_visible(check.is_active());
            spinner.set_spinning(check.is_active());
        }
        l10n::bind_text(&feedback, "Checking selected servers...");
        feedback.set_visible(true);
        let (action, finished, selected, busy, completed) = (
            action.clone(),
            finished.clone(),
            selected.clone(),
            busy.clone(),
            completed.clone(),
        );
        let credential_choices = credential_choices.clone();
        let refresh_confirm = refresh_confirm.clone();
        let (heading, dialog) = (heading.clone(), titled.clone());
        let (feedback, button, cancel, skip, body, rows, details, lede, notice) = (
            feedback.clone(),
            button.clone(),
            cancel.clone(),
            skip.clone(),
            body.clone(),
            rows.clone(),
            details.clone(),
            lede.clone(),
            notice.clone(),
        );
        let (shown, keyboard, step_line) = (shown.clone(), keyboard.clone(), step_line.clone());
        let (entries, refit) = (entries.clone(), refit.clone());
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || action(chosen, choices, inputs)).await;
            busy.set(false);
            // Closed while it ran: refresh after a success, but build no result
            // page for a window that is gone (D136).
            if !button
                .root()
                .and_downcast::<gtk::Window>()
                .is_some_and(|window| window.is_visible())
            {
                if matches!(result, Ok(Ok(_))) {
                    finished();
                }
                return;
            }
            for (name, key, choice) in credential_choices.iter() {
                choice.set_sensitive(
                    !key.starts_with("__")
                        && selected.iter().any(|(_, unsupported, _, _, server, _, _)| {
                            server == name && !unsupported
                        }),
                );
            }
            cancel.set_sensitive(true);
            skip.set_sensitive(true);
            button.set_sensitive(true);
            for (check, unsupported, spinner, _, _, row_error, _) in selected.iter() {
                row_error.set_visible(false);
                check.set_sensitive(!unsupported);
                spinner.set_spinning(false);
                spinner.set_visible(false);
            }
            match result {
                Ok(Ok(outcome)) => {
                    completed.set(true);
                    cancel.set_visible(false);
                    skip.set_visible(false);
                    // Next while more steps follow, Done on the last.
                    l10n::bind_text(&button, if more { "Next" } else { "Done" });
                    entries.set_visible(false);
                    rows.set_visible(false);
                    details.set_visible(false);
                    lede.set_visible(false);
                    // The notice describes what connecting will do; the result
                    // page says what happened, as React's does.
                    if let Some(notice) = &notice {
                        notice.set_visible(false);
                    }
                    feedback.set_visible(false);
                    // React's result: the title on the left says what happened, a
                    // centred check under it, then the text on the left.
                    // React's "{client} connected" for a connect. Other results have no
                    // separate title, so their message's first sentence is the title.
                    let done_title = match &outcome.connect {
                        Some(connect) => crate::i18n::trf(
                            "{value0} connected",
                            &[("value0", connect.client.as_str())],
                        ),
                        None => outcome
                            .message
                            .split_once(". ")
                            .map_or(outcome.message.as_str(), |(title, _)| title)
                            .to_string(),
                    };
                    heading.set_label(&done_title);
                    dialog.set_title(Some(&done_title));
                    // GTK's built-in check icon; emblem-ok is missing from some themes.
                    let status = gtk::Image::from_icon_name("object-select-symbolic");
                    status.set_pixel_size(40);
                    status.set_halign(gtk::Align::Center);
                    status.set_widget_name("toolport-setup-success-icon");
                    status.add_css_class("toolport-success-icon");
                    status.set_accessible_role(gtk::AccessibleRole::Presentation);
                    match &step_line {
                        Some(line) => body.insert_child_after(&status, Some(line)),
                        None => body.prepend(&status),
                    }
                    let next_step = match &outcome.connect {
                        // React's line, naming the client.
                        Some(connect) => Some(crate::i18n::trf(
                            "Restart {client} to load Toolport.",
                            &[("client", &connect.client)],
                        )),
                        None => outcome
                            .message
                            .split_once(". ")
                            .map(|(_, description)| description.to_string()),
                    };
                    if let Some(description) = next_step {
                        let next_step = gtk::Label::builder()
                            .label(description)
                            .xalign(0.0)
                            .wrap(true)
                            .build();
                        next_step.set_widget_name("toolport-setup-next-step");
                        body.insert_child_after(&next_step, Some(&status));
                    }
                    let results = gtk::ListBox::new();
                    results.add_css_class("boxed-list");
                    results.set_selection_mode(gtk::SelectionMode::None);
                    for server in outcome.servers {
                        let credential = match server.credential_state.as_str() {
                            "stored" => "Stored in keychain",
                            "missing" => "Needs input",
                            _ => "No credentials needed",
                        };
                        let row = adw::ActionRow::builder()
                            .title(&server.name)
                            .subtitle(crate::i18n::trf(
                                "{value0} tools · {value1}",
                                &[
                                    ("value0", &format!("{}", server.tool_count)),
                                    ("value1", credential),
                                ],
                            ))
                            .build();
                        results.append(&row);
                    }
                    body.append(&results);
                    results.set_visible(results.first_child().is_some());
                    for warning in &outcome.warnings {
                        let label = gtk::Label::builder()
                            .label(warning)
                            .wrap(true)
                            .wrap_mode(gtk::pango::WrapMode::WordChar)
                            .selectable(true)
                            .xalign(0.0)
                            .build();
                        label.add_css_class("dim-label");
                        body.append(&label);
                    }
                    // A connect always says what the agent sees, as React's result does,
                    // including when nothing moved.
                    if !outcome.tools.is_empty() || outcome.connect.is_some() {
                        let (agent, content) =
                            details_expander(&crate::i18n::tr("What your agent sees"));
                        agent.set_widget_name("toolport-setup-agent-sees");
                        if outcome.tools.is_empty() {
                            let none = gtk::Label::builder()
                                .label(crate::i18n::tr(
                                    "No servers moved, so your agent has no new tools.",
                                ))
                                .xalign(0.0)
                                .wrap(true)
                                .build();
                            none.add_css_class("toolport-muted");
                            content.append(&none);
                        }
                        for tool in outcome.tools {
                            let name = gtk::Label::builder()
                                .label(tool["name"].as_str().unwrap_or("Tool"))
                                .xalign(0.0)
                                .wrap(true)
                                .selectable(true)
                                .build();
                            name.set_wrap_mode(gtk::pango::WrapMode::WordChar);
                            name.add_css_class("monospace");
                            content.append(&name);
                        }
                        body.append(&agent);
                    }
                    if let Some(connect) = &outcome.connect {
                        // React's Details: the config, then the backup when one was saved.
                        let (detail, content) = details_expander(&crate::i18n::tr("Details"));
                        detail.set_widget_name("toolport-setup-details");
                        // Lines that hold a path are mono 13 (spec §2).
                        let mut lines = vec![(
                            crate::i18n::trf("Config: {path}", &[("path", &connect.config_path)]),
                            true,
                        )];
                        if let Some(backup) = &outcome.backup {
                            lines.push((
                                backup_date(backup)
                                    .map(|date| {
                                        crate::i18n::trf(
                                            "Backup saved {value0}",
                                            &[("value0", &date)],
                                        )
                                    })
                                    .unwrap_or_else(|| crate::i18n::tr("Backup saved")),
                                false,
                            ));
                            lines.push((
                                crate::i18n::trf("Backup: {path}", &[("path", backup)]),
                                true,
                            ));
                        }
                        for (line, path) in lines {
                            let label = gtk::Label::builder()
                                .label(line)
                                .xalign(0.0)
                                .wrap(true)
                                .selectable(true)
                                .build();
                            label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
                            if path {
                                label.add_css_class("toolport-path-text");
                            }
                            content.append(&label);
                        }
                        body.append(&detail);
                    } else if let Some(backup) = outcome.backup {
                        let (detail, content) = details_expander(&crate::i18n::tr("Details"));
                        let date = backup_date(&backup).unwrap_or_else(|| "saved".into());
                        let label = {
                            let widget = gtk::Label::builder()
                                .label("")
                                .wrap(true)
                                .selectable(true)
                                .build();
                            l10n::bind_formatted_label(
                                &widget,
                                "Backup {value0}\n{value1}",
                                &[
                                    ("value0", &date.to_string()),
                                    ("value1", &backup.to_string()),
                                ],
                            );
                            widget
                        };
                        content.append(&label);
                        body.append(&detail);
                    }
                    // Return activates Next or Done without first tabbing through the
                    // results; the button was disabled while the step ran, which dropped
                    // focus and its ring.
                    refit();
                    if let Some(window) = button.root().and_downcast::<gtk::Window>() {
                        window.set_default_widget(Some(&button));
                    }
                    focus_with_ring(&shown, &button, keyboard.get());
                    finished();
                }
                result => {
                    let error = match result {
                        Ok(Err(error)) => error,
                        _ => crate::i18n::tr("Setup stopped. Retry."),
                    };
                    feedback.set_label(&error);
                    feedback.add_css_class("error");
                    l10n::bind_text(&button, "Retry");
                    skip.set_visible(in_batch);
                    refresh_confirm();
                    let mut has_row_error = false;
                    for (_, _, _, _, name, row_error, error_chip) in selected.iter() {
                        if error.contains(name) {
                            has_row_error = true;
                            error_chip.set_visible(true);
                            row_error.set_label(&error);
                            row_error.set_visible(true);
                        }
                    }
                    feedback.set_visible(!has_row_error);
                    // Retry takes focus; Skip and Cancel are next to it in the footer.
                    focus_with_ring(&shown, &button, keyboard.get());
                }
            }
        });
    });
    dialog.present();
}

/// The client's name for the connect review, found the way the review itself finds the
/// client: a row the page lists without saving it yet (a T3 Code home, a launcher home)
/// included. React shows the same name.
fn review_client_name(client_id: &str) -> String {
    crate::clients::detect_client(client_id)
        .map(|client| client.name)
        .unwrap_or_else(|| crate::i18n::tr("Client"))
}

pub(super) fn connect(
    parent: &gtk::Window,
    client_id: String,
    profile: Option<String>,
    force: bool,
    finished: impl Fn() + 'static,
    credential_page: Option<super::ServerPage>,
) {
    connect_step(
        parent,
        client_id,
        profile,
        force,
        std::rc::Rc::new(finished),
        credential_page,
        Step::default(),
    );
}

/// [`connect`] as one step of a batch. When the review can't be read, its window offers
/// Retry, which reads it again, and Skip in a batch.
pub(super) fn connect_step(
    parent: &gtk::Window,
    client_id: String,
    profile: Option<String>,
    force: bool,
    finished: std::rc::Rc<dyn Fn()>,
    credential_page: Option<super::ServerPage>,
    step: Step,
) {
    let parent = parent.clone();
    gtk::glib::spawn_future_local(async move {
        let id = client_id.clone();
        let flag = step.keyring_quiet.clone();
        let quiet = quiet_try(&flag, step.retried);
        let preview = gtk::gio::spawn_blocking(move || {
            let review = crate::registry_controller::preview_step(&id, quiet);
            if let Err(error) = &review {
                note_keyring(&flag, error);
            }
            (review, review_client_name(&id))
        })
        .await;
        match preview {
            Ok((Ok(preview), client_name)) => {
                let disclosure = crate::i18n::trf(
                    if force {
                        crate::i18n::msg("Config: {value0}\nBackup directory: {value1}\nSelected entries move after gateway verification. Unchecked entries and plugin servers stay in place. This replaces the customized Toolport entry.")
                    } else {
                        crate::i18n::msg("Config: {value0}\nBackup directory: {value1}\nSelected entries move after gateway verification. Unchecked entries and plugin servers stay in place.")
                    },
                    &[
                        ("value0", &preview.config_path.to_string()),
                        ("value1", &preview.backup_dir.to_string()),
                    ],
                );
                let notice = gateway_settings_notice(&client_name, &preview.gateway_settings);
                // The step's first connect follows the batch's keyring flag; a Retry the
                // user chooses may ask.
                let flag = step.keyring_quiet.clone();
                let first = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
                    first_connect_follows_batch(&flag, step.retried, preview.keyring_asked),
                ));
                review_with_notice(
                    &parent,
                    &review_title(&client_name),
                    preview.items,
                    &disclosure,
                    Some(connect_review_lede(force)),
                    notice,
                    crate::i18n::msg("Connect to Toolport"),
                    true,
                    move |selected, choices, inputs| {
                        let retried = !first.swap(false, std::sync::atomic::Ordering::SeqCst);
                        let outcome = crate::registry_controller::connect_step(
                            quiet_try(&flag, retried),
                            || {
                                crate::registry_controller::migrate_client_reviewed_inputs(
                                    &client_id,
                                    profile.as_deref(),
                                    force,
                                    &selected,
                                    &preview.revision,
                                    &choices,
                                    &inputs,
                                )
                            },
                        )
                        .inspect_err(|error| note_keyring(&flag, error))?;
                        Ok(Completion {
                            message: crate::i18n::trf(
                                "{value0} connected. Restart it to load Toolport.",
                                &[("value0", &client_name.to_string())],
                            ),
                            servers: outcome.servers,
                            tools: outcome.tools,
                            backup: outcome.result.outcome.backup.clone(),
                            warnings: outcome.result.outcome.warnings.clone(),
                            connect: Some(ConnectResult {
                                client: client_name.clone(),
                                config_path: outcome.result.outcome.path.clone(),
                            }),
                        })
                    },
                    move || finished(),
                    credential_page,
                    step,
                );
            }
            failed => {
                let (error, client_name) = match failed {
                    Ok((Err(error), name)) => (error, name),
                    _ => (
                        crate::i18n::tr("Client review stopped."),
                        crate::i18n::tr("Client"),
                    ),
                };
                // Cancel, Skip in a batch, and Retry, which reads the review again, as
                // React's dialog offers when the config can't be read.
                let again = (parent.clone(), step.clone());
                let retry = std::rc::Rc::new(move || {
                    connect_step(
                        &again.0,
                        client_id.clone(),
                        profile.clone(),
                        force,
                        finished.clone(),
                        credential_page.clone(),
                        Step {
                            retried: true,
                            ..again.1.clone()
                        },
                    )
                });
                review_with_notice(
                    &parent,
                    &unreadable_review_title(&client_name),
                    Vec::new(),
                    &error,
                    None,
                    None,
                    crate::i18n::msg("Retry"),
                    false,
                    |_, _, _| Err(String::new()),
                    || {},
                    None,
                    Step {
                        retry: Some(retry),
                        ..step
                    },
                );
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Connect all asks about a locked keyring at most once: a step that fails with the
    /// keyring still locked makes the rest of the batch quiet, while a Retry the user
    /// chooses may still ask. Outside a batch nothing is quiet. React's batch matches.
    #[test]
    fn a_batch_goes_quiet_once_the_keyring_stays_locked() {
        assert!(!quiet_try(&None, false));
        let flag = Some(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
            false,
        )));
        assert!(!quiet_try(&flag, false), "the first step may ask");
        crate::clients::backup_vault::fake::locked_store(
            crate::clients::backup_vault::Unlock::StillLocked,
            || note_keyring(&flag, "Launch failed"),
        );
        assert!(quiet_try(&flag, false), "later steps don't ask");
        assert!(!quiet_try(&flag, true), "a Retry may ask");
    }

    /// A step's review asked to unlock and the ask was dismissed: that step's Connect is
    /// quiet (no second ask in the same step), the rest of the batch too, and a Retry the
    /// user chooses may ask again. A single Connect outside a batch is unchanged.
    #[test]
    fn a_review_that_asked_keeps_its_steps_connect_quiet() {
        let flag = Some(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
            false,
        )));
        let first = first_connect_follows_batch(&flag, false, true);
        assert!(
            quiet_try(&flag, !first),
            "the step's Connect does not ask again"
        );
        assert!(quiet_try(&flag, false), "later steps stay quiet");
        assert!(!quiet_try(&flag, true), "a Retry may ask");
        // The review of a step the user retried asked: its Connect still does not ask.
        let flag = Some(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
            false,
        )));
        let first = first_connect_follows_batch(&flag, true, true);
        assert!(quiet_try(&flag, !first));
        // No ask at review: the step's Connect may ask, as before.
        let flag = Some(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
            false,
        )));
        let first = first_connect_follows_batch(&flag, false, false);
        assert!(!quiet_try(&flag, !first));
        // A single Connect: never quiet, whatever the review did.
        let first = first_connect_follows_batch(&None, false, true);
        assert!(!quiet_try(&None, !first));
    }

    /// A T3 Code home the page lists before it is saved is named in the connect review as
    /// it is on the page, not "Client".
    #[test]
    fn the_connect_review_names_an_unsaved_t3_code_home() {
        let _lock = crate::registry::data_dir_test_lock();
        let root = std::env::temp_dir().join(format!(
            "toolport-gtk-review-name-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let home = root.join("home");
        std::fs::create_dir_all(home.join(".t3/userdata")).unwrap();
        std::fs::create_dir_all(home.join(".t3/cx/codex")).unwrap();
        std::fs::write(
            home.join(".t3/userdata/settings.json"),
            r#"{"providerInstances": {"cx": {"driver": "codex", "config": {"homePath": "~/.t3/cx/codex"}}}}"#,
        )
        .unwrap();
        let _data = crate::registry::DataDirOverride::set(root.join("data"));
        let _home = crate::clients::TestHome::set(&home);
        let row = crate::clients::detect_clients_for_skills()
            .into_iter()
            .find(|client| client.id.starts_with("codex@t3-"))
            .expect("the T3 Code home is listed");
        assert_eq!(review_client_name(&row.id), row.name);
        assert_ne!(row.name, crate::i18n::tr("Client"));
        let _ = std::fs::remove_dir_all(&root);
    }
    /// Whether the widget's accessible description is exactly `expected`, read through
    /// GTK's own accessibility test check.
    fn described_as(widget: &impl IsA<gtk::Accessible>, expected: &str) -> bool {
        use gtk::glib::translate::ToGlibPtr;
        let expected = std::ffi::CString::new(expected).unwrap();
        let accessible: &gtk::Accessible = widget.as_ref();
        // SAFETY: a live accessible, a known string property and a NUL-terminated value;
        // the returned message, when there is one, is ours to free.
        unsafe {
            let message = gtk::ffi::gtk_test_accessible_check_property(
                accessible.to_glib_none().0,
                gtk::ffi::GTK_ACCESSIBLE_PROPERTY_DESCRIPTION,
                expected.as_ptr(),
            );
            if message.is_null() {
                true
            } else {
                gtk::glib::ffi::g_free(message.cast());
                false
            }
        }
    }

    /// The connect dialogs' plain folds: the app stylesheet reaches the title inside
    /// GtkExpander's box (React's tight title: 2px above and below the text).
    #[test]
    #[ignore = "requires isolated GTK display"]
    fn disclosure_title_styles_reach_the_expander_title() {
        adw::init().unwrap();
        let provider = gtk::CssProvider::new();
        provider.load_from_data(&super::super::theme::OmarchyPalette::default().css());
        gtk::style_context_add_provider_for_display(
            &gtk::gdk::Display::default().unwrap(),
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        let (expander, _) = details_expander("Details");
        expander.add_css_class("small");
        let window = gtk::Window::new();
        window.set_child(Some(&expander));
        window.present();
        let context = gtk::glib::MainContext::default();
        while context.pending() {
            context.iteration(false);
        }
        let label = expander.label_widget().unwrap();
        let title = label.parent().unwrap();
        assert_eq!(title.css_name(), "title");
        let font = label.pango_context().font_description().unwrap();
        assert_eq!(font.size(), 13 * gtk::pango::SCALE);
        // Measured, not the last allocation, which may predate the stylesheet.
        let natural = |widget: &gtk::Widget| widget.measure(gtk::Orientation::Vertical, -1).1;
        let content = natural(&label).max(natural(&title.first_child().unwrap()));
        assert_eq!(natural(&title), content + 4);
        window.close();
    }

    fn descendants(widget: &gtk::Widget) -> Vec<gtk::Widget> {
        let mut out = vec![widget.clone()];
        let mut child = widget.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            out.extend(descendants(&widget));
        }
        out
    }
    fn review_window() -> gtk::Window {
        gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Window>().ok())
            .find(|w| w.title().as_deref() == Some("Could not review setup"))
            .unwrap()
    }
    #[test]
    fn review_title_names_the_client() {
        assert_eq!(
            review_title("Claude Code (claude_personal)"),
            "Review and connect Claude Code (claude_personal)"
        );
    }

    #[test]
    fn a_completed_step_closed_without_next_or_done_stops_the_batch() {
        assert_eq!(step_end(true, true, false), StepEnd::Completed);
        assert_eq!(step_end(true, false, false), StepEnd::CompletedAndStopped);
        assert_eq!(step_end(false, false, true), StepEnd::Skipped);
        assert_eq!(step_end(false, false, false), StepEnd::Stopped);
    }

    fn wait_for(done: &dyn Fn() -> bool) {
        let context = gtk::glib::MainContext::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !done() && std::time::Instant::now() < deadline {
            context.iteration(false);
            std::thread::yield_now();
        }
    }
    fn window_titled(title: &str) -> Option<gtk::Window> {
        gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Window>().ok())
            .find(|w| w.title().as_deref() == Some(title) && w.is_visible())
    }
    fn button_named(window: &gtk::Window, name: &str) -> gtk::Button {
        descendants(window.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Button>().ok())
            .find(|b| b.widget_name() == name)
            .unwrap()
    }
    fn label_shown(window: &gtk::Window, text: &str) -> bool {
        descendants(window.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Label>().ok())
            .any(|l| l.text() == text && l.is_visible())
    }
    /// A batch step whose end is recorded in the returned cell.
    fn recorded_step(
        line: Option<&'static str>,
        more: bool,
    ) -> (Step, std::rc::Rc<std::cell::Cell<Option<StepEnd>>>) {
        let ended = std::rc::Rc::new(std::cell::Cell::new(None));
        let record = ended.clone();
        let step = Step {
            line: line.map(|line| std::rc::Rc::new(move || line.to_string()) as _),
            more,
            closed: Some(std::rc::Rc::new(move |end, _| record.set(Some(end)))),
            ..Step::default()
        };
        (step, ended)
    }
    fn review_fixture(title: &str, ok: bool, step: Step) -> gtk::Window {
        review_with_notice(
            &gtk::Window::new(),
            title,
            Vec::new(),
            "Fixture",
            None,
            None,
            "Connect",
            true,
            move |_, _, _| {
                if ok {
                    Ok("Cursor connected. Restart it to load Toolport.".into())
                } else {
                    Err("Permission denied".into())
                }
            },
            || {},
            None,
            step,
        );
        window_titled(title).unwrap()
    }

    #[test]
    #[ignore = "requires isolated GTK display"]
    fn batch_step_offers_skip_after_a_failure_and_next_after_success() {
        adw::init().unwrap();
        // A failing step: its line shows, then Retry and Skip; Skip ends it as skipped.
        let (step, ended) = recorded_step(Some("Step 1 of 2: Cursor"), true);
        let window = review_fixture("Failing step", false, step);
        assert!(label_shown(&window, "Step 1 of 2: Cursor"));
        let confirm = button_named(&window, "toolport-setup-confirm");
        let skip = button_named(&window, "toolport-setup-skip");
        assert!(!skip.is_visible(), "Skip waits for a failure");
        confirm.emit_clicked();
        wait_for(&|| confirm.label().as_deref() == Some("Retry"));
        assert!(skip.is_visible());
        assert_eq!(skip.label().as_deref(), Some("Skip"));
        assert!(
            gtk::prelude::GtkWindowExt::focus(&window).as_ref() == Some(confirm.upcast_ref()),
            "Retry takes focus when the step fails"
        );
        // Retry runs the step again and fails again; Skip still ends it.
        confirm.emit_clicked();
        wait_for(&|| confirm.is_sensitive());
        assert_eq!(confirm.label().as_deref(), Some("Retry"));
        assert!(ended.get().is_none());
        skip.emit_clicked();
        wait_for(&|| ended.get().is_some());
        assert_eq!(ended.get(), Some(StepEnd::Skipped));

        // A succeeding step with more to come says Next and focuses it; Next ends it.
        let (step, ended) = recorded_step(None, true);
        let window = review_fixture("Passing step", true, step);
        let confirm = button_named(&window, "toolport-setup-confirm");
        confirm.emit_clicked();
        wait_for(&|| confirm.label().as_deref() == Some("Next"));
        assert!(!button_named(&window, "toolport-setup-skip").is_visible());
        assert!(
            gtk::prelude::GtkWindowExt::focus(&window).as_ref() == Some(confirm.upcast_ref()),
            "Next takes focus when the step succeeds"
        );
        confirm.emit_clicked();
        wait_for(&|| ended.get().is_some());
        assert_eq!(ended.get(), Some(StepEnd::Completed));

        // The last step says Done.
        let (step, _) = recorded_step(None, false);
        let window = review_fixture("Last step", true, step);
        let confirm = button_named(&window, "toolport-setup-confirm");
        confirm.emit_clicked();
        wait_for(&|| confirm.label().as_deref() == Some("Done"));
        assert_eq!(confirm.label().as_deref(), Some("Done"));
        window.close();

        // Outside a batch a failure never offers Skip.
        let window = review_fixture("Single failing review", false, Step::default());
        let confirm = button_named(&window, "toolport-setup-confirm");
        confirm.emit_clicked();
        wait_for(&|| confirm.label().as_deref() == Some("Retry"));
        assert!(!button_named(&window, "toolport-setup-skip").is_visible());
        window.close();
    }

    #[test]
    #[ignore = "requires isolated GTK display"]
    fn escape_stops_a_batch_step_before_or_after_it_connects() {
        adw::init().unwrap();
        // Escape on the review: the batch stops.
        let (step, ended) = recorded_step(None, true);
        let window = review_fixture("Cancelled step", false, step);
        window.close();
        wait_for(&|| ended.get().is_some());
        assert_eq!(ended.get(), Some(StepEnd::Stopped));
        // Escape on the success screen: this step connected, the batch stops.
        let (step, ended) = recorded_step(None, true);
        let window = review_fixture("Escaped step", true, step);
        let confirm = button_named(&window, "toolport-setup-confirm");
        confirm.emit_clicked();
        wait_for(&|| confirm.label().as_deref() == Some("Next"));
        window.close();
        wait_for(&|| ended.get().is_some());
        assert_eq!(ended.get(), Some(StepEnd::CompletedAndStopped));
    }

    #[test]
    #[ignore = "requires isolated GTK display"]
    fn closing_a_step_window_while_it_connects_keeps_it_open() {
        use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
        adw::init().unwrap();
        let release = std::sync::Arc::new(AtomicBool::new(false));
        let wrote = std::sync::Arc::new(AtomicBool::new(false));
        let (step, ended) = recorded_step(None, true);
        let (hold, done) = (release.clone(), wrote.clone());
        review_with_notice(
            &gtk::Window::new(),
            "Slow step",
            Vec::new(),
            "Fixture",
            None,
            None,
            "Connect",
            true,
            move |_, _, _| {
                while !hold.load(SeqCst) {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                done.store(true, SeqCst);
                Ok("Cursor connected. Restart it to load Toolport.".into())
            },
            || {},
            None,
            step,
        );
        let window = window_titled("Slow step").unwrap();
        let confirm = button_named(&window, "toolport-setup-confirm");
        confirm.emit_clicked();
        wait_for(&|| !confirm.is_sensitive());
        // The window manager asks the window to close while the config is being written.
        window.close();
        let context = gtk::glib::MainContext::default();
        for _ in 0..20 {
            while context.iteration(false) {}
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let still_open = window.is_visible();
        let not_ended = ended.get().is_none();
        release.store(true, SeqCst);
        assert!(still_open, "the window stays open while it connects");
        assert!(not_ended, "the step has not ended");
        wait_for(&|| confirm.label().as_deref() == Some("Next"));
        assert!(wrote.load(SeqCst));
        // Once it has connected, closing it stops the batch with this row connected.
        window.close();
        wait_for(&|| ended.get().is_some());
        assert_eq!(ended.get(), Some(StepEnd::CompletedAndStopped));
    }

    #[test]
    fn a_review_that_cannot_be_read_is_titled_as_react_titles_it() {
        assert_eq!(unreadable_review_title("Cursor"), "Connect Cursor");
    }

    #[test]
    #[ignore = "requires isolated GTK display"]
    fn a_step_whose_review_cannot_be_read_offers_retry_and_skip() {
        adw::init().unwrap();
        let _lock = crate::registry::data_dir_test_lock();
        let root = std::env::temp_dir().join(format!(
            "toolport-gtk-step-preview-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("home")).unwrap();
        let _data = crate::registry::DataDirOverride::set(root.join("data"));
        let _home = crate::clients::TestHome::set(&root.join("home"));
        let parent = gtk::Window::new();
        let (step, ended) = recorded_step(Some("Step 2 of 2: Gone"), false);
        connect_step(
            &parent,
            "no-such-client".into(),
            None,
            false,
            std::rc::Rc::new(|| {}),
            None,
            step,
        );
        wait_for(&|| window_titled("Connect Client").is_some());
        let window = window_titled("Connect Client").unwrap();
        assert!(label_shown(&window, "Step 2 of 2: Gone"));
        // The error is the body, as in React's dialog: not a review lede over an empty
        // list with the reason folded under Details.
        assert!(!label_shown(
            &window,
            "Review the selected servers before continuing."
        ));
        let shown = descendants(window.upcast_ref());
        assert!(
            !shown
                .iter()
                .any(|w| w.is::<gtk::Expander>() && w.is_visible()),
            "no Details expander"
        );
        assert!(
            !shown
                .iter()
                .any(|w| w.is::<gtk::ListBox>() && w.is_visible()),
            "no empty server list"
        );
        assert!(
            shown.iter().any(|w| w
                .downcast_ref::<gtk::Label>()
                .is_some_and(|l| l.has_css_class("warning")
                    && l.is_visible()
                    && !l.text().is_empty())),
            "the error is shown"
        );
        let confirm = button_named(&window, "toolport-setup-confirm");
        assert_eq!(confirm.label().as_deref(), Some("Retry"));
        assert!(button_named(&window, "toolport-setup-skip").is_visible());
        assert!(
            gtk::prelude::GtkWindowExt::focus(&window).as_ref() == Some(confirm.upcast_ref()),
            "Retry takes focus"
        );
        // Retry reads the review again: a new window, and the step has not ended.
        confirm.emit_clicked();
        wait_for(&|| window_titled("Connect Client").is_some_and(|again| again != window));
        assert!(ended.get().is_none(), "Retry does not end the step");
        let again = window_titled("Connect Client").unwrap();
        assert_ne!(again, window);
        button_named(&again, "toolport-setup-skip").emit_clicked();
        wait_for(&|| ended.get().is_some());
        assert_eq!(ended.get(), Some(StepEnd::Skipped));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn connect_review_names_the_codex_settings_it_adds() {
        let (sentence, lines) = gateway_settings_notice(
            "Codex",
            &[
                "supports_parallel_tool_calls = true".into(),
                "tool_timeout_sec = 600".into(),
            ],
        )
        .unwrap();
        assert!(sentence.contains("Codex entry, each only if it is not set"));
        assert!(sentence.contains("A setting you delete stays deleted until you connect again."));
        assert_eq!(
            lines,
            "supports_parallel_tool_calls = true\ntool_timeout_sec = 600"
        );
        assert!(gateway_settings_notice("Cursor", &[]).is_none());
    }

    #[test]
    #[ignore = "requires isolated GTK display"]
    fn gateway_settings_notice_is_selectable_but_not_a_tab_stop() {
        adw::init().unwrap();
        let long = format!("tool_timeout_sec = {}", "9".repeat(190));
        let card = gateway_settings_card("Toolport also adds these settings.", &long);
        let labels = descendants(card.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Label>().ok())
            .collect::<Vec<_>>();
        assert_eq!(labels.len(), 2);
        for label in &labels {
            assert!(label.is_selectable(), "{} stays selectable", label.text());
            assert!(!label.is_focusable(), "{} is not a Tab stop", label.text());
        }
        let lines = labels
            .iter()
            .find(|l| l.has_css_class("toolport-path-text"))
            .unwrap();
        assert!(lines.wraps());
        // React's 13px notice: the sentence in body text, the settings in 13px mono.
        assert!(labels
            .iter()
            .any(|l| l.text() == "Toolport also adds these settings."
                && l.has_css_class("toolport-dialog-field")));
        assert_eq!(lines.wrap_mode(), gtk::pango::WrapMode::WordChar);
        let (min, _, _, _) = card.measure(gtk::Orientation::Horizontal, -1);
        assert!(
            min < 400,
            "a long setting line must not widen the notice: {min}"
        );
    }

    /// The connect review and its result page say what React's do: the same lede, the
    /// restart line naming the client, "What your agent sees" even when nothing moved,
    /// warnings, and Details with the config path.
    #[test]
    #[ignore = "requires isolated GTK display"]
    fn connect_review_and_result_match_react() {
        adw::init().unwrap();
        let parent = gtk::Window::new();
        review_with_notice(
            &parent,
            "Connect parity fixture",
            Vec::new(),
            "Fixture",
            Some(connect_review_lede(false)),
            None,
            "Connect",
            true,
            |_, _, _| {
                Ok(Completion {
                    // A translated message with no ". " (Japanese ends sentences with
                    // "。"): the title still comes from the client, as React's does.
                    message: "Claude Code に接続しました。再起動してください。".into(),
                    // Enough moved servers that the page is taller than the window.
                    servers: (0..8)
                        .map(|i| crate::registry_controller::SetupServerResult {
                            name: format!("server-{i}"),
                            tool_count: 3,
                            credential_state: "none".into(),
                        })
                        .collect(),
                    warnings: vec![
                        "Other servers were not verified during this connection: a".into()
                    ],
                    connect: Some(ConnectResult {
                        client: "Claude Code".into(),
                        config_path: "/home/test/.claude.json".into(),
                    }),
                    ..Completion::default()
                })
            },
            || {},
            None,
            Step::default(),
        );
        let window = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Window>().ok())
            .find(|w| w.title().as_deref() == Some("Connect parity fixture"))
            .unwrap();
        let labels = |window: &gtk::Window| {
            descendants(window.upcast_ref())
                .into_iter()
                .filter_map(|w| w.downcast::<gtk::Label>().ok())
                .filter(|l| l.is_visible() && l.parent().is_some_and(|p| p.is_visible()))
                .map(|l| l.text().to_string())
                .collect::<Vec<_>>()
        };
        assert!(labels(&window)
            .iter()
            .any(|t| t == "Selected entries move into Toolport after verification."));
        assert!(!labels(&window)
            .iter()
            .any(|t| t == "Review the selected servers before continuing."));
        // Spec §2 and §8: a 13px secondary lede, React's 512px width, and a height that
        // follows the content instead of a fixed 540px with an empty band.
        let named_widget = |name: &str| {
            descendants(window.upcast_ref())
                .into_iter()
                .find(|w| w.widget_name() == name)
                .unwrap_or_else(|| panic!("{name}"))
        };
        assert!(named_widget("toolport-setup-lede").has_css_class("toolport-dialog-lede"));
        assert!(named_widget("toolport-setup-title").has_css_class("toolport-dialog-title"));
        // The window takes its content's size: 512 wide, and as tall as the content
        // instead of the old fixed 540px. Checked on the real allocation, after layout.
        let context = gtk::glib::MainContext::default();
        let settle = |done: &dyn Fn() -> bool| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while !done() && std::time::Instant::now() < deadline {
                context.iteration(false);
                std::thread::yield_now();
            }
        };
        assert!(!window.is_resizable());
        let content = window.child().unwrap();
        let natural_height = || content.measure(gtk::Orientation::Vertical, REVIEW_WIDTH).1;
        let fits = || {
            window.width() == REVIEW_WIDTH
                // The content plus the window's own frame, and no more.
                && (0..=12).contains(&(window.height() - natural_height()))
        };
        settle(&fits);
        assert!(
            fits(),
            "{}x{} for {}px of content",
            window.width(),
            window.height(),
            natural_height()
        );
        let natural = natural_height();
        assert!(
            natural < 540,
            "this small review needs less than the old fixed height"
        );
        let scroller = named_widget("toolport-setup-scroller")
            .downcast::<gtk::ScrolledWindow>()
            .unwrap();
        assert!(scroller.propagates_natural_height());
        // Details sits right under the lede, above the entries, as in React.
        let review_details = descendants(window.upcast_ref())
            .into_iter()
            .find(|w| w.widget_name() == "toolport-setup-review-details")
            .unwrap();
        let before = review_details
            .prev_sibling()
            .and_downcast::<gtk::Label>()
            .unwrap();
        assert_eq!(
            before.text(),
            "Selected entries move into Toolport after verification."
        );
        // React's plain, small Details fold, not a bordered card.
        assert!(review_details.has_css_class("toolport-disclosure"));
        assert!(review_details.has_css_class("small"));
        assert!(!review_details.has_css_class("toolport-details-expander"));
        // Opening a fold grows the window to fit, and closing it shrinks it back.
        let folded = window.height();
        let fold = review_details.downcast_ref::<gtk::Expander>().unwrap();
        fold.set_expanded(true);
        settle(&|| window.height() > folded && fits());
        assert!(
            window.height() > folded && fits(),
            "{} after opening, {folded} before",
            window.height()
        );
        fold.set_expanded(false);
        settle(&|| window.height() <= folded + 2 && fits());
        assert!(
            window.height() <= folded + 2,
            "{} after closing",
            window.height()
        );
        assert!(review_details
            .next_sibling()
            .is_some_and(|w| w.widget_name() == "toolport-setup-review-entries"));
        assert_eq!(
            review_title("Claude Code"),
            "Review and connect Claude Code"
        );
        // React's header: the title on the left, a close button, no header bar.
        let named = |name: &str| {
            descendants(window.upcast_ref())
                .into_iter()
                .find(|w| w.widget_name() == name)
                .unwrap_or_else(|| panic!("{name} is in the window"))
        };
        assert!(!descendants(window.upcast_ref())
            .iter()
            .any(|w| w.is::<adw::HeaderBar>()));
        let heading = named("toolport-setup-title")
            .downcast::<gtk::Label>()
            .unwrap();
        assert_eq!(heading.text(), "Connect parity fixture");
        assert_eq!(heading.xalign(), 0.0);
        assert!(named("toolport-setup-close").is_sensitive());
        let connect = descendants(window.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Button>().ok())
            .find(|b| b.label().as_deref() == Some("Connect"))
            .unwrap();
        // Spec section 5: Cancel then the confirm, together at the right of a footer
        // (a batch step's Skip sits between them, hidden until a step fails).
        let cancel = std::iter::successors(connect.prev_sibling(), |w| w.prev_sibling())
            .find(|w| w.is_visible())
            .and_downcast::<gtk::Button>()
            .unwrap();
        assert_eq!(cancel.label().as_deref(), Some("Cancel"));
        assert!(connect.next_sibling().is_none());
        connect.emit_clicked();
        let context = gtk::glib::MainContext::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while connect.label().as_deref() != Some("Done") && std::time::Instant::now() < deadline {
            context.iteration(false);
            std::thread::yield_now();
        }
        assert_eq!(connect.label().as_deref(), Some("Done"));
        // React's result: the title on the left says what happened, with a centred check
        // and left-aligned text; Done stays in the right of the footer.
        assert_eq!(heading.text(), "Claude Code connected");
        assert!(!descendants(window.upcast_ref())
            .iter()
            .any(|w| w.is::<adw::StatusPage>()));
        let icon = named("toolport-setup-success-icon");
        assert_eq!(icon.halign(), gtk::Align::Center);
        let next = named("toolport-setup-next-step")
            .downcast::<gtk::Label>()
            .unwrap();
        assert_eq!(next.xalign(), 0.0);
        assert!(connect.next_sibling().is_none());
        assert!(!cancel.is_visible());
        let shown = labels(&window);
        assert!(
            shown
                .iter()
                .any(|t| t == "Restart Claude Code to load Toolport."),
            "{shown:?}"
        );
        assert!(shown
            .iter()
            .any(|t| t == "Other servers were not verified during this connection: a"));
        let expander = |name: &str| {
            descendants(window.upcast_ref())
                .into_iter()
                .find(|w| w.widget_name() == name)
                .and_then(|w| w.downcast::<gtk::Expander>().ok())
                .unwrap_or_else(|| panic!("{name} is on the result page"))
        };
        let agent = expander("toolport-setup-agent-sees");
        assert!(agent.is_visible());
        assert_eq!(agent.label().as_deref(), Some("What your agent sees"));
        let inside = |expander: &gtk::Expander| {
            descendants(expander.child().unwrap().upcast_ref())
                .into_iter()
                .filter_map(|w| w.downcast::<gtk::Label>().ok())
                .map(|l| l.text().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            inside(&agent),
            vec!["No servers moved, so your agent has no new tools.".to_string()]
        );
        let details = expander("toolport-setup-details");
        assert_eq!(
            inside(&details),
            vec!["Config: /home/test/.claude.json".to_string()]
        );
        window.close();
    }

    #[test]
    #[ignore = "requires isolated GTK display"]
    fn connected_page_drops_the_gateway_settings_notice() {
        adw::init().unwrap();
        let parent = gtk::Window::new();
        review_with_notice(
            &parent,
            "Notice success fixture",
            Vec::new(),
            "Fixture",
            None,
            gateway_settings_notice("Codex", &["tool_timeout_sec = 600".into()]),
            "Connect",
            true,
            |_, _, _| Ok("Codex connected. Restart it to load Toolport.".into()),
            || {},
            None,
            Step::default(),
        );
        let window = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Window>().ok())
            .find(|w| w.title().as_deref() == Some("Notice success fixture"))
            .unwrap();
        let notice = || {
            descendants(window.upcast_ref())
                .into_iter()
                .find(|w| w.widget_name() == "toolport-setup-gateway-settings")
                .unwrap()
        };
        assert!(notice().is_visible(), "the review shows the notice");
        let connect = descendants(window.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Button>().ok())
            .find(|b| b.label().as_deref() == Some("Connect"))
            .unwrap();
        connect.emit_clicked();
        let context = gtk::glib::MainContext::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while connect.label().as_deref() != Some("Done") && std::time::Instant::now() < deadline {
            context.iteration(false);
            std::thread::yield_now();
        }
        assert_eq!(connect.label().as_deref(), Some("Done"));
        assert!(!notice().is_visible(), "the connected page has no notice");
        window.close();
    }

    #[test]
    #[ignore = "requires isolated GTK display"]
    fn close_action_closes_review_error() {
        adw::init().unwrap();
        let _lock = crate::registry::data_dir_test_lock();
        let scratch = std::env::temp_dir().join(format!(
            "toolport-setup-close-regression-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&scratch).unwrap();
        let _data = crate::registry::DataDirOverride::set(&scratch);
        let previous = crate::i18n::language();
        for language in ["en", "de", "ja"] {
            crate::i18n::set_language(language).unwrap();
            let parent = gtk::Window::new();
            review(
                &parent,
                "Could not review setup",
                Vec::new(),
                "Invalid fixture config",
                crate::i18n::msg("Close"),
                false,
                |_, _, _| Err("not an import".into()),
                || {},
                None,
            );
            let window = review_window();
            assert!(descendants(window.upcast_ref())
                .iter()
                .filter(|w| w.is::<gtk::Expander>())
                .all(|w| w.has_css_class("toolport-disclosure")
                    && !w.has_css_class("toolport-details-expander")));
            let button = descendants(window.upcast_ref())
                .into_iter()
                .filter_map(|w| w.downcast::<gtk::Button>().ok())
                .find(|b| b.widget_name() == "toolport-setup-confirm")
                .unwrap();
            assert_eq!(
                button.label().as_deref(),
                Some(crate::i18n::tr("Close").as_str())
            );
            button.emit_clicked();
            assert!(
                !window.is_visible(),
                "Close must close the {language} error dialog"
            );
        }
        crate::i18n::set_language(&previous).unwrap();
        std::fs::remove_dir_all(scratch).unwrap();
    }

    #[test]
    #[ignore = "requires isolated GTK display"]
    fn credential_state_tracks_keychain_choice() {
        adw::init().unwrap();
        let parent = gtk::Window::new();
        review(
            &parent,
            "Credential choice fixture",
            vec![SetupItem {
                key: "one".into(),
                name: "One".into(),
                transport: "stdio".into(),
                command: Some("one".into()),
                args: vec![],
                url: None,
                env_keys: vec!["PORT".into()],
                is_new: true,
                credentials: vec![crate::registry_controller::CredentialReview {
                    key: "PORT".into(),
                    secret: false,
                    present: true,
                    required: true,
                }],
                unsupported: None,
                updates: Vec::new(),
                source_name: None,
            }],
            "Fixture",
            "Connect",
            true,
            |_, _, _| Err("fixture".into()),
            || {},
            None,
        );
        let window = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Window>().ok())
            .find(|w| w.title().as_deref() == Some("Credential choice fixture"))
            .unwrap();
        for expander in descendants(window.upcast_ref())
            .iter()
            .filter_map(|w| w.downcast_ref::<gtk::Expander>())
        {
            expander.set_expanded(true);
        }
        let widgets = descendants(window.upcast_ref());
        let choice = widgets
            .iter()
            .filter_map(|w| w.downcast_ref::<gtk::CheckButton>())
            .find(|c| c.label().as_deref() == Some("Keep PORT in keychain"))
            .unwrap();
        // React's one line: the choice, then the value's state, with no row fill.
        let line = choice.parent().unwrap();
        assert_eq!(line.widget_name(), "toolport-setup-credential");
        assert!(line
            .last_child()
            .and_downcast::<gtk::Label>()
            .is_some_and(|state| state.text() == "Found"));
        assert!(!widgets
            .iter()
            .any(|w| w.is::<gtk::ListBox>() && w.ancestor(gtk::Expander::static_type()).is_some()));
        assert!(!choice.is_active());
        choice.set_active(true);
        assert!(widgets
            .iter()
            .filter_map(|w| w.downcast_ref::<gtk::Label>())
            .any(|label| label.text() == "Found, goes to keychain"));
        choice.set_active(false);
        assert!(!widgets
            .iter()
            .filter_map(|w| w.downcast_ref::<gtk::Label>())
            .any(|label| label.text() == "Found, goes to keychain"));
        window.close();
    }

    #[test]
    #[ignore = "requires isolated GTK display"]
    fn reviewed_optional_value_does_not_block_connection() {
        adw::init().unwrap();
        let parent = gtk::Window::new();
        review(
            &parent,
            "Optional fixture",
            vec![SetupItem {
                key: "one".into(),
                name: "One".into(),
                transport: "stdio".into(),
                command: Some("fixture".into()),
                args: vec![],
                url: None,
                env_keys: vec!["PAT".into()],
                is_new: true,
                credentials: vec![crate::registry_controller::CredentialReview {
                    key: "PAT".into(),
                    secret: true,
                    present: false,
                    required: false,
                }],
                unsupported: None,
                updates: vec![],
                source_name: None,
            }],
            "Fixture",
            "Connect",
            true,
            |_, _, _| Err("fixture".into()),
            || {},
            None,
        );
        let window = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Window>().ok())
            .find(|w| w.title().as_deref() == Some("Optional fixture"))
            .unwrap();
        for expander in descendants(window.upcast_ref())
            .iter()
            .filter_map(|w| w.downcast_ref::<gtk::Expander>())
        {
            expander.set_expanded(true);
        }
        let widgets = descendants(window.upcast_ref());
        assert!(widgets
            .iter()
            .filter_map(|w| w.downcast_ref::<gtk::Button>())
            .find(|b| b.label().as_deref() == Some("Connect"))
            .unwrap()
            .is_sensitive());
        assert!(widgets
            .iter()
            .filter_map(|w| w.downcast_ref::<gtk::Label>())
            .any(|l| l.text() == "Optional"));
        window.close();
    }

    fn import_item(name: &str, source: Option<&str>, unsupported: Option<&str>) -> SetupItem {
        SetupItem {
            key: name.into(),
            name: name.into(),
            transport: "http".into(),
            command: None,
            args: Vec::new(),
            url: Some("https://example.invalid/mcp".into()),
            env_keys: Vec::new(),
            is_new: true,
            credentials: Vec::new(),
            unsupported: unsupported.map(str::to_string),
            updates: Vec::new(),
            source_name: source.map(str::to_string),
        }
    }

    #[test]
    fn import_hint_names_the_saved_name_only_when_it_differs() {
        assert_eq!(
            import_hint(&import_item("github (2)", Some("github"), None)).as_deref(),
            Some("Import as github (2)")
        );
        assert_eq!(
            import_hint(&import_item("github", Some("github"), None)),
            None
        );
        assert_eq!(import_hint(&import_item("github", None, None)), None);
        // An unsupported server is not imported, so it lands nowhere.
        assert_eq!(
            import_hint(&import_item("github (2)", Some("github"), Some("SSE"))),
            None
        );
    }

    #[test]
    #[ignore = "requires isolated GTK display"]
    fn import_review_shows_saved_names_and_marks_unsupported_for_review() {
        adw::init().unwrap();
        let parent = gtk::Window::new();
        review(
            &parent,
            "Import fixture",
            vec![
                import_item("github (2)", Some("github"), None),
                import_item(
                    "remote",
                    None,
                    Some("The saved connection could not be resolved."),
                ),
            ],
            "Fixture",
            "Import selected servers",
            false,
            |_, _, _| Err("fixture".into()),
            || {},
            None,
        );
        let window = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Window>().ok())
            .find(|w| w.title().as_deref() == Some("Import fixture"))
            .unwrap();
        let toggles = descendants(window.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::ToggleButton>().ok())
            .filter(|t| t.has_css_class("toolport-review-item"))
            .collect::<Vec<_>>();
        assert_eq!(toggles.len(), 2);
        let visible_texts = |toggle: &gtk::ToggleButton| {
            descendants(toggle.upcast_ref())
                .into_iter()
                .filter_map(|w| w.downcast::<gtk::Label>().ok())
                .filter(|l| l.is_visible())
                .map(|l| l.text().to_string())
                .collect::<Vec<_>>()
        };
        // The client's own name, its status, then where it lands above the command.
        let (github, remote) = (&toggles[0], &toggles[1]);
        assert_eq!(
            visible_texts(github),
            [
                "github",
                "New",
                "Import as github (2)",
                "https://example.invalid/mcp"
            ]
        );
        assert!(described_as(
            github,
            "Import as github (2), https://example.invalid/mcp, New"
        ));
        assert!(github.is_active() && github.is_sensitive());
        let remote_texts = visible_texts(remote);
        assert_eq!(remote_texts[..2], ["remote", "Needs review"]);
        assert!(!remote_texts.iter().any(|t| t.starts_with("Import as")));
        assert!(!remote.is_active() && !remote.is_sensitive());
        window.close();
    }

    /// Each entry is one toggle like React's review button: a square dot that fills when
    /// selected, the name and command read by screen readers, the pressed state on the
    /// toggle, and an unsupported entry off and not pressable.
    #[test]
    #[ignore = "requires isolated GTK display"]
    fn review_entries_are_dot_toggles_like_react() {
        adw::init().unwrap();
        let parent = gtk::Window::new();
        let item = |key: &str, unsupported: Option<&str>| SetupItem {
            key: key.into(),
            name: key.into(),
            transport: "stdio".into(),
            command: Some(format!("/usr/bin/{key}")),
            args: Vec::new(),
            url: None,
            env_keys: Vec::new(),
            is_new: true,
            credentials: Vec::new(),
            unsupported: unsupported.map(str::to_string),
            updates: Vec::new(),
            source_name: None,
        };
        review(
            &parent,
            "Toggle fixture",
            vec![item("notes", None), item("legacy", Some("SSE"))],
            "Fixture",
            "Connect",
            true,
            |_, _, _| Err("fixture".into()),
            || {},
            None,
        );
        let window = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Window>().ok())
            .find(|w| w.title().as_deref() == Some("Toggle fixture"))
            .unwrap();
        let widgets = descendants(window.upcast_ref());
        assert!(!widgets.iter().any(|w| w.is::<gtk::CheckButton>()));
        let toggles = widgets
            .iter()
            .filter_map(|w| w.downcast_ref::<gtk::ToggleButton>())
            .filter(|t| t.has_css_class("toolport-review-item"))
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(toggles.len(), 2);
        let (notes, legacy) = (&toggles[0], &toggles[1]);
        assert_eq!(notes.accessible_role(), gtk::AccessibleRole::ToggleButton);
        assert!(notes.is_focusable());
        assert!(notes.is_active() && notes.is_sensitive());
        assert!(descendants(notes.upcast_ref())
            .iter()
            .any(|w| w.has_css_class("toolport-select-dot")));
        // The Error chip stays hidden until a check fails.
        let labels = descendants(notes.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Label>().ok())
            .filter(|l| l.is_visible())
            .collect::<Vec<_>>();
        let texts = labels
            .iter()
            .map(|l| l.text().to_string())
            .collect::<Vec<_>>();
        // React's ImportRow: the name and its status on the dot's line, the command on
        // its own line under them, wrapping when short enough.
        assert_eq!(texts, ["notes", "New", "/usr/bin/notes"]);
        // A screen reader hears the command and the status beside the name, as in React's
        // button text, and a status shown later (the Error chip) joins it.
        assert!(described_as(notes, "/usr/bin/notes, New"));
        let error_chip = descendants(notes.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Label>().ok())
            .find(|l| l.text() == "Error")
            .unwrap();
        error_chip.set_visible(true);
        assert!(described_as(notes, "/usr/bin/notes, New, Error"));
        error_chip.set_visible(false);
        assert!(described_as(notes, "/usr/bin/notes, New"));
        let (name, command) = (&labels[0], &labels[2]);
        assert!(name.has_css_class("toolport-review-name"));
        let dot_line = name.parent().unwrap();
        assert!(dot_line
            .first_child()
            .is_some_and(|w| w.has_css_class("toolport-select-dot")));
        assert_eq!(
            command.parent().unwrap().first_child().as_ref(),
            Some(&dot_line)
        );
        assert!(command.wraps() && command.has_css_class("monospace"));
        assert_eq!(command.tooltip_text().as_deref(), Some("/usr/bin/notes"));
        // Pressing it deselects it, as React's button does.
        notes.emit_clicked();
        assert!(!notes.is_active());
        notes.emit_clicked();
        assert!(notes.is_active());
        assert!(!legacy.is_active() && !legacy.is_sensitive());
        assert!(descendants(legacy.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Label>().ok())
            .any(|l| l.text() == "Unsupported: SSE"));
        window.close();
    }

    #[test]
    #[ignore = "requires isolated GTK display"]
    fn review_rows_show_markup_characters_as_text() {
        adw::init().unwrap();
        let parent = gtk::Window::new();
        let mut item = import_item("R&D <tools>", None, None);
        item.command = Some("npx".into());
        item.args = vec!["--token".into(), "<launch-input>".into()];
        item.url = None;
        review(
            &parent,
            "Markup fixture",
            vec![item],
            "Fixture",
            "Import selected servers",
            false,
            |_, _, _| Err("fixture".into()),
            || {},
            None,
        );
        let window = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Window>().ok())
            .find(|w| w.title().as_deref() == Some("Markup fixture"))
            .unwrap();
        let row = descendants(window.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::ToggleButton>().ok())
            .find(|t| t.has_css_class("toolport-review-item"))
            .unwrap();
        let shown = descendants(row.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Label>().ok())
            .map(|l| l.text().to_string())
            .collect::<Vec<_>>();
        assert!(
            shown.iter().any(|t| t == "npx --token <launch-input>"),
            "{shown:?}"
        );
        assert!(shown.iter().any(|t| t == "R&D <tools>"), "{shown:?}");
        window.close();
    }

    #[test]
    #[ignore = "requires isolated GTK display"]
    fn reviewed_missing_value_and_existing_update_are_explicit() {
        adw::init().unwrap();
        let parent = gtk::Window::new();
        review(
            &parent,
            "Update fixture",
            vec![SetupItem {
                key: "one".into(),
                name: "One".into(),
                transport: "stdio".into(),
                command: Some("fixture".into()),
                args: Vec::new(),
                url: None,
                env_keys: vec!["PAT".into()],
                is_new: false,
                credentials: vec![crate::registry_controller::CredentialReview {
                    key: "PAT".into(),
                    secret: true,
                    present: false,
                    required: true,
                }],
                unsupported: None,
                updates: vec!["Environment".into(), "Launch settings".into()],
                source_name: None,
            }],
            "Fixture",
            "Connect",
            true,
            |_, _, _| Err("fixture".into()),
            || {},
            None,
        );
        let window = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Window>().ok())
            .find(|w| w.title().as_deref() == Some("Update fixture"))
            .unwrap();
        let widgets = descendants(window.upcast_ref());
        let confirm = widgets
            .iter()
            .filter_map(|w| w.downcast_ref::<gtk::Button>())
            .find(|b| b.label().as_deref() == Some("Connect"))
            .unwrap();
        assert!(!confirm.is_sensitive());
        assert!(widgets
            .iter()
            .filter_map(|w| w.downcast_ref::<gtk::Label>())
            .any(|l| l.text() == "Enter PAT or deselect One"
                // It names the key, so it wraps anywhere instead of widening the dialog.
                && l.wrap_mode() == gtk::pango::WrapMode::WordChar));
        let selected = widgets
            .iter()
            .filter_map(|w| w.downcast_ref::<gtk::ToggleButton>())
            .find(|toggle| toggle.has_css_class("toolport-review-item"))
            .unwrap();
        selected.set_active(false);
        assert!(confirm.is_sensitive());
        selected.set_active(true);
        assert!(!confirm.is_sensitive());
        assert!(widgets.iter().any(|widget| widget.is::<gtk::Expander>()));
        assert!(
            !widgets.iter().any(|widget| widget.is::<adw::ExpanderRow>()),
            "setup must use the app's standard expander under Yaru"
        );
        for expander in widgets
            .iter()
            .filter_map(|w| w.downcast_ref::<gtk::Expander>())
        {
            expander.set_expanded(true);
        }
        let widgets = descendants(window.upcast_ref());
        assert!(widgets
            .iter()
            .filter_map(|w| w.downcast_ref::<gtk::Button>())
            .any(|b| b.label().as_deref() == Some("Enter value")));
        assert!(!widgets
            .iter()
            .filter_map(|w| w.downcast_ref::<gtk::Button>())
            .any(|b| b.label().as_deref() == Some("Open Credentials")));
        assert!(widgets
            .iter()
            .filter_map(|w| w.downcast_ref::<gtk::Label>())
            .any(|l| l.text() == "Updates existing server"));
        assert!(widgets
            .iter()
            .filter_map(|w| w.downcast_ref::<gtk::Label>())
            .any(|l| l.text() == "Environment, Launch settings"));
        window.close();
    }

    #[test]
    #[ignore = "requires isolated GTK display"]
    fn successful_review_shows_restart_and_only_done() {
        adw::init().unwrap();
        let parent = gtk::Window::new();
        review(
            &parent,
            "Success fixture",
            Vec::new(),
            "Fixture",
            "Connect",
            true,
            |_, _, _| {
                Ok(Completion {
                    message: "Claude Code connected. Restart it to load Toolport.".into(),
                    // A connect's cleanup warning is shown, as React shows it.
                    warnings: vec![
                        "connected; could not remove the old keychain token: locked".into()
                    ],
                    ..Completion::default()
                })
            },
            || {},
            None,
        );
        let window = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Window>().ok())
            .find(|w| w.title().as_deref() == Some("Success fixture"))
            .unwrap();
        let widgets = descendants(window.upcast_ref());
        let connect = widgets
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Button>().ok())
            .find(|b| b.label().as_deref() == Some("Connect"))
            .unwrap();
        connect.emit_clicked();
        let context = gtk::glib::MainContext::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while connect.label().as_deref() != Some("Done") && std::time::Instant::now() < deadline {
            context.iteration(false);
            std::thread::yield_now();
        }
        assert_eq!(connect.label().as_deref(), Some("Done"));
        let done = Some(connect.clone().upcast::<gtk::Widget>());
        assert_eq!(window.default_widget(), done);
        assert_eq!(gtk::prelude::GtkWindowExt::focus(&window), done);
        let widgets = descendants(window.upcast_ref());
        assert!(widgets
            .iter()
            .filter_map(|w| w.downcast_ref::<gtk::Label>())
            .any(|label| label.text() == "Restart it to load Toolport." && label.is_visible()));
        assert!(widgets
            .iter()
            .filter_map(|w| w.downcast_ref::<gtk::Label>())
            .any(|label| label.text()
                == "connected; could not remove the old keychain token: locked"
                && label.is_visible()));
        assert!(!widgets
            .iter()
            .filter_map(|w| w.downcast_ref::<gtk::Button>())
            .any(|b| b.label().as_deref() == Some("Connect")));
        window.close();
    }

    #[test]
    #[ignore = "requires isolated GTK display"]
    fn a_review_closed_while_connecting_only_refreshes() {
        adw::init().unwrap();
        let parent = gtk::Window::new();
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let wait = std::sync::Mutex::new(wait);
        let refreshed = std::rc::Rc::new(std::cell::Cell::new(0));
        let count = refreshed.clone();
        review(
            &parent,
            "Closed fixture",
            Vec::new(),
            "Fixture",
            "Connect",
            true,
            move |_, _, _| {
                let _ = wait.lock().unwrap().recv();
                Ok("Claude Code connected. Restart it to load Toolport.".into())
            },
            move || count.set(count.get() + 1),
            None,
        );
        let window = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Window>().ok())
            .find(|w| w.title().as_deref() == Some("Closed fixture"))
            .unwrap();
        let connect = descendants(window.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Button>().ok())
            .find(|b| b.label().as_deref() == Some("Connect"))
            .unwrap();
        connect.emit_clicked();
        window.set_visible(false);
        release.send(()).unwrap();
        let context = gtk::glib::MainContext::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while refreshed.get() == 0 && std::time::Instant::now() < deadline {
            context.iteration(false);
            std::thread::yield_now();
        }
        assert_eq!(refreshed.get(), 1, "a success still refreshes the page");
        assert_eq!(connect.label().as_deref(), Some("Connect"));
        window.close();
    }

    #[test]
    #[ignore = "manual isolated setup screenshot fixture"]
    fn setup_screenshot_fixture() {
        // A manual fixture: it keeps the window open for a screenshot. Run it only
        // when asked (TOOLPORT_SETUP_FIXTURE_STATE set, for example to "ready"),
        // so a run of every ignored test under xvfb does not wait on it forever.
        let Ok(state) = std::env::var("TOOLPORT_SETUP_FIXTURE_STATE") else {
            eprintln!(
                "setup_screenshot_fixture skipped: set TOOLPORT_SETUP_FIXTURE_STATE to run it"
            );
            return;
        };
        adw::init().unwrap();
        let scratch =
            std::env::temp_dir().join(format!("toolport-gtk-setup-fixture-{}", std::process::id()));
        let _data = crate::registry::DataDirOverride::set(&scratch);
        let mut registry = crate::registry::Registry::default();
        registry.add_server(serde_json::from_value(serde_json::json!({"id":"calendar", "name":"Calendar", "transport":"stdio", "command":"fixture-calendar", "args":[], "env":[{"key":"PAT", "value":null,"secret":true}]})).unwrap());
        crate::registry::save(&registry).unwrap();
        let app = adw::Application::builder()
            .application_id("com.toolport.SetupFixture")
            .build();
        app.register(None::<&gtk::gio::Cancellable>).unwrap();
        let parent = adw::ApplicationWindow::new(&app);
        let theme = super::super::theme::ThemeController::new();
        theme.attach(&parent);
        let parent = parent.upcast::<gtk::Window>();
        let broker = crate::approval_broker::start_native();
        let (_, credential_page, _) = super::super::build_content(&app, broker);
        parent.set_title(Some("Toolport fixture"));
        parent.set_default_size(1000, 740);
        parent.present();
        let items = ["Notes", "Calendar"]
            .into_iter()
            .map(|name| SetupItem {
                key: name.into(),
                name: name.into(),
                transport: "stdio".into(),
                command: Some(format!("/usr/bin/fixture-{}", name.to_lowercase())),
                args: Vec::new(),
                url: None,
                env_keys: if name == "Calendar" {
                    vec!["PAT".into()]
                } else {
                    Vec::new()
                },
                is_new: true,
                credentials: if name == "Calendar" {
                    vec![crate::registry_controller::CredentialReview {
                        key: "PAT".into(),
                        secret: true,
                        present: state != "missing" && state != "optional",
                        required: state != "optional",
                    }]
                } else {
                    Vec::new()
                },
                unsupported: None,
                updates: Vec::new(),
                source_name: None,
            })
            .collect();
        review(
            &parent,
            "Review and connect Claude Code",
            items,
            "Config: /home/sbx/.claude.json\nBackups will be saved in Toolport/backups/claude-code",
            "Connect",
            true,
            move |_, _, _| {
                if state == "verifying" {
                    use std::io::Read;
                    let mut file = std::fs::File::open("/home/sbx/setup-release")
                        .map_err(|e| e.to_string())?;
                    let mut bytes = [0u8; 1];
                    let _ = file.read(&mut bytes).map_err(|e| e.to_string())?;
                }
                if state == "failure" {
                    return Err("Calendar could not start. Check its command and retry. Client config unchanged.".into());
                }
                if state == "missing" {
                    return Err("Calendar needs credentials. Enter value and retry. Client config unchanged.".into());
                }
                Ok(Completion {
                    message: "Claude Code connected. Restart it to load Toolport.".into(),
                    servers: vec![
                        crate::registry_controller::SetupServerResult {
                            name: "Notes".into(),
                            tool_count: 3,
                            credential_state: "none".into(),
                        },
                        crate::registry_controller::SetupServerResult {
                            name: "Calendar".into(),
                            tool_count: 5,
                            credential_state: "stored".into(),
                        },
                    ],
                    tools: vec![
                        serde_json::json!({"name":"toolport_search_tools"}),
                        serde_json::json!({"name":"toolport_call_tool"}),
                    ],
                    backup: None,
                    warnings: Vec::new(),
                    connect: None,
                })
            },
            || {},
            Some(credential_page),
        );
        // Even when asked for, close after a while (TOOLPORT_SETUP_FIXTURE_SECONDS,
        // 300 by default) so a forgotten run ends.
        let seconds = std::env::var("TOOLPORT_SETUP_FIXTURE_SECONDS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(300);
        let main_loop = gtk::glib::MainLoop::new(None, false);
        let quit = main_loop.clone();
        gtk::glib::timeout_add_seconds_local_once(seconds, move || quit.quit());
        main_loop.run();
    }

    /// A connect dialog never runs off a narrow window: it takes React's width, capped
    /// to the window less 16px each side, and its credential field is masked with no
    /// show/hide control, as React's.
    #[test]
    #[ignore = "requires isolated GTK display"]
    fn connect_dialogs_fit_a_narrow_window_and_mask_the_credential() {
        adw::init().unwrap();
        let owner = gtk::Window::builder()
            .default_width(300)
            .default_height(600)
            .build();
        owner.present();
        let context = gtk::glib::MainContext::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while owner.width() == 0 && std::time::Instant::now() < deadline {
            context.iteration(false);
        }
        assert_eq!(dialog_width(&owner, REVIEW_WIDTH), owner.width() - 32);
        assert_eq!(
            dialog_width(&gtk::Window::new(), REVIEW_WIDTH),
            REVIEW_WIDTH
        );
        // The window's allocation can still settle without a window manager, so the
        // prompt is checked against React's width rather than an exact figure.
        let prompt = credential_prompt(&owner, "notes", "NOTES_TOKEN", |_| {});
        assert!(prompt.default_width() < 384, "{}", prompt.default_width());
        let entry = descendants(prompt.upcast_ref())
            .into_iter()
            .find_map(|w| w.downcast::<gtk::PasswordEntry>().ok())
            .unwrap();
        assert!(!entry.shows_peek_icon());
        prompt.close();
        // A long key wraps instead of widening the sheet past the window.
        let key = "A_VERY_LONG_ENVIRONMENT_VARIABLE_NAME_FOR_A_PERSONAL_ACCESS_TOKEN_VALUE";
        let prompt = credential_prompt(&owner, "notes", key, |_| {});
        let field = descendants(prompt.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Label>().ok())
            .find(|l| l.text() == key)
            .unwrap();
        let (minimum, _, _, _) = field.measure(gtk::Orientation::Horizontal, -1);
        assert!(minimum < 200, "{minimum}");
        prompt.close();
        owner.close();
    }

    /// Spec §5: the credential prompt is a sheet with a right-aligned footer, Cancel then
    /// "Use for connection", not a message dialog's response strip.
    #[test]
    #[ignore = "requires isolated GTK display"]
    fn credential_prompt_uses_the_dialog_footer() {
        adw::init().unwrap();
        let owner = gtk::Window::new();
        let saved = std::rc::Rc::new(std::cell::RefCell::new(None::<String>));
        let prompt = {
            let saved = saved.clone();
            credential_prompt(&owner, "notes", "NOTES_TOKEN", move |value| {
                *saved.borrow_mut() = Some(value)
            })
        };
        // React's default dialog width, its header with a close button, its lede, and the
        // key as the field's label.
        assert_eq!(prompt.default_width(), 384);
        let widgets = descendants(prompt.upcast_ref());
        assert!(widgets
            .iter()
            .filter_map(|w| w.downcast_ref::<gtk::Button>())
            .any(|b| b.icon_name().as_deref() == Some("window-close-symbolic")));
        let labels = widgets
            .iter()
            .filter_map(|w| w.downcast_ref::<gtk::Label>())
            .collect::<Vec<_>>();
        assert!(labels.iter().any(|l| l.text()
            == "This value is used only for the selected server. It is saved only if connection succeeds."
            && l.has_css_class("toolport-dialog-lede")));
        assert!(labels
            .iter()
            .any(|l| l.text() == "NOTES_TOKEN" && l.has_css_class("toolport-dialog-field")));
        let footer = widgets
            .iter()
            .find(|w| w.has_css_class("toolport-dialog-footer"))
            .expect("the dialog footer");
        let buttons = descendants(footer)
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Button>().ok())
            .collect::<Vec<_>>();
        assert_eq!(
            buttons
                .iter()
                .map(|b| b.label().unwrap_or_default().to_string())
                .collect::<Vec<_>>(),
            ["Cancel", "Use for connection"]
        );
        assert!(buttons[0].has_css_class("flat"));
        assert!(buttons[1].has_css_class("suggested-action"));
        assert!(!buttons[1].is_sensitive(), "nothing to use yet");
        let heading = widgets
            .iter()
            .filter_map(|w| w.downcast_ref::<gtk::Label>())
            .find(|l| l.has_css_class("toolport-dialog-title"))
            .unwrap();
        assert_eq!(heading.text(), "notes credentials");
        let entry = widgets
            .iter()
            .find_map(|w| w.downcast_ref::<gtk::PasswordEntry>())
            .unwrap()
            .clone();
        assert!(entry.has_css_class("toolport-input"));
        // Spaces alone are no value: React's prompt and the review's check both refuse it.
        entry.set_text("   ");
        assert!(!buttons[1].is_sensitive(), "spaces are not a value");
        buttons[1].emit_clicked();
        assert!(saved.borrow().is_none());
        assert!(prompt.is_visible());
        entry.set_text("secret-value");
        assert!(buttons[1].is_sensitive());
        buttons[1].emit_clicked();
        assert_eq!(saved.borrow().as_deref(), Some("secret-value"));
        assert!(!prompt.is_visible(), "saving closes the sheet");
        owner.destroy();
    }

    #[test]
    #[ignore = "requires isolated GTK display"]
    fn review_list_caps_at_parent_height_and_keeps_footer_visible() {
        adw::init().unwrap();
        let parent = gtk::Window::new();
        parent.set_default_size(480, 600);
        parent.present();
        wait_for(&|| parent.width() >= 480 && parent.height() >= 600);
        let item = |i: usize| SetupItem {
            key: format!("s{i}"),
            name: format!("srv-{i:02}"),
            source_name: None,
            transport: "stdio".into(),
            command: Some("/bin/true".into()),
            args: vec![],
            url: None,
            env_keys: vec![],
            is_new: true,
            credentials: vec![],
            unsupported: None,
            updates: vec![],
        };
        review(
            &parent,
            "Height",
            (0..40).map(item).collect(),
            "Config: /c\nBackup: /b",
            "Connect",
            true,
            |_, _, _| {
                Ok(Completion {
                    servers: (0..40)
                        .map(|i| crate::registry_controller::SetupServerResult {
                            name: format!("srv-{i:02}"),
                            tool_count: 3,
                            credential_state: "none".into(),
                        })
                        .collect(),
                    connect: Some(ConnectResult {
                        client: "Claude Code".into(),
                        config_path: "/c".into(),
                    }),
                    ..Completion::default()
                })
            },
            || {},
            None,
        );
        let win = window_titled("Height").unwrap();
        let all = descendants(win.upcast_ref());
        let named = |n: &str| all.iter().find(|w| w.widget_name() == n).unwrap().clone();
        let footer = all
            .iter()
            .find(|w| w.has_css_class("toolport-dialog-footer"))
            .unwrap();
        let confirm = button_named(&win, "toolport-setup-confirm");
        let details: gtk::Expander = named("toolport-setup-review-details").downcast().unwrap();
        let entries: gtk::ScrolledWindow =
            named("toolport-setup-review-entries").downcast().unwrap();
        let outer: gtk::ScrolledWindow = named("toolport-setup-scroller").downcast().unwrap();
        let fits = |w: &gtk::Widget, bx: &gtk::Widget, pad: f32| {
            let ok = |b: gtk::graphene::Rect| {
                b.height() > 0.0 && b.y() >= -1.0 && b.y() + b.height() <= bx.height() as f32 + pad
            };
            w.compute_bounds(bx).is_some_and(ok)
        };
        let footer_ok =
            || footer.is_visible() && confirm.is_visible() && fits(footer, win.upcast_ref(), 1.0);
        wait_for(&|| {
            win.height() > 0 && footer.height() > 0 && win.height() <= parent.height() - 32
        });
        assert!(win.width() == parent.width() - 32 && win.height() <= parent.height() - 32);
        assert!(footer_ok() && !entries.is_focusable());
        assert!(entries.vadjustment().upper() > entries.vadjustment().page_size());
        assert!(outer.vadjustment().upper() <= outer.vadjustment().page_size() + 1.0);
        let (t_lbl, l_lbl) = (named("toolport-setup-title"), named("toolport-setup-lede"));
        for w in [&t_lbl, &l_lbl, details.upcast_ref(), footer] {
            assert!(!w.is_ancestor(&entries) && !entries.is_ancestor(w));
        }
        details.set_expanded(true);
        wait_for(&|| details.height() > 40 && win.height() <= parent.height() - 32);
        assert!(win.height() <= parent.height() - 32 && footer_ok());
        assert!(outer.vadjustment().upper() <= outer.vadjustment().page_size() + 1.0);
        details.set_expanded(false);
        wait_for(&|| details.height() < 40 && win.height() <= parent.height() - 32);
        assert!(footer_ok());
        let toggles = descendants(entries.upcast_ref())
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::ToggleButton>().ok());
        let last = toggles.last().unwrap();
        last.grab_focus();
        wait_for(&|| {
            entries.vadjustment().value() > 0.0
                && fits(last.upcast_ref(), entries.upcast_ref(), 2.0)
        });
        assert!(fits(last.upcast_ref(), entries.upcast_ref(), 2.0));
        let h0 = win.height();
        parent.set_default_size(960, 840);
        wait_for(&|| parent.height() >= 820 && win.height() > h0);
        assert!(win.width() == REVIEW_WIDTH && win.height() <= parent.height() - 32 && footer_ok());
        parent.set_default_size(480, 500);
        wait_for(&|| parent.height() <= 520 && win.height() <= parent.height() - 32);
        assert!(win.height() <= parent.height() - 32 && footer_ok());
        // A result page taller than the window scrolls as a whole, at full heights:
        // its check and lines are not squeezed to fit.
        confirm.emit_clicked();
        wait_for(&|| confirm.label().as_deref() == Some("Done"));
        let unsqueezed = |name: &str| {
            let widget = descendants(win.upcast_ref())
                .into_iter()
                .find(|w| w.widget_name() == name)
                .unwrap();
            widget.height() >= widget.measure(gtk::Orientation::Vertical, widget.width()).1
        };
        wait_for(&|| outer.vadjustment().upper() > outer.vadjustment().page_size());
        assert!(win.height() <= parent.height() - 32 && footer_ok());
        assert!(outer.vadjustment().upper() > outer.vadjustment().page_size());
        assert!(unsqueezed("toolport-setup-success-icon"));
        assert!(unsqueezed("toolport-setup-next-step"));
        win.close();
        // Many unsatisfied required credentials must not collapse the multi-item review list.
        let cred_item = |i: usize| SetupItem {
            credentials: vec![crate::registry_controller::CredentialReview {
                key: format!("TOKEN_{i}"),
                secret: true,
                present: false,
                required: true,
            }],
            ..item(i)
        };
        review(
            &parent,
            "Warnings",
            (0..12).map(cred_item).collect(),
            "Config: /c",
            "Connect",
            true,
            |_, _, _| Ok(Completion::default()),
            || {},
            None,
        );
        let warn_win = window_titled("Warnings").unwrap();
        let warn_all = descendants(warn_win.upcast_ref());
        let warn_entries: gtk::ScrolledWindow = warn_all
            .iter()
            .find(|w| w.widget_name() == "toolport-setup-review-entries")
            .unwrap()
            .clone()
            .downcast()
            .unwrap();
        let warn_footer = warn_all
            .iter()
            .find(|w| w.has_css_class("toolport-dialog-footer"))
            .unwrap();
        wait_for(&|| {
            warn_win.height() > 0
                && warn_footer.height() > 0
                && warn_win.height() <= parent.height() - 32
        });
        assert!(warn_entries.height() >= 90);
        assert!(fits(warn_footer, warn_win.upcast_ref(), 1.0));
        warn_win.close();
        parent.close();
    }
}
