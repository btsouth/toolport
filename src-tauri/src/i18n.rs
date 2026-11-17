//! Shared native localization. Only explicit source messages are looked up;
//! caller-owned values are inserted verbatim after lookup.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, RwLock};

type Catalog = HashMap<String, String>;

pub const LANGUAGES: &[(&str, &str)] = &[
    ("en", "English"),
    ("ja", "日本語"),
    ("zh-CN", "简体中文"),
    ("zh-TW", "繁體中文"),
    ("ko", "한국어"),
    ("es", "Español"),
    ("fr", "Français"),
    ("de", "Deutsch"),
    ("pt-BR", "Português (Brasil)"),
];

// Gate embedded UI assets out of headless builds, including Dockerfile.source.
// Each locale is parsed at most once, only when selected or used as fallback.
#[cfg(any(feature = "desktop", feature = "gtk-desktop"))]
mod catalogs {
    use super::{merge_catalog, Catalog};
    use std::sync::OnceLock;

    macro_rules! groups {
        ($locale:literal) => {
            [
                include_str!(concat!("../../src/locales/shell/", $locale, ".json")),
                include_str!(concat!("../../src/locales/connections/", $locale, ".json")),
                include_str!(concat!("../../src/locales/workflows/", $locale, ".json")),
                include_str!(concat!("../../src/locales/common/", $locale, ".json")),
                include_str!(concat!("../../src/locales/native/", $locale, ".json")),
            ]
        };
    }

    struct CatalogEntry {
        locale: &'static str,
        groups: [&'static str; 5],
        parsed: OnceLock<Result<Catalog, String>>,
    }
    impl CatalogEntry {
        const fn new(locale: &'static str, groups: [&'static str; 5]) -> Self {
            Self {
                locale,
                groups,
                parsed: OnceLock::new(),
            }
        }
        fn get(&self) -> Option<&Catalog> {
            self.parsed
                .get_or_init(|| merge_catalog(&self.groups))
                .as_ref()
                .ok()
        }
    }
    static CATALOGS: [CatalogEntry; 9] = [
        CatalogEntry::new("en", groups!("en")),
        CatalogEntry::new("ja", groups!("ja")),
        CatalogEntry::new("zh-CN", groups!("zh-CN")),
        CatalogEntry::new("zh-TW", groups!("zh-TW")),
        CatalogEntry::new("ko", groups!("ko")),
        CatalogEntry::new("es", groups!("es")),
        CatalogEntry::new("fr", groups!("fr")),
        CatalogEntry::new("de", groups!("de")),
        CatalogEntry::new("pt-BR", groups!("pt-BR")),
    ];
    pub(super) fn get(locale: &str) -> Option<&'static Catalog> {
        CATALOGS.iter().find(|entry| entry.locale == locale)?.get()
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn a_lookup_parses_only_its_selected_catalog() {
            let first = CatalogEntry::new(
                "first",
                [r#"{"Language":"Sprache"}"#, "{}", "{}", "{}", "{}"],
            );
            let other = CatalogEntry::new("other", ["{}"; 5]);
            assert!(first.parsed.get().is_none());
            assert_eq!(first.get().unwrap()["Language"], "Sprache");
            assert!(other.parsed.get().is_none());
        }
        #[test]
        fn embedded_catalogs_are_valid() {
            for entry in &CATALOGS {
                assert!(entry.get().is_some(), "{}", entry.locale);
            }
        }
    }
}

fn catalog(locale: &str) -> Option<&'static Catalog> {
    #[cfg(any(feature = "desktop", feature = "gtk-desktop"))]
    {
        catalogs::get(locale)
    }
    #[cfg(not(any(feature = "desktop", feature = "gtk-desktop")))]
    {
        let _ = locale;
        None
    }
}

struct LanguageState {
    choice: String,
    locale: &'static str,
}
static STATE: LazyLock<RwLock<LanguageState>> = LazyLock::new(|| {
    RwLock::new(LanguageState {
        choice: "system".to_owned(),
        locale: "en",
    })
});
static REVISION: AtomicU64 = AtomicU64::new(0);
static UPDATE_LOCK: Mutex<()> = Mutex::new(());

