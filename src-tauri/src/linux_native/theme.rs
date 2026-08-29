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

.toolport-native,
.toolport-native {{
  background-color: alpha(@toolport_bg, 0.955);
  color: @toolport_fg;
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

.toolport-mark {{
  min-width: 24px;
  min-height: 24px;
  border-radius: 7px;
  background-color: @toolport_accent;
  color: @toolport_bg_dark;
  font-weight: 800;
}}

.toolport-nav-item {{
  min-height: 40px;
  padding: 0 13px;
  border-radius: 9px;
  color: @toolport_muted;
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

.toolport-theme-status {{
  margin: 12px;
  padding: 10px 12px;
  border: 1px solid alpha(@toolport_fg, 0.08);
  border-radius: 10px;
  background-color: alpha(@toolport_fg, 0.025);
}}

.toolport-accent {{ color: @toolport_accent; }}

.toolport-mode-badge {{
  margin: 7px 2px;
  padding: 4px 9px;
  border: 1px solid alpha(@toolport_fg, 0.10);
  border-radius: 999px;
  background-color: alpha(@toolport_fg, 0.04);
  color: @toolport_muted;
  font-size: 0.88em;
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

.toolport-editor-body {{
  padding: 24px;
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
  padding: 16px;
  border-radius: 13px;
  background-color: alpha(@toolport_surface, 0.62);
  border: 1px solid alpha(@toolport_fg, 0.13);
}}

.toolport-field-label {{
  color: alpha(@toolport_fg, 0.72);
  font-size: 0.88em;
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
  margin-top: 2px;
  margin-bottom: 6px;
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

.toolport-summary {{
  margin-top: 8px;
  margin-bottom: 8px;
}}

.toolport-summary-item {{
  background-color: alpha(@toolport_surface, 0.32);
  border: 1px solid alpha(@toolport_fg, 0.09);
  border-radius: 11px;
  padding: 13px 15px;
}}

.toolport-search {{
  min-height: 40px;
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
  padding: 14px 16px;
  transition: 120ms ease;
}}

.toolport-card:hover {{
  background-color: alpha(@toolport_surface, 0.56);
  border-color: alpha(@toolport_accent, 0.30);
}}

.toolport-settings-group {{
  background-color: alpha(@toolport_surface, 0.38);
  border: 1px solid alpha(@toolport_fg, 0.10);
  border-radius: 11px;
}}

.toolport-setting-row {{
  padding: 14px 16px;
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
  min-width: 34px;
  min-height: 34px;
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

.toolport-server-state {{
  color: alpha(@toolport_fg, 0.62);
  font-size: 0.88em;
}}

.toolport-card switch {{
  margin-left: 2px;
}}

.toolport-action-menu contents {{
  padding: 5px;
  border-radius: 11px;
  background-color: @toolport_bg;
  border: 1px solid alpha(@toolport_fg, 0.12);
  box-shadow: 0 10px 28px alpha(@toolport_bg_dark, 0.52);
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

selection {{ background: @toolport_selection; }}
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
        assert!(css.contains(".toolport-shell { background-color: transparent; }"));
        assert!(css.contains("background-color: alpha(@toolport_surface, 0.42)"));
    }
}
