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
    /// Set for a client connect: its result page then shows what React's does (the
    /// restart line naming the client, warnings, "What your agent sees" even when nothing
    /// moved, and Details with the config and backup).
    pub connect: Option<ConnectResult>,
}

/// What a client connect's result page needs beyond the servers and tools.
#[derive(Default)]
pub(super) struct ConnectResult {
    pub client: String,
    pub config_path: String,
    pub warnings: Vec<String>,
}

/// The connect review's title, naming the client as React's does.
pub(super) fn connect_review_title(client_name: &str) -> String {
    crate::i18n::trf("Review and connect {value0}", &[("value0", client_name)])
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
/// The tallest the review's body grows before it scrolls.
const REVIEW_MAX_BODY_HEIGHT: i32 = 600;

/// The confirm while a check runs: a spinner and "Checking…", insensitive. Spec §11 keeps
/// spinners inside the button that is working.
fn show_working(button: &gtk::Button) {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    content.set_halign(gtk::Align::Center);
    let spinner = gtk::Spinner::new();
    spinner.start();
    content.append(&spinner);
    content.append(&l10n::label(crate::i18n::msg("Checking…")));
    button.set_child(Some(&content));
    button.set_sensitive(false);
}

/// Puts `label` back on a button `show_working` changed.
fn end_working(button: &gtk::Button, label: &'static str) {
    button.set_child(None::<&gtk::Widget>);
    l10n::bind_text(button, label);
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
        .default_width(384)
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
    // The key names the field above it, as React's label does.
    let field = gtk::Label::builder().label(key).xalign(0.0).build();
    field.add_css_class("toolport-field-label");
    let entry = gtk::PasswordEntry::builder()
        .show_peek_icon(true)
        .activates_default(true)
        .build();
    entry.update_property(&[gtk::accessible::Property::Label(key)]);
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
        let save = save.clone();
        entry.connect_changed(move |entry| save.set_sensitive(!entry.text().is_empty()));
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
            if entry.text().is_empty() {
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
    subtitle: gtk::Label,
    suffixes: gtk::Box,
    check: gtk::ToggleButton,
    name: String,
}

impl ReviewItem {
    fn new(check: &gtk::ToggleButton, name: &str, command: &str) -> Self {
        // React's ImportRow: the dot, the name and the status on one line, then the
        // command on its own line across the whole entry.
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

    /// The name is the button's accessible name and the command its description, as a
    /// screen reader reads React's button text; the toggle itself reports pressed.
    fn describe(&self) {
        self.check.update_property(&[
            gtk::accessible::Property::Label(&self.name),
            gtk::accessible::Property::Description(&self.subtitle.label()),
        ]);
    }
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

/// React's plain `<details>` disclosure in the connect dialogs: the title is a toggle with
/// no border or fill, and the content sits under it. The spec has no disclosure style, so
/// React's decides.
fn details_expander(title: &str) -> (gtk::Expander, gtk::Box) {
    let expander = gtk::Expander::new(Some(title));
    expander.add_css_class("toolport-disclosure");
    let content = gtk::Box::new(gtk::Orientation::Vertical, 2);
    content.set_margin_top(8);
    expander.set_child(Some(&content));
    // A dialog sized to its content grows and shrinks with the fold, as React's does,
    // up to the body's maximum height.
    expander.connect_expanded_notify(|expander| {
        let expander = expander.downgrade();
        gtk::glib::idle_add_local_once(move || {
            let Some(window) = expander
                .upgrade()
                .and_then(|expander| expander.root())
                .and_downcast::<gtk::Window>()
            else {
                return;
            };
            let width = if window.width() > 0 {
                window.width()
            } else {
                window.default_width()
            };
            let (_, natural, _, _) = window.measure(gtk::Orientation::Vertical, width);
            window.set_default_size(width, natural);
        });
    });
    (expander, content)
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
            label.add_css_class("monospace");
            label.set_margin_bottom(12);
        } else {
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
) {
    let dialog = adw::Window::builder()
        .transient_for(parent)
        .modal(true)
        .title(title)
        // React's dialog width; the height follows the content, so no empty band
        // sits above the footer (spec §8).
        .default_width(REVIEW_WIDTH)
        // -1: the natural height (adw::Window otherwise starts at 200px).
        .default_height(-1)
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
    root.append(&header);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 16);
    body.set_margin_top(12);
    body.set_margin_bottom(24);
    body.set_margin_start(24);
    body.set_margin_end(24);
    let lede = {
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
    rows.set_widget_name("toolport-setup-review-entries");
    let mut selected = Vec::new();
    let mut credential_choices = Vec::new();
    let credential_inputs =
        std::rc::Rc::new(std::cell::RefCell::new(std::collections::BTreeMap::<
            String,
            std::collections::BTreeMap<String, String>,
        >::new()));
    let busy = std::rc::Rc::new(std::cell::Cell::new(false));
    let required_fields = std::rc::Rc::new(std::cell::RefCell::new(Vec::<(
        gtk::glib::WeakRef<gtk::ToggleButton>,
        String,
        Vec<crate::registry_controller::CredentialReview>,
    )>::new()));
    let missing_reason = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
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
        let row = ReviewItem::new(&check, &item.name, &command);
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
        let tag = l10n::label(if !item.updates.is_empty() {
            crate::i18n::msg("Updates existing server")
        } else if item.is_new {
            crate::i18n::msg("New")
        } else {
            crate::i18n::msg("In Toolport")
        });
        if !item.updates.is_empty() {
            let changes = adw::ActionRow::builder()
                .title(crate::i18n::tr("Changes"))
                .subtitle(item.updates.join(", "))
                .build();
            credential_rows.append(&changes);
        }
        tag.add_css_class("dim-label");
        row.add_suffix(&tag);
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
            item.key,
            item.name,
            row_error,
            error_chip,
        ));
    }
    body.append(&rows);
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
        .max_content_height(REVIEW_MAX_BODY_HEIGHT)
        .build();
    scroller.set_widget_name("toolport-setup-scroller");
    root.append(&scroller);
    root.append(&super::dialog_footer(&[&cancel, &confirm]));
    dialog.set_content(Some(&root));
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
    let selected = std::rc::Rc::new(selected);
    let credential_choices = std::rc::Rc::new(credential_choices);
    let action = std::sync::Arc::new(action);
    let finished = std::rc::Rc::new(finished);
    let completed = std::rc::Rc::new(std::cell::Cell::new(false));
    let closing = dialog.clone();
    let titled = dialog.clone();
    confirm.connect_clicked(move |button| {
        if completed.get() {
            closing.close();
            return;
        }
        let chosen = selected
            .iter()
            .filter(|(check, _, _, _, _, _)| check.is_active())
            .map(|(_, _, key, _, _, _)| key.clone())
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
        for (check, _row, _, name, row_error, error_chip) in selected.iter() {
            error_chip.set_visible(false);
            check.set_sensitive(false);
            // Plain text under each entry being checked, as React's review shows; the
            // spinner stays in the confirm.
            row_error.remove_css_class("toolport-inline-error");
            row_error.add_css_class("toolport-muted");
            row_error.set_label(&crate::i18n::trf("Checking {name}...", &[("name", name)]));
            row_error.set_visible(check.is_active());
        }
        // The confirm is the working button: the only place a spinner shows (spec §11).
        show_working(button);
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
        let (feedback, button, cancel, body, rows, details, lede, notice) = (
            feedback.clone(),
            button.clone(),
            cancel.clone(),
            body.clone(),
            rows.clone(),
            details.clone(),
            lede.clone(),
            notice.clone(),
        );
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || action(chosen, choices, inputs)).await;
            busy.set(false);
            for (name, key, choice) in credential_choices.iter() {
                choice.set_sensitive(
                    !key.starts_with("__")
                        && selected.iter().any(|(_, unsupported, _, server, _, _)| {
                            server == name && !unsupported
                        }),
                );
            }
            cancel.set_sensitive(true);
            button.set_sensitive(true);
            end_working(&button, confirm_label);
            for (check, unsupported, _, _, row_error, _) in selected.iter() {
                row_error.set_visible(false);
                row_error.remove_css_class("toolport-muted");
                row_error.add_css_class("toolport-inline-error");
                check.set_sensitive(!unsupported);
            }
            match result {
                Ok(Ok(outcome)) => {
                    completed.set(true);
                    cancel.set_visible(false);
                    l10n::bind_text(&button, "Done");
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
                    let done_title = outcome
                        .message
                        .split_once(". ")
                        .map_or(outcome.message.as_str(), |(title, _)| title)
                        .to_string();
                    heading.set_label(&done_title);
                    dialog.set_title(Some(&done_title));
                    // GTK's built-in check icon; emblem-ok is missing from some themes.
                    let status = gtk::Image::from_icon_name("object-select-symbolic");
                    status.set_pixel_size(40);
                    status.set_halign(gtk::Align::Center);
                    status.set_widget_name("toolport-setup-success-icon");
                    status.add_css_class("toolport-success-icon");
                    status.set_accessible_role(gtk::AccessibleRole::Presentation);
                    body.prepend(&status);
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
                    if let Some(connect) = &outcome.connect {
                        for warning in &connect.warnings {
                            let line = gtk::Label::builder()
                                .label(warning)
                                .xalign(0.0)
                                .wrap(true)
                                .selectable(true)
                                .build();
                            line.add_css_class("toolport-muted");
                            body.append(&line);
                        }
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
                    // Return activates Done without first tabbing through the results.
                    button.grab_focus();
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
                    refresh_confirm();
                    let mut has_row_error = false;
                    for (_, _, _, name, row_error, error_chip) in selected.iter() {
                        if error.contains(name) {
                            has_row_error = true;
                            error_chip.set_visible(true);
                            row_error.set_label(&error);
                            row_error.set_visible(true);
                        }
                    }
                    feedback.set_visible(!has_row_error);
                    button.grab_focus();
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
    connect_with(
        parent,
        client_id,
        profile,
        force,
        std::rc::Rc::new(finished),
        credential_page,
    );
}

fn connect_with(
    parent: &gtk::Window,
    client_id: String,
    profile: Option<String>,
    force: bool,
    finished: std::rc::Rc<dyn Fn()>,
    credential_page: Option<super::ServerPage>,
) {
    let parent = parent.clone();
    gtk::glib::spawn_future_local(async move {
        let id = client_id.clone();
        let client_name = gtk::gio::spawn_blocking(move || review_client_name(&id))
            .await
            .unwrap_or_else(|_| crate::i18n::tr("Client"));
        let title = connect_review_title(&client_name);
        // The loading and error sheets are titled as React's are.
        let sheet_title = crate::i18n::trf("Connect {value0}", &[("value0", &client_name)]);
        // Spec §11 Loading: nothing for the first 300ms, then the review's sheet with
        // three skeleton rows until the preview is ready. Cancel there drops the review.
        let cancelled = std::rc::Rc::new(std::cell::Cell::new(false));
        let loading = std::rc::Rc::new(std::cell::RefCell::new(None::<adw::Window>));
        let timer = std::rc::Rc::new(std::cell::Cell::new(None::<gtk::glib::SourceId>));
        timer.set(Some({
            let (parent, title, cancelled, loading, timer) = (
                parent.clone(),
                sheet_title.clone(),
                cancelled.clone(),
                loading.clone(),
                timer.clone(),
            );
            gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
                timer.take();
                *loading.borrow_mut() = Some(loading_sheet(&parent, &title, cancelled));
            })
        }));
        let id = client_id.clone();
        let preview =
            gtk::gio::spawn_blocking(move || crate::registry_controller::preview_client_setup(&id))
                .await;
        if let Some(pending) = timer.take() {
            pending.remove();
        }
        if let Some(sheet) = loading.borrow_mut().take() {
            sheet.close();
        }
        if cancelled.get() {
            return;
        }
        // Retry from the error callout runs the whole review again.
        let retry: std::rc::Rc<dyn Fn()> = {
            let (parent, client_id, profile, finished, credential_page) = (
                parent.clone(),
                client_id.clone(),
                profile.clone(),
                finished.clone(),
                credential_page.clone(),
            );
            std::rc::Rc::new(move || {
                connect_with(
                    &parent,
                    client_id.clone(),
                    profile.clone(),
                    force,
                    finished.clone(),
                    credential_page.clone(),
                )
            })
        };
        match preview {
            Ok(Ok(preview)) => {
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
                review_with_notice(
                    &parent,
                    &title,
                    preview.items,
                    &disclosure,
                    Some(connect_review_lede(force)),
                    notice,
                    crate::i18n::msg("Connect to Toolport"),
                    true,
                    move |selected, choices, inputs| {
                        let outcome = crate::registry_controller::migrate_client_reviewed_inputs(
                            &client_id,
                            profile.as_deref(),
                            force,
                            &selected,
                            &preview.revision,
                            &choices,
                            &inputs,
                        )?;
                        Ok(Completion {
                            message: crate::i18n::trf(
                                "{value0} connected. Restart it to load Toolport.",
                                &[("value0", &client_name.to_string())],
                            ),
                            servers: outcome.servers,
                            tools: outcome.tools,
                            backup: outcome.result.outcome.backup.clone(),
                            connect: Some(ConnectResult {
                                client: client_name.clone(),
                                config_path: outcome.result.outcome.path.clone(),
                                warnings: outcome.result.outcome.warnings.clone(),
                            }),
                        })
                    },
                    move || finished(),
                    credential_page,
                );
            }
            Ok(Err(error)) => {
                error_sheet(&parent, &sheet_title, &error, retry);
            }
            Err(_) => {
                error_sheet(
                    &parent,
                    &sheet_title,
                    &crate::i18n::tr("Client review stopped."),
                    retry,
                );
            }
        }
    });
}

/// A small sheet for the review's other states, as React's: its dialog header (left
/// title and a close button) over `content`. Esc closes it.
fn state_sheet(parent: &gtk::Window, title: &str, content: &gtk::Widget) -> adw::Window {
    let sheet = adw::Window::builder()
        .transient_for(parent)
        .modal(true)
        .title(title)
        .default_width(REVIEW_WIDTH)
        .default_height(-1)
        .build();
    let heading = gtk::Label::builder()
        .label(title)
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .build();
    let (header, _) = dialog_header(&sheet, &heading);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 16);
    body.set_margin_top(12);
    body.set_margin_bottom(24);
    body.set_margin_start(24);
    body.set_margin_end(24);
    body.append(content);
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&header);
    root.append(&body);
    sheet.set_content(Some(&root));
    super::close_on_escape(&sheet);
    sheet
}

/// The review while its preview loads: three skeleton rows. Closing it drops the review.
fn loading_sheet(
    parent: &gtk::Window,
    title: &str,
    cancelled: std::rc::Rc<std::cell::Cell<bool>>,
) -> adw::Window {
    let rows = super::skeleton_rows(crate::i18n::msg("Checking…"));
    // The sheet itself only opens after 300ms, so the rows show at once.
    rows.set_opacity(1.0);
    let sheet = state_sheet(parent, title, rows.upcast_ref());
    sheet.set_widget_name("toolport-setup-loading");
    // Closing it any way but by the preview finishing drops the review.
    sheet.connect_close_request(move |_| {
        cancelled.set(true);
        gtk::glib::Propagation::Proceed
    });
    sheet.present();
    sheet
}

/// A review that could not start: the spec §11 danger callout with the cause, View log
/// and Retry.
fn error_sheet(
    parent: &gtk::Window,
    title: &str,
    cause: &str,
    retry: std::rc::Rc<dyn Fn()>,
) -> adw::Window {
    let holder = std::rc::Rc::new(std::cell::RefCell::new(
        None::<gtk::glib::WeakRef<adw::Window>>,
    ));
    let callout = super::action_error_callout(
        &crate::i18n::tr("Couldn't read config"),
        cause,
        Some({
            let holder = holder.clone();
            std::rc::Rc::new(move || {
                if let Some(sheet) = holder.borrow().as_ref().and_then(|weak| weak.upgrade()) {
                    sheet.close();
                }
                retry();
            })
        }),
    );
    let sheet = state_sheet(parent, title, callout.upcast_ref());
    sheet.set_widget_name("toolport-setup-error");
    *holder.borrow_mut() = Some(sheet.downgrade());
    sheet.present();
    sheet
}

#[cfg(test)]
mod tests {
    use super::*;

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
            .find(|l| l.has_css_class("monospace"))
            .unwrap();
        assert!(lines.wraps());
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
                    message: "Claude Code connected. Restart it to load Toolport.".into(),
                    connect: Some(ConnectResult {
                        client: "Claude Code".into(),
                        config_path: "/home/test/.claude.json".into(),
                        warnings: vec![
                            "Other servers were not verified during this connection: a".into()
                        ],
                    }),
                    ..Completion::default()
                })
            },
            || {},
            None,
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
        assert_eq!(window.default_width(), REVIEW_WIDTH);
        // GTK reports the size it took as the default once shown, so compare it with
        // the content's natural height instead of the old fixed 540px.
        let content = window.child().unwrap();
        let (_, natural, _, _) = content.measure(gtk::Orientation::Vertical, REVIEW_WIDTH);
        assert!(
            // 200px is the window's own minimum height.
            window.default_height() <= natural.max(200) + 2,
            "{} tall for {natural}px of content",
            window.default_height()
        );
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
        let folded = window.default_height();
        let fold = review_details.downcast_ref::<gtk::Expander>().unwrap();
        let context = gtk::glib::MainContext::default();
        let settle = |done: &dyn Fn() -> bool| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while !done() && std::time::Instant::now() < deadline {
                context.iteration(false);
                std::thread::yield_now();
            }
        };
        fold.set_expanded(true);
        settle(&|| window.default_height() > folded);
        assert!(
            window.default_height() > folded,
            "{} stayed {folded}",
            window.default_height()
        );
        fold.set_expanded(false);
        settle(&|| window.default_height() <= folded + 2);
        assert!(window.default_height() <= folded + 2);
        assert!(review_details
            .next_sibling()
            .is_some_and(|w| w.widget_name() == "toolport-setup-review-entries"));
        assert_eq!(
            connect_review_title("Claude Code"),
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
        // Spec section 5: Cancel then the confirm, together at the right of a footer.
        let cancel = connect
            .prev_sibling()
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
            .any(|l| l.text() == "Enter PAT or deselect One"));
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
            |_, _, _| Ok("Claude Code connected. Restart it to load Toolport.".into()),
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
        let widgets = descendants(window.upcast_ref());
        assert!(widgets
            .iter()
            .filter_map(|w| w.downcast_ref::<gtk::Label>())
            .any(|label| label.text() == "Restart it to load Toolport." && label.is_visible()));
        assert!(!widgets
            .iter()
            .filter_map(|w| w.downcast_ref::<gtk::Button>())
            .any(|b| b.label().as_deref() == Some("Connect")));
        window.close();
    }

    #[test]
    #[ignore = "manual isolated setup screenshot fixture"]
    fn setup_screenshot_fixture() {
        adw::init().unwrap();
        let state = std::env::var("TOOLPORT_SETUP_FIXTURE_STATE").unwrap_or_default();
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
                    connect: None,
                })
            },
            || {},
            Some(credential_page),
        );
        gtk::glib::MainLoop::new(None, false).run();
    }

    /// Spec §11: the confirm is the only working control while a check runs. It shows a
    /// spinner and "Checking…", then its own label again.
    #[test]
    #[ignore = "requires isolated GTK display"]
    fn the_confirm_is_the_working_button() {
        adw::init().unwrap();
        let button = l10n::button("Connect to Toolport");
        show_working(&button);
        assert!(!button.is_sensitive());
        let inside = descendants(button.upcast_ref());
        assert!(inside.iter().any(|w| w.is::<gtk::Spinner>()));
        assert!(inside
            .iter()
            .filter_map(|w| w.downcast_ref::<gtk::Label>())
            .any(|l| l.text() == "Checking…"));
        end_working(&button, "Connect to Toolport");
        assert_eq!(button.label().as_deref(), Some("Connect to Toolport"));
        assert!(!descendants(button.upcast_ref())
            .iter()
            .any(|w| w.is::<gtk::Spinner>()));
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
            .any(|l| l.text() == "NOTES_TOKEN" && l.has_css_class("toolport-field-label")));
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
        entry.set_text("secret-value");
        assert!(buttons[1].is_sensitive());
        buttons[1].emit_clicked();
        assert_eq!(saved.borrow().as_deref(), Some("secret-value"));
        assert!(!prompt.is_visible(), "saving closes the sheet");
        owner.destroy();
    }

    /// Spec §11: a review that can't start is a danger callout with the cause, View log
    /// and Retry, and Retry runs the review again; a slow preview shows skeleton rows, and
    /// closing them drops the review. Both are titled and closed as React's are.
    #[test]
    #[ignore = "requires isolated GTK display"]
    fn review_waits_and_failures_follow_the_spec() {
        adw::init().unwrap();
        let parent = gtk::Window::new();
        let retried = std::rc::Rc::new(std::cell::Cell::new(0));
        let sheet = {
            let retried = retried.clone();
            error_sheet(
                &parent,
                "Connect Cursor",
                "expected value at line 1 column 3",
                std::rc::Rc::new(move || retried.set(retried.get() + 1)),
            )
        };
        let widgets = descendants(sheet.upcast_ref());
        let callout = widgets
            .iter()
            .find(|w| w.has_css_class("toolport-error-callout"))
            .expect("a danger callout");
        assert_eq!(callout.accessible_role(), gtk::AccessibleRole::Alert);
        let texts = descendants(callout)
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Label>().ok())
            .map(|l| l.text().to_string())
            .collect::<Vec<_>>();
        assert!(texts.contains(&"Couldn't read config".to_string()));
        assert!(texts.contains(&"expected value at line 1 column 3".to_string()));
        let close_button = |widgets: &[gtk::Widget]| {
            assert!(!widgets
                .iter()
                .any(|w| w.has_css_class("toolport-dialog-footer")));
            widgets
                .iter()
                .filter_map(|w| w.downcast_ref::<gtk::Button>())
                .find(|b| b.icon_name().as_deref() == Some("window-close-symbolic"))
                .expect("the header's close button")
                .clone()
        };
        close_button(&widgets);
        assert!(widgets
            .iter()
            .filter_map(|w| w.downcast_ref::<gtk::Label>())
            .any(|l| l.text() == "Connect Cursor" && l.has_css_class("toolport-dialog-title")));
        let retry = descendants(callout)
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Button>().ok())
            .find(|b| b.label().as_deref() == Some("Retry"))
            .unwrap();
        retry.emit_clicked();
        assert_eq!(retried.get(), 1);
        assert!(!sheet.is_visible(), "Retry replaces the sheet");

        let cancelled = std::rc::Rc::new(std::cell::Cell::new(false));
        let loading = loading_sheet(&parent, "Connect Cursor", cancelled.clone());
        let widgets = descendants(loading.upcast_ref());
        let skeleton = widgets
            .iter()
            .find(|w| w.has_css_class("toolport-skeleton-group"))
            .expect("skeleton rows");
        assert_eq!(skeleton.opacity(), 1.0, "the sheet itself waited 300ms");
        assert!(
            !widgets.iter().any(|w| w.is::<gtk::Spinner>()),
            "no spinner"
        );
        close_button(&widgets).emit_clicked();
        assert!(cancelled.get(), "closing drops the review");
        parent.destroy();
    }
}