/// Explicit marker for source keys stored in arrays or other deferred metadata.
pub const fn msg(source: &'static str) -> &'static str {
    source
}

#[cfg(any(feature = "desktop", feature = "gtk-desktop", test))]
fn merge_catalog(groups: &[&str]) -> Result<Catalog, String> {
    let mut merged = Catalog::new();
    for group in groups {
        let entries: Catalog = serde_json::from_str(group).map_err(|error| error.to_string())?;
        for (key, value) in entries {
            if merged.insert(key.clone(), value).is_some() {
                return Err(format!("duplicate translation source: {key}"));
            }
        }
    }
    Ok(merged)
}

/// Match React's supported locale resolution, including script-first Chinese.
pub fn resolve_locale(tags: impl IntoIterator<Item = impl AsRef<str>>) -> &'static str {
    for tag in tags {
        let normalized = tag.as_ref().replace('_', "-").to_lowercase();
        let parts: Vec<_> = normalized.split('-').collect();
        match parts[0] {
            "zh" => {
                if parts.contains(&"hant") {
                    return "zh-TW";
                }
                if parts.contains(&"hans") {
                    return "zh-CN";
                }
                return if parts.iter().any(|part| ["tw", "hk", "mo"].contains(part)) {
                    "zh-TW"
                } else {
                    "zh-CN"
                };
            }
            "pt" => return "pt-BR",
            base => {
                if let Some((locale, _)) = LANGUAGES.iter().find(|(locale, _)| *locale == base) {
                    return locale;
                }
            }
        }
    }
    "en"
}

/// Only offer a supported non-English system locale to an undecided upgrader.
pub fn suggested_language(pending: bool) -> Option<(&'static str, &'static str)> {
    suggested_language_for(pending, sys_locale::get_locales())
}

fn suggested_language_for(
    pending: bool,
    tags: impl IntoIterator<Item = impl AsRef<str>>,
) -> Option<(&'static str, &'static str)> {
    if !pending {
        return None;
    }
    let locale = resolve_locale(tags);
    LANGUAGES
        .iter()
        .copied()
        .find(|(code, _)| *code == locale && *code != "en")
}

fn update_language(choice: String) -> bool {
    let requested = if choice == "system" {
        resolve_locale(sys_locale::get_locales())
    } else {
        LANGUAGES
            .iter()
            .find(|(code, _)| *code == choice)
            .map(|(code, _)| *code)
            .unwrap_or("en")
    };
    // Formatting callers must see English too when a dictionary cannot be used.
    let effective = if catalog(requested).is_some() {
        requested
    } else {
        "en"
    };
    let mut state = STATE.write().unwrap_or_else(|error| error.into_inner());
    if state.choice == choice && state.locale == effective {
        return false;
    }
    state.choice = choice;
    state.locale = effective;
    REVISION.fetch_add(1, Ordering::Release);
    true
}

/// Call before constructing any native window, tray or notification.
pub fn initialize() -> Result<(), String> {
    reload_language().map(|_| ())
}

/// Re-read the machine preference after another shell changes it.
pub fn reload_language() -> Result<bool, String> {
    let _guard = UPDATE_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    crate::ui_settings::language().map(update_language)
}

/// Publish a new language only after its machine-local atomic save succeeds.
pub fn set_language(choice: &str) -> Result<(), String> {
    let _guard = UPDATE_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    crate::ui_settings::set_language(choice)?;
    update_language(choice.to_owned());
    Ok(())
}

pub fn language() -> String {
    STATE
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .choice
        .clone()
}

pub fn locale() -> &'static str {
    STATE
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .locale
}

/// A whole number with the current language's digit grouping, as JavaScript's
/// `toLocaleString` writes it in the React app: 1,536 in English, 1.536 in German.
pub fn group_digits(value: usize) -> String {
    group_digits_for(locale(), value)
}

pub(crate) fn group_digits_for(locale: &str, value: usize) -> String {
    let digits = value.to_string();
    let (separator, min_digits) = match locale {
        "de" | "pt-BR" => (".", 4),
        // Spanish leaves four-digit numbers alone.
        "es" => (".", 5),
        "fr" => ("\u{202f}", 4),
        _ => (",", 4),
    };
    if digits.len() < min_digits {
        return digits;
    }
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push_str(separator);
        }
        out.push(digit);
    }
    out
}

