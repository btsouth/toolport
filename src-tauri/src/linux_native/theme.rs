use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::gio;
use serde::Deserialize;

const FALLBACK_BACKGROUND: &str = "#1d2027";
const FALLBACK_DARK_BACKGROUND: &str = "#20232b";
const FALLBACK_LIGHTER_BACKGROUND: &str = "#252931";
const FALLBACK_FOREGROUND: &str = "#e8e9ec";
const FALLBACK_MUTED: &str = "#a1a9b8";
const FALLBACK_ACCENT: &str = "#7cb0ff";
const FALLBACK_SELECTION: &str = "#334155";
const FALLBACK_RED: &str = "#f38ba8";
const FALLBACK_GREEN: &str = "#7fd0a0";
const FALLBACK_YELLOW: &str = "#f0c674";
const FALLBACK_CYAN: &str = "#38bdf8";

/// WCAG AA for body, secondary and status text (12 to 14px).
const TEXT_CONTRAST: f64 = 4.5;
/// WCAG minimum for placeholders, which never carry information on their own.
const DISABLED_CONTRAST: f64 = 3.0;
/// The strongest foreground tint (chips) and accent tint (selected rows) that
/// secondary text sits on; the weaker hover and info tints fall between these
/// and the plain surface.
const FG_TINT: f64 = 0.07;
const SELECTION_TINT: f64 = 0.14;
/// Status badges and tiles: the status color at 12% under its own text.
const STATUS_TINT: f64 = 0.12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThemeMode {
    Light,
    Dark,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OmarchyPalette {
    mode: ThemeMode,
    background: String,
    dark_background: String,
    lighter_background: String,
    foreground: String,
    muted: String,
    accent: String,
    selection: String,
    red: String,
    green: String,
    yellow: String,
    cyan: String,
}

#[derive(Debug, Default, Deserialize)]
struct RawPalette {
    mode: Option<String>,
    background: Option<String>,
    dark_background: Option<String>,
    lighter_background: Option<String>,
    foreground: Option<String>,
    muted: Option<String>,
    accent: Option<String>,
    selection: Option<String>,
    red: Option<String>,
    green: Option<String>,
    yellow: Option<String>,
    cyan: Option<String>,
}

impl Default for OmarchyPalette {
    fn default() -> Self {
        Self {
            mode: ThemeMode::Dark,
            background: FALLBACK_BACKGROUND.into(),
            dark_background: FALLBACK_DARK_BACKGROUND.into(),
            lighter_background: FALLBACK_LIGHTER_BACKGROUND.into(),
            foreground: FALLBACK_FOREGROUND.into(),
            muted: FALLBACK_MUTED.into(),
            accent: FALLBACK_ACCENT.into(),
            selection: FALLBACK_SELECTION.into(),
            red: FALLBACK_RED.into(),
            green: FALLBACK_GREEN.into(),
            yellow: FALLBACK_YELLOW.into(),
            cyan: FALLBACK_CYAN.into(),
        }
    }
}

impl OmarchyPalette {
    fn parse(input: &str) -> Result<Self, String> {
        let raw: RawPalette = toml::from_str(input)
            .map_err(|error| format!("could not parse Omarchy colors.toml: {error}"))?;
        let fallback = Self::default();
        Ok(Self {
            mode: match raw.mode.as_deref() {
                Some("light") => ThemeMode::Light,
                Some("dark") => ThemeMode::Dark,
                _ => fallback.mode,
            },
            background: safe_color(raw.background, &fallback.background),
            dark_background: safe_color(raw.dark_background, &fallback.dark_background),
            lighter_background: safe_color(raw.lighter_background, &fallback.lighter_background),
            foreground: safe_color(raw.foreground, &fallback.foreground),
            muted: safe_color(raw.muted, &fallback.muted),
            accent: safe_color(raw.accent, &fallback.accent),
            selection: safe_color(raw.selection, &fallback.selection),
            red: safe_color(raw.red, &fallback.red),
            green: safe_color(raw.green, &fallback.green),
            yellow: safe_color(raw.yellow, &fallback.yellow),
            cyan: safe_color(raw.cyan, &fallback.cyan),
        })
    }

    fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .map_err(|error| format!("could not read {}: {error}", path.display()))
            .and_then(|contents| Self::parse(&contents))
            .unwrap_or_else(|error| {
                eprintln!("toolport-gtk: {error}; using the fallback palette");
                Self::default()
            })
    }

    /// Colors the stylesheet derives from the palette. Every surface that holds
    /// text is opaque. Text colors start from the theme's own color and move
    /// toward black (light themes) or white (dark themes) only as far as WCAG AA
    /// needs on every surface they sit on. Stock themes whose `muted` is a border
    /// color, or whose status colors are pale on a light background, stay
    /// readable and keep their hue.
    fn tokens(&self) -> Tokens {
        let background = rgb(&self.background);
        let card = mix(background, rgb(&self.lighter_background), 0.5).map(f64::round);
        let surfaces = [background, rgb(&self.dark_background), card];
        let (black, white) = ([0.0; 3], [255.0; 3]);
        let toward = if contrast(black, background) > contrast(white, background) {
            black
        } else {
            white
        };
        let (foreground, accent) = (rgb(&self.foreground), rgb(&self.accent));
        // Secondary text sits on the plain surfaces and on the foreground and
        // selection tints drawn over them (chips, info badges, selected rows).
        let secondary_backdrops = |_: Rgb| {
            surfaces
                .iter()
                .flat_map(|&surface| {
                    [
                        surface,
                        mix(surface, foreground, FG_TINT),
                        mix(surface, accent, SELECTION_TINT),
                    ]
                })
                .collect::<Vec<_>>()
        };
        let secondary = readable(rgb(&self.muted), toward, TEXT_CONTRAST, secondary_backdrops);
        // Placeholders: the secondary color pulled 30% toward the page, kept at 3:1.
        let disabled = readable(
            mix(secondary, background, 0.3).map(f64::round),
            toward,
            DISABLED_CONTRAST,
            secondary_backdrops,
        );
        // Status text sits on the surfaces and on its own 12% tint over them.
        let status = |color: &str| {
            hex(readable(rgb(color), toward, TEXT_CONTRAST, |candidate| {
                surfaces
                    .iter()
                    .flat_map(|&surface| [surface, mix(surface, candidate, STATUS_TINT)])
                    .collect()
            }))
        };
        Tokens {
            card: hex(card),
            text_secondary: hex(secondary),
            text_disabled: hex(disabled),
            success: status(&self.green),
            warning: status(&self.yellow),
            danger: status(&self.red),
            partial: status(&self.cyan),
        }
    }

    fn css(&self) -> String {
        let tokens = self.tokens();
        format!(
            r#"
/* Palette from Omarchy's colors.toml. */
@define-color toolport_bg {background};
@define-color toolport_bg_dark {dark_background};
@define-color toolport_surface {lighter_background};
@define-color toolport_fg {foreground};
@define-color toolport_muted {muted};
@define-color toolport_accent {accent};
@define-color toolport_selection {selection};

/* Semantic tokens (design spec: bg, sidebar, surface, raised, border, text,
   text-secondary, text-disabled, accent and the status colors). Surfaces are
   opaque; alpha is only for tints, hover, selection and borders on top of them. */
@define-color toolport_sidebar @toolport_bg_dark;
@define-color toolport_card {card};
@define-color toolport_raised @toolport_bg_dark;
@define-color toolport_border alpha(@toolport_fg, 0.10);
@define-color toolport_text_secondary {text_secondary};
@define-color toolport_text_disabled {text_disabled};
@define-color toolport_success {success};
@define-color toolport_warning {warning};
@define-color toolport_error {danger};
@define-color toolport_partial {partial};
@define-color toolport_info @toolport_text_secondary;
@define-color toolport_info_tint alpha(@toolport_fg, 0.06);
@define-color toolport_off @toolport_text_secondary;

/* libadwaita before 1.6 (the crate supports 1.4) styles widgets and the
   .accent/.success/.warning/.error classes from these named colors instead of
   the CSS variables below, so define both. */
@define-color accent_bg_color @toolport_accent;
@define-color accent_fg_color @toolport_bg_dark;
@define-color accent_color @toolport_accent;
@define-color destructive_bg_color @toolport_error;
@define-color destructive_fg_color @toolport_bg_dark;
@define-color destructive_color @toolport_error;
@define-color success_bg_color @toolport_success;
@define-color success_fg_color @toolport_bg_dark;
@define-color success_color @toolport_success;
@define-color warning_bg_color @toolport_warning;
@define-color warning_fg_color @toolport_bg_dark;
@define-color warning_color @toolport_warning;
@define-color error_bg_color @toolport_error;
@define-color error_fg_color @toolport_bg_dark;
@define-color error_color @toolport_error;
@define-color window_bg_color @toolport_bg;
@define-color window_fg_color @toolport_fg;
@define-color view_bg_color @toolport_bg_dark;
@define-color view_fg_color @toolport_fg;
@define-color headerbar_bg_color @toolport_sidebar;
@define-color headerbar_fg_color @toolport_fg;
@define-color headerbar_border_color @toolport_border;
@define-color sidebar_bg_color @toolport_sidebar;
@define-color sidebar_fg_color @toolport_fg;
@define-color card_bg_color @toolport_card;
@define-color card_fg_color @toolport_fg;
@define-color dialog_bg_color @toolport_raised;
@define-color dialog_fg_color @toolport_fg;
@define-color popover_bg_color @toolport_raised;
@define-color popover_fg_color @toolport_fg;
@define-color shade_color alpha(black, 0.28);

:root {{
  --accent-bg-color: @toolport_accent;
  --accent-fg-color: @toolport_bg_dark;
  --accent-color: @toolport_accent;
  --destructive-bg-color: @toolport_error;
  --destructive-fg-color: @toolport_bg_dark;
  --destructive-color: @toolport_error;
  --success-bg-color: @toolport_success;
  --success-fg-color: @toolport_bg_dark;
  --success-color: @toolport_success;
  --warning-bg-color: @toolport_warning;
  --warning-fg-color: @toolport_bg_dark;
  --warning-color: @toolport_warning;
  --error-bg-color: @toolport_error;
  --error-fg-color: @toolport_bg_dark;
  --error-color: @toolport_error;
  --window-bg-color: @toolport_bg;
  --window-fg-color: @toolport_fg;
  --view-bg-color: @toolport_bg_dark;
  --view-fg-color: @toolport_fg;
  --headerbar-bg-color: @toolport_sidebar;
  --headerbar-fg-color: @toolport_fg;
  --headerbar-border-color: @toolport_border;
  --sidebar-bg-color: @toolport_sidebar;
  --sidebar-fg-color: @toolport_fg;
  --card-bg-color: @toolport_card;
  --card-fg-color: @toolport_fg;
  --dialog-bg-color: @toolport_raised;
  --dialog-fg-color: @toolport_fg;
  --popover-bg-color: @toolport_raised;
  --popover-fg-color: @toolport_fg;
  --shade-color: alpha(black, 0.28);
}}

/* libadwaita fades secondary labels with opacity, which drops them below AA
   on most themes. Use the opaque secondary color instead, and the opaque
   disabled color (3:1) for placeholders. */
.toolport-native .dim-label,
.toolport-native .dimmed,
.toolport-native row label.subtitle,
.toolport-native headerbar .subtitle,
.toolport-native windowtitle .subtitle {{
  opacity: 1;
  color: @toolport_text_secondary;
}}

.toolport-native entry > text > placeholder,
.toolport-native spinbutton > text > placeholder,
.toolport-native textview > text > placeholder {{
  opacity: 1;
  color: @toolport_text_disabled;
}}

tooltip.background {{
  background-color: @toolport_raised;
  color: @toolport_fg;
  border: 1px solid @toolport_border;
  border-radius: 8px;
}}

window {{
  background-color: @toolport_bg;
  color: @toolport_fg;
  font-family: "Inter", system-ui, sans-serif;
  font-size: 14px;
}}

window.messagedialog,
window.dialog-window.alert,
dialog-host > dialog.alert sheet {{
  background-color: @toolport_raised;
  color: @toolport_fg;
}}

window.messagedialog .message-area > .body,
window.dialog-window.alert .message-area > .body,
dialog-host > dialog.alert .message-area > .body {{
  color: @toolport_text_secondary;
}}

window.messagedialog .response-area > button,
window.dialog-window.alert .response-area > button,
dialog-host > dialog.alert .response-area > button {{
  border: 1px solid @toolport_border;
  color: @toolport_fg;
  background-color: @toolport_card;
  box-shadow: none;
}}

window.messagedialog .response-area > button:hover,
window.dialog-window.alert .response-area > button:hover,
dialog-host > dialog.alert .response-area > button:hover {{
  background-color: alpha(@toolport_accent, 0.14);
  border-color: alpha(@toolport_accent, 0.30);
}}

window.messagedialog .response-area > button.suggested-action,
window.dialog-window.alert .response-area > button.suggested-action,
dialog-host > dialog.alert .response-area > button.suggested-action {{
  color: @toolport_bg_dark;
  background-color: @toolport_accent;
  border-color: @toolport_accent;
}}

window.messagedialog .response-area > button.destructive-action,
window.dialog-window.alert .response-area > button.destructive-action,
dialog-host > dialog.alert .response-area > button.destructive-action {{
  color: @toolport_bg_dark;
  background-color: @toolport_error;
  border-color: @toolport_error;
}}

.toolport-native {{
  background-color: @toolport_bg;
  color: @toolport_fg;
  /* Omarchy's 0.9091 text scale resolves Adwaita Sans 11 to 13.33px. At that
     fractional raster size Pango drops the top stroke of capitals such as E,
     F, P, and T. Keep the chosen family and weights, but use the nearest whole
     CSS pixel so glyph hinting remains intact. */
  font-family: "Inter", system-ui, sans-serif;
  font-size: 14px;
}}

.toolport-native .caption {{
  font-size: 13px;
}}

.toolport-shell {{ background-color: @toolport_bg; }}

.toolport-content {{
  background-color: @toolport_bg;
  color: @toolport_fg;
}}

/* The page itself is opaque; scrolled windows and viewports inside it, in its
   cards and in its popovers draw nothing, so they show their container. */
.toolport-content scrolledwindow,
.toolport-content viewport {{
  background-color: transparent;
}}

.toolport-sidebar {{
  background-color: @toolport_sidebar;
  color: @toolport_fg;
  border-right: 1px solid @toolport_border;
}}

/* Shared page header and sidebar structure. */
.toolport-page-header {{ padding: 16px 24px; border-bottom: 1px solid @toolport_border; background: @toolport_sidebar; }}
.toolport-page-subtitle {{ font-size: 13px; font-weight: 400; }}
.toolport-page-back-divider {{ margin: 4px 0; background: @toolport_border; }}
.toolport-page-actions button, .toolport-page-actions menubutton > button {{ min-height: 32px; padding-top: 0; padding-bottom: 0; }}
.toolport-page-actions button.toolport-secondary-action,
.toolport-page-actions menubutton.toolport-secondary-action > button {{ background: @toolport_sidebar; border: 1px solid @toolport_border; border-radius: 8px; }}
.toolport-page-actions button.toolport-secondary-action:hover,
.toolport-page-actions menubutton.toolport-secondary-action > button:hover {{ background: @toolport_card; }}
.toolport-page-actions button.toolport-destructive {{ color: @toolport_error; background: alpha(@toolport_error, 0.12); border-color: transparent; }}
.toolport-page-actions button.toolport-destructive:hover {{ background: alpha(@toolport_error, 0.20); }}
.toolport-page-actions entry {{ min-height: 32px; padding-top: 0; padding-bottom: 0; }}
.toolport-sidebar-brand {{ padding: 16px; }}
.toolport-sidebar-footer {{ padding: 12px 16px; border-top: 1px solid @toolport_border; }}
.toolport-sidebar-savings {{ margin: 8px 0; padding: 8px 12px; border: 1px solid alpha(@toolport_success, 0.3); background: alpha(@toolport_success, 0.05); border-radius: 8px; }}

.toolport-header {{
  background-color: @toolport_sidebar;
  border-bottom: 1px solid @toolport_border;
  box-shadow: none;
}}

.toolport-header button.toolport-header-add {{
  min-width: 32px;
  min-height: 32px;
  padding: 0;
  border-radius: 8px;
  color: @toolport_accent;
  background-color: alpha(@toolport_accent, 0.10);
}}

.toolport-header button.toolport-header-add:hover {{
  background-color: alpha(@toolport_accent, 0.18);
}}

popover.toolport-main-menu > contents {{
  min-width: 220px;
  padding: 8px;
  border: 1px solid @toolport_border;
  border-radius: 12px;
  color: @toolport_fg;
  background-color: @toolport_raised;
  box-shadow: 0 12px 30px alpha(black, 0.32);
}}

popover.toolport-main-menu button.toolport-main-menu-item {{
  min-height: 36px;
  padding: 0 10px;
  border: none;
  border-radius: 8px;
  color: @toolport_fg;
  background-color: transparent;
  box-shadow: none;
}}

popover.toolport-main-menu button.toolport-main-menu-item:hover {{
  color: @toolport_fg;
  background-color: alpha(@toolport_accent, 0.12);
}}

popover.toolport-main-menu button.toolport-main-menu-quit {{
  color: @toolport_error;
}}

popover.toolport-main-menu .toolport-menu-shortcut {{
  color: @toolport_text_secondary;
  font-size: 12px;
}}

popover.toolport-main-menu separator {{
  min-height: 1px;
  margin: 5px 8px;
  background-color: @toolport_border;
}}

popover.toolport-menu > contents {{
  min-width: 220px;
  padding: 8px;
  border: 1px solid @toolport_border;
  border-radius: 12px;
  color: @toolport_fg;
  background-color: @toolport_raised;
  box-shadow: 0 12px 30px alpha(black, 0.32);
}}

popover.toolport-menu button.toolport-menu-item {{
  min-height: 36px;
  padding: 0 10px;
  border: none;
  border-radius: 8px;
  color: @toolport_fg;
  background-color: transparent;
  box-shadow: none;
}}

popover.toolport-menu button.toolport-menu-item:hover {{
  color: @toolport_fg;
  background-color: alpha(@toolport_accent, 0.12);
}}

popover.toolport-menu button.toolport-menu-item.destructive-action {{
  color: @toolport_error;
}}

popover.toolport-menu .toolport-menu-heading {{
  margin: 5px 10px 2px;
  color: @toolport_text_secondary;
  font-size: 12px;
  font-weight: 600;
}}

popover.toolport-menu .toolport-menu-check {{
  color: @toolport_accent;
}}

popover.toolport-menu separator {{
  min-height: 1px;
  margin: 5px 8px;
  background-color: @toolport_border;
}}

popover.toolport-catalog-suggestions > contents {{
  padding: 8px;
  border: 1px solid @toolport_border;
  border-radius: 12px;
  color: @toolport_fg;
  background-color: @toolport_raised;
  box-shadow: 0 12px 30px alpha(black, 0.34);
}}

.toolport-suggestion-heading {{
  padding: 4px 7px 5px;
}}

.toolport-suggestion-row {{
  min-height: 42px;
  padding: 5px 7px;
  border-radius: 8px;
  background-color: transparent;
}}

.toolport-suggestion-row:hover {{
  background-color: alpha(@toolport_accent, 0.08);
}}

.toolport-suggestion-message {{
  padding: 12px;
}}

.toolport-suggestion-message.error {{
  color: @toolport_error;
}}

button.toolport-suggestion-more {{
  min-height: 30px;
  margin-top: 3px;
  border-radius: 8px;
  color: @toolport_accent;
  background-color: alpha(@toolport_accent, 0.08);
}}

popover.background > contents,
popover.menu > contents,
dropdown popover > contents {{
  border: 1px solid @toolport_border;
  border-radius: 12px;
  color: @toolport_fg;
  background-color: @toolport_raised;
  box-shadow: 0 12px 30px alpha(black, 0.32);
}}

popover.menu modelbutton:hover,
dropdown popover row:hover {{
  background-color: alpha(@toolport_accent, 0.12);
}}

dropdown popover listview {{
  color: @toolport_fg;
  background-color: transparent;
}}

dropdown popover row:selected {{
  color: @toolport_fg;
  background-color: alpha(@toolport_accent, 0.18);
}}

.toolport-brand-mark {{
  min-width: 28px;
  min-height: 28px;
}}

.toolport-client-logo,
.toolport-server-logo {{
  min-width: 32px;
  min-height: 32px;
  margin: 1px;
}}

.toolport-server-logo {{
  border-radius: 8px;
  background-color: alpha(white, 0.88);
}}

/* Single-color client marks are drawn in black. Dark themes turn them white;
   the hue rotation keeps an accent such as Amazon Q's orange close to its own. */
.toolport-mono-logo {{
  filter: {mono_logo_filter};
}}

.toolport-nav-item {{
  min-height: 34px;
  padding: 0 12px;
  border-radius: 8px;
  /* Omarchy's `muted` is a border colour and falls below 3:1 on most stock
     themes; navigation is primary text, as in Omarchy's own menus. */
  color: @toolport_fg;
  transition: 120ms ease;
}}

.toolport-nav-item:hover {{
  background-color: alpha(@toolport_fg, 0.06);
  color: @toolport_fg;
}}

.toolport-nav-item.selected {{
  background-color: alpha(@toolport_accent, 0.075);
  color: @toolport_fg;
  box-shadow: inset 3px 0 @toolport_accent;
}}

.toolport-nav-item.selected image {{
  color: @toolport_accent;
}}



.toolport-mode-badge {{
  margin: 7px 2px;
  padding: 4px 9px;
  border: 1px solid @toolport_border;
  border-radius: 999px;
  background-color: @toolport_info_tint;
  color: @toolport_text_secondary;
  font-size: 12px;
}}

.toolport-feedback {{
  padding: 10px 14px;
  border-radius: 0 12px 12px 0;
  background-color: alpha(@toolport_accent, 0.08);
  border: none;
  border-left: 3px solid @toolport_accent;
  color: @toolport_fg;
  font-size: 13px;
}}

/* Holds its place so rows and open menus do not jump, but shows nothing. */
.toolport-feedback.idle {{
  background-color: transparent;
  border-color: transparent;
}}

.toolport-feedback.success {{
  color: @toolport_fg;
  border-color: @toolport_success;
  background-color: alpha(@toolport_success, 0.08);
}}

.toolport-feedback.error {{
  color: @toolport_fg;
  border-color: @toolport_error;
  background-color: alpha(@toolport_error, 0.08);
}}

.toolport-editor {{
  background-color: @toolport_bg;
  color: @toolport_fg;
}}

.toolport-editor headerbar {{
  min-height: 48px;
  padding: 4px 8px;
  background-color: @toolport_sidebar;
  border-bottom: 1px solid @toolport_border;
  box-shadow: none;
}}

.toolport-editor headerbar button {{
  min-height: 32px;
  min-width: 32px;
  padding: 0 12px;
  border-radius: 8px;
  background-color: @toolport_card;
  border: 1px solid @toolport_border;
}}

.toolport-editor headerbar button:hover {{
  background-color: alpha(@toolport_fg, 0.10);
}}

.toolport-editor headerbar button.suggested-action {{
  color: @toolport_bg_dark;
  background-color: @toolport_accent;
  border-color: alpha(@toolport_accent, 0.72);
}}

.toolport-editor headerbar button.suggested-action:disabled {{
  color: alpha(@toolport_fg, 0.58);
  background-color: alpha(@toolport_accent, 0.16);
  border-color: alpha(@toolport_accent, 0.20);
  opacity: 1;
}}

.toolport-editor-body {{
  padding: 20px;
}}

.toolport-import-scroller {{
  border: 1px solid @toolport_border;
  border-radius: 12px;
  background-color: @toolport_card;
}}

.toolport-import-scroller viewport,
.toolport-import-list {{
  background-color: transparent;
}}

.toolport-import-list {{
  padding: 8px;
}}

.toolport-import-row {{
  min-height: 48px;
  padding: 9px 11px;
  border-radius: 8px;
  background-color: @toolport_card;
}}

.toolport-import-row:hover {{
  background-color: alpha(@toolport_accent, 0.08);
}}

.toolport-editor-lede {{
  margin: 2px 2px 4px;
  color: @toolport_text_secondary;
}}

.toolport-editor-intro {{
  margin-bottom: 4px;
}}

.toolport-editor-icon {{
  min-width: 42px;
  min-height: 42px;
  border-radius: 12px;
  background-color: alpha(@toolport_accent, 0.12);
  color: @toolport_accent;
}}

.toolport-form-section {{
  padding: 14px 4px;
  border-radius: 0;
  background-color: transparent;
  border: none;
  border-top: 1px solid @toolport_border;
}}

.toolport-paste-expander {{
  padding: 0 14px;
  border-radius: 12px;
  color: @toolport_fg;
  background-color: @toolport_card;
  border: 1px solid @toolport_border;
}}

.toolport-paste-expander > title {{
  min-height: 46px;
  font-weight: 600;
}}

.toolport-paste-expander > title > expander {{
  color: @toolport_accent;
}}

.toolport-field-label {{
  color: @toolport_text_secondary;
  font-size: 12px;
  font-weight: 600;
}}

.toolport-input,
.toolport-input > text,
.toolport-input > button,
.toolport-text-area,
.toolport-text-area textview,
.toolport-text-area text {{
  background-color: @toolport_card;
  color: @toolport_fg;
}}

.toolport-input,
.toolport-input > button,
.toolport-text-area {{
  min-height: 40px;
  border-radius: 8px;
  border: 1px solid alpha(@toolport_fg, 0.15);
  box-shadow: none;
}}

.toolport-input:focus-within,
.toolport-text-area:focus-within {{
  border-color: alpha(@toolport_accent, 0.72);
  box-shadow: 0 0 0 2px alpha(@toolport_accent, 0.12);
}}

.toolport-compact-select,
.toolport-compact-select > button {{
  min-height: 32px;
  padding-top: 0;
  padding-bottom: 0;
}}

.toolport-editor-note {{
  padding: 11px 13px;
  border-radius: 12px;
  background-color: @toolport_info_tint;
  border: 1px solid @toolport_border;
  color: @toolport_text_secondary;
}}

.toolport-editor .toolport-muted {{
  color: @toolport_text_secondary;
}}

.toolport-editor button.toolport-secondary-action {{
  background-color: @toolport_raised;
  border: 1px solid @toolport_border;
}}

.toolport-editor button.toolport-destructive-icon {{
  color: @toolport_error;
  background-color: alpha(@toolport_error, 0.07);
  border: 1px solid alpha(@toolport_error, 0.15);
}}

.toolport-credential-row {{
  padding: 13px 4px;
  border-radius: 0;
  border: none;
  border-bottom: 1px solid @toolport_border;
  background-color: transparent;
}}

.toolport-approvals {{
  padding: 12px;
}}

.toolport-global-alert {{
  color: @toolport_fg;
  background-color: @toolport_bg;
  border: none;
  border-left: 3px solid @toolport_accent;
  border-radius: 0 12px 12px 0;
  box-shadow: 0 12px 34px alpha(@toolport_bg_dark, 0.58);
}}

.toolport-global-alert.security {{
  padding: 12px 14px;
  border-color: alpha(@toolport_accent, 0.34);
  background-color: mix(@toolport_bg, @toolport_accent, 0.035);
}}

.toolport-global-alert.security > image {{
  color: @toolport_accent;
}}

.toolport-global-alert.security-event {{
  padding: 11px 14px;
  border-color: alpha(@toolport_error, 0.30);
  background-color: mix(@toolport_bg, @toolport_error, 0.035);
}}

.toolport-global-alert.security-event > image {{
  color: @toolport_error;
}}

.toolport-approval-card {{
  padding: 14px 16px;
  border: none;
  border-left: 3px solid @toolport_warning;
  border-radius: 0 12px 12px 0;
  background-color: alpha(@toolport_warning, 0.08);
}}

.toolport-approval-deadline {{
  padding: 3px 8px;
  border-radius: 999px;
  color: @toolport_accent;
  background-color: alpha(@toolport_accent, 0.10);
}}

.toolport-sensitive-review {{
  padding: 10px 12px;
  border-radius: 0 12px 12px 0;
  color: @toolport_fg;
  background-color: alpha(@toolport_error, 0.08);
  border: none;
  border-left: 3px solid @toolport_error;
}}

.toolport-arguments {{
  margin-top: 8px;
  padding: 10px 12px;
  border-radius: 8px;
  background-color: @toolport_bg_dark;
  font-family: monospace;
}}

.toolport-page {{
  background-color: @toolport_bg;
}}

.toolport-dialog-content {{
  padding: 18px;
}}

.toolport-summary {{
  margin-top: 8px;
  margin-bottom: 8px;
}}

.toolport-summary-item {{
  background-color: transparent;
  border: none;
  border-bottom: 1px solid @toolport_border;
  border-radius: 0;
  padding: 10px 13px;
}}

.toolport-security-status {{
  min-height: 48px;
  padding: 8px 12px;
  color: @toolport_fg;
  background-color: @toolport_card;
  border: none;
  border-left: 3px solid @toolport_success;
  border-radius: 0 12px 12px 0;
  box-shadow: none;
}}

.toolport-security-status:hover {{
  background-color: mix(@toolport_card, @toolport_fg, 0.04);
  border-color: alpha(@toolport_accent, 0.28);
}}

.toolport-security-status.attention {{
  background-color: alpha(@toolport_error, 0.055);
  border-color: alpha(@toolport_error, 0.24);
}}

.toolport-security-status.review {{
  border-color: @toolport_warning;
}}

.toolport-security-status-icon.review {{
  color: @toolport_warning;
}}

.toolport-security-status-icon {{
  color: @toolport_success;
}}

.toolport-security-status-icon.attention {{
  color: @toolport_error;
}}

.toolport-activity-filter,
.toolport-activity-filter > button {{
  min-height: 32px;
  padding-top: 0;
  padding-bottom: 0;
}}

button.toolport-activity-filter {{
  padding-left: 11px;
  padding-right: 11px;
}}

.toolport-search {{
  min-height: 36px;
  border-radius: 8px;
  color: @toolport_fg;
  background-color: @toolport_card;
  border: 1px solid @toolport_border;
  box-shadow: none;
}}

.toolport-search:focus-within {{
  border-color: alpha(@toolport_accent, 0.62);
  background-color: @toolport_card;
  box-shadow: 0 0 0 2px alpha(@toolport_accent, 0.10);
}}

.toolport-card {{
  background-color: transparent;
  border: none;
  border-bottom: 1px solid @toolport_border;
  border-radius: 0;
  padding: 12px 4px;
  transition: 120ms ease;
}}

.toolport-card:hover {{
  background-color: alpha(@toolport_accent, 0.04);
}}

/* The name is a flat button for opening details; drop button padding so it
   lines up with the origin and status lines under it. */
.toolport-card button.toolport-card-title {{
  padding: 0;
  margin: 0;
  min-height: 0;
  background: none;
  box-shadow: none;
}}

.toolport-card button.toolport-card-title:hover label {{
  text-decoration: underline;
}}

.toolport-server-chip {{
  border: 1px solid @toolport_border;
  border-radius: 6px;
  background-color: alpha(@toolport_fg, 0.04);
  color: @toolport_fg;
  padding: 4px 8px;
}}
.toolport-gateway-flow {{
  border: 1px solid @toolport_border;
  border-radius: 12px;
  background-color: @toolport_card;
  padding: 20px 16px;
}}
.toolport-gateway-flow flowboxchild {{ padding: 0; }}

.toolport-details-expander {{
  padding: 11px 14px;
  border: 1px solid @toolport_border;
  border-radius: 12px;
  color: @toolport_fg;
  background-color: @toolport_card;
}}

.toolport-details-expander:hover {{
  border-color: alpha(@toolport_accent, 0.24);
  background-color: mix(@toolport_card, @toolport_fg, 0.04);
}}

.toolport-details-expander > title {{
  min-height: 30px;
  font-weight: 600;
}}

.toolport-stat-row {{
  min-height: 28px;
  padding: 2px 4px;
}}

.toolport-stat-metrics {{
  font-variant-numeric: tabular-nums;
}}

.toolport-settings-parent {{ font-weight: 600; }}
.toolport-safety-card {{ border: 1px solid @toolport_border; border-radius: 12px; padding: 16px; background-color: @toolport_card; }}
.toolport-safety-card:checked {{ border-color: @toolport_accent; background: mix(@toolport_card, @toolport_accent, 0.08); }}
.toolport-settings-group.toolport-switch-group {{
  border: 1px solid @toolport_border;
  border-radius: 12px;
  background-color: @toolport_card;
}}
.toolport-settings-group {{
  background-color: transparent;
  border: none;
  border-top: 1px solid @toolport_border;
  border-radius: 0;
}}

/* Groups whose children are plain form fields rather than `toolport-setting-row`,
   which is where the other groups get their inner padding from. */
.toolport-settings-group.toolport-padded-group {{
  padding: 16px 18px;
}}

/* Text-colour equivalents of the badge variants, for rows that state their
   status in the subtitle instead of repeating it in a chip. */
.toolport-state-success {{
  color: @toolport_success;
}}
.toolport-state-review {{
  color: @toolport_warning;
}}
.toolport-state-muted {{
  color: @toolport_off;
}}

/* Wider than `toolport-summary-item`, which is sized for a number over a
   one-word label rather than a heading over a wrapped sentence. */
.toolport-value-card {{
  background-color: transparent;
  border: none;
  border-bottom: 1px solid @toolport_border;
  border-radius: 0;
  padding: 15px 17px;
}}

.toolport-setting-row {{
  padding: 11px 14px;
  border-bottom: 1px solid alpha(@toolport_fg, 0.075);
}}

.toolport-setting-row:last-child {{
  border-bottom: none;
}}

.toolport-project-files {{
  background-color: @toolport_card;
  border: 1px solid @toolport_border;
  border-radius: 12px;
}}

.toolport-project-file-row {{
  padding: 11px 13px;
  border-bottom: 1px solid alpha(@toolport_fg, 0.07);
}}

.toolport-project-file-row:last-child {{
  border-bottom: none;
}}

.toolport-card button.toolport-secondary-action {{
  background-color: @toolport_raised;
  border: 1px solid @toolport_border;
}}

.toolport-card-icon {{
  min-width: 32px;
  min-height: 32px;
  margin: 1px;
  border-radius: 8px;
  background-color: alpha(@toolport_accent, 0.12);
  color: @toolport_accent;
}}

.toolport-muted {{ color: @toolport_text_secondary; }}

/* One status chip: a pill with the status color at 12% and full-color text,
   no border. The React Badge variants use the same colors. `review` and
   `approval-warning` are warning; `disabled` is off. */
.toolport-badge {{
  background-color: @toolport_info_tint;
  color: @toolport_text_secondary;
  border: none;
  border-radius: 999px;
  padding: 2px 8px;
}}

.toolport-badge.success {{
  background-color: alpha(@toolport_success, 0.12);
  color: @toolport_success;
}}

.toolport-badge.off,
.toolport-badge.info,
.toolport-badge.disabled {{
  background-color: @toolport_info_tint;
  color: @toolport_off;
}}

.toolport-badge.warning,
.toolport-badge.review,
.toolport-badge.approval-warning {{
  color: @toolport_warning;
  background-color: alpha(@toolport_warning, 0.12);
}}

.toolport-badge.partial {{
  color: @toolport_partial;
  background-color: alpha(@toolport_partial, 0.12);
}}

.toolport-badge.error,
.toolport-badge.approval-denied {{
  color: @toolport_error;
  background-color: alpha(@toolport_error, 0.12);
}}
image.approval-denied {{ color: @toolport_error; }}
image.approval-warning {{ color: @toolport_warning; }}
image.success {{
  color: @toolport_success;
  -gtk-icon-palette: error @toolport_success, warning @toolport_success, success @toolport_success;
}}
image.disabled {{ color: @toolport_off; }}

.toolport-catalog-added {{
  min-height: 30px;
  padding: 0 8px;
  border: none;
  border-radius: 8px;
  background-color: alpha(@toolport_success, 0.12);
  color: @toolport_success;
  font-size: 12px;
  font-weight: 600;
}}

button.toolport-catalog-action {{
  min-height: 32px;
  padding: 0 8px;
  border-radius: 8px;
}}

.toolport-card switch {{
  margin-left: 2px;
}}

/* Track: accent when on, the foreground at 18% when off. The knob is white in
   both states and in both modes, as in React. */
.toolport-native switch {{
  color: @toolport_fg;
  background-color: alpha(@toolport_fg, 0.18);
  border: none;
  box-shadow: none;
}}

.toolport-native switch:checked {{
  color: @toolport_bg_dark;
  background-color: @toolport_accent;
}}

.toolport-native switch slider,
.toolport-native switch:checked slider {{
  background-color: white;
  box-shadow: 0 1px 2px alpha(black, 0.30);
}}

.toolport-action-item {{
  min-width: 150px;
  min-height: 36px;
  padding: 0 10px;
  border-radius: 8px;
  background-color: transparent;
  border: none;
}}

.toolport-action-item:hover {{
  background-color: alpha(@toolport_fg, 0.07);
}}

.toolport-action-item.destructive-action {{
  color: @toolport_error;
}}

.toolport-state-card {{
  padding: 40px 24px;
  border: 1px solid @toolport_border;
  border-radius: 12px;
  background-color: @toolport_card;
}}
/* A state that already sits inside a row group, like React's framed={{false}}. */
.toolport-state-card.frameless {{
  border: none;
  border-radius: 0;
  background-color: transparent;
}}

.toolport-state-icon {{ color: @toolport_text_secondary; }}
.toolport-state-card.error {{
  border-left: 3px solid @toolport_error;
}}
.toolport-state-card.error .toolport-state-icon {{ color: @toolport_error; }}

.toolport-manual-code {{ background: @toolport_card; border: 1px solid @toolport_border; border-radius: 12px; padding: 16px; }}
.toolport-error-callout {{ background: @toolport_card; border: 1px solid @toolport_border; border-left: 3px solid @toolport_error; border-radius: 12px; padding: 16px; }}
.toolport-error-tile {{ background: alpha(@toolport_error, 0.12); color: @toolport_error; border-radius: 8px; }}
.toolport-inline-error {{ font-size: 13px; color: @toolport_error; }}
.toolport-skeleton-group {{ background: @toolport_card; border: 1px solid @toolport_border; border-radius: 12px; }}
.toolport-skeleton {{ min-height: 24px; margin: 16px; border-radius: 8px; background: alpha(@toolport_fg, 0.06); }}
toast {{ background: @toolport_raised; color: @toolport_fg; border: 1px solid @toolport_border; border-radius: 12px; }}
statuspage.toolport-success image.icon {{ color: @toolport_success; opacity: 1; }}

.toolport-native .title-2 {{ font-size: 22px; font-weight: 600; }}
.toolport-native .title-3 {{ font-size: 17px; font-weight: 600; }}
.toolport-native .heading {{ font-weight: 600; }}
.toolport-section-label {{
  font-size: 12px;
  font-weight: 600;
  letter-spacing: 0.6px;
  text-transform: uppercase;
  color: @toolport_text_secondary;
}}

/* Clients page: one line per client, controls on the right. */
.toolport-card.toolport-client-row {{ padding: 9px 4px; }}
.toolport-client-row .toolport-client-logo {{ min-width: 28px; min-height: 28px; }}
.toolport-card.toolport-client-profile {{ padding-left: 44px; }}
button.toolport-client-group-toggle {{
  padding: 0;
  margin: 0;
  min-height: 0;
  background: none;
  box-shadow: none;
}}
.toolport-client-row .toolport-badge.caption {{ padding: 1px 8px; }}

/* Skills page: the React view's list-and-detail layout with one status column per agent. */
.toolport-skill-tabbar {{
  border-bottom: 1px solid @toolport_border;
}}
button.toolport-skill-tab {{
  border-radius: 0;
  padding: 4px 2px 9px 2px;
  min-height: 0;
  background: none;
  box-shadow: none;
  border-bottom: 2px solid transparent;
  color: @toolport_text_secondary;
}}
button.toolport-skill-tab:hover {{ color: @toolport_fg; background: none; }}
button.toolport-skill-tab:checked {{
  color: @toolport_fg;
  font-weight: 600;
  background: none;
  border-bottom-color: @toolport_accent;
}}
.toolport-skill-count {{
  background-color: @toolport_info_tint;
  color: @toolport_text_secondary;
  border-radius: 999px;
  padding: 0 6px;
  font-weight: 500;
}}
.toolport-skill-count.warning {{
  background-color: alpha(@toolport_warning, 0.12);
  color: @toolport_warning;
}}
.toolport-skill-banner {{
  border: 1px solid @toolport_border;
  border-left: 3px solid @toolport_warning;
  border-radius: 12px;
  background-color: @toolport_card;
  padding: 12px 14px;
  margin-bottom: 14px;
}}
.toolport-skill-tile {{
  border-radius: 8px;
  background-color: alpha(@toolport_accent, 0.12);
  color: @toolport_accent;
}}
.toolport-skill-tile.warning {{ background-color: alpha(@toolport_warning, 0.12); color: @toolport_warning; }}
.toolport-skill-tile.error {{ background-color: alpha(@toolport_error, 0.12); color: @toolport_error; }}
/* Info is neutral in both shells. */
.toolport-skill-tile.info {{ background-color: @toolport_info_tint; color: @toolport_info; }}
.toolport-skill-head {{
  border-bottom: 1px solid @toolport_border;
  padding: 0 8px 6px 8px;
}}
.toolport-skill-hint {{ padding: 0 8px 6px 8px; min-height: 16px; }}
.toolport-skill-head button.toolport-skill-col {{
  padding: 2px 0;
  margin: 0;
  min-width: 0;
  min-height: 0;
  border-radius: 8px;
}}
.toolport-skill-head button.toolport-skill-col.selected {{
  background-color: alpha(@toolport_accent, 0.16);
  box-shadow: inset 0 0 0 1px alpha(@toolport_accent, 0.60);
}}
.toolport-skill-head .toolport-client-logo {{ min-width: 20px; min-height: 20px; margin: 0; }}
.toolport-skill-logo .toolport-client-logo {{ min-width: 20px; min-height: 20px; margin: 0; }}
.toolport-skill-chip .toolport-client-logo {{ min-width: 14px; min-height: 14px; margin: 0; }}
.toolport-skill-pip {{
  margin: 1px 3px;
  min-width: 6px;
  min-height: 6px;
  border-radius: 999px;
  background-color: @toolport_accent;
}}
list.toolport-skill-list {{ background: transparent; }}
list.toolport-skill-list > row {{ border-radius: 8px; padding: 0; }}
list.toolport-skill-list > row:hover {{ background-color: alpha(@toolport_fg, 0.04); }}
list.toolport-skill-list > row:selected {{ background-color: alpha(@toolport_accent, 0.14); }}
list.toolport-skill-list > row.toolport-skill-group-row:hover {{ background: none; }}
.toolport-skill-row {{ padding: 0 8px; }}
.toolport-skill-row-copy {{ padding: 12px 0; }}
.toolport-skill-row-stacked {{ padding: 10px 8px 8px 8px; }}
.toolport-skill-col-name {{ font-size: 12px; }}
.toolport-skill-col-hl {{ background-color: alpha(@toolport_accent, 0.08); }}
.toolport-chip-dot {{ min-width: 8px; min-height: 8px; border-radius: 999px; background-color: currentColor; }}
.toolport-chip-dot.name-only {{ background-image: linear-gradient(90deg, currentColor 50%, transparent 50%); background-color: transparent; border: 1px solid currentColor; }}
.toolport-chip-dot.none {{ min-width: 4px; min-height: 4px; }}
.toolport-chip-dot.ring {{ background-color: transparent; border: 1px solid currentColor; }}
.toolport-lock:focus-visible {{ outline: 2px solid alpha(@toolport_accent, 0.50); outline-offset: 2px; }}
.toolport-skill-issue button.toolport-secondary-action {{ background-color: @toolport_raised; border: 1px solid @toolport_border; }}
.toolport-skill-issue button.toolport-secondary-action:hover {{ background-color: @toolport_card; }}
.toolport-skill-issue.toolport-issue-warning {{ border-left: 3px solid @toolport_warning; }}
.toolport-skill-issue.toolport-issue-error {{ border-left: 3px solid @toolport_error; }}
.toolport-skill-name {{ font-weight: 600; }}
.toolport-skill-group {{ padding: 14px 8px 5px 8px; }}
.toolport-skill-dot {{
  min-width: 12px;
  min-height: 12px;
  border-radius: 999px;
}}
.toolport-skill-dot.on {{ background-color: @toolport_success; }}
.toolport-skill-dot.name-only {{
  min-width: 8px;
  min-height: 8px;
  border: 2px solid @toolport_partial;
  background-image: linear-gradient(90deg, @toolport_partial 50%, transparent 50%);
}}
.toolport-skill-dot.shadowed {{
  min-width: 8px;
  min-height: 8px;
  border: 2px solid @toolport_warning;
}}
.toolport-skill-dot.broken {{
  min-width: 10px;
  min-height: 10px;
  border: 2px solid @toolport_error;
}}
.toolport-skill-dot-core {{
  min-width: 4px;
  min-height: 4px;
  border-radius: 999px;
  background-color: @toolport_error;
}}
.toolport-skill-dot.none {{
  min-width: 4px;
  min-height: 4px;
  background-color: alpha(@toolport_fg, 0.28);
}}
.toolport-skill-off {{ color: @toolport_off; }}
.toolport-skill-legend {{
  border-top: 1px solid @toolport_border;
  padding-top: 8px;
  margin-top: 6px;
}}
.toolport-skill-agents {{
  border: 1px solid @toolport_border;
  border-radius: 12px;
  background-color: @toolport_card;
}}
.toolport-skill-agent {{
  padding: 10px 12px;
  border-bottom: 1px solid @toolport_border;
}}
.toolport-skill-agent:last-child {{ border-bottom: none; }}
/* No taller than the switch beside it, so every agent card is one height. */
menubutton.toolport-row-more > button {{ min-height: 26px; min-width: 26px; padding: 0; }}
.toolport-skill-logo {{
  min-width: 28px;
  min-height: 28px;
  border-radius: 8px;
  background-color: alpha(@toolport_fg, 0.06);
}}
levelbar.toolport-skill-meter trough {{
  min-height: 5px;
  border-radius: 999px;
  background-color: alpha(@toolport_fg, 0.10);
  border: none;
  box-shadow: none;
  padding: 0;
}}
levelbar.toolport-skill-meter block {{ min-height: 5px; border-radius: 999px; border: none; }}
levelbar.toolport-skill-meter block.filled {{ background-color: @toolport_success; }}
levelbar.toolport-skill-meter block.empty {{ background: none; }}
levelbar.toolport-skill-meter.over block.filled {{ background-color: @toolport_warning; }}
.toolport-skill-issue {{
  border: 1px solid @toolport_border;
  border-radius: 12px;
  background-color: @toolport_card;
  padding: 16px;
}}
.toolport-skill-chip {{
  background-color: alpha(@toolport_fg, 0.07);
  border-radius: 8px;
  padding: 3px 7px;
}}
.toolport-skill-chip.logo {{ padding: 0; }}
button.toolport-skill-chip {{ min-height: 0; min-width: 0; padding: 3px 8px; }}
button.toolport-skill-chip:hover {{ background-color: alpha(@toolport_fg, 0.13); }}
.toolport-fix-paths {{
  background-color: @toolport_card;
  border-radius: 8px;
  padding: 8px 12px;
  margin-top: 4px;
}}
.toolport-skill-segments button {{ padding: 2px 9px; min-height: 24px; }}

/* Sync page: one amber block for everything that needs a decision. */
.toolport-attention {{
  border-left: 3px solid @toolport_warning;
  border-radius: 0 12px 12px 0;
  background-color: alpha(@toolport_warning, 0.08);
}}

.toolport-attention-item {{
  padding: 10px 14px;
  border-bottom: 1px solid alpha(@toolport_warning, 0.14);
}}

.toolport-attention-item:last-child {{
  border-bottom: none;
}}

.toolport-sync-row {{
  padding: 9px 2px;
  border-bottom: 1px solid @toolport_border;
}}

.toolport-sync-row:last-child {{
  border-bottom: none;
}}

.toolport-sync-tag {{
  color: @toolport_warning;
}}

.toolport-sync-status {{
  color: @toolport_text_secondary;
}}

.toolport-sync-status.healthy {{
  color: @toolport_success;
}}
.toolport-muted {{ font-size: 13px; }}
.toolport-native button {{ border-radius: 8px; font-weight: 600; }}
.toolport-feedback.warning {{
  border-color: @toolport_warning;
  background-color: alpha(@toolport_warning, 0.08);
  color: @toolport_fg;
}}
.toolport-feedback .heading {{ color: @toolport_fg; }}
.toolport-feedback + .toolport-feedback {{
  border-top: 1px solid @toolport_border;
}}

.toolport-native .toolport-badge {{
  font-size: 12px;
  font-weight: 500;
}}

selection {{
  background: @toolport_selection;
  color: @toolport_bg_dark;
}}
button.suggested-action {{
  background-color: @toolport_accent;
  color: @toolport_bg_dark;
}}

/* Button levels (spec 5). Secondary: raised fill and a border, for every
   repeated row action. Destructive trigger: red text on a 12% red tint. Only
   the final confirm in a dialog is filled red. */
button.toolport-secondary-action,
menubutton.toolport-secondary-action > button {{
  min-height: 32px;
  padding: 0 12px;
  border-radius: 8px;
  background-color: @toolport_raised;
  border: 1px solid @toolport_border;
  box-shadow: none;
}}
button.toolport-secondary-action:hover,
menubutton.toolport-secondary-action > button:hover {{ background-color: mix(@toolport_raised, @toolport_fg, 0.06); }}
button.toolport-destructive {{
  min-height: 32px;
  padding: 0 12px;
  border-radius: 8px;
  color: @toolport_error;
  background-color: alpha(@toolport_error, 0.12);
  border: 1px solid transparent;
  box-shadow: none;
}}
button.toolport-destructive:hover {{ background-color: alpha(@toolport_error, 0.20); }}
/* Row group (spec 9): one card, rows divided by the border color, no gaps.
   Rows are two lines: 12px vertical padding at the edges, 16px horizontal. */
.toolport-row-group {{
  background-color: @toolport_card;
  border: 1px solid @toolport_border;
  border-radius: 12px;
}}
.toolport-row-group .toolport-card {{
  min-height: 32px;
  padding: 12px 16px;
  border: none;
  border-top: 1px solid @toolport_border;
  border-radius: 0;
  background-color: transparent;
}}
.toolport-row-group > .toolport-card:first-child,
.toolport-row-group > :first-child > .toolport-card:first-child {{ border-top: none; }}
.toolport-row-group .toolport-card:hover {{ background-color: alpha(@toolport_fg, 0.04); }}
.toolport-row-group .toolport-card.toolport-client-profile {{ padding-left: 60px; }}
expander-widget.toolport-row-group {{ padding: 12px 16px; }}

/* Segmented controls (spec 13): the selected segment has the raised fill, a
   border and the text color; the others are secondary text. */
stackswitcher,
.toolport-segmented {{
  padding: 2px;
  border: 1px solid @toolport_border;
  border-radius: 8px;
  background-color: alpha(@toolport_fg, 0.04);
}}
stackswitcher > button,
.toolport-segmented > button {{
  min-height: 28px;
  padding: 0 12px;
  border: 1px solid transparent;
  border-radius: 6px;
  background: none;
  box-shadow: none;
  color: @toolport_text_secondary;
  font-weight: 500;
}}
stackswitcher > button:hover,
.toolport-segmented > button:hover {{ color: @toolport_fg; background-color: alpha(@toolport_fg, 0.04); }}
stackswitcher > button:checked,
.toolport-segmented > button:checked {{
  color: @toolport_fg;
  background-color: @toolport_raised;
  border-color: @toolport_border;
  font-weight: 600;
  box-shadow: 0 1px 2px alpha(black, 0.18);
}}

/* Inputs (spec 13): 32px, regular text. A dropdown draws its border on its
   button only, so it is not twice as tall as an entry. */
.toolport-input,
.toolport-input > button,
.toolport-text-area {{ min-height: 32px; }}
dropdown.toolport-input {{
  min-height: 0;
  padding: 0;
  border: none;
  background: none;
}}
dropdown.toolport-input > button {{
  min-height: 32px;
  padding: 0 10px;
  font-weight: 400;
}}
dropdown.toolport-input > button label {{ font-weight: 400; }}

/* Activity rows: a neutral icon tile for the call kind. */
image.toolport-kind-icon {{
  min-width: 32px;
  min-height: 32px;
  border-radius: 8px;
  background-color: alpha(@toolport_fg, 0.06);
  color: @toolport_text_secondary;
}}
image.toolport-kind-emblem {{ color: @toolport_text_secondary; }}

/* Sheet header: only the title and the window's close button, drawn as usual. */
.toolport-editor headerbar windowcontrols button {{
  min-width: 0;
  min-height: 0;
  padding: 0;
  border: none;
  background: none;
}}
expander-widget.toolport-paste-expander > box > title {{ min-height: 32px; }}

/* Sheet footer (spec 5): Cancel, then the confirm, right-aligned. */
.toolport-dialog-footer {{
  padding: 12px 20px;
  border-top: 1px solid @toolport_border;
}}

window:backdrop .toolport-card,
window:backdrop .toolport-summary-item {{
  border-color: alpha(@toolport_fg, 0.06);
}}
"#,
            card = tokens.card,
            text_secondary = tokens.text_secondary,
            text_disabled = tokens.text_disabled,
            success = tokens.success,
            warning = tokens.warning,
            danger = tokens.danger,
            partial = tokens.partial,
            background = self.background,
            dark_background = self.dark_background,
            lighter_background = self.lighter_background,
            foreground = self.foreground,
            muted = self.muted,
            accent = self.accent,
            selection = self.selection,
            mono_logo_filter = match self.mode {
                ThemeMode::Dark => "invert(1) hue-rotate(180deg)",
                ThemeMode::Light => "none",
            },
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Tokens {
    card: String,
    text_secondary: String,
    text_disabled: String,
    success: String,
    warning: String,
    danger: String,
    partial: String,
}

type Rgb = [f64; 3];

/// Parses a color that `safe_color` has already validated as `#rrggbb`.
fn rgb(hex: &str) -> Rgb {
    let channel = |at: usize| f64::from(u8::from_str_radix(&hex[at..at + 2], 16).unwrap_or(0));
    [channel(1), channel(3), channel(5)]
}

fn hex(color: Rgb) -> String {
    let channel = |value: f64| value.round().clamp(0.0, 255.0) as u8;
    format!(
        "#{:02x}{:02x}{:02x}",
        channel(color[0]),
        channel(color[1]),
        channel(color[2])
    )
}

/// The same blend as GTK's `mix(a, b, amount)`.
fn mix(a: Rgb, b: Rgb, amount: f64) -> Rgb {
    [0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * amount)
}

fn luminance(color: Rgb) -> f64 {
    let linear = |value: f64| {
        let value = value / 255.0;
        if value <= 0.039_28 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(color[0]) + 0.7152 * linear(color[1]) + 0.0722 * linear(color[2])
}

fn contrast(a: Rgb, b: Rgb) -> f64 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// The first blend of `color` toward `toward` that reaches `minimum` contrast
/// on every backdrop the text is drawn on. `backdrops` gets the candidate, so a
/// status color can be checked against its own tint.
fn readable(color: Rgb, toward: Rgb, minimum: f64, backdrops: impl Fn(Rgb) -> Vec<Rgb>) -> Rgb {
    (0..=20)
        // Rounded first, so the check holds for the hex value the CSS gets.
        .map(|step| mix(color, toward, f64::from(step) / 20.0).map(f64::round))
        .find(|&candidate| {
            backdrops(candidate)
                .iter()
                .all(|&backdrop| contrast(candidate, backdrop.map(f64::round)) >= minimum)
        })
        .unwrap_or(toward)
}

fn safe_color(candidate: Option<String>, fallback: &str) -> String {
    candidate
        .filter(|value| is_hex_color(value))
        .unwrap_or_else(|| fallback.to_string())
}

fn is_hex_color(value: &str) -> bool {
    value.len() == 7
        && value.starts_with('#')
        && value.as_bytes()[1..]
            .iter()
            .all(|byte| byte.is_ascii_hexdigit())
}

fn palette_path() -> PathBuf {
    let state_home = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(dirs::state_dir)
        .unwrap_or_else(|| PathBuf::from(".local/state"));
    state_home.join("omarchy/current/theme/colors.toml")
}

pub(super) struct ThemeController {
    path: PathBuf,
    provider: gtk::CssProvider,
    monitors: RefCell<Vec<gio::FileMonitor>>,
}

impl ThemeController {
    pub(super) fn new() -> Rc<Self> {
        Rc::new(Self {
            path: palette_path(),
            provider: gtk::CssProvider::new(),
            monitors: RefCell::new(Vec::new()),
        })
    }

    pub(super) fn attach(self: &Rc<Self>, window: &adw::ApplicationWindow) {
        let Some(display) = gtk::gdk::Display::default() else {
            return;
        };
        gtk::style_context_add_provider_for_display(
            &display,
            &self.provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        self.reload();
        self.start_monitors();

        let controller = Rc::clone(self);
        window.connect_destroy(move |_| {
            controller.monitors.borrow_mut().clear();
        });
    }

    fn reload(&self) {
        let palette = OmarchyPalette::load(&self.path);
        self.provider.load_from_data(&palette.css());
        let scheme = match palette.mode {
            ThemeMode::Light => adw::ColorScheme::ForceLight,
            ThemeMode::Dark => adw::ColorScheme::ForceDark,
        };
        adw::StyleManager::default().set_color_scheme(scheme);
    }

    fn start_monitors(self: &Rc<Self>) {
        if !self.monitors.borrow().is_empty() {
            return;
        }

        let mut monitors = Vec::new();
        if let Ok(monitor) = gio::File::for_path(&self.path)
            .monitor_file(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
        {
            connect_reload(&monitor, Rc::downgrade(self));
            monitors.push(monitor);
        }

        if let Some(current_dir) = self.path.parent().and_then(Path::parent) {
            if let Ok(monitor) = gio::File::for_path(current_dir)
                .monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
            {
                connect_reload(&monitor, Rc::downgrade(self));
                monitors.push(monitor);
            }
        }
        *self.monitors.borrow_mut() = monitors;
    }
}

fn connect_reload(monitor: &gio::FileMonitor, controller: Weak<ThemeController>) {
    monitor.connect_changed(move |_, _, _, _| {
        if let Some(controller) = controller.upgrade() {
            controller.reload();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_omarchy_foundational_palette() {
        let palette = OmarchyPalette::parse(
            r##"
mode = "light"
background = "#f0f1f2"
dark_background = "#d0d1d2"
lighter_background = "#ffffff"
foreground = "#101112"
muted = "#606162"
accent = "#3366ff"
selection = "#ccddee"
red = "#ff3344"
green = "#22aa66"
"##,
        )
        .unwrap();

        assert_eq!(palette.mode, ThemeMode::Light);
        assert_eq!(palette.background, "#f0f1f2");
        assert_eq!(palette.accent, "#3366ff");
        assert_eq!(palette.green, "#22aa66");
    }

    #[test]
    fn invalid_colors_cannot_enter_css() {
        let palette = OmarchyPalette::parse(
            r##"
background = "#123456; } window { color: red;"
accent = "not-a-color"
foreground = "#abcdef"
"##,
        )
        .unwrap();

        assert_eq!(palette.background, FALLBACK_BACKGROUND);
        assert_eq!(palette.accent, FALLBACK_ACCENT);
        assert_eq!(palette.foreground, "#abcdef");
        assert!(!palette.css().contains("window { color"));
    }

    #[test]
    fn incomplete_palette_uses_role_specific_fallbacks() {
        let palette = OmarchyPalette::parse("mode = \"dark\"\naccent = \"#123abc\"\n").unwrap();

        assert_eq!(palette.mode, ThemeMode::Dark);
        assert_eq!(palette.accent, "#123abc");
        assert_eq!(palette.background, FALLBACK_BACKGROUND);
        assert_eq!(palette.red, FALLBACK_RED);
    }

    #[test]
    fn malformed_toml_fails_closed_to_the_caller() {
        assert!(OmarchyPalette::parse("background = [").is_err());
    }

    #[test]
    fn all_generated_color_tokens_are_validated_hex_values() {
        let palette = OmarchyPalette::default();
        for value in [
            &palette.background,
            &palette.dark_background,
            &palette.lighter_background,
            &palette.foreground,
            &palette.muted,
            &palette.accent,
            &palette.selection,
            &palette.red,
            &palette.green,
        ] {
            assert!(is_hex_color(value));
        }
    }

    #[test]
    fn generated_css_draws_opaque_surfaces() {
        let css = OmarchyPalette::default().css();

        assert!(css.contains("font-size: 14px"));
        assert!(css.contains(".toolport-native .caption"));
        assert!(!css.contains("font-size: 0.88em"));
        assert!(css.contains(".toolport-shell { background-color: @toolport_bg; }"));
        assert!(css.contains("--card-bg-color: @toolport_card"));
        assert!(css.contains("--dialog-bg-color: @toolport_raised"));
        assert!(css.contains("--popover-bg-color: @toolport_raised"));
        assert!(css.contains("window.messagedialog"));
        assert!(css.contains("dialog-host > dialog.alert sheet"));
        // Only tints, hover, selection and borders may use alpha; a surface
        // color with alpha would let the wallpaper or the rows behind a menu
        // show through.
        for surface in [
            "@toolport_bg",
            "@toolport_bg_dark",
            "@toolport_surface",
            "@toolport_card",
        ] {
            assert!(
                !css.contains(&format!("background-color: alpha({surface},")),
                "{surface}"
            );
        }
        for selector in [
            ".toolport-native {",
            ".toolport-sidebar {",
            ".toolport-header {",
            "popover.toolport-menu > contents {",
            "popover.toolport-main-menu > contents {",
            "popover.toolport-catalog-suggestions > contents {",
            "dropdown popover > contents {",
            "tooltip.background {",
            ".toolport-search {",
            ".toolport-skill-issue {",
            ".toolport-settings-group.toolport-switch-group {",
        ] {
            let start = css.find(selector).expect("the selector is generated");
            let rule = &css[start..start + css[start..].find('}').unwrap()];
            let background = rule
                .lines()
                .find(|line| line.trim_start().starts_with("background-color:"))
                .unwrap_or_else(|| panic!("{selector} sets a background"));
            assert!(!background.contains("alpha("), "{selector}: {background}");
            assert!(
                !background.contains("transparent"),
                "{selector}: {background}"
            );
        }
    }

    #[test]
    fn switch_knob_is_white_in_both_states() {
        let css = OmarchyPalette::default().css();
        let start = css.find(".toolport-native switch slider,").unwrap();
        let rule = &css[start..start + css[start..].find('}').unwrap()];
        assert!(rule.contains(".toolport-native switch:checked slider"));
        assert!(rule.contains("background-color: white;"));
    }

    #[test]
    fn one_warning_and_partial_color_replace_the_hardcoded_ones() {
        let css = OmarchyPalette::default().css();
        for old in [
            "#f0c674",
            "#956016",
            "#d9a441",
            "#38bdf8",
            "@warning_color",
            "@purple_2",
        ] {
            assert!(
                !css.replace("@define-color toolport_warning #f0c674;", "")
                    .replace("@define-color toolport_partial #38bdf8;", "")
                    .contains(old),
                "{old}"
            );
        }
        let palette =
            OmarchyPalette::parse("mode = \"dark\"\nyellow = \"#e8c66a\"\ncyan = \"#7dd7dc\"\n")
                .unwrap();
        let css = palette.css();
        assert!(css.contains("@define-color toolport_warning #e8c66a;"));
        assert!(css.contains("@define-color toolport_partial #7dd7dc;"));
    }

    /// Every stock Omarchy theme (/usr/share/omarchy/themes, Omarchy 4.0), as
    /// colors.toml with the keys Toolport reads.
    const STOCK_PALETTES: [(&str, &str); 23] = [
        (
            "catppuccin-latte",
            r##"mode = "light"
background = "#eff1f5"
dark_background = "#e3e4e8"
lighter_background = "#dce0e8"
foreground = "#4c4f69"
muted = "#acb0be"
accent = "#1e66f5"
red = "#d20f39"
yellow = "#df8e1d"
green = "#40a02b"
cyan = "#179299"
"##,
        ),
        (
            "catppuccin",
            r##"mode = "dark"
background = "#1e1e2e"
dark_background = "#161622"
lighter_background = "#313244"
foreground = "#cdd6f4"
muted = "#585b70"
accent = "#89b4fa"
red = "#f38ba8"
yellow = "#f9e2af"
green = "#a6e3a1"
cyan = "#94e2d5"
"##,
        ),
        (
            "ethereal",
            r##"mode = "dark"
background = "#060B1E"
dark_background = "#040816"
lighter_background = "#131a3a"
foreground = "#ffcead"
muted = "#6d7db6"
accent = "#7d82d9"
red = "#ED5B5A"
yellow = "#E9BB4F"
green = "#92a593"
cyan = "#a3bfd1"
"##,
        ),
        (
            "everforest",
            r##"mode = "dark"
background = "#2d353b"
dark_background = "#21272c"
lighter_background = "#343f44"
foreground = "#d3c6aa"
muted = "#475258"
accent = "#7fbbb3"
red = "#e67e80"
yellow = "#dbbc7f"
green = "#a7c080"
cyan = "#83c092"
"##,
        ),
        (
            "flexoki-light",
            r##"mode = "light"
background = "#FFFCF0"
dark_background = "#f2efe4"
lighter_background = "#E6E4D9"
foreground = "#100F0F"
muted = "#B7B5AC"
accent = "#205EA6"
red = "#D14D41"
yellow = "#D0A215"
green = "#879A39"
cyan = "#3AA99F"
"##,
        ),
        (
            "gruvbox",
            r##"mode = "dark"
background = "#282828"
dark_background = "#1e1e1e"
lighter_background = "#3c3836"
foreground = "#d4be98"
muted = "#665c54"
accent = "#7daea3"
red = "#ea6962"
yellow = "#d8a657"
green = "#a9b665"
cyan = "#89b482"
"##,
        ),
        (
            "hackerman",
            r##"mode = "dark"
background = "#0B0C16"
dark_background = "#080910"
lighter_background = "#151828"
foreground = "#ddf7ff"
muted = "#2d3450"
accent = "#82FB9C"
red = "#50f872"
yellow = "#50f7d4"
green = "#4fe88f"
cyan = "#7cf8f7"
"##,
        ),
        (
            "kanagawa",
            r##"mode = "dark"
background = "#1f1f28"
dark_background = "#17171e"
lighter_background = "#223249"
foreground = "#dcd7ba"
muted = "#54546D"
accent = "#dcd7ba"
red = "#c34043"
yellow = "#c0a36e"
green = "#76946a"
cyan = "#6a9589"
"##,
        ),
        (
            "last-horizon",
            r##"mode = "dark"
background = "#0c0b0c"
dark_background = "#090809"
lighter_background = "#0c0b0c"
foreground = "#FAFCFB"
muted = "#584e51"
accent = "#b59790"
red = "#c38b7b"
yellow = "#6B5E73"
green = "#87a9b0"
cyan = "#a5a0b6"
"##,
        ),
        (
            "lumon",
            r##"mode = "dark"
background = "#16242d"
dark_background = "#101b21"
lighter_background = "#1b2d40"
foreground = "#d6e2ee"
muted = "#304860"
accent = "#8bc9eb"
red = "#4d86b0"
yellow = "#6fa4c9"
green = "#5e95bc"
cyan = "#b4e4f6"
"##,
        ),
        (
            "lupine",
            r##"mode = "light"
background = "#fafafa"
dark_background = "#ececec"
lighter_background = "#f5f5f5"
foreground = "#212121"
muted = "#9e9e9e"
accent = "#3264eb"
red = "#c900c4"
yellow = "#026fde"
green = "#4a2fd0"
cyan = "#0c67de"
"##,
        ),
        (
            "matte-black",
            r##"mode = "dark"
background = "#121212"
dark_background = "#0d0d0d"
lighter_background = "#1e1e1e"
foreground = "#bebebe"
muted = "#333333"
accent = "#e68e0d"
red = "#D35F5F"
yellow = "#b91c1c"
green = "#FFC107"
cyan = "#bebebe"
"##,
        ),
        (
            "miasma",
            r##"mode = "dark"
background = "#222222"
dark_background = "#191919"
lighter_background = "#2c2c2c"
foreground = "#c2c2b0"
muted = "#666666"
accent = "#78824b"
red = "#685742"
yellow = "#b36d43"
green = "#5f875f"
cyan = "#c9a554"
"##,
        ),
        (
            "nord",
            r##"mode = "dark"
background = "#2e3440"
dark_background = "#222730"
lighter_background = "#3b4252"
foreground = "#d8dee9"
muted = "#4c566a"
accent = "#81a1c1"
red = "#bf616a"
yellow = "#ebcb8b"
green = "#a3be8c"
cyan = "#88c0d0"
"##,
        ),
        (
            "osaka-jade",
            r##"mode = "dark"
background = "#111c18"
dark_background = "#0c1512"
lighter_background = "#23372B"
foreground = "#C1C497"
muted = "#53685B"
accent = "#509475"
red = "#FF5345"
yellow = "#459451"
green = "#549e6a"
cyan = "#2DD5B7"
"##,
        ),
        (
            "retro-82",
            r##"mode = "dark"
background = "#05182e"
dark_background = "#031222"
lighter_background = "#0a2540"
foreground = "#f6dcac"
muted = "#2a6b78"
accent = "#faa968"
red = "#f85525"
yellow = "#e97b3c"
green = "#028391"
cyan = "#8cbfb8"
"##,
        ),
        (
            "ristretto",
            r##"mode = "dark"
background = "#2c2525"
dark_background = "#211b1b"
lighter_background = "#3d2f2a"
foreground = "#e6d9db"
muted = "#72696a"
accent = "#f38d70"
red = "#fd6883"
yellow = "#f9cc6c"
green = "#adda78"
cyan = "#85dacc"
"##,
        ),
        (
            "rose-pine",
            r##"mode = "light"
background = "#faf4ed"
dark_background = "#ede7e1"
lighter_background = "#f2e9e1"
foreground = "#575279"
muted = "#cecacd"
accent = "#56949f"
red = "#b4637a"
yellow = "#ea9d34"
green = "#286983"
cyan = "#d7827e"
"##,
        ),
        (
            "solitude",
            r##"mode = "dark"
background = "#101315"
dark_background = "#0c0e10"
lighter_background = "#101315"
foreground = "#cacccc"
muted = "#4b4e55"
accent = "#798186"
red = "#565d60"
yellow = "#d9dbdc"
green = "#9fa5a9"
cyan = "#707070"
"##,
        ),
        (
            "starship",
            r##"mode = "dark"
background = "#1c1c1e"
dark_background = "#161618"
lighter_background = "#2c2c2e"
foreground = "#f5f5f7"
muted = "#8e8e93"
accent = "#ff9f0a"
red = "#ff5f4a"
yellow = "#ffc27a"
green = "#ff9f0a"
cyan = "#a6dcef"
"##,
        ),
        (
            "tokyo-night",
            r##"mode = "dark"
background = "#1a1b26"
dark_background = "#13141c"
lighter_background = "#24283b"
foreground = "#a9b1d6"
muted = "#414868"
accent = "#7aa2f7"
red = "#f7768e"
yellow = "#e0af68"
green = "#9ece6a"
cyan = "#449dab"
"##,
        ),
        (
            "vantablack",
            r##"mode = "dark"
background = "#000000"
dark_background = "#090909"
lighter_background = "#1a1a1a"
foreground = "#ffffff"
muted = "#7a7a7a"
accent = "#8d8d8d"
red = "#a4a4a4"
yellow = "#cecece"
green = "#b6b6b6"
cyan = "#b0b0b0"
"##,
        ),
        (
            "white",
            r##"mode = "light"
background = "#ffffff"
dark_background = "#f5f5f5"
lighter_background = "#c0c0c0"
foreground = "#000000"
muted = "#808080"
accent = "#6e6e6e"
red = "#2a2a2a"
yellow = "#4a4a4a"
green = "#3a3a3a"
cyan = "#3e3e3e"
"##,
        ),
    ];

    #[test]
    fn secondary_status_and_placeholder_text_meet_their_contrast_on_every_backdrop() {
        let themes = STOCK_PALETTES
            .iter()
            .copied()
            .chain(std::iter::once(("fallback", "")));
        for (theme, input) in themes {
            let palette = OmarchyPalette::parse(input).unwrap();
            let tokens = palette.tokens();
            let (foreground, accent) = (rgb(&palette.foreground), rgb(&palette.accent));
            let surfaces = [
                rgb(&palette.background),
                rgb(&palette.dark_background),
                rgb(&tokens.card),
            ];
            let check = |name: &str, color: &str, minimum: f64, backdrop: &dyn Fn(Rgb) -> Rgb| {
                for surface in surfaces {
                    let ratio = contrast(rgb(color), backdrop(surface).map(f64::round));
                    assert!(ratio >= minimum, "{theme}: {name} {color} is {ratio:.2}:1");
                }
            };
            for (name, color, minimum) in [
                ("text_secondary", &tokens.text_secondary, TEXT_CONTRAST),
                ("text_disabled", &tokens.text_disabled, DISABLED_CONTRAST),
            ] {
                // Plain surface, hover (fg 4%), the info tint (fg 6%), chips
                // (fg 7%) and selected rows (accent 14%).
                check(name, color, minimum, &|s| s);
                for amount in [0.04, 0.06, FG_TINT] {
                    check(name, color, minimum, &|s| mix(s, foreground, amount));
                }
                check(name, color, minimum, &|s| mix(s, accent, SELECTION_TINT));
            }
            for (name, color) in [
                ("success", &tokens.success),
                ("warning", &tokens.warning),
                ("danger", &tokens.danger),
                ("partial", &tokens.partial),
            ] {
                let status = rgb(color);
                check(name, color, TEXT_CONTRAST, &|s| s);
                check(name, color, TEXT_CONTRAST, &|s| mix(s, status, 0.08));
                check(name, color, TEXT_CONTRAST, &|s| mix(s, status, STATUS_TINT));
            }
        }
    }

    #[test]
    fn readable_colors_keep_the_theme_color_when_it_already_passes() {
        let palette = OmarchyPalette::default();
        let tokens = palette.tokens();
        assert_eq!(tokens.text_secondary, FALLBACK_MUTED);
        assert_eq!(tokens.success, FALLBACK_GREEN);
        assert_eq!(tokens.warning, FALLBACK_YELLOW);
    }

    #[test]
    fn navigation_and_status_text_never_use_the_muted_border_colour() {
        let css = OmarchyPalette::default().css();
        for selector in [
            ".toolport-nav-item {",
            ".toolport-mode-badge {",
            ".toolport-state-muted {",
            ".toolport-section-label {",
        ] {
            let start = css.find(selector).expect("the selector is generated");
            let rule = &css[start..start + css[start..].find('}').unwrap()];
            assert!(!rule.contains("@toolport_muted"), "{selector}");
        }
    }
}
