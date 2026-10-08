use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::gio;
use serde::Deserialize;

const FALLBACK_BACKGROUND: &str = "#101315";
const FALLBACK_DARK_BACKGROUND: &str = "#0b0d0f";
const FALLBACK_LIGHTER_BACKGROUND: &str = "#1a1f22";
const FALLBACK_FOREGROUND: &str = "#e8eaed";
const FALLBACK_MUTED: &str = "#92999f";
const FALLBACK_ACCENT: &str = "#8ab4f8";
const FALLBACK_SELECTION: &str = "#334155";
const FALLBACK_RED: &str = "#f38ba8";
const FALLBACK_GREEN: &str = "#a6e3a1";

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

    fn css(&self) -> String {
        format!(
            r#"
@define-color toolport_bg {background};
@define-color toolport_bg_dark {dark_background};
@define-color toolport_surface {lighter_background};
@define-color toolport_fg {foreground};
@define-color toolport_muted {muted};
@define-color toolport_accent {accent};
@define-color toolport_selection {selection};
@define-color toolport_error {red};
@define-color toolport_success {green};

:root {{
  --accent-bg-color: @toolport_accent;
  --accent-fg-color: @toolport_bg_dark;
  --accent-color: @toolport_accent;
  --destructive-bg-color: @toolport_error;
  --destructive-fg-color: @toolport_bg_dark;
  --destructive-color: @toolport_error;
  --success-bg-color: @toolport_success;
  --success-color: @toolport_success;
  --error-bg-color: @toolport_error;
  --error-color: @toolport_error;
  --window-bg-color: @toolport_bg;
  --window-fg-color: @toolport_fg;
  --view-bg-color: @toolport_bg_dark;
  --view-fg-color: @toolport_fg;
  --headerbar-bg-color: @toolport_bg;
  --headerbar-fg-color: @toolport_fg;
  --headerbar-border-color: alpha(@toolport_fg, 0.10);
  --sidebar-bg-color: @toolport_surface;
  --sidebar-fg-color: @toolport_fg;
  --card-bg-color: alpha(@toolport_surface, 0.72);
  --card-fg-color: @toolport_fg;
  --dialog-bg-color: @toolport_bg_dark;
  --dialog-fg-color: @toolport_fg;
  --popover-bg-color: @toolport_bg_dark;
  --popover-fg-color: @toolport_fg;
  --shade-color: alpha(black, 0.28);
}}

window {{
  background-color: @toolport_bg;
  color: @toolport_fg;
  font-size: 14px;
}}

window.messagedialog,
window.dialog-window.alert,
dialog-host > dialog.alert sheet {{
  background-color: alpha(@toolport_bg_dark, 0.98);
  color: @toolport_fg;
}}

window.messagedialog .message-area > .body,
window.dialog-window.alert .message-area > .body,
dialog-host > dialog.alert .message-area > .body {{
  color: alpha(@toolport_fg, 0.78);
}}

window.messagedialog .response-area > button,
window.dialog-window.alert .response-area > button,
dialog-host > dialog.alert .response-area > button {{
  border: 1px solid alpha(@toolport_fg, 0.10);
  color: @toolport_fg;
  background-color: alpha(@toolport_surface, 0.64);
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

.toolport-native,
.toolport-native {{
  background-color: alpha(@toolport_bg, 0.955);
  color: @toolport_fg;
  /* Omarchy's 0.9091 text scale resolves Adwaita Sans 11 to 13.33px. At that
     fractional raster size Pango drops the top stroke of capitals such as E,
     F, P, and T. Keep the chosen family and weights, but use the nearest whole
     CSS pixel so glyph hinting remains intact. */
  font-size: 14px;
}}

.toolport-native .caption {{
  font-size: 12px;
}}

.toolport-shell {{ background-color: transparent; }}

.toolport-content,
.toolport-content scrolledwindow,
.toolport-content viewport {{
  background-color: transparent;
  color: @toolport_fg;
}}

.toolport-sidebar {{
  background-color: alpha(@toolport_bg_dark, 0.34);
  color: @toolport_fg;
  border-right: 1px solid alpha(@toolport_fg, 0.12);
}}

.toolport-header {{
  background-color: alpha(@toolport_bg, 0.18);
  border-bottom: 1px solid alpha(@toolport_fg, 0.08);
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
  padding: 7px;
  border: 1px solid alpha(@toolport_accent, 0.24);
  border-radius: 12px;
  color: @toolport_fg;
  background-color: alpha(@toolport_bg_dark, 0.97);
  box-shadow: 0 12px 30px alpha(black, 0.32);
}}

popover.toolport-main-menu button.toolport-main-menu-item {{
  min-height: 36px;
  padding: 0 10px;
  border: none;
  border-radius: 8px;
  color: alpha(@toolport_fg, 0.82);
  background-color: transparent;
  box-shadow: none;
}}

popover.toolport-main-menu button.toolport-main-menu-item:hover {{
  color: @toolport_fg;
  background-color: alpha(@toolport_accent, 0.12);
}}

popover.toolport-main-menu button.toolport-main-menu-quit {{
  color: alpha(@toolport_error, 0.86);
}}

popover.toolport-main-menu .toolport-menu-shortcut {{
  color: alpha(@toolport_muted, 0.72);
  font-size: 12px;
}}

popover.toolport-main-menu separator {{
  min-height: 1px;
  margin: 5px 8px;
  background-color: alpha(@toolport_fg, 0.10);
}}

popover.toolport-menu > contents {{
  min-width: 220px;
  padding: 6px;
  border: 1px solid alpha(@toolport_accent, 0.22);
  border-radius: 12px;
  color: @toolport_fg;
  background-color: alpha(@toolport_bg_dark, 0.97);
  box-shadow: 0 12px 30px alpha(black, 0.32);
}}

popover.toolport-menu button.toolport-menu-item {{
  min-height: 36px;
  padding: 0 10px;
  border: none;
  border-radius: 8px;
  color: alpha(@toolport_fg, 0.82);
  background-color: transparent;
  box-shadow: none;
}}

popover.toolport-menu button.toolport-menu-item:hover {{
  color: @toolport_fg;
  background-color: alpha(@toolport_accent, 0.12);
}}

popover.toolport-menu button.toolport-menu-item.destructive-action {{
  color: alpha(@toolport_error, 0.88);
}}

popover.toolport-menu .toolport-menu-heading {{
  margin: 5px 10px 2px;
  color: alpha(@toolport_muted, 0.78);
  font-size: 12px;
  font-weight: 700;
}}

popover.toolport-menu .toolport-menu-check {{
  color: @toolport_accent;
}}

popover.toolport-menu separator {{
  min-height: 1px;
  margin: 5px 8px;
  background-color: alpha(@toolport_fg, 0.10);
}}

popover.toolport-catalog-suggestions > contents {{
  padding: 7px;
  border: 1px solid alpha(@toolport_accent, 0.24);
  border-radius: 11px;
  color: @toolport_fg;
  background-color: alpha(@toolport_bg_dark, 0.98);
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

popover.menu > contents,
dropdown popover > contents {{
  border: 1px solid alpha(@toolport_accent, 0.22);
  border-radius: 12px;
  color: @toolport_fg;
  background-color: alpha(@toolport_bg_dark, 0.97);
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

.toolport-nav-item {{
  min-height: 34px;
  padding: 0 12px;
  border-radius: 9px;
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
  border: 1px solid alpha(@toolport_fg, 0.10);
  border-radius: 999px;
  background-color: alpha(@toolport_fg, 0.04);
  color: alpha(@toolport_fg, 0.62);
  font-size: 12px;
}}

.toolport-feedback {{
  padding: 9px 12px;
  border-radius: 9px;
  background-color: alpha(@toolport_fg, 0.04);
  border: 1px solid alpha(@toolport_fg, 0.10);
}}

.toolport-feedback.success {{
  color: @toolport_success;
  border-color: alpha(@toolport_success, 0.22);
  background-color: alpha(@toolport_success, 0.08);
}}

.toolport-feedback.error {{
  color: @toolport_error;
  border-color: alpha(@toolport_error, 0.24);
  background-color: alpha(@toolport_error, 0.08);
}}

.toolport-editor {{
  background-color: @toolport_bg;
  color: @toolport_fg;
}}

.toolport-editor headerbar {{
  min-height: 48px;
  padding: 4px 8px;
  background-color: alpha(@toolport_surface, 0.24);
  border-bottom: 1px solid alpha(@toolport_fg, 0.09);
  box-shadow: none;
}}

.toolport-editor headerbar button {{
  min-height: 32px;
  min-width: 32px;
  padding: 0 12px;
  border-radius: 9px;
  background-color: alpha(@toolport_fg, 0.055);
  border: 1px solid alpha(@toolport_fg, 0.07);
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
  border: 1px solid alpha(@toolport_fg, 0.10);
  border-radius: 11px;
  background-color: alpha(@toolport_bg_dark, 0.38);
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
  background-color: alpha(@toolport_surface, 0.34);
}}

.toolport-import-row:hover {{
  background-color: alpha(@toolport_accent, 0.08);
}}

.toolport-editor-lede {{
  margin: 2px 2px 4px;
  color: alpha(@toolport_fg, 0.72);
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
  padding: 14px;
  border-radius: 12px;
  background-color: alpha(@toolport_surface, 0.46);
  border: 1px solid alpha(@toolport_fg, 0.11);
}}

.toolport-paste-expander {{
  padding: 0 14px;
  border-radius: 12px;
  color: @toolport_fg;
  background-color: alpha(@toolport_surface, 0.36);
  border: 1px solid alpha(@toolport_fg, 0.10);
}}

.toolport-paste-expander > title {{
  min-height: 46px;
  font-weight: 600;
}}

.toolport-paste-expander > title > expander {{
  color: @toolport_accent;
}}

.toolport-field-label {{
  color: alpha(@toolport_fg, 0.72);
  font-size: 12px;
  font-weight: 600;
}}

.toolport-input,
.toolport-input > text,
.toolport-input > button,
.toolport-text-area,
.toolport-text-area textview,
.toolport-text-area text {{
  background-color: alpha(@toolport_bg_dark, 0.46);
  color: @toolport_fg;
}}

.toolport-input,
.toolport-input > button,
.toolport-text-area {{
  min-height: 40px;
  border-radius: 9px;
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
  border-radius: 10px;
  background-color: alpha(@toolport_accent, 0.055);
  border: 1px solid alpha(@toolport_accent, 0.14);
  color: alpha(@toolport_fg, 0.66);
}}

.toolport-editor .toolport-muted {{
  color: alpha(@toolport_fg, 0.66);
}}

.toolport-editor button.toolport-secondary-action {{
  background-color: alpha(@toolport_fg, 0.055);
  border: 1px solid alpha(@toolport_fg, 0.09);
}}

.toolport-editor button.toolport-destructive-icon {{
  color: @toolport_error;
  background-color: alpha(@toolport_error, 0.07);
  border: 1px solid alpha(@toolport_error, 0.15);
}}

.toolport-credential-row {{
  padding: 13px;
  border-radius: 11px;
  border: 1px solid alpha(@toolport_fg, 0.10);
  background-color: alpha(@toolport_bg_dark, 0.24);
}}

.toolport-approvals {{
  padding: 12px;
}}

.toolport-global-alert {{
  color: @toolport_fg;
  background-color: @toolport_bg;
  border: 1px solid alpha(@toolport_accent, 0.34);
  border-radius: 13px;
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
  border: 1px solid alpha(@toolport_accent, 0.28);
  border-radius: 11px;
  background-color: alpha(@toolport_surface, 0.48);
}}

.toolport-approval-deadline {{
  padding: 3px 8px;
  border-radius: 999px;
  color: @toolport_accent;
  background-color: alpha(@toolport_accent, 0.10);
}}

.toolport-sensitive-review {{
  padding: 10px 12px;
  border-radius: 9px;
  color: @toolport_error;
  background-color: alpha(@toolport_error, 0.07);
  border: 1px solid alpha(@toolport_error, 0.20);
}}

.toolport-arguments {{
  margin-top: 8px;
  padding: 10px 12px;
  border-radius: 8px;
  background-color: alpha(@toolport_bg_dark, 0.36);
  font-family: monospace;
}}

.toolport-page {{
  background-image: radial-gradient(circle at 90% 0%, alpha(@toolport_accent, 0.10), transparent 34%);
}}

.toolport-dialog-content {{
  padding: 18px;
}}

.toolport-summary {{
  margin-top: 8px;
  margin-bottom: 8px;
}}

.toolport-summary-item {{
  background-color: alpha(@toolport_surface, 0.32);
  border: 1px solid alpha(@toolport_fg, 0.09);
  border-radius: 11px;
  padding: 10px 13px;
}}

.toolport-security-status {{
  min-height: 48px;
  padding: 8px 12px;
  color: @toolport_fg;
  background-color: alpha(@toolport_surface, 0.28);
  border: 1px solid alpha(@toolport_success, 0.16);
  border-radius: 10px;
  box-shadow: none;
}}

.toolport-security-status:hover {{
  background-color: alpha(@toolport_surface, 0.42);
  border-color: alpha(@toolport_accent, 0.28);
}}

.toolport-security-status.attention {{
  background-color: alpha(@toolport_error, 0.055);
  border-color: alpha(@toolport_error, 0.24);
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
  border-radius: 10px;
  color: @toolport_fg;
  background-color: alpha(@toolport_surface, 0.28);
  border: 1px solid alpha(@toolport_fg, 0.09);
  box-shadow: none;
}}

.toolport-search:focus-within {{
  border-color: alpha(@toolport_accent, 0.62);
  background-color: alpha(@toolport_surface, 0.40);
  box-shadow: 0 0 0 2px alpha(@toolport_accent, 0.10);
}}

.toolport-card {{
  background-color: alpha(@toolport_surface, 0.42);
  border: 1px solid alpha(@toolport_fg, 0.11);
  border-radius: 11px;
  padding: 10px 14px;
  transition: 120ms ease;
}}

.toolport-card:hover {{
  background-color: alpha(@toolport_surface, 0.56);
  border-color: alpha(@toolport_accent, 0.30);
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

.toolport-card-off .toolport-card-icon,
.toolport-card-off button.toolport-card-title {{
  opacity: 0.6;
}}

.toolport-setup-expander.expander image.expander-row-arrow:dir(ltr),
.toolport-setup-expander.expander image.expander-row-arrow:dir(rtl) {{
    -gtk-icon-source: -gtk-icontheme("pan-end-symbolic");
    -gtk-icon-transform: rotate(0turn);
}}
.toolport-setup-expander.expander:checked image.expander-row-arrow:dir(ltr),
.toolport-setup-expander.expander:checked image.expander-row-arrow:dir(rtl) {{
    -gtk-icon-source: -gtk-icontheme("pan-down-symbolic");
    -gtk-icon-transform: rotate(0turn);
}}

.toolport-details-expander {{
  padding: 11px 14px;
  border: 1px solid alpha(@toolport_fg, 0.10);
  border-radius: 11px;
  color: @toolport_fg;
  background-color: alpha(@toolport_surface, 0.30);
}}

.toolport-details-expander:hover {{
  border-color: alpha(@toolport_accent, 0.24);
  background-color: alpha(@toolport_surface, 0.40);
}}

.toolport-details-expander > title {{
  min-height: 30px;
  font-weight: 700;
}}

.toolport-stat-row {{
  min-height: 28px;
  padding: 2px 4px;
}}

.toolport-stat-metrics {{
  font-variant-numeric: tabular-nums;
}}

.toolport-settings-group {{
  background-color: alpha(@toolport_surface, 0.38);
  border: 1px solid alpha(@toolport_fg, 0.10);
  border-radius: 11px;
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
  color: @toolport_accent;
}}
.toolport-state-muted {{
  color: alpha(@toolport_fg, 0.62);
}}

/* Wider than `toolport-summary-item`, which is sized for a number over a
   one-word label rather than a heading over a wrapped sentence. */
.toolport-value-card {{
  background-color: alpha(@toolport_surface, 0.32);
  border: 1px solid alpha(@toolport_fg, 0.09);
  border-radius: 11px;
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
  background-color: alpha(@toolport_bg_dark, 0.22);
  border: 1px solid alpha(@toolport_fg, 0.075);
  border-radius: 9px;
}}

.toolport-project-file-row {{
  padding: 11px 13px;
  border-bottom: 1px solid alpha(@toolport_fg, 0.07);
}}

.toolport-project-file-row:last-child {{
  border-bottom: none;
}}

.toolport-card button.toolport-secondary-action {{
  background-color: alpha(@toolport_fg, 0.055);
  border: 1px solid alpha(@toolport_fg, 0.09);
}}

.toolport-card-icon {{
  min-width: 32px;
  min-height: 32px;
  margin: 1px;
  border-radius: 9px;
  background-color: alpha(@toolport_accent, 0.12);
  color: @toolport_accent;
}}

.toolport-muted {{ color: alpha(@toolport_fg, 0.62); }}

.toolport-badge {{
  background-color: alpha(@toolport_muted, 0.14);
  border: 1px solid alpha(@toolport_muted, 0.16);
  border-radius: 999px;
  padding: 4px 9px;
}}

.toolport-badge.success {{
  background-color: alpha(@toolport_success, 0.12);
  border-color: alpha(@toolport_success, 0.22);
  color: @toolport_success;
}}

.toolport-badge.disabled {{
  color: @toolport_muted;
}}

.toolport-badge.review {{
  color: @toolport_accent;
  border-color: alpha(@toolport_accent, 0.22);
  background-color: alpha(@toolport_accent, 0.08);
}}
.toolport-badge.error {{
  color: @toolport_error;
  border-color: alpha(@toolport_error, 0.24);
  background-color: alpha(@toolport_error, 0.08);
}}

.toolport-badge.approval-denied {{
  color: @toolport_error;
  border-color: alpha(@toolport_error, 0.6);
  background-color: transparent;
}}
.toolport-badge.approval-warning {{
  color: #d9a441;
  border-color: alpha(#d9a441, 0.4);
  background-color: alpha(#d9a441, 0.08);
}}
image.approval-denied {{ color: @toolport_error; }}
image.approval-warning {{ color: #d9a441; }}
image.success {{
  color: @toolport_success;
  -gtk-icon-palette: error @toolport_success, warning @toolport_success, success @toolport_success;
}}
image.disabled {{ color: @toolport_muted; }}

.toolport-catalog-added {{
  min-height: 30px;
  padding: 0 8px;
  border: 1px solid alpha(@toolport_success, 0.22);
  border-radius: 8px;
  background-color: alpha(@toolport_success, 0.10);
  color: @toolport_success;
  font-size: 12px;
  font-weight: 700;
}}

button.toolport-catalog-action {{
  min-height: 32px;
  padding: 0 8px;
  border-radius: 8px;
}}

.toolport-server-state {{
  color: alpha(@toolport_fg, 0.62);
  font-size: 12px;
}}

.toolport-card switch {{
  margin-left: 2px;
}}

.toolport-native switch {{
  color: @toolport_fg;
  background-color: alpha(@toolport_muted, 0.28);
  border: 1px solid alpha(@toolport_fg, 0.14);
  box-shadow: none;
}}

.toolport-native switch:checked {{
  color: @toolport_bg_dark;
  background-color: @toolport_accent;
  border-color: alpha(@toolport_accent, 0.82);
}}

.toolport-native switch:checked slider {{
  background-color: @toolport_bg_dark;
}}

.toolport-native switch slider {{
  background-color: @toolport_fg;
  box-shadow: 0 1px 2px alpha(@toolport_bg_dark, 0.34);
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
  min-height: 180px;
  padding: 28px;
  border: 1px dashed alpha(@toolport_fg, 0.15);
  border-radius: 11px;
  background-color: alpha(@toolport_surface, 0.20);
}}

.toolport-state-icon {{ color: @toolport_accent; }}
.toolport-state-card.error {{
  border-color: alpha(@toolport_error, 0.28);
}}
.toolport-state-card.error .toolport-state-icon {{ color: @toolport_error; }}

selection {{
  background: @toolport_selection;
  color: @toolport_bg_dark;
}}
button.suggested-action {{
  background-color: @toolport_accent;
  color: @toolport_bg_dark;
}}

window:backdrop .toolport-card,
window:backdrop .toolport-summary-item {{
  border-color: alpha(@toolport_fg, 0.06);
}}
"#,
            background = self.background,
            dark_background = self.dark_background,
            lighter_background = self.lighter_background,
            foreground = self.foreground,
            muted = self.muted,
            accent = self.accent,
            selection = self.selection,
            red = self.red,
            green = self.green,
        )
    }
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
    fn generated_css_preserves_the_translucent_surface_hierarchy() {
        let css = OmarchyPalette::default().css();

        assert!(css.contains("background-color: alpha(@toolport_bg, 0.955)"));
        assert!(css.contains("font-size: 14px"));
        assert!(css.contains(".toolport-native .caption"));
        assert!(!css.contains("font-size: 0.88em"));
        assert!(css.contains(".toolport-shell { background-color: transparent; }"));
        assert!(css.contains("background-color: alpha(@toolport_surface, 0.42)"));
        assert!(css.contains("--dialog-bg-color: @toolport_bg_dark"));
        assert!(css.contains("window.messagedialog"));
        assert!(css.contains("dialog-host > dialog.alert sheet"));
    }

    #[test]
    fn navigation_and_status_text_never_use_the_muted_border_colour() {
        let css = OmarchyPalette::default().css();
        for selector in [
            ".toolport-nav-item {",
            ".toolport-mode-badge {",
            ".toolport-state-muted {",
        ] {
            let start = css.find(selector).expect("the selector is generated");
            let rule = &css[start..start + css[start..].find('}').unwrap()];
            assert!(!rule.contains("@toolport_muted"), "{selector}");
        }
    }
}