/// A local date and time, as the React app writes it with `fmtTs(ms, "dateTime")`
/// (`toLocaleString(locale, {year, month, day: numeric, hour: numeric, minute: 2-digit})`):
/// 11/16/2026, 1:04 AM in English, 16.11.2026, 1:04 in German.
pub fn format_date_time(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> String {
    format_date_time_for(locale(), year, month, day, hour, minute)
}

fn format_date_time_for(
    locale: &str,
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
) -> String {
    // 12-hour clocks show 12 for midnight and noon.
    let h12 = match hour % 12 {
        0 => 12,
        h => h,
    };
    let am = hour < 12;
    match locale {
        "de" => format!("{day}.{month}.{year}, {hour}:{minute:02}"),
        "es" => format!("{day}/{month}/{year}, {hour}:{minute:02}"),
        "fr" => format!("{day:02}/{month:02}/{year} {hour}:{minute:02}"),
        "pt-BR" => format!("{day:02}/{month:02}/{year}, {hour}:{minute:02}"),
        "ja" | "zh-CN" => format!("{year}/{month}/{day} {hour}:{minute:02}"),
        "ko" => format!(
            "{year}. {month}. {day}. {} {h12}:{minute:02}",
            if am { "오전" } else { "오후" }
        ),
        "zh-TW" => format!(
            "{year}/{month}/{day} {}{h12}:{minute:02}",
            if am { "上午" } else { "下午" }
        ),
        _ => format!(
            "{month}/{day}/{year}, {h12}:{minute:02} {}",
            if am { "AM" } else { "PM" }
        ),
    }
}

pub fn revision() -> u64 {
    REVISION.load(Ordering::Acquire)
}

/// Lookup implementation for source-tracking binding helpers. Never pass user data.
pub fn lookup(source: &str) -> String {
    lookup_for(locale(), source)
}

fn lookup_for(locale: &str, source: &str) -> String {
    catalog(locale)
        .and_then(|catalog| catalog.get(source))
        .or_else(|| catalog("en").and_then(|catalog| catalog.get(source)))
        .cloned()
        .unwrap_or_else(|| source.to_owned())
}

pub fn tr(source: &str) -> String {
    lookup(source)
}

pub fn trf(source: &str, values: &[(&str, &str)]) -> String {
    format_message(source, values)
}

/// Formatting implementation for explicit source bindings with owned parameters.
pub fn format_message(source: &str, values: &[(&str, &str)]) -> String {
    interpolate(&lookup(source), values)
}

/// Scan the translated template once. A value containing another placeholder is
/// data, so it must never be replaced by a later interpolation pass.
fn interpolate(template: &str, values: &[(&str, &str)]) -> String {
    let mut result = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        result.push_str(&rest[..open]);
        let token = &rest[open + 1..];
        let Some(close) = token.find('}') else {
            result.push_str(&rest[open..]);
            return result;
        };
        let name = &token[..close];
        if let Some((_, value)) = values.iter().find(|(key, _)| *key == name) {
            result.push_str(value);
        } else {
            result.push_str(&rest[open..open + close + 2]);
        }
        rest = &token[close + 1..];
    }
    result.push_str(rest);
    result
}

#[cfg(test)]
mod tests {
    #[test]
    fn digits_group_like_the_react_app() {
        assert_eq!(super::group_digits_for("en", 1536), "1,536");
        assert_eq!(super::group_digits_for("de", 1536), "1.536");
        assert_eq!(super::group_digits_for("es", 1536), "1536");
        assert_eq!(super::group_digits_for("es", 15360), "15.360");
        assert_eq!(super::group_digits_for("fr", 1536), "1\u{202f}536");
        assert_eq!(super::group_digits_for("ja", 999), "999");
        assert_eq!(super::group_digits_for("en", 1234567), "1,234,567");
    }

    /// The same text the React app's `fmtTs(ms, "dateTime")` gives in each language
    /// (Node's ICU, checked for 2026-11-16 01:04, 2026-01-05 13:30, 2026-12-31 00:00).
    #[test]
    fn dates_read_like_the_react_app() {
        let f = super::format_date_time_for;
        let cases = [
            (
                "en",
                "11/16/2026, 1:04 AM",
                "1/5/2026, 1:30 PM",
                "12/31/2026, 12:00 AM",
            ),
            (
                "de",
                "16.11.2026, 1:04",
                "5.1.2026, 13:30",
                "31.12.2026, 0:00",
            ),
            (
                "es",
                "16/11/2026, 1:04",
                "5/1/2026, 13:30",
                "31/12/2026, 0:00",
            ),
            (
                "fr",
                "16/11/2026 1:04",
                "05/01/2026 13:30",
                "31/12/2026 0:00",
            ),
            ("ja", "2026/11/16 1:04", "2026/1/5 13:30", "2026/12/31 0:00"),
            (
                "ko",
                "2026. 11. 16. 오전 1:04",
                "2026. 1. 5. 오후 1:30",
                "2026. 12. 31. 오전 12:00",
            ),
            (
                "pt-BR",
                "16/11/2026, 1:04",
                "05/01/2026, 13:30",
                "31/12/2026, 0:00",
            ),
            (
                "zh-CN",
                "2026/11/16 1:04",
                "2026/1/5 13:30",
                "2026/12/31 0:00",
            ),
            (
                "zh-TW",
                "2026/11/16 上午1:04",
                "2026/1/5 下午1:30",
                "2026/12/31 上午12:00",
            ),
        ];
        for (locale, a, b, c) in cases {
            assert_eq!(f(locale, 2026, 11, 16, 1, 4), a, "{locale}");
            assert_eq!(f(locale, 2026, 1, 5, 13, 30), b, "{locale}");
            assert_eq!(f(locale, 2026, 12, 31, 0, 0), c, "{locale}");
        }
    }

    use super::*;

    #[test]
    fn locale_resolution_matches_react() {
        for (tag, expected) in [
            ("de-DE", "de"),
            ("pt-PT", "pt-BR"),
            ("zh-HK", "zh-TW"),
            ("zh_Hant_CN", "zh-TW"),
            ("zh-Hans-TW", "zh-CN"),
            ("zh-MO", "zh-TW"),
            ("ja-JP", "ja"),
            ("it-IT", "en"),
        ] {
            assert_eq!(resolve_locale([tag]), expected);
        }
        assert_eq!(resolve_locale(["it-IT", "fr-CA"]), "fr");
    }

    #[test]
    fn interpolation_preserves_inserted_values_and_unknown_tokens() {
        assert_eq!(
            interpolate(
                "Run {command} for {name}; {unknown}",
                &[("command", "{name}"), ("name", "Removed")]
            ),
            "Run {name} for Removed; {unknown}"
        );
        assert_eq!(
            interpolate("UTF-8 日本語 {value} {unfinished", &[("value", "yes")]),
            "UTF-8 日本語 yes {unfinished"
        );
    }

    #[test]
    fn duplicate_sources_are_rejected_and_missing_sources_fall_back() {
        assert!(
            merge_catalog(&[r#"{"Language":"Sprache"}"#, r#"{"Language":"Deutsch"}"#]).is_err()
        );
        assert_eq!(lookup_for("unsupported", "command yes"), "command yes");
    }

    #[cfg(any(feature = "desktop", feature = "gtk-desktop"))]
    #[test]
    fn lookup_uses_the_requested_language() {
        assert_eq!(lookup_for("de", "Language"), "Sprache");
    }

    #[cfg(not(any(feature = "desktop", feature = "gtk-desktop")))]
    #[test]
    fn headless_messages_stay_english_without_catalog_assets() {
        assert!(catalog("de").is_none());
        assert_eq!(lookup_for("de", "Language"), "Language");
    }
    #[test]
    fn suggestions_require_pending_and_a_supported_non_english_system() {
        assert_eq!(
            suggested_language_for(true, ["ja-JP"]),
            Some(("ja", "日本語"))
        );
        assert_eq!(suggested_language_for(false, ["ja-JP"]), None);
        assert_eq!(suggested_language_for(true, ["en-US"]), None);
        assert_eq!(suggested_language_for(true, ["nl-NL"]), None);
    }
}
