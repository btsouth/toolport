//! Exact-value redaction of resolved secrets at every persistence boundary.
//!
//! Toolport hands a server's secrets to that server, and an upstream is free to echo
//! them back in an error, a result or its stderr. The caller still gets that text
//! unchanged; what Toolport itself writes to disk (audit, gateway and daemon logs,
//! security events, live inspection, search traces) must never carry the value.
//!
//! Every secret this process resolves or stores (vault, encrypted file, environment
//! override, secret reference, OAuth state, HTTP auth headers) is registered here.
//! The log writers then replace any occurrence of the value, or of the encodings an
//! upstream is likely to echo (percent-encoded, base64 in its four common forms,
//! JSON-escaped inside a serialized line), with `[secret:NAME]`. A `Bearer <value>`
//! header is covered by the raw match, and an auth header saved with its scheme also
//! registers the bare credential.
//!
//! Secrets shorter than [`SHORT_SECRET_CHARS`] are matched only as a whole token
//! (not touching a letter, digit or underscore on either side), so a four-digit PIN
//! does not shred every number containing those digits, while `pin=4821` still loses
//! its value. Overlapping matches of different secrets are merged into one marker so
//! no tail of either survives.
//!
//! Cost: with no secret registered every entry point is one relaxed atomic load.
//! Otherwise a line is scanned once by an Aho-Corasick automaton and returned
//! borrowed when nothing matches; only a hit allocates.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};
use base64::Engine as _;
use serde_json::Value;

/// Secrets shorter than this (in chars) are redacted only as a whole token.
pub const SHORT_SECRET_CHARS: usize = 6;
/// Fewest digits a numeric secret needs before a JSON number equal to it is
/// replaced (see [`Matcher::numeric_secret`]).
pub const MIN_NUMERIC_SECRET_DIGITS: usize = 6;
/// How long the background builder waits after a registration for more to
/// arrive, so a burst (every server's secrets at startup) costs one main build.
const MAIN_BUILD_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(25);
/// Records which secrets (by `server/key`, never value) a scrub has covered. A
/// record only: an id can come back with a new value, and nothing derived from
/// a value is kept on disk to tell them apart, so every process rescrubs for
/// everything it knows on its first pass.
const SCRUB_MARKER: &str = "secret-scrub-v1.json";
/// Present while a scrub is owed: written when one is queued, removed when a
/// pass completes. A process that ended first leaves it for the next one (and
/// for status) to find.
pub const SCRUB_PENDING_FILE: &str = "secret-scrub-pending";
/// How long a scrub waits for another process's scrub of the same directory.
/// Long enough for a multi-megabyte audit log on a slow disk.
const SCRUB_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

#[derive(Clone)]
struct Pattern {
    name: usize,
    /// Index into [`Matcher::values`]: the registered value this form spells,
    /// the identity a request-scoped exemption is keyed on.
    value: usize,
    whole_token: bool,
    /// A [`fragment_windows`] window, not a whole value or encoding of one.
    fragment: bool,
}

/// An immutable set of secrets compiled for matching. Pure, so the matching rules
/// are unit-testable without touching the process-global registry.
pub struct Matcher {
    names: Vec<String>,
    /// Every registered value, by the index its patterns carry.
    values: Vec<String>,
    /// Raw-text forms, for log text and JSON string values after parsing.
    raw: Option<AhoCorasick>,
    raw_patterns: Vec<(String, Pattern)>,
    /// Forms matched in any ASCII case: the hex of every secret, and a secret
    /// that is itself hex or base32, which tools print in either case or mixed.
    ci: Option<AhoCorasick>,
    ci_patterns: Vec<Pattern>,
    /// Longest raw pattern in chars, used to bound how much text a truncation site
    /// must scan.
    longest_chars: usize,
    /// Edge index for [`Matcher::redact_fragment`]: the first and the last
    /// [`SHORT_SECRET_CHARS`] characters of every whole-value pattern, each with the
    /// patterns that start (or end) with them.
    heads: Option<(AhoCorasick, Vec<Vec<usize>>)>,
    tails: Option<(AhoCorasick, Vec<Vec<usize>>)>,
    /// Every registered value as written (and trimmed), for whole-value checks of
    /// JSON numbers and identifiers.
    exact: HashMap<String, (usize, usize)>,
    /// Every whole form, sorted (raw), and every case-free form lowercased and
    /// sorted: what [`Matcher::hold_from`] checks a stream's tail against.
    prefixes: Vec<String>,
    ci_prefixes: Vec<String>,
    /// Longest entry of either list, in bytes.
    prefix_reach: usize,
    /// An automaton could not be built, so this matcher cannot find every
    /// form: it then withholds all text ([`WITHHELD`]) instead of letting any
    /// through unredacted.
    broken: bool,
    /// A larger matcher searched together with this one, its names numbered
    /// after this one's: the process registry's main matcher, while this one
    /// holds only the values registered since it was built. Hits from both are
    /// merged in one pass, exactly as if they were one matcher. Never nested.
    extra: Option<Arc<Matcher>>,
}

/// `(start, end, name, fragment, value)`: one automaton hit.
type Hit = (usize, usize, usize, bool, usize);

/// A merged replacement: `(start, end, names)`.
type Span = (usize, usize, Vec<usize>);

/// Index `keys` (one per pattern) into an automaton over the distinct keys and,
/// per distinct key, the patterns that have it.
fn edge_index(keys: Vec<(String, usize)>) -> Option<(AhoCorasick, Vec<Vec<usize>>)> {
    let mut distinct: Vec<String> = Vec::new();
    let mut owners: Vec<Vec<usize>> = Vec::new();
    let mut slot: HashMap<String, usize> = HashMap::new();
    for (key, pattern) in keys {
        match slot.get(&key) {
            Some(&i) => owners[i].push(pattern),
            None => {
                slot.insert(key.clone(), distinct.len());
                distinct.push(key);
                owners.push(vec![pattern]);
            }
        }
    }
    build(&distinct).map(|ac| (ac, owners))
}

fn marker_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        .collect();
    let cleaned = cleaned.trim_matches('_');
    if cleaned.is_empty() {
        "secret".to_string()
    } else {
        cleaned.to_string()
    }
}

/// Shortest base64 inner string registered for a secret embedded at an unknown
/// byte offset inside a longer base64 blob (`Basic base64(user:password)`).
/// Twelve characters carry 72 bits of the secret: on an 89.5 MB corpus of
/// code, docs and logs (`examples/secret-redact-false-positives.rs`) no value
/// from 8 to 24 produced a single false positive, and 12 is what finds an
/// 11-character password behind any username. A secret too short to reach it
/// is still matched in its whole-value encodings.
pub const MIN_BASE64_INNER_CHARS: usize = 12;

/// Shortest line of a multi-line secret (a PEM body) registered on its own.
const MIN_SECRET_LINE_CHARS: usize = 16;

const BASE64_ENGINES: [&base64::engine::GeneralPurpose; 4] = [
    &base64::engine::general_purpose::STANDARD,
    &base64::engine::general_purpose::STANDARD_NO_PAD,
    &base64::engine::general_purpose::URL_SAFE,
    &base64::engine::general_purpose::URL_SAFE_NO_PAD,
];

/// The characters of `base64(prefix ++ value ++ suffix)` that depend only on
/// `value`, for each of the three alignments a prefix of unknown bytes can give
/// it, in both alphabets. A character covers six bits; it is stable when all six
/// fall inside the value's bytes, so the first `ceil(8k/6)` characters (the
/// prefix's influence) and the last partial one (the suffix's) are dropped.
/// Strings shorter than `min_chars` are dropped ([`MIN_BASE64_INNER_CHARS`]).
fn base64_inner_with(value: &[u8], min_chars: usize) -> Vec<String> {
    let mut out = Vec::new();
    for k in 0..3usize {
        let mut shifted = vec![0u8; k];
        shifted.extend_from_slice(value);
        let start = (8 * k).div_ceil(6);
        let end = 8 * (k + value.len()) / 6;
        if end <= start || end - start < min_chars {
            continue;
        }
        for engine in [
            &base64::engine::general_purpose::STANDARD_NO_PAD,
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        ] {
            let encoded = engine.encode(&shifted);
            out.push(encoded[start..end].to_string());
        }
    }
    out
}

#[cfg(test)]
fn base64_inner(value: &[u8]) -> Vec<String> {
    base64_inner_with(value, MIN_BASE64_INNER_CHARS)
}

/// Length of the windows [`fragment_windows`] takes from a secret.
pub const FRAGMENT_CHARS: usize = 8;
/// Secrets at least this long are also matched by fragment.
pub const FRAGMENT_MIN_SECRET_CHARS: usize = 12;

/// The length thresholds a [`Matcher`] is built with. Production always uses
/// [`Thresholds::default`] (the constants above); other values exist only so
/// the secret-redact-false-positives example can measure each one.
#[doc(hidden)]
#[derive(Clone, Copy, Debug)]
pub struct Thresholds {
    /// [`FRAGMENT_CHARS`].
    pub fragment_chars: usize,
    /// [`FRAGMENT_MIN_SECRET_CHARS`]; `usize::MAX` turns fragment windows off.
    pub fragment_min_secret_chars: usize,
    /// Edge length for [`Matcher::redact_fragment`] ([`SHORT_SECRET_CHARS`]);
    /// `usize::MAX` turns dangling-edge removal off.
    pub edge_chars: usize,
    /// [`MIN_BASE64_INNER_CHARS`]; `usize::MAX` turns embedded base64 off.
    pub base64_inner_chars: usize,
    /// Count upper- and lowercase letters as one class when choosing windows,
    /// so a window must hold a digit or a symbol. Off in production.
    pub letters_one_class: bool,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            fragment_chars: FRAGMENT_CHARS,
            fragment_min_secret_chars: FRAGMENT_MIN_SECRET_CHARS,
            edge_chars: SHORT_SECRET_CHARS,
            base64_inner_chars: MIN_BASE64_INNER_CHARS,
            letters_one_class: false,
        }
    }
}

/// Every [`FRAGMENT_CHARS`]-character window of a secret of at least
/// [`FRAGMENT_MIN_SECRET_CHARS`], so any piece of it that long is found wherever it
/// appears (a stderr tail that starts mid-value, a value split across two
/// messages): adjacent window hits merge into one span covering the whole piece.
/// Windows of a single character class (`password`, `12345678`) are skipped, since
/// an ordinary word inside a passphrase must not redact that word everywhere.
///
/// Containers are matched whole, never by fragment: a URL, a JSON document (OAuth
/// state) or an auth header with its scheme would otherwise turn `https://`,
/// `{"access_` or `Bearer a` into markers everywhere. The secrets inside them are
/// registered on their own and windowed there.
fn fragment_windows(value: &str, t: &Thresholds) -> Vec<String> {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() < t.fragment_min_secret_chars || is_container(value) {
        return Vec::new();
    }
    let width = t.fragment_chars.max(1);
    // A JWT's header (and the start of its payload) is the same in every token
    // an issuer signs, so a window there would redact other tokens and every
    // log that quotes one. Only windows that reach the signature are used:
    // the signature is what makes a piece of a token usable, and it differs
    // per token. The whole token is still matched in every form.
    let from = jwt_signature_start(value).map_or(0, |sig| sig.saturating_sub(width - 1));
    // PEM armor lines are public too: no window touches one.
    let mut armor = Vec::with_capacity(chars.len());
    for line in value.split_inclusive('\n') {
        let public = line.trim().starts_with("-----");
        armor.extend(line.chars().map(|_| public));
    }
    chars
        .windows(width)
        .enumerate()
        .skip(from)
        .filter(|(i, _)| !armor[*i..*i + width].contains(&true))
        .map(|(_, window)| window)
        .filter(|window| {
            let lower = window.iter().any(|c| c.is_lowercase());
            let upper = window.iter().any(|c| c.is_uppercase());
            let (lower, upper) = if t.letters_one_class {
                (lower || upper, false)
            } else {
                (lower, upper)
            };
            let classes = [
                lower,
                upper,
                window.iter().any(|c| c.is_numeric()),
                window
                    .iter()
                    .any(|c| !c.is_alphanumeric() && !c.is_whitespace()),
            ];
            classes.iter().filter(|present| **present).count() >= 2
        })
        .map(|window| window.iter().collect())
        .collect()
}

/// The char index where a JWT's signature starts (`header.payload.signature`,
/// base64url, the header a JSON object), or `None` when `value` is not one.
fn jwt_signature_start(value: &str) -> Option<usize> {
    let parts: Vec<&str> = value.split('.').collect();
    let base64url = |p: &str| {
        !p.is_empty()
            && p.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    };
    (parts.len() == 3 && parts[0].starts_with("eyJ") && parts.iter().all(|p| base64url(p)))
        .then(|| parts[0].len() + parts[1].len() + 2)
}

fn is_container(value: &str) -> bool {
    let trimmed = value.trim_start();
    let lower = trimmed.get(..7).unwrap_or(trimmed).to_ascii_lowercase();
    value.contains("://")
        || trimmed.starts_with(['{', '['])
        || ["bearer ", "basic ", "token ", "bot "]
            .iter()
            .any(|scheme| lower.starts_with(scheme))
}

/// Text forms an upstream error formatter realistically applies to a value it
/// quotes: the value itself, trimmed, upper- and lowercased, Unicode NFC/NFKC
/// (a formatter or terminal that normalizes), and each line of a multi-line value
/// (a PEM key quoted one line at a time, or with its newlines collapsed).
fn text_forms(value: &str) -> Vec<String> {
    use unicode_normalization::UnicodeNormalization as _;
    let mut out = vec![value.to_string()];
    let mut push = |v: String| {
        if !v.trim().is_empty() && !out.contains(&v) {
            out.push(v);
        }
    };
    push(value.trim().to_string());
    push(value.to_uppercase());
    push(value.to_lowercase());
    if !value.is_ascii() {
        push(value.nfc().collect());
        push(value.nfkc().collect());
    }
    if value.contains(['\n', '\r']) {
        // PEM armor ("-----BEGIN ... KEY-----") is public, the same in
        // every key: never a form of this one.
        for line in value.lines().map(str::trim) {
            if line.chars().count() >= MIN_SECRET_LINE_CHARS && !line.starts_with("-----") {
                push(line.to_string());
            }
        }
        push(value.split_whitespace().collect::<Vec<_>>().join(" "));
    }
    out
}

/// Every form an upstream plausibly echoes. Deduplicated; the raw value first.
///
/// For the value and each [`text_forms`] form: itself, percent-encoded (upper and
/// lower hex, and form encoding), hex of its bytes (lower and upper), JSON-escaped
/// once and twice (a JSON document quoted inside a JSON string), and base64 in
/// four variants. For the value alone: base64 of `:value` (HTTP Basic with an
/// empty username) and the [`base64_inner`] strings, which find it at any offset
/// inside a longer base64 blob, Basic credentials with an unknown username among
/// them.
/// Forms of `value` matched in any ASCII case: the hex of its bytes, and the
/// value itself when it is hex (8+ chars) or base32 (16+ chars), alphabets in
/// which case carries no meaning.
fn case_free_forms(value: &str) -> Vec<String> {
    let value = value.trim();
    let mut out = vec![value
        .bytes()
        .map(|b| format!("{b:02x}"))
        .collect::<String>()];
    let body = value.trim_end_matches('=');
    let hex = body.len() >= 8 && body.bytes().all(|b| b.is_ascii_hexdigit());
    let base32 = body.len() >= 16
        && body
            .bytes()
            .all(|b| b.is_ascii_alphabetic() || (b'2'..=b'7').contains(&b));
    if hex || base32 {
        out.push(value.to_ascii_lowercase());
    }
    out
}

fn variants(value: &str, t: &Thresholds) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let push = |v: String, out: &mut Vec<String>| {
        if !v.is_empty() && !out.contains(&v) {
            out.push(v);
        }
    };
    for form in text_forms(value) {
        for spelling in encodings(&form) {
            push(spelling, &mut out);
        }
    }
    for engine in BASE64_ENGINES {
        push(engine.encode(format!(":{value}")), &mut out);
    }
    for inner in base64_inner_with(value.as_bytes(), t.base64_inner_chars) {
        push(inner, &mut out);
    }
    out
}

/// `form` and its lossless encodings: percent, form, hex, JSON escapes, base64.
fn encodings(form: &str) -> Vec<String> {
    let mut out = vec![form.to_string()];
    let percent = urlencoding::encode(form).into_owned();
    out.push(percent.to_ascii_lowercase_hex());
    out.push(percent);
    out.push(url::form_urlencoded::byte_serialize(form.as_bytes()).collect());
    let hex: String = form.bytes().map(|b| format!("{b:02x}")).collect();
    out.push(hex.to_ascii_uppercase());
    out.push(hex);
    let once = json_escaped(form);
    out.push(json_escaped(&once));
    out.push(once);
    for engine in BASE64_ENGINES {
        out.push(engine.encode(form.as_bytes()));
    }
    out
}

/// Whether `text` spells all of `value`, as written or losslessly encoded
/// (at any decoding level). A line, a case-folded copy or another partial
/// form never does: it does not show the client knows the value.
fn spells_whole(text: &str, value: &str) -> bool {
    let all = encodings(value);
    let exact = |s: &str| all.iter().any(|e| e == s);
    exact(text)
        || crate::secret_normalize::levels(text)
            .iter()
            .any(|l| exact(&l.text))
}

trait LowerHex {
    fn to_ascii_lowercase_hex(&self) -> String;
}
impl LowerHex for String {
    /// `%2F` -> `%2f`, leaving every other character alone.
    fn to_ascii_lowercase_hex(&self) -> String {
        let mut out = String::with_capacity(self.len());
        let mut chars = self.chars().peekable();
        while let Some(c) = chars.next() {
            out.push(c);
            if c == '%' {
                for _ in 0..2 {
                    if let Some(h) = chars.next() {
                        out.push(h.to_ascii_lowercase());
                    }
                }
            }
        }
        out
    }
}

fn json_escaped(text: &str) -> String {
    let quoted = serde_json::to_string(text).unwrap_or_default();
    quoted[1..quoted.len().saturating_sub(1).max(1)].to_string()
}

fn is_token_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Every automaton is a contiguous NFA. A DFA's memory grows with its states
/// times the alphabet: 940 patterns for 20 secrets took two DFAs of 9.4 MiB
/// each, against 1 MiB for the contiguous NFAs, for a per-line cost that stays
/// in the hundreds of nanoseconds either way (see the secret-redact-performance
/// example).
fn build(patterns: &[String]) -> Option<AhoCorasick> {
    if patterns.is_empty() {
        return None;
    }
    built(
        AhoCorasickBuilder::new()
            .match_kind(MatchKind::Standard)
            .kind(Some(aho_corasick::AhoCorasickKind::ContiguousNFA))
            .build(patterns),
    )
}

#[cfg(test)]
thread_local! {
    /// Test hook: every automaton build on this thread fails, as one past the
    /// library's limits would.
    static FAIL_BUILDS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

thread_local! {
    /// Set when an automaton for a non-empty pattern set could not be built
    /// during the [`Matcher::new_with`] running on this thread.
    static BUILD_FAILED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn built(result: Result<AhoCorasick, aho_corasick::BuildError>) -> Option<AhoCorasick> {
    #[cfg(test)]
    let result = if FAIL_BUILDS.with(|f| f.get()) {
        Err(())
    } else {
        result.map_err(|_| ())
    };
    match result {
        Ok(ac) => Some(ac),
        Err(_) => {
            BUILD_FAILED.with(|f| f.set(true));
            None
        }
    }
}

/// What a matcher that could not be built writes in place of any text.
pub const WITHHELD: &str = "[withheld: secret redaction unavailable]";

impl Matcher {
    pub fn new<N: AsRef<str>, V: AsRef<str>>(secrets: &[(N, V)]) -> Self {
        Self::new_with(secrets, &HashSet::new())
    }

    /// Like [`Matcher::new`], with `whole_only` values matched in their whole forms
    /// but never by fragment: values that are not themselves a secret but contain
    /// one (a `user:password` pair, a username) whose secret part is registered on
    /// its own.
    pub fn new_with<N: AsRef<str>, V: AsRef<str>>(
        secrets: &[(N, V)],
        whole_only: &HashSet<String>,
    ) -> Self {
        Self::new_tuned(secrets, whole_only, &Thresholds::default())
    }

    /// [`Matcher::new_with`] with other [`Thresholds`]; for measurement only.
    #[doc(hidden)]
    pub fn new_tuned<N: AsRef<str>, V: AsRef<str>>(
        secrets: &[(N, V)],
        whole_only: &HashSet<String>,
        t: &Thresholds,
    ) -> Self {
        let edge_chars = t.edge_chars;
        let mut names: Vec<String> = Vec::new();
        let mut values: Vec<String> = Vec::new();
        let mut raw_patterns: Vec<(String, Pattern)> = Vec::new();
        let mut seen = HashSet::new();
        let mut ci_patterns: Vec<(String, Pattern)> = Vec::new();
        let mut ci_seen = HashSet::new();
        for (name, value) in secrets {
            let value = value.as_ref();
            if value.trim().is_empty() {
                continue;
            }
            let marker = marker_name(name.as_ref());
            let name = match names.iter().position(|n| n == &marker) {
                Some(i) => i,
                None => {
                    names.push(marker);
                    names.len() - 1
                }
            };
            let vid = values.len();
            values.push(value.to_string());
            // A short secret keeps the whole-token rule in every encoding: its hex or
            // base64 is long enough to look specific but is still only a few bytes.
            let short = value.chars().count() < SHORT_SECRET_CHARS;
            for variant in variants(value, t) {
                if seen.insert(variant.clone()) {
                    let whole_token = short || variant.chars().count() < SHORT_SECRET_CHARS;
                    raw_patterns.push((
                        variant,
                        Pattern {
                            name,
                            value: vid,
                            whole_token,
                            fragment: false,
                        },
                    ));
                }
            }
            let windows = if whole_only.contains(value) {
                Vec::new()
            } else {
                fragment_windows(value, t)
            };
            for window in windows {
                if seen.insert(window.clone()) {
                    raw_patterns.push((
                        window,
                        Pattern {
                            name,
                            value: vid,
                            whole_token: false,
                            fragment: true,
                        },
                    ));
                }
            }
            for form in case_free_forms(value) {
                if ci_seen.insert(form.clone()) {
                    ci_patterns.push((
                        form,
                        Pattern {
                            name,
                            value: vid,
                            whole_token: short,
                            fragment: false,
                        },
                    ));
                }
            }
        }
        BUILD_FAILED.with(|f| f.set(false));
        let raw_texts: Vec<String> = raw_patterns.iter().map(|(p, _)| p.clone()).collect();
        let longest_chars = raw_texts
            .iter()
            .map(|p| p.chars().count())
            .max()
            .unwrap_or(0);
        let edge_patterns = || {
            raw_patterns.iter().enumerate().filter(|(_, (text, meta))| {
                !meta.whole_token && !meta.fragment && text.chars().count() > edge_chars
            })
        };
        let heads = edge_index(
            edge_patterns()
                .map(|(i, (text, _))| (text.chars().take(edge_chars).collect(), i))
                .collect(),
        );
        let tails = edge_index(
            edge_patterns()
                .map(|(i, (text, _))| {
                    let skip = text.chars().count() - edge_chars;
                    (text.chars().skip(skip).collect(), i)
                })
                .collect(),
        );
        let mut exact = std::collections::HashMap::new();
        let mut vid = 0;
        for (name, value) in secrets {
            let value = value.as_ref();
            if value.trim().is_empty() {
                continue;
            }
            let marker = marker_name(name.as_ref());
            if let Some(n) = names.iter().position(|m| *m == marker) {
                exact.entry(value.to_string()).or_insert((n, vid));
                exact.entry(value.trim().to_string()).or_insert((n, vid));
            }
            vid += 1;
        }
        let ci_texts: Vec<String> = ci_patterns.iter().map(|(t, _)| t.clone()).collect();
        let ci = (!ci_texts.is_empty())
            .then(|| {
                built(
                    AhoCorasickBuilder::new()
                        .match_kind(MatchKind::Standard)
                        .kind(Some(aho_corasick::AhoCorasickKind::ContiguousNFA))
                        .ascii_case_insensitive(true)
                        .build(&ci_texts),
                )
            })
            .flatten();
        let mut prefixes: Vec<String> = raw_patterns
            .iter()
            .filter(|(_, meta)| !meta.fragment)
            .map(|(text, _)| text.clone())
            .collect();
        prefixes.sort_unstable();
        prefixes.dedup();
        let mut ci_prefixes: Vec<String> =
            ci_texts.iter().map(|t| t.to_ascii_lowercase()).collect();
        ci_prefixes.sort_unstable();
        ci_prefixes.dedup();
        let prefix_reach = prefixes
            .iter()
            .chain(&ci_prefixes)
            .map(String::len)
            .max()
            .unwrap_or(0);
        let raw = build(&raw_texts);
        let broken = BUILD_FAILED.with(|f| f.replace(false));
        Self {
            names,
            values,
            raw,
            raw_patterns,
            ci,
            ci_patterns: ci_patterns.into_iter().map(|(_, p)| p).collect(),
            broken,
            longest_chars,
            heads,
            tails,
            exact,
            prefixes,
            ci_prefixes,
            prefix_reach,
            extra: None,
        }
    }

    /// This matcher searched together with `main` (see [`Matcher::extra`]).
    fn over(mut self, main: Option<Arc<Matcher>>) -> Self {
        debug_assert!(main.as_ref().is_none_or(|m| m.extra.is_none()));
        self.extra = main;
        self
    }

    /// The marker name of name index `n` (this matcher's, then `extra`'s).
    fn name(&self, n: usize) -> &str {
        match (n.checked_sub(self.names.len()), &self.extra) {
            (Some(i), Some(extra)) => extra.names[i].as_str(),
            _ => self.names[n].as_str(),
        }
    }

    fn base(&self) -> usize {
        self.names.len()
    }

    /// The registered value of value index `v` (this matcher's, then `extra`'s).
    fn value(&self, v: usize) -> &str {
        match (v.checked_sub(self.values.len()), &self.extra) {
            (Some(i), Some(extra)) => extra.values[i].as_str(),
            _ => self.values[v].as_str(),
        }
    }

    /// `extra`'s hits for `f`, with names and values shifted after this matcher's.
    fn extra_hits(&self, f: impl Fn(&Matcher) -> Vec<Hit>) -> Vec<Hit> {
        let base = self.base();
        let vbase = self.values.len();
        self.extra.as_ref().map_or_else(Vec::new, |m| {
            f(m).into_iter()
                .map(|(s, e, n, frag, v)| (s, e, n + base, frag, v + vbase))
                .collect()
        })
    }

    fn longest(&self) -> usize {
        self.extra.as_ref().map_or(self.longest_chars, |m| {
            m.longest_chars.max(self.longest_chars)
        })
    }

    /// `(name, value)` of the registered value `text` is exactly.
    fn exact_entry(&self, text: &str) -> Option<(usize, usize)> {
        self.exact.get(text).copied().or_else(|| {
            let (base, vbase) = (self.base(), self.values.len());
            self.extra
                .as_ref()
                .and_then(|m| m.exact.get(text).map(|(n, v)| (n + base, v + vbase)))
        })
    }

    fn has_exact(&self) -> bool {
        !self.exact.is_empty() || self.extra.as_ref().is_some_and(|m| !m.exact.is_empty())
    }

    /// Heap bytes held by the compiled automata, for the performance notes.
    pub fn memory_usage(&self) -> usize {
        let edge = |e: &Option<(AhoCorasick, Vec<Vec<usize>>)>| {
            e.as_ref().map_or(0, |(ac, _)| ac.memory_usage())
        };
        self.raw.as_ref().map_or(0, AhoCorasick::memory_usage)
            + self.ci.as_ref().map_or(0, AhoCorasick::memory_usage)
            + edge(&self.heads)
            + edge(&self.tails)
            + self
                .prefixes
                .iter()
                .chain(&self.ci_prefixes)
                .map(|p| p.capacity() + std::mem::size_of::<String>())
                .sum::<usize>()
            + self.extra.as_ref().map_or(0, |m| m.memory_usage())
    }

    /// Whether this matcher (or the one searched with it) could not be built.
    pub fn is_broken(&self) -> bool {
        self.broken || self.extra.as_ref().is_some_and(|m| m.broken)
    }

    pub fn is_empty(&self) -> bool {
        !self.is_broken() && self.raw.is_none() && self.extra.as_ref().is_none_or(|m| m.is_empty())
    }

    fn marker(&self, names: &[usize]) -> String {
        if names.is_empty() {
            return WITHHELD.to_string();
        }
        let mut listed: Vec<&str> = Vec::new();
        for &n in names {
            let name = self.name(n);
            if !listed.contains(&name) {
                listed.push(name);
            }
        }
        format!("[secret:{}]", listed.join(","))
    }

    /// Merged `(start, end, names)` spans of every secret occurrence in `text`.
    fn spans(&self, text: &str) -> Vec<Span> {
        self.spans_with(text, true)
    }

    /// [`Self::spans`], optionally without fragment windows (whole forms only).
    fn spans_with(&self, text: &str, fragments: bool) -> Vec<Span> {
        self.piece_spans(text, &[], fragments, None)
    }

    /// The one search every sink uses. `text` is a logical text made of pieces
    /// (string leaves of one value, lines of a stream, messages of one progress
    /// stream) that start at `starts` (empty: one piece). The separators between
    /// pieces (other JSON, line endings) are not in it, so a secret written
    /// across pieces is found like one written whole.
    ///
    /// Text with an encoding introducer (`\`, `%`, `&`) is searched at every
    /// decoding level ([`crate::secret_normalize::levels`]: JSON escapes,
    /// percent and form encoding and HTML entities, mixed per character, up to
    /// [`crate::secret_normalize::MAX_LEVELS`] deep), and each hit at a level is
    /// mapped back to the source span it was decoded from, so a secret spelled
    /// `sk-&#65;%42\u0043`, or a percent-encoded value encoded once more, is
    /// replaced where it is written and everything else is left as it is.
    ///
    /// A fragment window that crosses a piece boundary is dropped: windows are
    /// for a piece of a secret inside one text, and joining unrelated leaves
    /// would make new ones. Whole forms may cross any number of boundaries.
    ///
    /// With `exempt`, a span made only of whole forms of values in it (the
    /// values a client sent) is left as it is; see [`Self::values_in`].
    fn piece_spans(
        &self,
        text: &str,
        starts: &[usize],
        fragments: bool,
        exempt: Option<&HashSet<String>>,
    ) -> Vec<Span> {
        if self.is_broken() {
            return withheld_span(text.len());
        }
        // The whole logical text, with nothing to decode, matches nothing:
        // nothing can be in any piece or across them.
        if !crate::secret_normalize::has_encoding(text) && !self.may_contain_plain(text) {
            return Vec::new();
        }
        let mut hits = self.plain_hits(text, fragments, starts);
        for level in crate::secret_normalize::levels(text) {
            hits.extend(self.level_hits(&level, fragments, starts));
        }
        if starts.len() > 1 {
            hits.retain(|h| !h.3 || !crosses(starts, h.0, h.1));
        }
        self.merge_exempt(hits, exempt)
    }

    /// [`merge`], leaving out every span made only of whole forms of `exempt`
    /// values: never one that also holds another secret, or only a fragment.
    fn merge_exempt(&self, hits: Vec<Hit>, exempt: Option<&HashSet<String>>) -> Vec<Span> {
        let Some(exempt) = exempt.filter(|x| !x.is_empty()) else {
            return merge(hits);
        };
        let mut sorted = hits;
        sorted.sort_by_key(|h| (h.0, std::cmp::Reverse(h.1)));
        let mut out: Vec<Hit> = Vec::with_capacity(sorted.len());
        let mut group: Vec<Hit> = Vec::new();
        let mut end = 0;
        let flush = |group: &mut Vec<Hit>, out: &mut Vec<Hit>| {
            // The span is exactly one whole form of a value the client sent
            // (what it holds inside, such as a shorter secret, is part of it).
            let (gs, ge) = (
                group.iter().map(|h| h.0).min().unwrap_or(0),
                group.iter().map(|h| h.1).max().unwrap_or(0),
            );
            let exempted = group
                .iter()
                .any(|h| !h.3 && h.0 == gs && h.1 == ge && exempt.contains(self.value(h.4)));
            if !exempted {
                out.append(group);
            } else {
                group.clear();
            }
        };
        for h in sorted {
            if !group.is_empty() && h.0 >= end {
                flush(&mut group, &mut out);
            }
            end = if group.is_empty() { h.1 } else { end.max(h.1) };
            group.push(h);
        }
        flush(&mut group, &mut out);
        merge(out)
    }

    /// Automaton hits in `text`. A piece boundary (one of `starts`) is a word
    /// boundary for the whole-token rule, as the end of a separate text is.
    fn plain_hits(&self, text: &str, fragments: bool, starts: &[usize]) -> Vec<Hit> {
        let mut hits = self.own_hits(text, fragments, starts);
        hits.extend(self.extra_hits(|m| m.own_hits(text, fragments, starts)));
        hits
    }

    /// [`Self::plain_hits`] at one decoding level, in source offsets.
    fn level_hits(
        &self,
        level: &crate::secret_normalize::Normalized,
        fragments: bool,
        starts: &[usize],
    ) -> Vec<Hit> {
        let level_starts = level_starts(level, starts);
        self.plain_hits(&level.text, fragments, &level_starts)
            .into_iter()
            .map(|(s, e, name, fragment, value)| {
                let (s, e) = crate::secret_normalize::source_range(level, s, e);
                (s, e, name, fragment, value)
            })
            .collect()
    }

    fn own_hits(&self, text: &str, fragments: bool, starts: &[usize]) -> Vec<Hit> {
        let edge = |i: usize| starts.binary_search(&i).is_ok();
        let Some(ac) = &self.raw else {
            return Vec::new();
        };
        let mut hits: Vec<Hit> = Vec::new();
        for m in ac.find_overlapping_iter(text) {
            let pattern = &self.raw_patterns[m.pattern().as_usize()].1;
            if pattern.fragment && !fragments {
                continue;
            }
            if pattern.whole_token {
                let before = text[..m.start()].chars().next_back();
                let after = text[m.end()..].chars().next();
                if (before.is_some_and(is_token_char) && !edge(m.start()))
                    || (after.is_some_and(is_token_char) && !edge(m.end()))
                {
                    continue;
                }
            }
            hits.push((
                m.start(),
                m.end(),
                pattern.name,
                pattern.fragment,
                pattern.value,
            ));
        }
        if let Some(ci) = &self.ci {
            for m in ci.find_overlapping_iter(text) {
                let pattern = &self.ci_patterns[m.pattern().as_usize()];
                if pattern.whole_token {
                    let before = text[..m.start()].chars().next_back();
                    let after = text[m.end()..].chars().next();
                    if (before.is_some_and(is_token_char) && !edge(m.start()))
                        || (after.is_some_and(is_token_char) && !edge(m.end()))
                    {
                        continue;
                    }
                }
                hits.push((m.start(), m.end(), pattern.name, false, pattern.value));
            }
        }
        hits
    }

    fn apply(&self, text: &str, spans: &[(usize, usize, Vec<usize>)]) -> String {
        let mut out = String::with_capacity(text.len());
        let mut at = 0;
        for (start, end, names) in spans {
            out.push_str(&text[at..*start]);
            out.push_str(&self.marker(names));
            at = *end;
        }
        out.push_str(&text[at..]);
        out
    }

    /// [`Self::spans`] over raw file bytes, which may not be valid UTF-8. A byte
    /// outside ASCII counts as a token character for the short-secret rule, the
    /// conservative choice for a whole-token check.
    fn byte_spans(&self, bytes: &[u8], starts: &[usize]) -> Vec<Span> {
        if self.is_broken() {
            return withheld_span(bytes.len());
        }
        let mut hits = self.own_byte_hits(bytes, starts);
        hits.extend(self.extra_hits(|m| m.own_byte_hits(bytes, starts)));
        // Encoded spellings, as in `piece_spans`, wherever the bytes are text.
        if let Ok(text) = std::str::from_utf8(bytes) {
            for level in crate::secret_normalize::levels(text) {
                hits.extend(self.level_hits(&level, true, starts));
            }
        }
        if starts.len() > 1 {
            hits.retain(|h| !h.3 || !crosses(starts, h.0, h.1));
        }
        merge(hits)
    }

    fn own_byte_hits(&self, bytes: &[u8], starts: &[usize]) -> Vec<Hit> {
        let Some(ac) = &self.raw else {
            return Vec::new();
        };
        let edge = |i: usize| starts.binary_search(&i).is_ok();
        let token = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80;
        let mut hits = Vec::new();
        for m in ac.find_overlapping_iter(bytes) {
            let pattern = &self.raw_patterns[m.pattern().as_usize()].1;
            if pattern.whole_token {
                let before = m.start().checked_sub(1).map(|i| bytes[i]);
                let after = bytes.get(m.end()).copied();
                if (before.is_some_and(token) && !edge(m.start()))
                    || (after.is_some_and(token) && !edge(m.end()))
                {
                    continue;
                }
            }
            hits.push((
                m.start(),
                m.end(),
                pattern.name,
                pattern.fragment,
                pattern.value,
            ));
        }
        if let Some(ci) = &self.ci {
            for m in ci.find_overlapping_iter(bytes) {
                let pattern = &self.ci_patterns[m.pattern().as_usize()];
                if pattern.whole_token {
                    let before = m.start().checked_sub(1).map(|i| bytes[i]);
                    let after = bytes.get(m.end()).copied();
                    if (before.is_some_and(token) && !edge(m.start()))
                        || (after.is_some_and(token) && !edge(m.end()))
                    {
                        continue;
                    }
                }
                hits.push((m.start(), m.end(), pattern.name, false, pattern.value));
            }
        }
        hits
    }

    /// Where a stream may be cut: the start of the earliest tail of `text` that is
    /// a proper prefix of a whole form (raw, or case-free in any case) at any
    /// decoding level, or an encoded sequence still being written, moved back
    /// before any span that would straddle the cut. Text before it can be
    /// redacted and written now; the rest is held until the stream continues,
    /// so no piece of a secret is written before the stream shows whether the
    /// rest follows. `text.len()` when nothing needs holding.
    ///
    /// The held tail is exactly the matcher's live prefix: what the automaton
    /// would still be in the middle of matching, so it is never longer than the
    /// longest form (in its encoded spelling) and separators between pieces do
    /// not end it.
    pub fn hold_from(&self, text: &str) -> usize {
        self.hold_with(text, &[], true)
    }

    fn hold_with(&self, text: &str, starts: &[usize], fragments: bool) -> usize {
        self.hold_and_spans(text, starts, fragments, None).0
    }

    /// [`Self::hold_from`] for a text of pieces, with the spans of
    /// [`Self::piece_spans`] found on the way: everything a stream needs from
    /// one search.
    fn hold_and_spans(
        &self,
        text: &str,
        starts: &[usize],
        fragments: bool,
        exempt: Option<&HashSet<String>>,
    ) -> (usize, Vec<Span>) {
        if self.is_broken() {
            // Everything is withheld anyway; holding would only grow buffers.
            return (text.len(), withheld_span(text.len()));
        }
        let mut cut = self.plain_hold(text);
        let mut hits = self.plain_hits(text, fragments, starts);
        if crate::secret_normalize::has_encoding(text) {
            for level in crate::secret_normalize::levels(text) {
                let len = level.text.len();
                let at = self.plain_hold(&level.text);
                if at < len {
                    cut = cut.min(crate::secret_normalize::source_range(&level, at, len).0);
                }
                hits.extend(self.level_hits(&level, fragments, starts));
            }
            // An encoded sequence still being written may decode to anything,
            // so it is held, and so is a live prefix that ends just before it
            // (the sequence may continue it).
            if let Some(at) = crate::secret_normalize::incomplete_suffix_start(text) {
                cut = cut.min(at).min(self.decided_hold(&text[..at]));
            }
        }
        if starts.len() > 1 {
            hits.retain(|h| !h.3 || !crosses(starts, h.0, h.1));
        }
        let spans = self.merge_exempt(hits, exempt);
        // A span crossing the cut would be written in part.
        while let Some((s, _, _)) = spans.iter().find(|(s, e, _)| *s < cut && cut < *e) {
            cut = *s;
        }
        (cut, spans)
    }

    /// The live-prefix cut of `text` at every decoding level.
    fn decided_hold(&self, text: &str) -> usize {
        let mut cut = self.plain_hold(text);
        for level in crate::secret_normalize::levels(text) {
            let len = level.text.len();
            let at = self.plain_hold(&level.text);
            if at < len {
                cut = cut.min(crate::secret_normalize::source_range(&level, at, len).0);
            }
        }
        cut
    }

    /// Dangling edges ([`Self::own_edges`]) of `text` for every matcher, at
    /// every decoding level, in source offsets.
    fn edges(&self, text: &str) -> Vec<Hit> {
        let mut hits = self.own_edges(text);
        hits.extend(self.extra_hits(|m| m.own_edges(text)));
        for level in crate::secret_normalize::levels(text) {
            let mut more = self.own_edges(&level.text);
            more.extend(self.extra_hits(|m| m.own_edges(&level.text)));
            for (s, e, name, fragment, value) in more {
                let (s, e) = crate::secret_normalize::source_range(&level, s, e);
                hits.push((s, e, name, fragment, value));
            }
        }
        hits
    }

    fn plain_hold(&self, text: &str) -> usize {
        let cut = self.own_hold(text);
        self.extra
            .as_ref()
            .map_or(cut, |m| cut.min(m.own_hold(text)))
    }

    fn own_hold(&self, text: &str) -> usize {
        if self.prefix_reach == 0 {
            return text.len();
        }
        let mut from = text.len().saturating_sub(self.prefix_reach);
        while !text.is_char_boundary(from) {
            from += 1;
        }
        let live = |list: &[String], tail: &str| {
            let k = list.partition_point(|p| p.as_str() < tail);
            list[k..]
                .iter()
                .take_while(|p| p.starts_with(tail))
                .any(|p| p.len() > tail.len())
        };
        // ASCII lowercasing keeps every byte offset: lower the window once
        // and slice it, rather than copy each tail.
        let lowered = if self.ci_prefixes.is_empty() {
            String::new()
        } else {
            text[from..].to_ascii_lowercase()
        };
        for (i, _) in text[from..].char_indices() {
            let tail = &text[from + i..];
            if live(&self.prefixes, tail) {
                return from + i;
            }
            if !self.ci_prefixes.is_empty() && live(&self.ci_prefixes, &lowered[i..]) {
                return from + i;
            }
        }
        text.len()
    }

    /// Replace every whole occurrence of a secret or one of its encodings.
    pub fn redact_text<'a>(&self, text: &'a str) -> Cow<'a, str> {
        let spans = self.spans(text);
        if spans.is_empty() {
            Cow::Borrowed(text)
        } else {
            Cow::Owned(self.apply(text, &spans))
        }
    }

    /// Like [`Self::redact_text`], for text that may itself be a cut of something
    /// longer (an excerpt window, an already-truncated message): also removes a
    /// trailing prefix or a leading suffix of a secret of at least
    /// [`SHORT_SECRET_CHARS`] chars, the part a reader could otherwise complete.
    pub fn redact_fragment<'a>(&self, text: &'a str) -> Cow<'a, str> {
        if self.is_broken() {
            return if text.is_empty() {
                Cow::Borrowed(text)
            } else {
                Cow::Owned(WITHHELD.to_string())
            };
        }
        let mut spans = self.spans(text);
        let edge = |spans: &mut Vec<(usize, usize, Vec<usize>)>, start: usize, end: usize, n| {
            if start < end && !spans.iter().any(|(s, e, _)| *s <= start && end <= *e) {
                spans.push((start, end, vec![n]));
            }
        };
        for (start, end, name, _, _) in self
            .own_edges(text)
            .into_iter()
            .chain(self.extra_hits(|m| m.own_edges(text)))
        {
            edge(&mut spans, start, end, name);
        }
        if spans.is_empty() {
            return Cow::Borrowed(text);
        }
        let hits = spans
            .into_iter()
            .flat_map(|(s, e, names)| names.into_iter().map(move |n| (s, e, n, false, 0)))
            .collect();
        let spans = merge(hits);
        Cow::Owned(self.apply(text, &spans))
    }

    /// This matcher's dangling prefix of a secret at the end of `text` and
    /// dangling suffix at its start, as hits.
    fn own_edges(&self, text: &str) -> Vec<Hit> {
        let mut out = Vec::new();
        // Only the ends of `text` matter, so only the last (first) stretch as long
        // as the longest pattern is searched for pattern heads (tails).
        let reach = self.longest_chars * 4;
        if let Some((ac, owners)) = &self.heads {
            let mut from = text.len().saturating_sub(reach);
            while !text.is_char_boundary(from) {
                from += 1;
            }
            // Longest dangling prefix: the earliest head whose pattern continues
            // past the end of `text`.
            let found = ac.find_overlapping_iter(&text[from..]).find_map(|m| {
                let start = from + m.start();
                let rest = &text[start..];
                owners[m.pattern().as_usize()].iter().find_map(|&p| {
                    let (pattern, meta) = &self.raw_patterns[p];
                    (pattern.len() > rest.len() && pattern.starts_with(rest))
                        .then_some((start, meta.name, meta.value))
                })
            });
            if let Some((start, name, value)) = found {
                out.push((start, text.len(), name, false, value));
            }
        }
        if let Some((ac, owners)) = &self.tails {
            let mut to = text.len().min(reach);
            while !text.is_char_boundary(to) {
                to -= 1;
            }
            // Longest dangling suffix: the last tail whose pattern began before
            // the start of `text`.
            let found = ac
                .find_overlapping_iter(&text[..to])
                .filter_map(|m| {
                    let end = m.end();
                    let head = &text[..end];
                    owners[m.pattern().as_usize()].iter().find_map(|&p| {
                        let (pattern, meta) = &self.raw_patterns[p];
                        (pattern.len() > head.len() && pattern.ends_with(head))
                            .then_some((end, meta.name, meta.value))
                    })
                })
                .max_by_key(|(end, _, _)| *end);
            if let Some((end, name, value)) = found {
                out.push((0, end, name, false, value));
            }
        }
        out
    }

    /// Redact then cut to `max_chars`, so a cut can never leave part of a secret.
    /// Scans only the window that can reach the kept prefix, so a multi-megabyte
    /// error costs no more than a short one.
    pub fn truncate_chars(&self, text: &str, max_chars: usize) -> String {
        let window_chars = max_chars.saturating_add(self.longest());
        let window = match text.char_indices().nth(window_chars) {
            Some((byte, _)) => &text[..byte],
            None => text,
        };
        // The window end may split a secret that starts past `max_chars`; that part
        // is cut away below, and any secret starting earlier ends inside the window.
        // Fragment rules, because the text may itself be a cut an upstream made: a
        // leading suffix or trailing prefix of a secret goes too.
        let redacted = self.redact_fragment(window);
        redacted.chars().take(max_chars).collect()
    }

    /// Redact every string (and object key) in a JSON value. True when anything changed.
    pub fn redact_value(&self, value: &mut Value) -> bool {
        self.value_with(value, true, None)
    }

    /// Text with secrets replaced, or `None` when nothing matched. `fragments`
    /// includes [`fragment_windows`] hits; a span made only of whole forms of
    /// `exempt` values is left as it is.
    fn text_with(
        &self,
        text: &str,
        fragments: bool,
        exempt: Option<&HashSet<String>>,
    ) -> Option<String> {
        let spans = self.piece_spans(text, &[], fragments, exempt);
        (!spans.is_empty()).then(|| self.apply(text, &spans))
    }

    /// [`Self::redact_value`] with the [`Self::text_with`] options: the string
    /// leaves are searched as pieces of one text ([`Self::redact_across_leaves`]),
    /// then numbers and object keys one by one.
    fn value_with(
        &self,
        value: &mut Value,
        fragments: bool,
        exempt: Option<&HashSet<String>>,
    ) -> bool {
        let leaves = self.redact_across_leaves(value, fragments, exempt);
        self.scalars_with(value, exempt) | leaves
    }

    /// Numbers that spell a registered value, and object keys holding one.
    fn scalars_with(&self, value: &mut Value, exempt: Option<&HashSet<String>>) -> bool {
        match value {
            // A number whose text is a registered value (a numeric PIN or account
            // number) becomes the marker string. Booleans and null carry no value.
            Value::Number(n) => match self
                .number_entry(n)
                .filter(|(_, v)| !exempt.is_some_and(|x| x.contains(self.value(*v))))
            {
                Some((name, _)) => {
                    *value = Value::String(self.marker(&[name]));
                    true
                }
                None => false,
            },
            Value::Array(items) => items.iter_mut().fold(false, |changed, item| {
                self.scalars_with(item, exempt) | changed
            }),
            Value::Object(map) => {
                let mut changed = false;
                for v in map.values_mut() {
                    changed |= self.scalars_with(v, exempt);
                }
                changed | self.redact_keys(map, exempt)
            }
            _ => false,
        }
    }

    /// Every string leaf of one value, whatever the nesting, searched as the
    /// pieces of one logical text ([`Self::piece_spans`]), so a secret split
    /// over any number of leaves of any sizes (`["sk-","AbC","123","xyZ"]`) is
    /// found like one written whole. Leaves are taken in document order, and
    /// object keys are in sorted order in a parsed value, so other fields can
    /// sit between two pieces (`"type":"text"` in every MCP content item): the
    /// leaves under each key name (array elements share one name) are also
    /// searched as their own text whenever another leaf sits between two of
    /// them. A span puts its marker where it starts and removes the rest of
    /// itself from every later leaf it reaches. Numbers, booleans and null are
    /// not text and are skipped.
    fn redact_across_leaves(
        &self,
        value: &mut Value,
        fragments: bool,
        exempt: Option<&HashSet<String>>,
    ) -> bool {
        let mut leaves = Vec::new();
        collect_leaves(value, "", &mut leaves);
        self.redact_leaf_streams(&mut leaves, fragments, exempt)
    }

    /// [`Self::redact_across_leaves`] over leaves already collected with their
    /// class (nearest key name, `[]` for array elements).
    fn redact_leaf_streams(
        &self,
        leaves: &mut [(String, &mut String)],
        fragments: bool,
        exempt: Option<&HashSet<String>>,
    ) -> bool {
        self.leaf_streams_found(leaves, fragments, exempt, &mut Vec::new())
    }

    /// [`Self::redact_leaf_streams`], adding the names it replaced to `found`.
    fn leaf_streams_found(
        &self,
        leaves: &mut [(String, &mut String)],
        fragments: bool,
        exempt: Option<&HashSet<String>>,
        found: &mut Vec<usize>,
    ) -> bool {
        if leaves.iter().all(|(_, l)| l.is_empty()) {
            return false;
        }
        let all: Vec<usize> = (0..leaves.len()).collect();
        let mut changed = self.redact_members(leaves, &all, fragments, exempt, false, found);
        let groups = apart_groups(leaves.iter().map(|(c, l)| (c.as_str(), !l.is_empty())));
        for members in groups {
            changed |= self.redact_members(leaves, &members, false, exempt, true, found);
        }
        changed
    }

    /// One stream of [`Self::redact_leaf_streams`]: the leaves at `members`
    /// joined and searched as pieces; with `crossing`, only spans that cross a
    /// boundary are applied (the rest were found by the stream of all leaves).
    fn redact_members(
        &self,
        leaves: &mut [(String, &mut String)],
        members: &[usize],
        fragments: bool,
        exempt: Option<&HashSet<String>>,
        crossing: bool,
        found: &mut Vec<usize>,
    ) -> bool {
        let mut joined = String::with_capacity(members.iter().map(|&i| leaves[i].1.len()).sum());
        let mut starts = Vec::with_capacity(members.len());
        for &i in members {
            starts.push(joined.len());
            joined.push_str(leaves[i].1);
        }
        let mut spans = self.piece_spans(&joined, &starts, fragments, exempt);
        if crossing {
            spans.retain(|(s, e, _)| crosses(&starts, *s, *e));
        }
        if spans.is_empty() {
            return false;
        }
        for (_, _, names) in &spans {
            found.extend(names);
        }
        for (k, &i) in members.iter().enumerate() {
            let leaf = &mut leaves[i].1;
            let (ls, le) = (starts[k], starts[k] + leaf.len());
            let mut out = String::new();
            let mut at = ls;
            let mut touched = false;
            // Spans are sorted and disjoint: only those from the first that
            // ends past this leaf's start, until one starts past its end.
            let first = spans.partition_point(|(_, e, _)| *e <= ls);
            for (s, e, names) in spans[first..].iter().take_while(|(s, _, _)| *s < le) {
                touched = true;
                if *s >= ls {
                    out.push_str(&joined[at..*s]);
                    out.push_str(&self.marker(names));
                }
                at = (*e).min(le);
            }
            if touched {
                out.push_str(&joined[at..le]);
                **leaf = out;
            }
        }
        true
    }

    /// The registered values a client's request holds (its strings, searched
    /// as pieces at every decoding level as [`Self::redact_across_leaves`]
    /// does, its numbers and keys): every value one of whose whole forms is in
    /// it, in any spelling. The identity of an exemption is the registered
    /// value, never its display name or the spelling the client used, so two
    /// secrets under one key name never exempt each other, a request that
    /// sends a value encoded exempts it, and an echo of it in another spelling
    /// is exempt too.
    fn values_in(&self, value: &Value, out: &mut HashSet<String>) {
        let mut leaves: Vec<(String, String)> = Vec::new();
        fn strings(value: &Value, class: &str, out: &mut Vec<(String, String)>) {
            match value {
                Value::String(s) => out.push((class.to_string(), s.clone())),
                Value::Array(items) => items.iter().for_each(|v| strings(v, "[]", out)),
                Value::Object(map) => map.iter().for_each(|(k, v)| {
                    out.push(("\u{0}key".to_string(), k.clone()));
                    strings(v, k, out)
                }),
                _ => {}
            }
        }
        strings(value, "", &mut leaves);
        let mut search = |members: Vec<&str>| {
            let mut joined = String::new();
            let mut starts = Vec::new();
            for m in members {
                starts.push(joined.len());
                joined.push_str(m);
            }
            let mut hits = self.plain_hits(&joined, false, &starts);
            for level in crate::secret_normalize::levels(&joined) {
                hits.extend(self.level_hits(&level, false, &starts));
            }
            // Only a value that is a whole span: never one inside a longer
            // secret the client sent.
            hits.sort_by_key(|h| (h.0, std::cmp::Reverse(h.1)));
            let mut i = 0;
            while i < hits.len() {
                let (gs, mut ge) = (hits[i].0, hits[i].1);
                let mut j = i + 1;
                while j < hits.len() && hits[j].0 < ge {
                    ge = ge.max(hits[j].1);
                    j += 1;
                }
                for h in &hits[i..j] {
                    let value = self.value(h.4);
                    if h.0 == gs && h.1 == ge && spells_whole(&joined[gs..ge], value) {
                        out.insert(value.to_string());
                    }
                }
                i = j;
            }
        };
        search(leaves.iter().map(|(_, s)| s.as_str()).collect());
        for members in apart_groups(leaves.iter().map(|(c, s)| (c.as_str(), !s.is_empty()))) {
            search(members.iter().map(|&i| leaves[i].1.as_str()).collect());
        }
        fn numbers(m: &Matcher, value: &Value, out: &mut HashSet<String>) {
            match value {
                Value::Number(n) => {
                    if let Some((_, v)) = m.exact_entry(&n.to_string()) {
                        out.insert(m.value(v).to_string());
                    }
                }
                Value::Array(items) => items.iter().for_each(|v| numbers(m, v, out)),
                Value::Object(map) => map.values().for_each(|v| numbers(m, v, out)),
                _ => {}
            }
        }
        numbers(self, value, out);
    }

    /// An identifier (a tool or prompt name, a resource URI or template, an
    /// argument name) with whole secret forms replaced by aliased markers
    /// (`[secret:NAME~3f9a0c12d4e5b6a7]`), recorded in `ids` so a call can name
    /// the original. `None` when it holds no secret.
    fn screen_identifier(&self, text: &str, part: Part, ids: &mut IdBuilder) -> Option<String> {
        let spans = self.spans_with(text, false);
        if spans.is_empty() {
            return None;
        }
        let mut out = String::with_capacity(text.len());
        let mut at = 0;
        let mut pieces = Vec::new();
        for (i, (start, end, names)) in spans.iter().enumerate() {
            out.push_str(&text[at..*start]);
            ids.found
                .extend(names.iter().map(|&n| self.name(n).to_string()));
            let listed = self.marker(names);
            let base = &listed[..listed.len() - 1];
            let marker = ids.alias(part, text, i, base)?;
            pieces.push((marker.clone(), text[*start..*end].to_string()));
            out.push_str(&marker);
            at = *end;
        }
        out.push_str(&text[at..]);
        ids.record(part, &out, text, pieces);
        Some(out)
    }

    /// Text that is not an identifier, alone (an object key outside a schema):
    /// whole forms become plain markers and nothing is recorded.
    fn screen_plain(&self, text: &str, ids: &mut IdBuilder) -> Option<String> {
        let spans = self.spans_with(text, false);
        for (_, _, names) in &spans {
            ids.found
                .extend(names.iter().map(|&n| self.name(n).to_string()));
        }
        (!spans.is_empty()).then(|| self.apply(text, &spans))
    }

    /// Screen one advertised definition (tool, prompt, resource or template) by
    /// whole forms only. Fragment windows are for error text; in a definition they
    /// would rename ordinary identifiers (`Tools-Read` is a piece of
    /// `sk-Tools-ReadSecret123`), change pins and break calls. Identifier fields
    /// get stable aliases ([`Self::screen_identifier`]), one at a time. Every
    /// other string (descriptions, titles, schema text) is searched as the
    /// pieces of one text, exactly as a result is ([`Self::redact_leaf_streams`]),
    /// so a secret split over several of them is found, and gets plain markers.
    fn screen_definition(&self, item: &mut Value, ids: &mut IdBuilder) {
        let catalog = ids.catalog;
        let role_of = |key: &str| match (catalog, key) {
            (Catalog::Tool | Catalog::Prompt, "name") => Role::Name,
            (Catalog::Resource, "uri") | (Catalog::Template, "uriTemplate") => Role::Name,
            (Catalog::Tool, "inputSchema") => Role::Schema,
            (Catalog::Prompt, "arguments") => Role::PromptArguments,
            _ => Role::Text,
        };
        // Text first, while every leaf is still where the server put it.
        let mut texts = Vec::new();
        match &mut *item {
            Value::Object(map) => {
                for (key, v) in map.iter_mut() {
                    text_leaves(v, role_of(key), key, &mut texts);
                }
            }
            other => text_leaves(other, Role::Text, "", &mut texts),
        }
        let mut found = Vec::new();
        self.leaf_streams_found(&mut texts, false, None, &mut found);
        drop(texts);
        ids.found
            .extend(found.into_iter().map(|n| self.name(n).to_string()));
        match item {
            Value::Object(map) => {
                for (key, v) in map.iter_mut() {
                    self.screen_in(v, role_of(key), ids);
                }
                self.screen_keys(map, Role::Text, ids);
            }
            other => {
                self.screen_in(other, Role::Text, ids);
            }
        }
    }

    /// Identifiers, numbers and object keys of a definition (its other strings
    /// are screened as one text by [`Self::screen_definition`]).
    fn screen_in(&self, value: &mut Value, role: Role, ids: &mut IdBuilder) {
        match value {
            Value::String(s) => {
                let new = match role {
                    Role::Name => self.screen_identifier(s, Part::Name, ids),
                    Role::Argument => self.screen_identifier(s, Part::Argument, ids),
                    _ => None,
                };
                if let Some(new) = new {
                    *s = new;
                }
            }
            Value::Number(n) => {
                if let Some(name) = self.number_secret(n) {
                    ids.found.push(self.name(name).to_string());
                    *value = Value::String(self.marker(&[name]));
                }
            }
            Value::Array(items) => {
                let inner = inner_role(role, None);
                for item in items.iter_mut() {
                    self.screen_in(item, inner, ids);
                }
            }
            Value::Object(map) => {
                for (key, v) in map.iter_mut() {
                    self.screen_in(v, inner_role(role, Some(key)), ids);
                }
                self.screen_keys(map, role, ids);
            }
            _ => {}
        }
    }

    /// Object keys of a definition: in an input schema they are argument names
    /// (aliased); anywhere else plain markers, made unique with `#2`, `#3`.
    fn screen_keys(
        &self,
        map: &mut serde_json::Map<String, Value>,
        role: Role,
        ids: &mut IdBuilder,
    ) {
        let renamed: Vec<(String, String)> = map
            .keys()
            .filter_map(|key| {
                let new = if role == Role::Schema {
                    self.screen_identifier(key, Part::Argument, ids)
                } else {
                    self.screen_plain(key, ids)
                };
                new.map(|new| (key.clone(), new))
            })
            .collect();
        for (old, new) in renamed {
            let Some(v) = map.remove(&old) else { continue };
            let mut key = new.clone();
            let mut n = 2;
            while map.contains_key(&key) {
                key = format!("{new}#{n}");
                n += 1;
            }
            map.insert(key, v);
        }
    }

    /// The secret a JSON number spells, by its text or, for an integral value
    /// written another way (`482193.0`, `4.82193e5`), by its integer text.
    fn number_secret(&self, n: &serde_json::Number) -> Option<usize> {
        self.number_entry(n).map(|(name, _)| name)
    }

    /// [`Self::number_secret`] as `(name, value)`.
    fn number_entry(&self, n: &serde_json::Number) -> Option<(usize, usize)> {
        if !self.has_exact() {
            return None;
        }
        let text = n.to_string();
        if let Some(hit) = self.numeric_entry(&text) {
            return Some(hit);
        }
        let f = n.as_f64()?;
        (f.fract() == 0.0 && f.abs() < 9.0e15)
            .then(|| self.numeric_entry(&format!("{}", f as i64)))
            .flatten()
    }

    /// A registered value spelled by a JSON number's text, if it has at least
    /// [`MIN_NUMERIC_SECRET_DIGITS`] digits. A shorter one (a port, a small id)
    /// would turn ordinary numbers into strings and change the types a model
    /// reads; those are named in status instead, and still redacted in text.
    fn numeric_secret(&self, text: &str) -> Option<usize> {
        self.numeric_entry(text).map(|(name, _)| name)
    }

    fn numeric_entry(&self, text: &str) -> Option<(usize, usize)> {
        (text.bytes().filter(u8::is_ascii_digit).count() >= MIN_NUMERIC_SECRET_DIGITS)
            .then(|| self.exact_entry(text))
            .flatten()
    }

    /// Replace object keys that contain a whole secret form (never a fragment: a
    /// key is an identifier). Two keys that become the same text are kept apart
    /// with `#2`, `#3`, so no value is overwritten and the result stays a valid,
    /// unambiguous object.
    fn redact_keys(
        &self,
        map: &mut serde_json::Map<String, Value>,
        exempt: Option<&HashSet<String>>,
    ) -> bool {
        let renamed: Vec<(String, String)> = map
            .keys()
            .filter_map(|key| {
                self.text_with(key, false, exempt)
                    .map(|new| (key.clone(), new))
            })
            .collect();
        if renamed.is_empty() {
            return false;
        }
        for (old, new) in renamed {
            let Some(v) = map.remove(&old) else { continue };
            let mut key = new.clone();
            let mut n = 2;
            while map.contains_key(&key) {
                key = format!("{new}#{n}");
                n += 1;
            }
            map.insert(key, v);
        }
        true
    }

    /// Rewrite number tokens in serialized JSON whose source text is a registered
    /// value as the quoted marker, before parsing loses that text (`1.50`, `0042`
    /// as written). Strings are skipped; the result is still valid JSON.
    fn redact_number_tokens(&self, body: &str) -> Option<String> {
        if !self.has_exact() {
            return None;
        }
        let bytes = body.as_bytes();
        let mut out = String::new();
        let mut at = 0;
        let mut i = 0;
        let mut in_string = false;
        while i < bytes.len() {
            let b = bytes[i];
            if in_string {
                match b {
                    b'\\' => i += 1,
                    b'"' => in_string = false,
                    _ => {}
                }
                i += 1;
                continue;
            }
            if b == b'"' {
                in_string = true;
                i += 1;
                continue;
            }
            if b == b'-' || b.is_ascii_digit() {
                let start = i;
                while i < bytes.len()
                    && matches!(bytes[i], b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')
                {
                    i += 1;
                }
                let token = &body[start..i];
                let name = self.numeric_secret(token).or_else(|| {
                    serde_json::from_str::<serde_json::Number>(token)
                        .ok()
                        .and_then(|n| self.number_secret(&n))
                });
                if let Some(name) = name {
                    out.push_str(&body[at..start]);
                    out.push_str(&serde_json::to_string(&self.marker(&[name])).ok()?);
                    at = i;
                }
                continue;
            }
            i += 1;
        }
        (at > 0).then(|| {
            out.push_str(&body[at..]);
            out
        })
    }

    /// Redact one persisted line. A JSON line is parsed and its strings redacted, so
    /// the result stays valid JSON and numbers are never touched; any other line is
    /// redacted as text. Unchanged lines come back borrowed.
    pub fn redact_line<'a>(&self, line: &'a str) -> Cow<'a, str> {
        self.line_with(line, true)
    }

    /// [`Self::redact_line`] with whole forms only, for serialized definitions
    /// (the tool cache), which [`screen_definitions`] screened the same way.
    pub fn redact_definition_line<'a>(&self, line: &'a str) -> Cow<'a, str> {
        self.line_with(line, false)
    }

    fn line_with<'a>(&self, line: &'a str, fragments: bool) -> Cow<'a, str> {
        if self.is_empty() {
            return Cow::Borrowed(line);
        }
        let body = line.trim_end_matches(['\n', '\r']);
        let ending = &line[body.len()..];
        if body.trim_start().starts_with(['{', '[']) {
            if let Some(tokens) = json_tokens(body) {
                if self.is_broken() {
                    return Cow::Owned(format!("{}{ending}", withheld_json(body, &tokens)));
                }
                match self.json_edits(body, &tokens, fragments) {
                    None => {
                        // An object key holds a secret: renaming keys needs the
                        // parsed object, to keep two of them apart.
                        if let Ok(mut value) = serde_json::from_str::<Value>(body) {
                            if let Some(n) = self.redact_number_tokens(body) {
                                if let Ok(v) = serde_json::from_str::<Value>(&n) {
                                    value = v;
                                }
                            }
                            self.value_with(&mut value, fragments, None);
                            return Cow::Owned(format!("{value}{ending}"));
                        }
                    }
                    Some(edits) if edits.is_empty() => return Cow::Borrowed(line),
                    Some(edits) => {
                        return Cow::Owned(format!("{}{ending}", apply_edits(body, edits)));
                    }
                }
            }
        }
        if self.is_broken() {
            return Cow::Owned(format!("{WITHHELD}{ending}"));
        }
        // Text, or a torn JSON line: one text, searched at every decoding level
        // (a JSON escape is one).
        match self.text_with(line, fragments, None) {
            Some(new) => Cow::Owned(new),
            None => Cow::Borrowed(line),
        }
    }

    /// The edits that redact a serialized JSON text in place: its string
    /// values searched as the pieces of one text (and per key name, as
    /// [`Self::redact_leaf_streams`] does for a parsed value), each in its
    /// escaped spelling, which the first decoding level unescapes; its number
    /// tokens by their source text. `None` when an object key holds a secret.
    /// Every byte outside an edit is kept, so the line keeps its format.
    fn json_edits(
        &self,
        body: &str,
        t: &JsonTokens<'_>,
        fragments: bool,
    ) -> Option<Vec<(usize, usize, String)>> {
        let mut edits: Vec<(usize, usize, String)> = Vec::new();
        if self.has_exact() {
            for &(s, e) in &t.numbers {
                let token = &body[s..e];
                let name = self.numeric_secret(token).or_else(|| {
                    serde_json::from_str::<serde_json::Number>(token)
                        .ok()
                        .and_then(|n| self.number_secret(&n))
                });
                if let Some(name) = name {
                    let marker = serde_json::to_string(&self.marker(&[name])).ok()?;
                    edits.push((s, e, marker));
                }
            }
        }
        // Each key on its own: a key is one identifier, never a piece of a
        // value, and a hit in it must not merge with one in a value.
        if t.keys
            .iter()
            .any(|&(s, e)| !self.spans_with(&body[s..e], false).is_empty())
        {
            return None;
        }
        let ranges: Vec<(usize, usize)> = t.values.iter().map(|(s, e, _)| (*s, *e)).collect();
        if ranges.is_empty() {
            return Some(edits);
        }
        let (joined, starts) = join_ranges(body, &ranges);
        let spans = self.piece_spans(&joined, &starts, fragments, None);
        self.spans_to_edits(body, &ranges, &starts, spans, &mut edits);
        let ranges = &ranges[..];
        for members in apart_groups(t.values.iter().map(|(s, e, c)| (*c, s < e))) {
            self.json_stream_edits(body, ranges, &members, false, true, &mut edits);
        }
        Some(edits)
    }

    /// One stream of [`Self::json_edits`]: the value contents at `members`.
    fn json_stream_edits(
        &self,
        body: &str,
        ranges: &[(usize, usize)],
        members: &[usize],
        fragments: bool,
        crossing: bool,
        edits: &mut Vec<(usize, usize, String)>,
    ) {
        let picked: Vec<(usize, usize)> = members.iter().map(|&i| ranges[i]).collect();
        let (joined, starts) = join_ranges(body, &picked);
        let mut spans = self.piece_spans(&joined, &starts, fragments, None);
        if crossing {
            spans.retain(|(s, e, _)| crosses(&starts, *s, *e));
        }
        self.spans_to_edits(body, &picked, &starts, spans, edits);
    }

    /// Spans of the joined `picked` ranges of `body` (starting at `starts`) as
    /// edits of `body`: the marker where a span starts, the rest of it removed
    /// from every later range it reaches, never splitting an escape.
    fn spans_to_edits(
        &self,
        body: &str,
        picked: &[(usize, usize)],
        starts: &[usize],
        spans: Vec<Span>,
        edits: &mut Vec<(usize, usize, String)>,
    ) {
        for (s, e, names) in spans {
            let marker = json_escaped(&self.marker(&names));
            // Pieces are in order: from the one holding the span's start, until
            // one starts at or past its end.
            let first = starts.partition_point(|&st| st <= s).saturating_sub(1);
            for (k, &(ps, pe)) in picked.iter().enumerate().skip(first) {
                let (ls, le) = (starts[k], starts[k] + (pe - ps));
                if ls >= e {
                    break;
                }
                if s >= le || e <= ls || (ls == le) {
                    continue;
                }
                let content = &body[ps..pe];
                let (a, b) = escape_bounds(content, s.max(ls) - ls, e.min(le) - ls);
                let text = if s >= ls {
                    marker.clone()
                } else {
                    String::new()
                };
                edits.push((ps + a, ps + b, text));
            }
        }
    }

    /// The fast reject for one text: false only when no form, plain or
    /// encoded at any level, can be in `text`. Not sound for a value split over
    /// several texts; callers with pieces search them joined instead.
    pub fn may_contain(&self, text: &str) -> bool {
        self.is_broken()
            || self.may_contain_plain(text)
            || crate::secret_normalize::levels(text)
                .iter()
                .any(|level| self.may_contain_plain(&level.text))
    }

    fn may_contain_plain(&self, text: &str) -> bool {
        self.raw.as_ref().is_some_and(|a| a.is_match(text))
            || self.ci.as_ref().is_some_and(|a| a.is_match(text))
            || self
                .extra
                .as_ref()
                .is_some_and(|m| m.may_contain_plain(text))
    }
}

/// The string, key and number tokens of one serialized JSON text.
struct JsonTokens<'a> {
    /// `(start, end, class)`: the content between the quotes of each string
    /// value, with its class (the key it is under, `[]` in an array).
    values: Vec<(usize, usize, &'a str)>,
    keys: Vec<(usize, usize)>,
    numbers: Vec<(usize, usize)>,
}

/// [`JsonTokens`] of `body`, or `None` when it is not one JSON text (a torn
/// line, or a text line that starts with a bracket), which is then redacted
/// as text: nothing outside its strings is searched, so anything else between
/// them would never be.
fn json_tokens(body: &str) -> Option<JsonTokens<'_>> {
    let b = body.as_bytes();
    let mut out = JsonTokens {
        values: Vec::new(),
        keys: Vec::new(),
        numbers: Vec::new(),
    };
    // (is an object, its current key)
    let mut stack: Vec<(bool, &str)> = Vec::new();
    let mut expect_key = false;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'{' => {
                stack.push((true, ""));
                expect_key = true;
                i += 1;
            }
            b'[' => {
                stack.push((false, "[]"));
                expect_key = false;
                i += 1;
            }
            b'}' | b']' => {
                let (object, _) = stack.pop()?;
                if object != (b[i] == b'}') {
                    return None;
                }
                expect_key = false;
                i += 1;
            }
            b',' => {
                expect_key = matches!(stack.last(), Some((true, _)));
                i += 1;
            }
            b'"' => {
                let start = i + 1;
                let mut j = start;
                loop {
                    j += memchr::memchr2(b'"', b'\\', &b[j..])?;
                    if b[j] == b'\\' {
                        j += 2;
                        if j > b.len() {
                            return None;
                        }
                    } else {
                        break;
                    }
                }
                if expect_key {
                    out.keys.push((start, j));
                    stack.last_mut()?.1 = &body[start..j];
                    expect_key = false;
                } else {
                    let class = stack.last().map_or("", |(_, k)| *k);
                    out.values.push((start, j, class));
                }
                i = j + 1;
            }
            b'-' | b'0'..=b'9' => {
                let start = i;
                while i < b.len() && matches!(b[i], b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')
                {
                    i += 1;
                }
                out.numbers.push((start, i));
            }
            b' ' | b'\t' | b'\n' | b'\r' | b':' => i += 1,
            _ => {
                let literal = ["true", "false", "null"]
                    .iter()
                    .find(|word| b[i..].starts_with(word.as_bytes()))?;
                i += literal.len();
            }
        }
    }
    // The bytes above are only tokens; the whole must also be valid JSON.
    let valid = serde_json::from_str::<serde::de::IgnoredAny>(body).is_ok();
    (stack.is_empty() && valid).then_some(out)
}

/// `ranges` of `body` joined, with each one's start in the joined text.
fn join_ranges(body: &str, ranges: &[(usize, usize)]) -> (String, Vec<usize>) {
    let mut joined = String::with_capacity(ranges.iter().map(|(s, e)| e - s).sum());
    let mut starts = Vec::with_capacity(ranges.len());
    for &(s, e) in ranges {
        starts.push(joined.len());
        joined.push_str(&body[s..e]);
    }
    (joined, starts)
}

/// `s..e` of a JSON string's escaped content widened so it never splits an
/// escape sequence (`\n`, `\u00e9`): an edit then always leaves valid JSON.
fn escape_bounds(content: &str, mut s: usize, mut e: usize) -> (usize, usize) {
    let b = content.as_bytes();
    let mut i = 0;
    while i < b.len() && i < e {
        if b[i] == b'\\' {
            let len = if b.get(i + 1) == Some(&b'u') { 6 } else { 2 };
            let end = (i + len).min(b.len());
            if i < s && s < end {
                s = i;
            }
            if i < e && e < end {
                e = end;
            }
            i = end;
        } else {
            i += 1;
        }
    }
    (s, e)
}

/// `body` with `edits` (`(start, end, replacement)`, possibly overlapping)
/// applied; overlapping edits become one, keeping each replacement.
fn apply_edits(body: &str, mut edits: Vec<(usize, usize, String)>) -> String {
    edits.sort_by_key(|(s, e, _)| (*s, std::cmp::Reverse(*e)));
    let mut merged: Vec<(usize, usize, String)> = Vec::new();
    for (s, e, text) in edits {
        match merged.last_mut() {
            Some(last) if s < last.1 => {
                last.1 = last.1.max(e);
                if !text.is_empty() && !last.2.contains(&text) {
                    last.2.push_str(&text);
                }
            }
            _ => merged.push((s, e, text)),
        }
    }
    let mut out = String::with_capacity(body.len());
    let mut at = 0;
    for (s, e, text) in merged {
        out.push_str(&body[at..s]);
        out.push_str(&text);
        at = e;
    }
    out.push_str(&body[at..]);
    out
}

/// A serialized JSON text with nothing of its content left, for a matcher
/// that could not be built: every string value is [`WITHHELD`], every number
/// `null`, and a key is kept only if it is a plain identifier
/// (`[A-Za-z0-9_]{1,32}`), so the line stays valid JSON with its structure.
fn withheld_json(body: &str, t: &JsonTokens<'_>) -> String {
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    for &(s, e, _) in &t.values {
        edits.push((s, e, WITHHELD.to_string()));
    }
    for &(s, e) in &t.numbers {
        edits.push((s, e, "null".to_string()));
    }
    for &(s, e) in &t.keys {
        let key = &body[s..e];
        let plain = (1..=32).contains(&key.len())
            && key.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_');
        if !plain {
            edits.push((s, e, "withheld".to_string()));
        }
    }
    apply_edits(body, edits)
}

/// The role of a definition's child under `key` (`None`: an array element).
fn inner_role(role: Role, key: Option<&str>) -> Role {
    match (role, key) {
        // `required: ["arg", ...]` names arguments.
        (Role::Required, None) => Role::Argument,
        (Role::PromptArguments, None) => Role::PromptArgument,
        (other, None) => other,
        (Role::Schema, Some("required")) => Role::Required,
        (Role::Schema, Some(_)) => Role::Schema,
        (Role::PromptArgument, Some("name")) => Role::Argument,
        (_, Some(_)) => Role::Text,
    }
}

/// The string leaves of a definition that are not identifiers, with their
/// class as in [`collect_leaves`].
fn text_leaves<'v>(
    value: &'v mut Value,
    role: Role,
    class: &str,
    out: &mut Vec<(String, &'v mut String)>,
) {
    match value {
        Value::String(s) => {
            if !matches!(role, Role::Name | Role::Argument) {
                out.push((class.to_string(), s));
            }
        }
        Value::Array(items) => {
            let inner = inner_role(role, None);
            items
                .iter_mut()
                .for_each(|v| text_leaves(v, inner, "[]", out));
        }
        Value::Object(map) => map
            .iter_mut()
            .for_each(|(k, v)| text_leaves(v, inner_role(role, Some(k)), k, out)),
        _ => {}
    }
}

/// The string leaves of `value` in document order, each with its class: the
/// nearest object key, `[]` for an array element, `""` at the top.
fn collect_leaves<'v>(value: &'v mut Value, class: &str, out: &mut Vec<(String, &'v mut String)>) {
    match value {
        Value::String(s) => out.push((class.to_string(), s)),
        Value::Array(items) => items.iter_mut().for_each(|v| collect_leaves(v, "[]", out)),
        Value::Object(map) => map.iter_mut().for_each(|(k, v)| collect_leaves(v, k, out)),
        _ => {}
    }
}

/// The byte offsets in `level`'s text whose character was decoded from text
/// starting at one of the piece `starts`.
fn level_starts(level: &crate::secret_normalize::Normalized, starts: &[usize]) -> Vec<usize> {
    if starts.len() < 2 {
        return Vec::new();
    }
    level
        .text
        .char_indices()
        .zip(&level.spans)
        .filter(|(_, (s, _))| starts.binary_search(s).is_ok())
        .map(|((b, _), _)| b)
        .collect()
}

/// The pieces of each class (key name) that are not all next to each other
/// among the non-empty pieces, found in one pass: the extra per-name streams
/// of [`Matcher::redact_leaf_streams`]. `pieces` yields `(class, non-empty)` in
/// document order. Empty when there is only one class.
fn apart_groups<'a>(pieces: impl Iterator<Item = (&'a str, bool)>) -> Vec<Vec<usize>> {
    // Per class: its members, the rank (among non-empty pieces) of the last
    // one, and whether two of them are apart.
    let mut groups: Vec<(Vec<usize>, usize, bool)> = Vec::new();
    let mut slot: HashMap<&str, usize> = HashMap::new();
    let mut rank = 0;
    for (i, (class, nonempty)) in pieces.enumerate() {
        if !nonempty {
            continue;
        }
        match slot.get(class) {
            Some(&g) => {
                let group = &mut groups[g];
                group.2 |= rank != group.1 + 1;
                group.0.push(i);
                group.1 = rank;
            }
            None => {
                slot.insert(class, groups.len());
                groups.push((vec![i], rank, false));
            }
        }
        rank += 1;
    }
    if groups.len() < 2 {
        return Vec::new();
    }
    groups
        .into_iter()
        .filter(|(_, _, apart)| *apart)
        .map(|(members, _, _)| members)
        .collect()
}

/// Whether `s..e` crosses one of the piece `starts` (strictly inside it).
fn crosses(starts: &[usize], s: usize, e: usize) -> bool {
    let i = starts.partition_point(|&p| p <= s);
    starts.get(i).is_some_and(|&p| p < e)
}

/// One span over all of a text, with no names: [`Matcher::marker`] writes
/// [`WITHHELD`] for it.
fn withheld_span(len: usize) -> Vec<(usize, usize, Vec<usize>)> {
    if len == 0 {
        Vec::new()
    } else {
        vec![(0, len, Vec::new())]
    }
}

/// Merge overlapping hits into spans. A span is named after the whole-value
/// matches inside it; only a span found purely by fragment windows lists the
/// secrets those windows came from.
fn merge(mut hits: Vec<Hit>) -> Vec<Span> {
    hits.sort_by_key(|h| (h.0, std::cmp::Reverse(h.1)));
    let mut spans: Vec<(usize, usize, Vec<usize>, Vec<usize>)> = Vec::new();
    for (s, e, n, fragment, _) in hits {
        let names = |last: &mut (usize, usize, Vec<usize>, Vec<usize>)| {
            let list = if fragment { &mut last.3 } else { &mut last.2 };
            if !list.contains(&n) {
                list.push(n);
            }
        };
        match spans.last_mut() {
            Some(last) if s < last.1 => {
                last.1 = last.1.max(e);
                names(last);
            }
            _ => {
                let mut span = (s, e, Vec::new(), Vec::new());
                names(&mut span);
                spans.push(span);
            }
        }
    }
    spans
        .into_iter()
        .map(|(s, e, full, fragments)| (s, e, if full.is_empty() { fragments } else { full }))
        .collect()
}

// ---------------------------------------------------------------------------
// Process-global registry.

struct Registry {
    /// `(id, marker name, value)`; `id` is `server/key` for the scrub marker.
    /// Never evicted: a value stays redacted for the life of the process, as
    /// long as any log could still hold it.
    secrets: Vec<(String, String, String)>,
    /// The same triples, by index, for constant-time duplicate checks.
    known: std::collections::HashMap<(String, String, String), usize>,
    /// Matcher over `secrets[..main_len]`, built off the registering thread by
    /// [`builder`] and swapped in whole; readers never wait for it.
    main: Option<Arc<Matcher>>,
    main_len: usize,
    /// Bumped when `whole_only` changes, so the main matcher is rebuilt.
    whole_gen: u64,
    main_whole_gen: u64,
    /// Matcher over `secrets[delta_start..delta_end]`, the values registered since
    /// `main` was built. Small, so [`add`] builds it before returning: a value is
    /// redacted from the moment its registration returns.
    delta: Option<Arc<Matcher>>,
    delta_end: usize,
    /// `(id, value)` pairs [`register`] has fully processed, so the repeat reads
    /// (remote servers read their credential from the vault on every call) cost one
    /// read-locked lookup instead of re-deriving forms.
    seen: HashSet<(String, String)>,
    /// Values matched whole but never by fragment (see [`Matcher::new_with`]).
    whole_only: HashSet<String>,
}

impl Registry {
    fn new() -> Self {
        Self {
            secrets: Vec::new(),
            known: std::collections::HashMap::new(),
            main: None,
            main_len: 0,
            whole_gen: 0,
            main_whole_gen: 0,
            delta: None,
            delta_end: 0,
            seen: HashSet::new(),
            whole_only: HashSet::new(),
        }
    }

    /// The matcher readers use: the delta (which searches the main matcher it
    /// was built over as its `extra`) while it covers values the main one lacks,
    /// else the main one.
    fn layers(&self) -> Option<Layers> {
        let current = if self.delta_end > self.main_len {
            self.delta.clone().or_else(|| self.main.clone())
        } else {
            self.main.clone()
        };
        current.map(|m| Layers(vec![m]))
    }
}

/// The matchers a reader applies in turn: the main one, plus (briefly, after a
/// registration) one over the values registered since it was built.
#[derive(Clone)]
struct Layers(Vec<Arc<Matcher>>);

impl Layers {
    fn fold<'a>(
        &self,
        text: &'a str,
        f: impl Fn(&Matcher, &str) -> Option<String>,
    ) -> Cow<'a, str> {
        let mut out = Cow::Borrowed(text);
        for m in &self.0 {
            if let Some(next) = f(m, out.as_ref()) {
                out = Cow::Owned(next);
            }
        }
        out
    }

    fn redact_text<'a>(&self, text: &'a str) -> Cow<'a, str> {
        self.fold(text, |m, t| owned(m.redact_text(t)))
    }

    fn redact_fragment<'a>(&self, text: &'a str) -> Cow<'a, str> {
        self.fold(text, |m, t| owned(m.redact_fragment(t)))
    }

    fn redact_line<'a>(&self, line: &'a str) -> Cow<'a, str> {
        self.fold(line, |m, t| owned(m.redact_line(t)))
    }

    fn redact_definition_line<'a>(&self, line: &'a str) -> Cow<'a, str> {
        self.fold(line, |m, t| owned(m.redact_definition_line(t)))
    }

    fn truncate_chars(&self, text: &str, max_chars: usize) -> String {
        let (first, rest) = self.0.split_first().expect("layers are never empty");
        let mut out = first.truncate_chars(text, max_chars);
        for m in rest {
            // The cut was made above; a later layer's secret may end at it.
            out = m
                .redact_fragment(&m.truncate_chars(&out, max_chars))
                .into_owned();
        }
        out
    }

    /// The matcher (the registry always serves exactly one).
    fn main(&self) -> &Matcher {
        &self.0[0]
    }

    fn value_with(&self, value: &mut Value, fragments: bool, request: Option<&Value>) -> bool {
        let mut changed = false;
        for m in &self.0 {
            let mut exempt = HashSet::new();
            if let Some(request) = request {
                m.values_in(request, &mut exempt);
            }
            changed |= m.value_with(value, fragments, (!exempt.is_empty()).then_some(&exempt));
        }
        changed
    }

    /// Screen one definition; the names of the secrets it held (none: untouched).
    /// Screen one definition; the names of the secrets it held (none:
    /// untouched), or `None` when one of its identifiers could not be given an
    /// alias of its own and the item must not be advertised.
    fn screen(&self, item: &mut Value, ids: &mut IdBuilder) -> Option<Vec<String>> {
        ids.found.clear();
        ids.refused = false;
        ids.pending.clear();
        for m in &self.0 {
            m.screen_definition(item, ids);
        }
        if ids.refused {
            ids.pending.clear();
            return None;
        }
        for (part, advertised, original, pieces) in std::mem::take(&mut ids.pending) {
            let table = match part {
                Part::Name => &mut ids.ids.names,
                Part::Argument => &mut ids.ids.arguments,
            };
            table.insert(advertised, original);
            ids.ids.pieces.extend(pieces);
        }
        let mut names = std::mem::take(&mut ids.found);
        names.sort();
        names.dedup();
        Some(names)
    }
}

fn owned(text: Cow<'_, str>) -> Option<String> {
    match text {
        Cow::Owned(s) => Some(s),
        Cow::Borrowed(_) => None,
    }
}

/// Which registry a secret belongs to. A shipped process has one data directory,
/// so one registry (`None`). Test builds run many tests in one process, each with
/// its own data directory, so there the registry is per directory: a value one
/// test stores (`new`, `private`) must not redact another test's output.
type Scope = Option<PathBuf>;

static ACTIVE: AtomicBool = AtomicBool::new(false);
type Registries = std::collections::HashMap<Scope, Registry>;
static REGISTRY: RwLock<Option<Registries>> = RwLock::new(None);

#[cfg(not(any(test, feature = "test-support")))]
fn scope_now() -> Scope {
    None
}

/// Never panics and never resolves the developer's real directory: the test
/// override, else the explicit `TOOLPORT_DATA_DIR` a binary fixture runs with.
#[cfg(any(test, feature = "test-support"))]
fn scope_now() -> Scope {
    crate::registry::data_dir_override().or_else(|| {
        let explicit =
            !cfg!(test) && crate::brand::env_var("TOOLPORT_DATA_DIR", "CONDUIT_DATA_DIR").is_some();
        explicit.then(crate::registry::conduit_dir).flatten()
    })
}

/// Wait until the main matcher of this scope covers every registered value
/// (the background rebuild a registration starts has finished), up to
/// `timeout`. For benchmarks that must time a whole rebuild.
#[cfg(any(test, feature = "test-support"))]
pub fn wait_for_main_build(timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let settled = REGISTRY
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .and_then(|map| map.get(&scope_now()))
            .is_none_or(|reg| {
                reg.secrets.len() == reg.main_len && reg.whole_gen == reg.main_whole_gen
            });
        if settled {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

#[cfg(not(any(test, feature = "test-support")))]
fn scope_of(_dir: &Path) -> Scope {
    None
}

#[cfg(any(test, feature = "test-support"))]
fn scope_of(dir: &Path) -> Scope {
    Some(dir.to_path_buf())
}

/// The scope of a file being written, for writers that run on a background thread
/// after the data directory may have changed (test builds only; see [`Scope`]).
#[cfg(not(any(test, feature = "test-support")))]
fn scope_for_path(_path: &Path) -> Scope {
    None
}

#[cfg(any(test, feature = "test-support"))]
fn scope_for_path(path: &Path) -> Scope {
    let guard = REGISTRY
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let map = guard.as_ref()?;
    path.ancestors()
        .map(|dir| Some(dir.to_path_buf()))
        .find(|scope| map.contains_key(scope))
        .flatten()
}

/// The installed matchers for `scope`. Never builds and never waits for a
/// build: a read lock held for two `Arc` clones.
fn current_in(scope: &Scope) -> Option<Layers> {
    if !ACTIVE.load(Ordering::Relaxed) {
        return None;
    }
    #[cfg(any(test, feature = "test-support"))]
    if FORCED_UNAVAILABLE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains(scope)
    {
        let mut broken = Matcher::new(&[("TEST", "forced-unavailable-value")]);
        broken.broken = true;
        return Some(Layers(vec![Arc::new(broken)]));
    }
    REGISTRY
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()?
        .get(scope)?
        .layers()
}

/// Scopes whose matcher reads as one that could not be built.
#[cfg(any(test, feature = "test-support"))]
static FORCED_UNAVAILABLE: Mutex<Vec<Scope>> = Mutex::new(Vec::new());

/// Test hook: in this test's scope, secret redaction behaves as if its
/// matcher could not be built (fail closed), until turned off.
#[cfg(any(test, feature = "test-support"))]
pub fn force_unavailable_for_test(on: bool) {
    let scope = scope_now();
    let mut list = FORCED_UNAVAILABLE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    list.retain(|s| *s != scope);
    if on {
        list.push(scope);
    }
}

fn current() -> Option<Layers> {
    if !ACTIVE.load(Ordering::Relaxed) {
        return None;
    }
    current_in(&scope_now())
}

/// One matcher over every value registered in `scope`, for the scrub.
fn complete_in(scope: &Scope) -> Option<Arc<Matcher>> {
    current_in(scope)?.0.into_iter().next()
}

/// Wakes [`builder`]: some scope has values its main matcher does not cover.
static BUILD_WANTED: (Mutex<bool>, std::sync::Condvar) =
    (Mutex::new(false), std::sync::Condvar::new());
static BUILDER_STARTED: std::sync::Once = std::sync::Once::new();

fn wake_builder() {
    BUILDER_STARTED.call_once(|| {
        let _ = std::thread::Builder::new()
            .name("toolport-redact-build".into())
            .spawn(builder);
    });
    *BUILD_WANTED
        .0
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
    BUILD_WANTED.1.notify_one();
}

/// Rebuilds main matchers off every caller's thread. A build reads a snapshot
/// under the read lock, compiles with no lock held, and swaps the result in
/// under a brief write lock only if nothing newer was installed meanwhile.
fn builder() {
    loop {
        {
            let mut wanted = BUILD_WANTED
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            while !*wanted {
                wanted = BUILD_WANTED
                    .1
                    .wait(wanted)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        }
        std::thread::sleep(MAIN_BUILD_DEBOUNCE);
        *BUILD_WANTED
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = false;
        rebuild_stale_mains();
    }
}

/// What [`rebuild_stale_mains`] reads for one scope: its entry count and
/// `whole_only` generation at the time, and the pairs and set to build from.
type MainSnapshot = (Scope, usize, u64, Vec<(String, String)>, HashSet<String>);

fn rebuild_stale_mains() {
    let stale: Vec<MainSnapshot> = REGISTRY
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .map(|map| {
            map.iter()
                .filter(|(_, reg)| {
                    reg.secrets.len() > reg.main_len || reg.whole_gen != reg.main_whole_gen
                })
                .map(|(scope, reg)| {
                    let pairs = reg
                        .secrets
                        .iter()
                        .map(|(_, n, v)| (n.clone(), v.clone()))
                        .collect();
                    (
                        scope.clone(),
                        reg.secrets.len(),
                        reg.whole_gen,
                        pairs,
                        reg.whole_only.clone(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    for (scope, len, whole_gen, pairs, whole_only) in stale {
        let built = Arc::new(Matcher::new_with(&pairs, &whole_only));
        let mut guard = REGISTRY
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(reg) = guard.as_mut().and_then(|map| map.get_mut(&scope)) else {
            continue;
        };
        // Replaced matchers are freed after the lock is released: a large one
        // takes a while to free, and readers must not wait for that.
        let mut retired = Vec::new();
        if len > reg.main_len || (len == reg.main_len && whole_gen != reg.main_whole_gen) {
            retired.extend(reg.main.replace(built));
            reg.main_len = len;
            reg.main_whole_gen = whole_gen;
            if reg.delta_end <= reg.main_len {
                retired.extend(reg.delta.take());
            }
        }
        let more = reg.secrets.len() > reg.main_len;
        drop(guard);
        drop(retired);
        if more {
            wake_builder();
        }
    }
}

/// Make sure the value at `index` in `scope` is covered before returning: build
/// a delta matcher over everything the main one lacks (outside the lock) and
/// install it unless a wider one already was.
/// Test builds: run inside [`cover`] just before each build, so a test can hold
/// a registering thread there.
#[cfg(test)]
static COVER_HOOK: Mutex<Option<Arc<dyn Fn() + Send + Sync>>> = Mutex::new(None);

fn cover(scope: &Scope, index: usize) {
    loop {
        let (end, pairs, whole_only, main) = {
            let guard = REGISTRY
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(reg) = guard.as_ref().and_then(|map| map.get(scope)) else {
                return;
            };
            if reg.main_len > index || (reg.delta_end > index && reg.delta.is_some()) {
                return;
            }
            let start = reg.main_len;
            let end = reg.secrets.len();
            let pairs: Vec<(String, String)> = reg.secrets[start..end]
                .iter()
                .map(|(_, n, v)| (n.clone(), v.clone()))
                .collect();
            (end, pairs, reg.whole_only.clone(), reg.main.clone())
        };
        #[cfg(test)]
        {
            let hook = COVER_HOOK.lock().unwrap().clone();
            if let Some(hook) = hook {
                hook();
            }
        }
        // Searched together with the main matcher it was read with, which covers
        // everything before the delta's first value.
        let built = Arc::new(Matcher::new_with(&pairs, &whole_only).over(main));
        // The delta started at `main_len` as read, and `main_len` only grows,
        // so it and the main matcher cover everything below `end`. A wider one
        // another thread installed meanwhile wins; the next pass sees it. The
        // write lock is released before that pass reads, and before a replaced
        // matcher is freed.
        let retired = {
            let mut guard = REGISTRY
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(reg) = guard.as_mut().and_then(|map| map.get_mut(scope)) else {
                return;
            };
            if end > reg.delta_end {
                reg.delta_end = end;
                reg.delta.replace(built)
            } else {
                Some(built)
            }
        };
        drop(retired);
    }
}

/// Match `value` whole only (see [`Matcher::new_with`]). Call before [`add`].
fn mark_whole_only(scope: &Scope, value: &str) {
    let mut guard = REGISTRY
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let reg = guard
        .get_or_insert_with(Default::default)
        .entry(scope.clone())
        .or_insert_with(Registry::new);
    if reg.whole_only.insert(value.to_string()) {
        // The main matcher is rebuilt to honour it. Until then an older layer may
        // still match its fragments, which redacts more, never less.
        reg.whole_gen += 1;
        drop(guard);
        wake_builder();
    }
}

/// Register a value matched only in its whole forms, never by fragment: text
/// that contains a secret without being one (the `client_id:client_secret` pair
/// an HTTP Basic header encodes).
pub fn register_whole(owner: &str, key: &str, value: &str) {
    let scope = scope_now();
    mark_whole_only(&scope, value);
    if add(&scope, &format!("{owner}/{key}"), key, value) {
        schedule_scrub(owner);
    }
}

fn add(scope: &Scope, id: &str, name: &str, value: &str) -> bool {
    if value.trim().is_empty() {
        return false;
    }
    let triple = (id.to_string(), name.to_string(), value.to_string());
    let index = {
        let mut guard = REGISTRY
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let reg = guard
            .get_or_insert_with(Default::default)
            .entry(scope.clone())
            .or_insert_with(Registry::new);
        // Known is not covered: a duplicate (another thread may still be
        // building for it) waits for the cover of the same index.
        if let Some(&index) = reg.known.get(&triple) {
            Err(index)
        } else {
            reg.secrets.push(triple.clone());
            let index = reg.secrets.len() - 1;
            reg.known.insert(triple, index);
            ACTIVE.store(true, Ordering::Release);
            Ok(index)
        }
    };
    crate::daemon_log::secret_registered();
    match index {
        Ok(index) => {
            cover(scope, index);
            wake_builder();
            true
        }
        Err(index) => {
            cover(scope, index);
            false
        }
    }
}

/// Register a resolved secret value. `owner` is the server id (or another namespace)
/// and `key` the secret's name, which becomes the marker.
///
/// Derived forms are registered too: a stored auth header also registers its bare
/// credential, a JSON blob (OAuth state) registers its token, secret and password
/// fields, and a URL registers its password and secret-named query values.
pub fn register(owner: &str, key: &str, value: &str) {
    if value.trim().is_empty() {
        return;
    }
    let scope = scope_now();
    let id = format!("{owner}/{key}");
    let pair = (id, value.to_string());
    if REGISTRY
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .and_then(|map| map.get(&scope))
        .is_some_and(|reg| reg.seen.contains(&pair))
    {
        return;
    }
    let (id, _) = &pair;
    let id = id.as_str();
    let added = register_forms(&scope, id, key, value);
    if let Some(reg) = REGISTRY
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_mut()
        .and_then(|map| map.get_mut(&scope))
    {
        reg.seen.insert(pair.clone());
    }
    if added {
        schedule_scrub(id);
    }
}

fn register_forms(scope: &Scope, id: &str, key: &str, value: &str) -> bool {
    let id = id.to_string();
    let mut added = add(scope, &id, key, value);
    let trimmed = value.trim();
    if trimmed != value {
        added |= add(scope, &id, key, trimmed);
    }
    for scheme in ["bearer ", "basic ", "token ", "bot "] {
        if trimmed.len() > scheme.len()
            && trimmed.is_char_boundary(scheme.len())
            && trimmed[..scheme.len()].eq_ignore_ascii_case(scheme)
        {
            added |= add(scope, &id, key, trimmed[scheme.len()..].trim());
        }
    }
    if trimmed.starts_with(['{', '[']) {
        if let Ok(json) = serde_json::from_str::<Value>(trimmed) {
            let mut found = Vec::new();
            secret_fields(&json, None, &mut found);
            for (field, v) in found {
                added |= add(scope, &id, &field, &v);
            }
        }
    }
    if let Ok(url) = url::Url::parse(trimmed) {
        if url.has_host() {
            if let Some(password) = url.password() {
                added |= add(scope, &id, key, password);
                let decoded = urlencoding::decode(password).map(|d| d.into_owned());
                if let Ok(decoded) = &decoded {
                    added |= add(scope, &id, key, decoded);
                }
                // The same userinfo sent as HTTP Basic: `base64(user:password)`.
                let user = url.username();
                if !user.is_empty() {
                    let mut pairs = vec![format!("{user}:{password}")];
                    if let (Ok(user), Ok(password)) = (urlencoding::decode(user), &decoded) {
                        pairs.push(format!("{user}:{password}"));
                    }
                    for pair in pairs {
                        mark_whole_only(scope, &pair);
                        added |= add(scope, &id, key, &pair);
                    }
                }
            }
            for (name, v) in url.query_pairs() {
                if crate::import_credentials::secret_url_name(&name) {
                    added |= add(scope, &id, key, &v);
                }
            }
        }
    }
    added
}

fn secret_fields(value: &Value, key: Option<&str>, out: &mut Vec<(String, String)>) {
    match value {
        Value::String(s) => {
            if let Some(key) = key {
                let k = key.to_ascii_lowercase();
                let named =
                    (k.contains("token") && !k.contains("endpoint") && !k.ends_with("type"))
                        || k.contains("secret")
                        || k.contains("password")
                        || k == "api_key"
                        || k == "apikey";
                if named {
                    out.push((key.to_string(), s.clone()));
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|v| secret_fields(v, key, out)),
        Value::Object(map) => map.iter().for_each(|(k, v)| secret_fields(v, Some(k), out)),
        _ => {}
    }
}

/// Redact text bound for a log file. Borrowed and free when nothing matches.
pub fn redact_text(text: &str) -> Cow<'_, str> {
    match current() {
        Some(m) => m.redact_text(text),
        None => Cow::Borrowed(text),
    }
}

/// [`Matcher::redact_fragment`] against every registered secret.
pub fn redact_fragment(text: &str) -> Cow<'_, str> {
    match current() {
        Some(m) => m.redact_fragment(text),
        None => Cow::Borrowed(text),
    }
}

/// Text that is an excerpt of a longer stream (the kept tail of a child's
/// stderr): its lines are the pieces of one text ([`Stream::lines`]), so a
/// secret a tool wrapped over any number of lines is matched across them, and
/// a piece of one cut off at either end of the excerpt is removed.
pub fn redact_excerpt(text: &str) -> String {
    let Some(m) = current() else {
        return text.to_string();
    };
    let mut stream = Stream::lines();
    stream.held.push(text, true);
    stream.finish_with(m.main())
}

/// Redact, then keep at most `max_chars`. See [`Matcher::truncate_chars`].
pub fn truncate_chars(text: &str, max_chars: usize) -> String {
    match current() {
        Some(m) => m.truncate_chars(text, max_chars),
        None => text.chars().take(max_chars).collect(),
    }
}

/// The catalog a definition belongs to. Identifier mappings are kept per server
/// and catalog, so a tool name never resolves a prompt or a resource.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Catalog {
    Tool,
    Resource,
    Template,
    Prompt,
}

impl Catalog {
    pub fn as_str(self) -> &'static str {
        match self {
            Catalog::Tool => "tool",
            Catalog::Resource => "resource",
            Catalog::Template => "resource template",
            Catalog::Prompt => "prompt",
        }
    }
}

/// Which identifier of a definition: its own name (or URI, or URI template) or
/// one of its argument names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Part {
    Name,
    Argument,
}

/// Where a string sits in a definition, for [`Matcher::screen_in`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Text,
    Name,
    Argument,
    Schema,
    Required,
    PromptArguments,
    PromptArgument,
}

/// The identifiers of one screened catalog: advertised text to the text the
/// server advertised, for names and for argument names, plus each aliased
/// marker to the span it replaced (a URI expanded from a template carries the
/// marker inside it). Only identifiers are kept; descriptions never are.
#[derive(Clone, Debug, Default)]
struct CatalogIds {
    names: HashMap<String, String>,
    arguments: HashMap<String, String>,
    pieces: HashMap<String, String>,
}

impl CatalogIds {
    fn is_empty(&self) -> bool {
        self.names.is_empty() && self.arguments.is_empty()
    }
}

/// Builds the [`CatalogIds`] for one screening of a catalog, choosing aliases.
struct IdBuilder {
    server: String,
    catalog: Catalog,
    /// Read on the first alias: most catalogs hold no secret and never need it.
    key: Option<[u8; 32]>,
    ids: CatalogIds,
    /// Each alias marker handed out, with the identifier and span it stands for.
    owners: HashMap<String, (Part, String, usize)>,
    /// For the item being screened: the secret names found, whether an alias
    /// was refused, and the identifiers to record once it is accepted.
    found: Vec<String>,
    refused: bool,
    pending: Vec<PendingId>,
}

/// `(part, advertised, original, alias pieces)` waiting for its item.
type PendingId = (Part, String, String, Vec<(String, String)>);

impl IdBuilder {
    fn new(server: &str, catalog: Catalog) -> Self {
        Self {
            server: server.to_string(),
            catalog,
            key: None,
            ids: CatalogIds::default(),
            owners: HashMap::new(),
            found: Vec::new(),
            refused: false,
            pending: Vec::new(),
        }
    }

    /// `base` (`[secret:NAME`) closed with an alias suffix: the first
    /// [`ALIAS_HEX_DIGITS`] hex digits of a keyed hash of the server, catalog,
    /// part, whole original identifier and span position, so the same original
    /// gets the same alias on every run and the suffix says nothing about the
    /// secret. An alias never changes meaning: when a different identifier in
    /// this catalog already holds the same alias (a 64-bit collision), this one
    /// gets none, and the item is not advertised (`None`).
    fn alias(&mut self, part: Part, original: &str, index: usize, base: &str) -> Option<String> {
        use hmac::{Hmac, Mac};
        let key = *self.key.get_or_insert_with(alias_key);
        let mut mac =
            Hmac::<sha2::Sha256>::new_from_slice(&key).expect("HMAC takes any key length");
        for field in [
            self.server.as_bytes(),
            self.catalog.as_str().as_bytes(),
            if part == Part::Name {
                b"name"
            } else {
                b"argument"
            },
            original.as_bytes(),
            index.to_string().as_bytes(),
        ] {
            mac.update(&(field.len() as u64).to_le_bytes());
            mac.update(field);
        }
        let digest = mac.finalize().into_bytes();
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        #[cfg(test)]
        let hex: String = match ALIAS_HEX_FOR_TEST.with(|h| h.borrow().clone()) {
            Some(forced) => forced,
            None => hex,
        };
        let marker = format!("{base}~{}]", &hex[..ALIAS_HEX_DIGITS.min(hex.len())]);
        let owner = (part, original.to_string(), index);
        // One identifier listed twice (a key and in `required`) is the same
        // owner and gets the same alias.
        if self.owners.get(&marker).is_some_and(|o| *o != owner) {
            self.refused = true;
            return None;
        }
        self.owners.insert(marker.clone(), owner);
        Some(marker)
    }

    fn record(
        &mut self,
        part: Part,
        advertised: &str,
        original: &str,
        pieces: Vec<(String, String)>,
    ) {
        self.pending
            .push((part, advertised.to_string(), original.to_string(), pieces));
    }
}

/// Hex digits in an identifier alias suffix (64 bits).
const ALIAS_HEX_DIGITS: usize = 16;

#[cfg(test)]
thread_local! {
    /// Test hook: every alias hash on this thread is this hex, so two
    /// identifiers collide.
    static ALIAS_HEX_FOR_TEST: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

/// The key for [`IdBuilder::alias`]: the installation's persisted task handle
/// key, so aliases survive restarts. If it cannot be read, a random key for this
/// process (aliases then change on restart, which re-pins the affected items,
/// but never expose the secret through an unkeyed hash).
fn alias_key() -> [u8; 32] {
    static FALLBACK: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();
    crate::secrets::task_handle_key().unwrap_or_else(|_| {
        *FALLBACK.get_or_init(|| {
            let mut key = [0u8; 32];
            if getrandom::getrandom(&mut key).is_err() {
                use sha2::Digest as _;
                key = sha2::Sha256::digest(format!(
                    "{:?}{}",
                    std::time::SystemTime::now(),
                    std::process::id()
                ))
                .into();
            }
            key
        })
    })
}

/// Per server and catalog, the identifiers [`screen_definitions`] replaced.
/// Memory only: it holds values the registry holds anyway, and lets a call name
/// the upstream tool, prompt, resource or argument even though clients only
/// ever saw the alias.
static IDENTITIES: Mutex<Option<HashMap<(String, Catalog), CatalogIds>>> = Mutex::new(None);

/// The result of [`screen_definitions`]: the affected items, and the identifier
/// mapping to install with [`Screened::commit`] once the screened catalog is the
/// one being advertised.
#[must_use = "commit the identifiers when the screened catalog is stored"]
pub struct Screened {
    pub flagged: Vec<(String, Vec<String>)>,
    /// Items left out because an identifier alias collided (the name as
    /// advertised so far, possibly already holding markers).
    pub refused: Vec<String>,
    server: String,
    catalog: Catalog,
    ids: CatalogIds,
}

impl Screened {
    /// Replace this server's mapping for this catalog with the one built from the
    /// catalog just screened. Call it when that catalog replaces the advertised
    /// one; a refresh that keeps the previous catalog keeps its mapping too.
    pub fn commit(self) {
        let mut guard = IDENTITIES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let all = guard.get_or_insert_with(Default::default);
        let key = (self.server, self.catalog);
        if self.ids.is_empty() {
            all.remove(&key);
        } else {
            all.insert(key, self.ids);
        }
    }
}

/// Screen definitions a server advertised for one catalog: any configured
/// secret value in them is replaced (so neither clients, the search index, pins
/// nor the tool cache see it) and each affected item is reported as
/// `(advertised name, secret names)`. Identifiers get stable aliases and stay
/// callable through [`upstream_identifier`] and [`upstream_arguments`] once the
/// result is committed. Definitions with no secret are left byte for byte.
pub fn screen_definitions(server_id: &str, catalog: Catalog, items: &mut Vec<Value>) -> Screened {
    let mut ids = IdBuilder::new(server_id, catalog);
    let mut flagged = Vec::new();
    let mut refused = Vec::new();
    if let Some(matcher) = current() {
        let item_name = |item: &Value| {
            ["name", "uri", "uriTemplate"]
                .iter()
                .find_map(|k| item.get(*k).and_then(Value::as_str))
                .unwrap_or("")
                .to_string()
        };
        items.retain_mut(|item| match matcher.screen(item, &mut ids) {
            None => {
                // Its name may still hold the secret its alias was refused for.
                refused.push(matcher.redact_text(&item_name(item)).into_owned());
                false
            }
            Some(names) => {
                if !names.is_empty() {
                    flagged.push((item_name(item), names));
                }
                true
            }
        });
    }
    Screened {
        flagged,
        refused,
        server: server_id.to_string(),
        catalog,
        ids: ids.ids,
    }
}

fn with_ids<T>(server_id: &str, catalog: Catalog, f: impl FnOnce(&CatalogIds) -> T) -> Option<T> {
    let guard = IDENTITIES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard
        .as_ref()
        .and_then(|all| all.get(&(server_id.to_string(), catalog)))
        .map(f)
}

/// The identifier `server_id` advertised in `catalog` for `advertised` (a tool
/// or prompt name, or a resource URI a client sends back). Unchanged unless
/// [`screen_definitions`] replaced a secret in it. A resource URI is also
/// looked up among expanded templates: each aliased marker in it is put back.
pub fn upstream_identifier<'a>(
    server_id: &str,
    catalog: Catalog,
    advertised: &'a str,
) -> Cow<'a, str> {
    if !advertised.contains("[secret:") {
        return Cow::Borrowed(advertised);
    }
    if let Some(Some(upstream)) =
        with_ids(server_id, catalog, |ids| ids.names.get(advertised).cloned())
    {
        return Cow::Owned(upstream);
    }
    if catalog == Catalog::Resource {
        let expanded = with_ids(server_id, Catalog::Template, |ids| {
            let mut out = advertised.to_string();
            for (marker, piece) in &ids.pieces {
                if out.contains(marker.as_str()) {
                    out = out.replace(marker.as_str(), piece);
                }
            }
            out
        });
        if let Some(out) = expanded.filter(|out| out != advertised) {
            return Cow::Owned(out);
        }
    }
    Cow::Borrowed(advertised)
}

/// `arguments` with object keys a client copied from a screened schema renamed
/// back to the keys `server_id` advertised for that catalog.
pub fn upstream_arguments(server_id: &str, catalog: Catalog, arguments: Value) -> Value {
    fn walk(table: &HashMap<String, String>, value: Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.into_iter()
                    .map(|(k, v)| {
                        let key = table.get(&k).cloned().unwrap_or(k);
                        (key, walk(table, v))
                    })
                    .collect(),
            ),
            Value::Array(items) => {
                Value::Array(items.into_iter().map(|v| walk(table, v)).collect())
            }
            other => other,
        }
    }
    let table = with_ids(server_id, catalog, |ids| ids.arguments.clone()).unwrap_or_default();
    if table.is_empty() {
        return arguments;
    }
    walk(&table, arguments)
}

/// Every original text held for `server_id`, for tests.
#[cfg(test)]
fn recorded_originals(server_id: &str) -> Vec<String> {
    let guard = IDENTITIES.lock().unwrap();
    guard
        .iter()
        .flat_map(|all| all.iter())
        .filter(|((s, _), _)| s == server_id)
        .flat_map(|(_, ids)| {
            ids.names
                .values()
                .chain(ids.arguments.values())
                .chain(ids.pieces.values())
                .cloned()
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Redaction for text written in pieces (a relayed stream, a child's stderr,
/// the messages of one progress stream, the text lines of a log): every piece
/// is matched as part of one logical text, the separators between pieces (line
/// endings, in [`Self::lines`] mode) are transparent to the matcher, and
/// [`Self::push`] returns the part that is safe to write now. What it holds is
/// exactly the matcher's live prefix ([`Matcher::hold_from`]): a tail that may
/// still become a secret, at any decoding level, however many pieces it spans.
/// It is never longer than the longest form, so there is nothing else to
/// bound, and no pair or line logic: a secret wrapped over any number of short
/// lines is one span.
///
/// [`Self::finish`] ends the stream. The held tail is then written as it is if
/// it is shorter than [`SHORT_SECRET_CHARS`] (the shortest dangling piece
/// [`Matcher::redact_fragment`] would remove anyway); a longer one goes through
/// the same dangling-edge rule, so the start of a secret cut off by the end of
/// the stream is removed and anything else is kept.
#[derive(Debug, Default)]
pub struct Stream {
    held: Held,
    fragments: bool,
    lines: bool,
    exempt: Option<Arc<HashSet<String>>>,
}

/// The logical text a [`Stream`] has not written yet, with the separators that
/// sit between its pieces.
#[derive(Debug, Default, Clone)]
struct Held {
    text: String,
    /// `(offset in text, separators)`, ascending; the separators at one offset
    /// are runs of one line ending repeated, so a long run of blank lines
    /// costs one entry.
    seps: Vec<(usize, Runs)>,
}

/// Line endings in order, each run one ending (`\n`, `\r` or `\r\n`) and
/// how many times it repeats.
type Runs = Vec<(&'static str, usize)>;

fn write_runs(out: &mut String, runs: &[(&'static str, usize)]) {
    for (unit, n) in runs {
        for _ in 0..*n {
            out.push_str(unit);
        }
    }
}

/// Add one line-ending char to `runs`, keeping `\r\n` pairs as one unit.
fn push_ending(runs: &mut Runs, c: char) {
    match (c, runs.last_mut()) {
        ('\n', Some(("\r", n))) => {
            *n -= 1;
            if *n == 0 {
                runs.pop();
            }
            match runs.last_mut() {
                Some(("\r\n", m)) => *m += 1,
                _ => runs.push(("\r\n", 1)),
            }
        }
        ('\n', Some(("\n", n))) | ('\r', Some(("\r", n))) => *n += 1,
        ('\n', _) => runs.push(("\n", 1)),
        _ => runs.push(("\r", 1)),
    }
}

impl Held {
    fn push(&mut self, chunk: &str, lines: bool) {
        if !lines {
            self.text.push_str(chunk);
            return;
        }
        for part in chunk.split_inclusive(['\n', '\r']) {
            let body = part.trim_end_matches(['\n', '\r']);
            self.text.push_str(body);
            self.sep(&part[body.len()..]);
        }
    }

    fn sep(&mut self, sep: &str) {
        if sep.is_empty() {
            return;
        }
        let at = self.text.len();
        if self.seps.last().is_none_or(|(p, _)| *p != at) {
            self.seps.push((at, Vec::new()));
        }
        let runs = &mut self.seps.last_mut().expect("pushed above").1;
        for c in sep.chars() {
            push_ending(runs, c);
        }
    }

    /// Piece starts: the start, and every separator.
    fn starts(&self) -> Vec<usize> {
        let mut out = Vec::with_capacity(self.seps.len() + 1);
        out.push(0);
        out.extend(self.seps.iter().map(|(p, _)| *p).filter(|&p| p > 0));
        out
    }

    /// Everything held, as written.
    fn plain(&mut self) -> String {
        let mut out = String::with_capacity(self.text.len() + self.seps.len());
        let mut at = 0;
        for (p, runs) in std::mem::take(&mut self.seps) {
            out.push_str(&self.text[at..p]);
            write_runs(&mut out, &runs);
            at = p;
        }
        out.push_str(&self.text[at..]);
        self.text.clear();
        out
    }

    /// Write `text[..cut]` with `spans` (those starting before `cut`, which end
    /// at or before it) replaced and the separators up to `cut` put back; keep
    /// the rest.
    fn take(&mut self, m: &Matcher, cut: usize, spans: &[Span]) -> String {
        let mut out = String::with_capacity(cut + 16);
        let mut at = 0;
        let mut k = 0;
        let seps = std::mem::take(&mut self.seps);
        let mut advance = |out: &mut String, at: &mut usize, to: usize, copy: bool| {
            while k < seps.len() && seps[k].0 <= to {
                let p = seps[k].0.max(*at);
                if copy {
                    out.push_str(&self.text[*at..p]);
                }
                *at = p;
                write_runs(out, &seps[k].1);
                k += 1;
            }
            if copy {
                out.push_str(&self.text[*at..to]);
            }
            *at = to;
        };
        for (s, e, names) in spans.iter().filter(|(s, _, _)| *s < cut) {
            advance(&mut out, &mut at, *s, true);
            out.push_str(&m.marker(names));
            advance(&mut out, &mut at, (*e).min(cut), false);
        }
        advance(&mut out, &mut at, cut, true);
        // The rest is moved, never copied, so a push costs only its own bytes.
        let mut seps = seps;
        let mut rest = seps.split_off(k);
        for (p, _) in &mut rest {
            *p -= cut;
        }
        self.text.drain(..cut);
        self.seps = rest;
        out
    }

    /// Bytes held: the text, and each run of line endings as its unit (a run
    /// costs the same whatever its count).
    fn bytes(&self) -> usize {
        self.text.len()
            + self
                .seps
                .iter()
                .flat_map(|(_, runs)| runs.iter().map(|(unit, _)| unit.len()))
                .sum::<usize>()
    }

    /// Runs of line endings held.
    fn runs(&self) -> usize {
        self.seps.iter().map(|(_, runs)| runs.len()).sum()
    }

    /// Stop waiting: [`WITHHELD`] in place of the held text, then the held
    /// separators, so a held start of a secret is never written in part.
    fn withhold(&mut self) -> String {
        let mut out = String::with_capacity(WITHHELD.len() + self.seps.len());
        if !self.text.is_empty() {
            out.push_str(WITHHELD);
        }
        for (_, runs) in std::mem::take(&mut self.seps) {
            write_runs(&mut out, &runs);
        }
        self.text.clear();
        out
    }
}

/// Most bytes a [`Stream`] holds (text and separators) while it waits to see
/// whether a held start becomes a secret. The longest form expected is a 4 KiB
/// private key in its longest registered spelling (JSON-escaped twice, about
/// 4.5 KiB); two further percent levels make that at most 9 times longer,
/// about 40 KiB. 64 KiB leaves room for it to be wrapped over lines. Line
/// endings are held as runs ([`MAX_HELD_RUNS`]), so blank lines cost nothing
/// against it. Past either bound the stream writes [`WITHHELD`] in place of
/// the held text, then the held line endings, and goes on.
pub const MAX_HELD_BYTES: usize = 64 * 1024;

/// Most runs of line endings a [`Stream`] holds: input that alternates kinds
/// of line ending after a held start of a secret makes a run each time.
pub const MAX_HELD_RUNS: usize = 256;

impl Stream {
    /// A log stream: fragment windows apply, as in [`redact_text`].
    pub fn new() -> Self {
        Self {
            fragments: true,
            ..Self::default()
        }
    }

    /// A stream of text whose line endings separate pieces (a child's stderr,
    /// the text lines of a log): they are written where they were, and a secret
    /// wrapped over lines is matched across them.
    pub fn lines() -> Self {
        Self {
            fragments: true,
            lines: true,
            ..Self::default()
        }
    }

    /// A stream of message text bound for a client: whole forms only, as in
    /// [`redact_outgoing`], so ordinary words in content are never renamed.
    pub fn whole() -> Self {
        Self::default()
    }

    /// [`Self::whole`] for messages answering one request: a secret the client
    /// sent in it ([`exempt_values`]) is left where it is echoed.
    pub fn whole_for(exempt: Option<Arc<HashSet<String>>>) -> Self {
        Self {
            exempt,
            ..Self::default()
        }
    }

    /// `chunk` appended to the held tail; the redacted part to write now.
    pub fn push(&mut self, chunk: &str) -> String {
        match current() {
            Some(m) => self.push_with(m.main(), chunk),
            None => {
                // No secret is registered: nothing can be live.
                self.held.push(chunk, self.lines);
                self.held.plain()
            }
        }
    }

    pub(crate) fn push_with(&mut self, m: &Matcher, chunk: &str) -> String {
        self.held.push(chunk, self.lines);
        let starts = self.held.starts();
        let (cut, spans) = m.hold_and_spans(
            &self.held.text,
            &starts,
            self.fragments,
            self.exempt.as_deref(),
        );
        let mut out = self.held.take(m, cut, &spans);
        // Nothing held is written out of order: held text and the line endings
        // after it go out together once decided, or past a bound as below.
        if self.held.bytes() > MAX_HELD_BYTES || self.held.runs() > MAX_HELD_RUNS {
            out.push_str(&self.held.withhold());
        }
        out
    }

    /// Bytes this stream holds back (at most [`MAX_HELD_BYTES`] after a push).
    pub fn held_bytes(&self) -> usize {
        self.held.bytes()
    }

    /// Whether a tail is being held.
    pub fn is_holding(&self) -> bool {
        !self.held.text.is_empty() || !self.held.seps.is_empty()
    }

    /// The end of the stream: the held tail, redacted, including a piece of a
    /// secret left dangling at its end.
    pub fn finish(&mut self) -> String {
        match current() {
            Some(m) => self.finish_with(m.main()),
            None => self.push(""),
        }
    }

    pub(crate) fn finish_with(&mut self, m: &Matcher) -> String {
        let starts = self.held.starts();
        let text = &self.held.text;
        let mut spans = m.piece_spans(text, &starts, self.fragments, self.exempt.as_deref());
        if !m.is_broken() && text.chars().count() >= SHORT_SECRET_CHARS {
            let mut hits: Vec<Hit> = spans
                .drain(..)
                .flat_map(|(s, e, names)| names.into_iter().map(move |n| (s, e, n, false, 0)))
                .collect();
            let exempt = self.exempt.as_deref();
            hits.extend(
                m.edges(text)
                    .into_iter()
                    .filter(|h| !exempt.is_some_and(|x| x.contains(m.value(h.4)))),
            );
            spans = merge(hits);
        }
        let len = text.len();
        self.held.take(m, len, &spans)
    }
}

/// The registered values a client's `request` holds, in any spelling (see
/// [`Matcher::values_in`]): what [`Stream::whole_for`] exempts.
pub fn exempt_values(request: Option<&Value>) -> Option<Arc<HashSet<String>>> {
    let request = request?;
    let m = current()?;
    let mut out = HashSet::new();
    m.main().values_in(request, &mut out);
    (!out.is_empty()).then(|| Arc::new(out))
}

/// A server URL for text that leaves the machine or the app (a team push, copied
/// diagnostics): userinfo, a key-like path segment and every query value are
/// replaced as in a public share, then any registered secret still in it.
pub fn redact_config_url(url: &str) -> String {
    // Registered values only: the credential heuristics of `redact_log_text`
    // would replace the whole URL, and teammates need its host and path.
    redact_text(&crate::sharing_controller::redact_share_url(url)).into_owned()
}

/// Text for a diagnostic log: registered secrets, URL userinfo and
/// credential-shaped strings (a key typed straight into a URL or argument) are
/// all removed. Every log Toolport writes goes through this.
pub fn redact_log_text(text: &str) -> Cow<'_, str> {
    let redacted = redact_text(text);
    let stripped = crate::registry::redact_secret_text(&crate::redact_url_userinfo(&redacted));
    if stripped == text {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(stripped)
    }
}

/// Last pass on a message to a client (a response, notification or server
/// request): any registered secret of any server, in whole form, becomes its
/// marker. A secret that is in the client's own `request` (an argument it sent)
/// is left where it is echoed back: the client already holds that value, and
/// replacing it would corrupt results such as a file it asked to read. Fragment
/// windows are not used here; they would rename ordinary words in content.
/// True when changed.
pub fn redact_outgoing(value: &mut Value, request: Option<&Value>) -> bool {
    let Some(m) = current() else {
        return false;
    };
    if m.main().is_broken() && value.get("jsonrpc").is_some() {
        *value = withheld_message(value);
        return true;
    }
    m.value_with(value, false, request)
}

/// [`redact_outgoing`] for a reply to a client's `request`: only its `result`
/// and `error` are redacted. Its `id` and `jsonrpc` are the client's protocol
/// fields, which it matches the reply by, and are never changed. Without a
/// working matcher the reply is withheld as [`redact_outgoing`] withholds it.
pub fn redact_reply(envelope: &mut Value, request: Option<&Value>) -> bool {
    let Some(m) = current() else {
        return false;
    };
    if m.main().is_broken() && envelope.get("jsonrpc").is_some() {
        *envelope = withheld_message(envelope);
        return true;
    }
    let mut changed = false;
    for key in ["result", "error"] {
        if let Some(part) = envelope.get_mut(key) {
            changed |= m.value_with(part, false, request);
        }
    }
    changed
}

/// [`redact_outgoing`] with the exemption already computed
/// ([`exempt_values`]), for messages that answer a request seen earlier (its
/// progress notifications).
pub fn redact_outgoing_exempt(value: &mut Value, exempt: Option<&HashSet<String>>) -> bool {
    let Some(m) = current() else {
        return false;
    };
    if m.main().is_broken() && value.get("jsonrpc").is_some() {
        *value = withheld_message(value);
        return true;
    }
    m.main().value_with(value, false, exempt)
}

/// What a client gets in place of any message while the matcher cannot be
/// built: a response keeps its `jsonrpc` and `id` and carries a fixed error;
/// anything else (a notification, a server request) becomes a fixed log
/// notification. Nothing of the original content is in either, and both are
/// valid protocol messages.
fn withheld_message(value: &Value) -> Value {
    match (value.get("id"), value.get("method")) {
        (Some(id), None) => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id.clone(),
            "error": { "code": -32603, "message": WITHHELD_RESPONSE },
        }),
        _ => serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/message",
            "params": { "level": "error", "logger": "toolport", "data": WITHHELD_RESPONSE },
        }),
    }
}

/// The error a client gets for every response while secret redaction is
/// unavailable.
pub const WITHHELD_RESPONSE: &str = "Toolport withheld this response: secret redaction is unavailable. Call toolport_status for the reason.";

/// Why secret redaction cannot run (its matcher could not be built), or `None`
/// when it works. Says nothing about any value.
pub fn unavailable_reason() -> Option<String> {
    current().filter(|m| m.main().is_broken()).map(|m| {
        format!(
            "Secret redaction is unavailable: the matcher for {} registered secret values could not be built, so Toolport withholds every response, notification and log line instead of sending them unredacted. Restart Toolport; if it persists, remove recently added secrets or report the problem.",
            m.main().values.len() + m.main().extra.as_ref().map_or(0, |x| x.values.len())
        )
    })
}

/// [`redact_outgoing`], returning how many times each secret was replaced
/// (names only, never values). Markers already in `value` are not counted.
pub fn redact_outgoing_counted(
    value: &mut Value,
    request: Option<&Value>,
) -> std::collections::BTreeMap<String, usize> {
    if current().is_none() {
        return Default::default();
    }
    let before = marker_counts(value);
    if !redact_outgoing(value, request) {
        return Default::default();
    }
    let mut after = marker_counts(value);
    for (name, n) in before {
        if let Some(count) = after.get_mut(&name) {
            *count = count.saturating_sub(n);
        }
    }
    after.retain(|_, n| *n > 0);
    after
}

/// Per secret name, the `[secret:...]` markers in the strings and keys of
/// `value` (a marker naming two secrets counts for each; an alias suffix is
/// not part of the name).
fn marker_counts(value: &Value) -> std::collections::BTreeMap<String, usize> {
    fn text(t: &str, out: &mut std::collections::BTreeMap<String, usize>) {
        let mut rest = t;
        while let Some(at) = rest.find("[secret:") {
            let body = &rest[at + "[secret:".len()..];
            let Some(end) = body.find(']') else { break };
            for name in body[..end].split(',') {
                let name = name.split('~').next().unwrap_or(name);
                if !name.is_empty() {
                    *out.entry(name.to_string()).or_default() += 1;
                }
            }
            rest = &body[end + 1..];
        }
    }
    fn walk(v: &Value, out: &mut std::collections::BTreeMap<String, usize>) {
        match v {
            Value::String(s) => text(s, out),
            Value::Array(items) => items.iter().for_each(|i| walk(i, out)),
            Value::Object(map) => map.iter().for_each(|(k, v)| {
                text(k, out);
                walk(v, out);
            }),
            _ => {}
        }
    }
    let mut out = Default::default();
    walk(value, &mut out);
    out
}

/// [`redact_outgoing`] for a serialized message with no client request behind
/// it. Unchanged (and unparsed) unless it may hold a registered form.
pub fn redact_outgoing_json(json: String) -> String {
    let Some(m) = current() else {
        return json;
    };
    if m.main().is_broken() {
        if let Ok(value) = serde_json::from_str::<Value>(&json) {
            return withheld_message(&value).to_string();
        }
    }
    // Edited in place, string values searched as pieces of one text: whole
    // forms only, as in [`redact_outgoing`].
    match m.main().redact_definition_line(&json) {
        Cow::Borrowed(_) => json,
        Cow::Owned(out) => keep_request_id(&json, out),
    }
}

/// `edited` with the `id` of `original` put back: a request's id is the
/// gateway's own (or the server's), and the client answers by it, so it is
/// never redacted, as a reply's id is not ([`redact_reply`]).
fn keep_request_id(original: &str, edited: String) -> String {
    let Ok(Value::Object(before)) = serde_json::from_str::<Value>(original) else {
        return edited;
    };
    let Some(id) = before.get("id") else {
        return edited;
    };
    match serde_json::from_str::<Value>(&edited) {
        Ok(Value::Object(mut after)) if after.get("id") != Some(id) => {
            after.insert("id".to_string(), id.clone());
            Value::Object(after).to_string()
        }
        _ => edited,
    }
}

/// [`Matcher::redact_definition_line`] against every registered secret.
pub fn redact_definition_line(line: &str) -> Cow<'_, str> {
    match current() {
        Some(m) => m.redact_definition_line(line),
        None => Cow::Borrowed(line),
    }
}

/// A definition loaded from an older cache, screened the way the live catalog is
/// (whole forms only). True when changed.
pub fn redact_definition(value: &mut Value) -> bool {
    current().is_some_and(|m| m.value_with(value, false, None))
}

/// Redact every string in a JSON value about to be written. True when changed.
pub fn redact_value(value: &mut Value) -> bool {
    current().is_some_and(|m| m.value_with(value, true, None))
}

/// Redact one serialized line (JSON or text) about to be appended.
pub fn redact_line(line: &str) -> Cow<'_, str> {
    redact_line_with(current(), line)
}

/// [`redact_line`] for a writer that knows the file it appends to, which may run
/// on a background thread (the telemetry writer).
pub fn redact_line_for<'a>(path: &Path, line: &'a str) -> Cow<'a, str> {
    if !ACTIVE.load(Ordering::Relaxed) {
        return Cow::Borrowed(line);
    }
    redact_line_with(current_in(&scope_for_path(path)), line)
}

fn redact_line_with(matcher: Option<Layers>, line: &str) -> Cow<'_, str> {
    match matcher {
        Some(m) => m.redact_line(line),
        None => Cow::Borrowed(line),
    }
}

// ---------------------------------------------------------------------------
// One-time scrub of files written before this fix.

/// Every file in the data directory that Toolport appends diagnostic text to.
pub fn persisted_log_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = [
        "audit.jsonl",
        "security.jsonl",
        "inspect.jsonl",
        "search-trace.jsonl",
        "search-trace-v2.jsonl",
        "savings.jsonl",
        "savings-v2.jsonl",
        "savings-v3.jsonl",
        "savings-v4.jsonl",
        "gateway.log",
        "oauth-debug.log",
        "daemon.log",
        "daemon.log.1",
        "last-daemon-exit.json",
    ]
    .iter()
    .map(|name| dir.join(name))
    .collect();
    // Rotated siblings (`audit.jsonl.1`, `gateway.log.old`, ...).
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let rotated = SCRUBBED_PREFIXES
                .iter()
                .any(|prefix| name.starts_with(prefix) && !name.ends_with(".lock"))
                // A write in flight: its writer renames it into place. One a
                // crashed writer left is removed by `remove_orphaned_temp_writes`.
                && !name.ends_with(".conduit-tmp");
            if rotated && !files.contains(&path) {
                files.push(path);
            }
        }
    }
    files
}

/// Rotated logs (`audit.jsonl.1`, `gateway.log.old`, ...), tool catalogs and held
/// (quarantined) definitions, which hold upstream definition text.
const SCRUBBED_PREFIXES: &[&str] = &[
    "audit.jsonl.",
    "security.jsonl.",
    "gateway.log.",
    "inspect.jsonl.",
    "tool-cache",
    "quarantine",
    "team-activity-",
];

/// Delete the temp copies (`<file>.<pid>.<seq>.conduit-tmp`) of the scrubbed
/// files that a writer which has since exited left behind, with any lock beside
/// them. Such a write was never renamed into place, so the file itself is
/// unchanged; the copy could still hold a secret the scrub would not reach.
/// A copy whose writer is still running is in flight and stays, unless it is
/// older than any write takes: then its pid belongs to another process now.
/// An error means such a copy may remain, so the scrub is not finished.
fn remove_orphaned_temp_writes(dir: &Path) -> std::io::Result<()> {
    /// Far longer than a temp write takes; a copy this old is no write in flight.
    const STALE_WRITE: std::time::Duration = std::time::Duration::from_secs(10 * 60);
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let remove = |path: &Path| match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(pid) = name
            .strip_suffix(".conduit-tmp")
            .filter(|_| SCRUBBED_PREFIXES.iter().any(|p| name.starts_with(p)))
            .and_then(|base| base.rsplit_once('.'))
            .and_then(|(base, seq)| seq.parse::<u64>().ok().and(base.rsplit_once('.')))
            .and_then(|(_, pid)| pid.parse::<u32>().ok())
        else {
            continue;
        };
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        if !metadata.is_file() {
            continue;
        }
        let stale = metadata
            .modified()
            .ok()
            .and_then(|at| at.elapsed().ok())
            .is_some_and(|age| age > STALE_WRITE);
        let writer_running = pid == std::process::id() || crate::daemon::process_exists(pid);
        if writer_running && !stale {
            continue;
        }
        remove(&path)?;
        remove(&dir.join(format!("{name}.lock")))?;
    }
    Ok(())
}

/// Rewrite one file with every registered secret redacted, under the same lock its
/// writers take. Returns whether anything changed. A missing file is not an error.
pub fn scrub_file(path: &Path) -> std::io::Result<bool> {
    let Some(matcher) = complete_in(&scope_for_path(path)) else {
        return Ok(false);
    };
    scrub_file_with(&matcher, path)
}

/// Logs a live process holds open by descriptor (the daemon's stderr is
/// `daemon.log`, and follows it into `daemon.log.1` on rotation). Replacing those
/// by rename would leave the daemon writing into an unlinked file, so they are
/// masked in place instead.
fn held_open(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("daemon.log"))
}

/// Overwrite each secret occurrence with a same-length mask (the marker when it
/// fits, padded with `*`), so concurrent `O_APPEND` writers and open descriptors
/// are unaffected: nothing moves and the file is never replaced. The file is
/// opened without following a link and must be a regular file in `dir`; the
/// ranges are journaled first, so a crash mid-mask is finished at the next start
/// ([`crate::scrub_io::recover_masks`]).
fn mask_in_place(matcher: &Matcher, dir: &Path, path: &Path) -> std::io::Result<bool> {
    // The daemon's own stderr writes take no lock; this one keeps two scrubbers
    // (or a scrub and a delete-time scrub) from masking the same file at once.
    let _lock = crate::registry::lock_at(path).map_err(std::io::Error::other)?;
    mask_locked(matcher, dir, path)
}

/// [`mask_in_place`] for a caller that already holds the file's lock. The
/// file's text is searched as the pieces of one text, as the rewrite does: the
/// string values of each JSON line, and each other line without its ending, so
/// a value split over strings or wrapped over lines is masked where each piece
/// is. A file with other hard links is never changed ([`multi_link`]).
fn mask_locked(matcher: &Matcher, dir: &Path, path: &Path) -> std::io::Result<bool> {
    let file = match crate::scrub_io::open_inside(dir, path)? {
        Ok(file) => file,
        Err(skip) => return skipped(path, skip),
    };
    if crate::scrub_io::link_count(&file)? > 1 {
        return multi_link(path);
    }
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut &file, &mut bytes)?;
    drop(file);
    // (offset in the file, length) of each piece, in order: string values and
    // text lines are one text; keys and numbers another.
    let mut pieces: Vec<(usize, usize)> = Vec::new();
    let mut others: Vec<(usize, usize)> = Vec::new();
    let mut at = 0;
    for line in bytes.split_inclusive(|&b| b == b'\n') {
        let body_len = line
            .iter()
            .rposition(|&b| b != b'\n' && b != b'\r')
            .map_or(0, |i| i + 1);
        let body = &line[..body_len];
        let tokens = std::str::from_utf8(body)
            .ok()
            .filter(|t| t.trim_start().starts_with(['{', '[']))
            .and_then(json_tokens);
        match tokens {
            Some(t) => {
                for (s, e, _) in t.values {
                    pieces.push((at + s, e - s));
                }
                for (s, e) in t.keys.into_iter().chain(t.numbers) {
                    others.push((at + s, e - s));
                }
            }
            None => pieces.push((at, body_len)),
        }
        at += line.len();
    }
    // One journaled pass per distinct marker, each range masked with it; a
    // span over several pieces is masked in each of them.
    let mut groups: Vec<(String, Vec<(u64, u64)>)> = Vec::new();
    for pieces in [pieces, others] {
        let mut logical = Vec::with_capacity(bytes.len());
        let mut starts = Vec::with_capacity(pieces.len());
        for &(s, len) in &pieces {
            starts.push(logical.len());
            logical.extend_from_slice(&bytes[s..s + len]);
        }
        let spans = matcher.byte_spans(&logical, &starts);
        for (start, end, names) in &spans {
            let marker = matcher.marker(names);
            let first = starts.partition_point(|&p| p <= *start) - 1;
            for k in first..starts.len() {
                let (ls, len) = (starts[k], pieces[k].1);
                if ls >= *end {
                    break;
                }
                let (s, e) = ((*start).max(ls), (*end).min(ls + len));
                if s >= e {
                    continue;
                }
                let range = ((pieces[k].0 + s - ls) as u64, (e - s) as u64);
                match groups.iter_mut().find(|(m, _)| *m == marker) {
                    Some((_, ranges)) => ranges.push(range),
                    None => groups.push((marker.clone(), vec![range])),
                }
            }
        }
    }
    if groups.is_empty() {
        return Ok(false);
    }
    let mut masked = false;
    for (marker, ranges) in groups {
        let mask = |len: usize| -> Vec<u8> {
            if marker.len() <= len {
                marker.as_bytes().to_vec()
            } else {
                Vec::new()
            }
        };
        masked |= crate::scrub_io::mask_ranges(dir, path, &ranges, &mask)?;
    }
    Ok(masked)
}

/// Per scope, the files the last scrub could not clean, with the reason
/// (file names only, never content).
static SCRUB_BLOCKED: std::sync::LazyLock<Mutex<HashMap<Scope, Vec<String>>>> =
    std::sync::LazyLock::new(Default::default);

fn blocked(path: &Path, note: String) -> std::io::Error {
    let mut all = SCRUB_BLOCKED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let list = all
        .entry(scope_of(path.parent().unwrap_or(path)))
        .or_default();
    if !list.contains(&note) {
        list.push(note.clone());
    }
    std::io::Error::other(note)
}

/// A file with more than one hard link is never changed: a rewrite would leave
/// the old content under the other names, and masking it in place would change
/// a file outside the data directory that shares it. The cleanup counts as
/// unfinished, so a retired value stays in the vault, and status names the
/// file and why.
fn multi_link(path: &Path) -> std::io::Result<bool> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    Err(blocked(
        path,
        crate::scrub_io::multi_link_error(&name).to_string(),
    ))
}

/// A file the scrub would not touch: a link, a reparse point, something that is
/// not a regular file, or one outside the data directory. Named (never its
/// content) so the skip is visible.
fn skipped(path: &Path, skip: crate::scrub_io::Skip) -> std::io::Result<bool> {
    if skip == crate::scrub_io::Skip::NotFound {
        return Ok(false);
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    // Not a clean result: whatever the link or entry points at may hold a copy,
    // so a retired value stays recoverable and status keeps saying so.
    Err(blocked(
        path,
        format!(
            "secret scrub skipped {name} ({skip:?}): not a regular file inside the data directory"
        ),
    ))
}

fn scrub_file_with(matcher: &Matcher, path: &Path) -> std::io::Result<bool> {
    let Some(dir) = path.parent() else {
        return Ok(false);
    };
    if held_open(path) {
        return mask_in_place(matcher, dir, path);
    }
    let _lock = crate::registry::lock_at(path).map_err(std::io::Error::other)?;
    // A tool catalog is screened like a live one (whole forms only) so a cached
    // identifier never differs from the advertised one.
    let catalog = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("tool-cache") || n.starts_with("quarantine"));
    // One streaming read decides whether a rewrite is needed at all, so an
    // unchanged log costs a read and no write. It runs the same redaction the
    // rewrite does, so a value split over strings or lines is seen too.
    let mut probe = ScrubLines::new(matcher, catalog);
    let mut diff = crate::scrub_io::Diff::default();
    let mut out = Vec::new();
    let read = crate::scrub_io::for_each_line(dir, path, &mut |_, bytes| {
        if diff.diverged() {
            return;
        }
        diff.input(bytes);
        out.clear();
        probe.feed(Some(bytes), &mut out);
        diff.output(&out);
    })?;
    if let Err(skip) = read {
        return skipped(path, skip);
    }
    out.clear();
    probe.feed(None, &mut out);
    diff.output(&out);
    if !diff.changed() {
        return Ok(false);
    }
    let mut redact = ScrubLines::new(matcher, catalog);
    // Streamed into a temp file that is never more permissive than the original
    // and keeps its owner; published by rename only if the file did not change
    // meanwhile. A file with other hard links is left as it is.
    let mut skip = None;
    let rewritten = crate::scrub_io::rewrite_stream(
        dir,
        path,
        &mut |line, out| redact.feed(line, out),
        &mut |s| skip = Some(s),
        &mut || multi_link(path),
    )?;
    match skip {
        Some(s) => skipped(path, s),
        None => Ok(rewritten),
    }
}

/// [`scrub_file_with`] over every log in `dir`. Every file is attempted; the
/// first error (including a file the scrub had to skip) is returned after the
/// rest are done, so the caller does not record the directory as clean.
fn scrub_each(matcher: &Matcher, dir: &Path, mut changed: impl FnMut(bool)) -> std::io::Result<()> {
    SCRUB_BLOCKED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&scope_of(dir));
    let mut first_error = None;
    for path in persisted_log_files(dir) {
        match scrub_file_with(matcher, &path) {
            Ok(c) => changed(c),
            Err(e) => {
                first_error.get_or_insert(e);
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// The redaction of one log file for [`scrub_file_with`]: each JSON line is
/// one record, redacted as [`Matcher::redact_line`] does (string values as
/// pieces of one text, edited in place); the other lines between them are one
/// [`Stream::lines`] text, so a value a writer wrapped over lines is found. A
/// tool catalog is screened like a live one (whole forms only), so a cached
/// identifier never differs from the advertised one.
struct ScrubLines<'m> {
    m: &'m Matcher,
    catalog: bool,
    text: Stream,
}

impl<'m> ScrubLines<'m> {
    fn new(m: &'m Matcher, catalog: bool) -> Self {
        let mut text = Stream::lines();
        text.fragments = !catalog;
        Self { m, catalog, text }
    }

    fn feed(&mut self, line: Option<&[u8]>, out: &mut Vec<u8>) {
        let Some(bytes) = line else {
            out.extend_from_slice(self.text.finish_with(self.m).as_bytes());
            return;
        };
        let Ok(line) = std::str::from_utf8(bytes) else {
            // Not text: the stream ends here, and the line is redacted as
            // well as it can be read (kept byte for byte when nothing matched).
            out.extend_from_slice(self.text.finish_with(self.m).as_bytes());
            let lossy = String::from_utf8_lossy(bytes);
            match self.m.redact_line(&lossy) {
                Cow::Borrowed(_) => out.extend_from_slice(bytes),
                Cow::Owned(new) => out.extend_from_slice(new.as_bytes()),
            }
            return;
        };
        let body = line.trim_end_matches(['\n', '\r']);
        let json = body.trim_start().starts_with(['{', '[']) && json_tokens(body).is_some();
        if !json {
            out.extend_from_slice(self.text.push_with(self.m, line).as_bytes());
            return;
        }
        out.extend_from_slice(self.text.finish_with(self.m).as_bytes());
        let redacted = if self.catalog {
            self.m.redact_definition_line(line)
        } else {
            self.m.redact_line(line)
        };
        out.extend_from_slice(redacted.as_bytes());
    }
}

/// Scrub every log file in `dir` for secrets not yet scrubbed there, then record
/// their ids (names only, never values) in the marker so later starts skip the work.
pub fn scrub_data_dir(dir: &Path) -> std::io::Result<usize> {
    remove_orphaned_temp_writes(dir)?;
    // Nothing on disk to scrub: create no lock or marker either, so a directory
    // that was just purged (or never used) stays exactly as it is.
    if !persisted_log_files(dir).iter().any(|path| path.is_file()) {
        return Ok(0);
    }
    let marker = dir.join(SCRUB_MARKER);
    // One scrub per data directory at a time, across every process. Within one
    // process each value is scrubbed for once; another process (which may know
    // a different value under the same id) scrubs for everything it knows.
    let _scrub = crate::registry::lock_at_for(&dir.join("secret-scrub"), SCRUB_LOCK_WAIT)
        .map_err(std::io::Error::other)?;
    // A mask a crash interrupted is finished before anything else.
    // One a hard link blocks stays journaled; the rest of the scrub still
    // runs, and the cleanup is reported unfinished like a file the scrub skips.
    let recovery = crate::scrub_io::recover_masks(dir).err();
    let unfinished = |recovery: Option<std::io::Error>| match recovery {
        Some(e) => Err(blocked(&dir.join(SCRUB_MARKER), e.to_string())),
        None => Ok(0),
    };
    let scope = scope_of(dir);
    let from = SCRUBBED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&scope)
        .copied()
        .unwrap_or(0);
    let done: HashSet<String> = std::fs::read_to_string(&marker)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| {
            v.get("scrubbed").and_then(Value::as_array).map(|ids| {
                ids.iter()
                    .filter_map(|id| id.as_str().map(str::to_string))
                    .collect()
            })
        })
        .unwrap_or_default();
    let (pending, known) = {
        let guard = REGISTRY
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(reg) = guard.as_ref().and_then(|map| map.get(&scope)) else {
            return unfinished(recovery);
        };
        let pending: Vec<(String, String, String)> =
            reg.secrets.get(from..).unwrap_or_default().to_vec();
        (pending, reg.secrets.len())
    };
    if pending.is_empty() {
        if recovery.is_none() {
            let _ = std::fs::remove_file(dir.join(SCRUB_PENDING_FILE));
        }
        return unfinished(recovery);
    }
    let Some(matcher) = complete_in(&scope) else {
        return unfinished(recovery);
    };
    let mut rewritten = 0;
    let each = scrub_each(&matcher, dir, |changed| rewritten += usize::from(changed));
    unfinished(recovery)?;
    each?;
    let mut ids: Vec<String> = done.into_iter().collect();
    ids.extend(pending.into_iter().map(|(id, _, _)| id));
    ids.sort();
    ids.dedup();
    let body = serde_json::json!({ "v": 1, "scrubbed": ids }).to_string();
    crate::registry::atomic_write(&marker, &body).map_err(std::io::Error::other)?;
    let mut scrubbed = SCRUBBED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let covered = scrubbed.entry(scope).or_insert(0);
    *covered = (*covered).max(known);
    drop(scrubbed);
    let _ = std::fs::remove_file(dir.join(SCRUB_PENDING_FILE));
    Ok(rewritten)
}

/// Replaced or deleted values still waiting for their log cleanup, as last
/// counted by [`crate::secrets`].
pub static RETIRED_PENDING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Lines for `toolport_status` about redaction. A secret under
/// [`SHORT_SECRET_CHARS`] characters is only matched as a whole token, so a
/// copy of it inside a longer word would still be written; it is named (never
/// shown) so the user can lengthen or rotate it. Owed cleanups are listed too.
pub fn status_notes() -> Vec<String> {
    let mut notes = Vec::new();
    match crate::daemon_log::stderr_redaction_state() {
        crate::daemon_log::StderrRedaction::Silenced { reason } => notes.push(format!(
            "stderr is discarded, not written unredacted: {reason}. Diagnostics still reach gateway.log."
        )),
        crate::daemon_log::StderrRedaction::Unprotected { reason } => notes.push(format!(
            "stderr is not redacted: {reason}. Restart Toolport; until then a secret a server prints to stderr may reach the terminal or the client's log."
        )),
        _ => {}
    }
    if current().is_some_and(|layers| layers.0.iter().any(|m| m.is_broken())) {
        notes.push(format!(
            "secret redaction could not be prepared for the configured secrets, so logs and messages show {WITHHELD} in place of their text until it can. Remove or shorten unusually long secret values."
        ));
    }
    let ids_where = |keep: &dyn Fn(&str) -> bool| -> Vec<String> {
        REGISTRY
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .and_then(|map| map.get(&scope_now()))
            .map(|reg| {
                let mut ids: Vec<String> = reg
                    .secrets
                    .iter()
                    .filter(|(id, _, v)| !id.starts_with("__toolport_internal__/") && keep(v))
                    .map(|(id, _, _)| id.clone())
                    .collect();
                ids.sort();
                ids.dedup();
                ids
            })
            .unwrap_or_default()
    };
    let short = ids_where(&|v| v.trim().chars().count() < SHORT_SECRET_CHARS);
    let short_numbers = ids_where(&|v| {
        let v = v.trim();
        !v.is_empty()
            && v.len() < MIN_NUMERIC_SECRET_DIGITS
            && v.bytes().all(|b| b.is_ascii_digit())
    });
    if !short_numbers.is_empty() {
        notes.push(format!(
            "numeric secret(s) with fewer than {MIN_NUMERIC_SECRET_DIGITS} digits are not replaced where they appear as a JSON number (a port or a count would be replaced too): {} (server/key).",
            short_numbers.join(", ")
        ));
    }
    if !short.is_empty() {
        notes.push(format!(
            "secret(s) shorter than {SHORT_SECRET_CHARS} characters are redacted only as whole words, so a copy run into other text may be logged: {} (server/key). Use a longer value where the service allows it.",
            short.join(", ")
        ));
    }
    let retired = RETIRED_PENDING.load(Ordering::Relaxed);
    if retired > 0 {
        notes.push(format!(
            "{retired} replaced or deleted secret(s) are kept in the vault until the log cleanup for them succeeds; it is retried at each start."
        ));
    }
    if let Some(list) = SCRUB_BLOCKED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&scope_now())
    {
        notes.extend(list.iter().cloned());
    }
    if scrub_dir_now().is_some_and(|dir| dir.join(SCRUB_PENDING_FILE).exists()) {
        notes.push("a log cleanup for newly learned secrets has not finished yet.".to_string());
    }
    notes
}

/// Per scope, how many registry entries this process has scrubbed for.
static SCRUBBED: std::sync::LazyLock<Mutex<std::collections::HashMap<Scope, usize>>> =
    std::sync::LazyLock::new(Default::default);

/// Scrub every log for one value right now, whatever the marker says: called just
/// before Toolport replaces or deletes a stored secret, the last moment the old
/// value is known. The value stays registered, so this process keeps redacting it.
/// Synchronous (a read of each log, a rewrite only on a hit) and serialized with
/// every other scrub of the directory. An error means a log may still hold the
/// value; the caller keeps it recoverable until a later scrub succeeds.
pub fn scrub_value_now(owner: &str, key: &str, value: &str) -> Result<(), String> {
    register(owner, key, value);
    let Some(dir) = scrub_dir_now() else {
        return Ok(());
    };
    // A copy a crashed writer left is deleted, not scrubbed: it must be gone
    // before the caller lets the value go.
    remove_orphaned_temp_writes(&dir)
        .map_err(|error| format!("scrub before retiring secret '{key}' failed: {error}"))?;
    if !persisted_log_files(&dir).iter().any(|path| path.is_file()) {
        return Ok(());
    }
    let id = format!("{owner}/{key}");
    let pairs: Vec<(String, String)> = REGISTRY
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .and_then(|map| map.get(&scope_of(&dir)))
        .map(|reg| {
            reg.secrets
                .iter()
                .filter(|(i, _, _)| *i == id)
                .map(|(_, n, v)| (n.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default();
    // Only this id's forms: the value being retired and what it derived.
    let pairs = if pairs.is_empty() {
        vec![(key.to_string(), value.to_string())]
    } else {
        pairs
    };
    let matcher = Matcher::new(&pairs);
    let result = crate::registry::lock_at_for(&dir.join("secret-scrub"), SCRUB_LOCK_WAIT)
        .map_err(std::io::Error::other)
        .and_then(|_scrub| scrub_each(&matcher, &dir, |_| {}));
    result.map_err(|error| format!("scrub before retiring secret '{key}' failed: {error}"))
}

/// Run a queued background scrub now, within `budget`, before the process
/// exits: a scrub owed for a value this process learned would otherwise wait
/// for the next process that happens to learn it too.
pub fn flush_pending_scrub(budget: std::time::Duration) {
    let dir = SCRUB_PENDING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    let Some(dir) = dir else {
        return;
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("toolport-secret-scrub-exit".into())
        .spawn(move || {
            if let Err(error) = scrub_data_dir(&dir) {
                eprintln!("toolport: secret scrub of old logs failed: {error}");
            }
            let _ = tx.send(());
        });
    if spawned.is_ok() {
        // Past the budget the durable pending marker stays for the next start.
        let _ = rx.recv_timeout(budget);
    }
}

/// Test support for every writer of a [`persisted_log_files`] file: hold
/// `path`'s lock the way a scrub does, run `append` on another thread, learn
/// `secret` (owner, key, value) while still holding it, then release and return
/// what the file holds once `append` is done. A writer that skips the lock (or
/// concedes it after a short wait) writes before the secret is known, and one
/// that redacts before taking the lock redacts with the old matcher; either
/// leaves the value in the returned text. The caller's data directory override
/// decides the scope.
#[cfg(any(test, feature = "test-support"))]
pub fn append_during_scrub_for_test(
    path: &Path,
    secret: (&str, &str, &str),
    append: impl FnOnce() + Send + 'static,
) -> String {
    let held = crate::registry::lock_at(path).expect("hold the log lock like a scrub");
    let writer = std::thread::spawn(append);
    // Longer than any short lock wait an appender might concede after.
    std::thread::sleep(std::time::Duration::from_millis(600));
    let (owner, key, value) = secret;
    register(owner, key, value);
    drop(held);
    writer.join().expect("appender thread");
    std::fs::read_to_string(path).unwrap_or_default()
}

static SCRUB_PENDING: Mutex<Option<PathBuf>> = Mutex::new(None);
static SCRUB_ENABLED: AtomicBool = AtomicBool::new(false);

/// Whether a newly learned secret starts the background scrub.
///
/// On in every shipped process that can learn a secret: the gateway and daemon,
/// the Tauri and GTK apps and the CLI all read secrets through [`register`], so
/// all of them scrub, with no per-entry-point call to forget. The scrub itself is
/// serialized across processes ([`scrub_data_dir`]). Unit tests share one process
/// and swap data directories, so there it stays off unless a test turns it on.
fn scrub_enabled() -> bool {
    !cfg!(test) || SCRUB_ENABLED.load(Ordering::Acquire)
}

/// Turn on the background scrub explicitly (and scrub for anything already
/// known). Shipped processes have it on already; kept for unit tests and as the
/// gateway's prompt to scrub as soon as its data directory is bound.
pub fn enable_background_scrub() {
    SCRUB_ENABLED.store(true, Ordering::Release);
    if ACTIVE.load(Ordering::Acquire) {
        schedule_scrub("");
    }
}

/// The data directory a secret learned now belongs to, without ever resolving
/// (or, in test builds, panicking on) the developer's real directory.
#[cfg(not(any(test, feature = "test-support")))]
fn scrub_dir_now() -> Option<PathBuf> {
    crate::registry::conduit_dir()
}

#[cfg(any(test, feature = "test-support"))]
fn scrub_dir_now() -> Option<PathBuf> {
    scope_now()
}

fn schedule_scrub(_id: &str) {
    if !scrub_enabled() {
        return;
    }
    // Capture the directory now: it is the one the secret was resolved for.
    let Some(dir) = scrub_dir_now() else {
        return;
    };
    let mut pending = SCRUB_PENDING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if pending.is_some() {
        return; // a scrub is already queued and will see this secret too
    }
    // Durable until a pass completes, so an exit before then is visible to the
    // next process and to status. Only where there are logs to scrub.
    if persisted_log_files(&dir).iter().any(|path| path.is_file()) {
        let _ = crate::scrub_io::create_marker(&dir.join(SCRUB_PENDING_FILE));
    }
    *pending = Some(dir);
    drop(pending);
    let spawned = std::thread::Builder::new()
        .name("toolport-secret-scrub".into())
        .spawn(|| {
            // Let a burst of resolutions (every server's env at start) settle so
            // one pass covers them all.
            std::thread::sleep(std::time::Duration::from_millis(750));
            let dir = SCRUB_PENDING
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(dir) = dir {
                if let Err(error) = scrub_data_dir(&dir) {
                    eprintln!("toolport: secret scrub of old logs failed: {error}");
                }
            }
        });
    if spawned.is_err() {
        SCRUB_PENDING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(pairs: &[(&str, &str)]) -> Matcher {
        Matcher::new(pairs)
    }

    #[test]
    fn exact_value_becomes_named_marker() {
        let m = m(&[("API_TOKEN", "sk-live-abcdef123456")]);
        assert_eq!(
            m.redact_text("denied for sk-live-abcdef123456 today"),
            "denied for [secret:API_TOKEN] today"
        );
        assert!(matches!(m.redact_text("nothing here"), Cow::Borrowed(_)));
    }

    #[test]
    fn encodings_an_upstream_echoes_are_redacted() {
        let secret = "p@ss/w0rd+key=1";
        let m = m(&[("PW", secret)]);
        let b64 = base64::engine::general_purpose::STANDARD.encode(secret);
        let b64url = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(secret);
        let pct = urlencoding::encode(secret).into_owned();
        let pct_lower = pct.to_ascii_lowercase_hex();
        let form: String = url::form_urlencoded::byte_serialize(secret.as_bytes()).collect();
        for text in [
            format!("Authorization: Bearer {secret}"),
            format!("basic {b64}"),
            format!("x={b64url}&"),
            format!("GET /?k={pct} 401"),
            format!("GET /?k={pct_lower} 401"),
            format!("form {form}"),
        ] {
            let out = m.redact_text(&text);
            assert!(out.contains("[secret:PW]"), "{text} -> {out}");
            for v in [secret, &b64, &b64url, &pct, &pct_lower, &form] {
                assert!(!out.contains(v), "{text} still has {v}: {out}");
            }
        }
    }

    #[test]
    fn hex_and_twice_escaped_json_are_redacted() {
        let secret = "pa\"ss\\word-Kx93";
        let m = m(&[("PW", secret)]);
        let hex: String = secret.bytes().map(|b| format!("{b:02x}")).collect();
        for text in [
            format!("digest {hex}"),
            format!("DIGEST {}", hex.to_uppercase()),
        ] {
            assert!(m.redact_text(&text).contains("[secret:PW]"), "{text}");
        }
        // A JSON document quoted inside a JSON string, inside an audit line.
        let inner = serde_json::json!({ "detail": format!("bad {secret}") }).to_string();
        let middle = serde_json::json!({ "upstream": inner }).to_string();
        let line = serde_json::json!({ "ok": false, "error": middle }).to_string();
        let out = m.redact_line(&line);
        let parsed: Value = serde_json::from_str(&out).expect("still one valid JSON line");
        let error = parsed["error"].as_str().unwrap();
        assert!(error.contains("[secret:PW]"), "{error}");
        assert!(!out.contains("Kx93"), "{out}");
    }

    #[test]
    fn base64_at_any_offset_inside_a_blob_is_redacted() {
        let secret = "inner-blob-secret-Qm7Z";
        let m = m(&[("TOKEN", secret)]);
        for prefix_len in 0..6usize {
            for engine in [
                &base64::engine::general_purpose::STANDARD,
                &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            ] {
                let mut blob = vec![0xA5u8; prefix_len];
                blob.extend_from_slice(secret.as_bytes());
                blob.extend_from_slice(&[0x3Cu8, 0xFF, 0x10, 0x7E]);
                let encoded = engine.encode(&blob);
                let line =
                    serde_json::json!({ "ok": false, "error": format!("payload={encoded}") })
                        .to_string();
                let out = m.redact_line(&line);
                let parsed: Value = serde_json::from_str(&out).expect("valid JSON");
                let error = parsed["error"].as_str().unwrap();
                assert!(
                    error.contains("[secret:TOKEN]"),
                    "offset {prefix_len}: {error}"
                );
                // At most the few edge characters that also encode neighbouring
                // bytes survive around the marker; never a run of the secret.
                let rest = error.replace("[secret:TOKEN]", "");
                let stable = base64_inner(secret.as_bytes());
                assert!(
                    stable.iter().all(|inner| !rest.contains(&inner[..8])),
                    "offset {prefix_len}: {error}"
                );
                assert!(
                    error.len() < encoded.len() + 24,
                    "length stays in class: {error}"
                );
            }
        }
        // The floor: a short secret has no inner form, so a blob at an offset is
        // not matched, while its whole-value encoding still is.
        assert!(base64_inner(b"shrt-Qm7").is_empty());
        assert!(!base64_inner(b"short-Qm7").is_empty());
        let short = m_short();
        let whole = base64::engine::general_purpose::STANDARD.encode("short-Qm7");
        assert!(short
            .redact_text(&format!("x {whole} y"))
            .contains("[secret:S]"));
    }

    fn m_short() -> Matcher {
        Matcher::new(&[("S", "short-Qm7")])
    }

    #[test]
    fn basic_auth_forms_are_redacted() {
        let secret = "basic-auth-secret-Lp44";
        let m = m(&[("PW", secret)]);
        for user in ["", "alice", "a-much-longer-service-account"] {
            let header =
                base64::engine::general_purpose::STANDARD.encode(format!("{user}:{secret}"));
            let text = format!("Authorization: Basic {header}");
            let out = m.redact_text(&text);
            assert!(out.contains("[secret:PW]"), "user {user:?}: {out}");
        }
        // Userinfo the gateway learns from a credential URL registers the exact
        // credential, so the whole Basic value is replaced.
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!("tp-basic-{}", std::process::id()));
        let _data = crate::registry::DataDirOverride::set(&dir);
        register(
            "basic-url-server",
            "URL_KEY",
            "https://svc-user:url-pass-Zr88xx@api.example.invalid/mcp",
        );
        let header = base64::engine::general_purpose::STANDARD.encode("svc-user:url-pass-Zr88xx");
        assert_eq!(
            redact_text(&format!("Basic {header}")),
            "Basic [secret:URL_KEY]"
        );
    }

    #[test]
    fn formatter_normalisations_are_redacted() {
        let m = m(&[
            ("KEY", "MixedCase-Secret-88"),
            ("WIDE", "ｗｉｄｅ-ｓｅｃｒｅｔ-７"),
        ]);
        assert_eq!(m.redact_text("got mixedcase-secret-88"), "got [secret:KEY]");
        assert_eq!(m.redact_text("GOT MIXEDCASE-SECRET-88"), "GOT [secret:KEY]");
        assert_eq!(m.redact_text("nfkc wide-secret-7"), "nfkc [secret:WIDE]");
        let pem = "-----BEGIN KEY-----\nMIIBVQIBADANBgkqhkiG9w0BAQEFAASC\nAT8wggE7AgEAAkEAq7BFUpkGp3+LQmlQ\n-----END KEY-----";
        let p = Matcher::new(&[("PEM", pem)]);
        let out = p.redact_text("line echoed: AT8wggE7AgEAAkEAq7BFUpkGp3+LQmlQ");
        assert_eq!(out, "line echoed: [secret:PEM]");
        let collapsed = pem.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(!p.redact_text(&collapsed).contains("MIIBVQ"));
    }

    #[test]
    fn fragments_of_a_secret_are_redacted_but_not_of_containers() {
        let m = Matcher::new_with(
            &[
                ("TOKEN", "frag-Secret-7Kq2-tail"),
                (
                    "URL",
                    "https://api.example.invalid/mcp?key=frag-Secret-7Kq2-tail",
                ),
                ("STATE", r#"{"access_token":"frag-Secret-7Kq2-tail"}"#),
                ("AUTH", "Bearer frag-Secret-7Kq2-tail"),
                ("PAIR", "service-user:frag-Secret-7Kq2-tail"),
            ],
            &HashSet::from(["service-user:frag-Secret-7Kq2-tail".to_string()]),
        );
        // A piece of the secret from the middle of a stderr tail.
        assert_eq!(
            m.redact_text("tail: cret-7Kq2-ta]"),
            "tail: [secret:TOKEN]]"
        );
        // Container text stays: other URLs, JSON keys, auth schemes, usernames.
        for text in [
            "see https://docs.example.invalid/errors",
            r#"{"access_token":"unrelated"}"#,
            "Bearer abcdefgh-unrelated",
            "service-user lacks scope",
        ] {
            assert_eq!(m.redact_text(text), text);
        }
        // An all-letters word inside a passphrase is not redacted on its own.
        let p = Matcher::new(&[("PASS", "correct horse battery staple 42")]);
        assert_eq!(p.redact_text("the battery is low"), "the battery is low");
    }

    #[test]
    fn numeric_values_and_keys_that_spell_a_secret_are_redacted() {
        let m = m(&[
            ("PIN", "482193"),
            ("RATE", "1.50"),
            ("KEY", "sk-key-abc123456"),
        ]);
        let line = r#"{"data":{"pin":482193,"alt":4.82193e5,"f":482193.0,"list":[1,482193],"rate":1.50,"other":4821930,"ok":true,"none":null}}"#;
        let out = m.redact_line(line);
        let v: Value = serde_json::from_str(&out).expect("valid JSON");
        let d = &v["data"];
        assert_eq!(d["pin"], "[secret:PIN]");
        assert_eq!(d["alt"], "[secret:PIN]");
        assert_eq!(d["f"], "[secret:PIN]");
        assert_eq!(d["list"], serde_json::json!([1, "[secret:PIN]"]));
        assert_eq!(
            d["rate"], 1.50,
            "a numeric secret under six digits leaves numbers alone"
        );
        assert_eq!(d["other"], 4821930, "a longer number is not the PIN");
        assert_eq!(d["ok"], true);
        assert!(d["none"].is_null());
        // The parsed-value path (writers that never had source text).
        let mut value = serde_json::json!({"pin": 482193, "n": [482193]});
        assert!(m.redact_value(&mut value));
        assert_eq!(
            value,
            serde_json::json!({"pin":"[secret:PIN]","n":["[secret:PIN]"]})
        );
        // A key that is a secret: replaced, and kept apart from a key that already
        // reads the same, so neither value is lost.
        let mut keyed = serde_json::json!({"sk-key-abc123456": 1, "[secret:KEY]": 2, "plain": 3});
        assert!(m.redact_value(&mut keyed));
        let obj = keyed.as_object().unwrap();
        assert_eq!(obj.len(), 3);
        assert!(obj.contains_key("[secret:KEY]") && obj.contains_key("[secret:KEY]#2"));
        assert!(!keyed.to_string().contains("abc123456"));
    }

    #[test]
    fn definitions_are_screened_by_whole_value_and_stay_callable() {
        use serde_json::json;
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!("tp-defs-{}", std::process::id()));
        let _data = crate::registry::DataDirOverride::set(&dir);
        let secret = "sk-Tools-ReadSecret123";
        register("defs-server", "API_KEY", secret);
        register("defs-server", "NAMED", "named-tool-secret-9Q");
        // Pieces of a secret in a definition are ordinary words, not the secret.
        let plain = json!({
            "name": "Tools-Read",
            "description": "Reads tools. ReadSecret123 is just a word here.",
            "inputSchema": {"type": "object", "properties": {"Tools-Read": {"type": "string"}}}
        });
        let leaky = json!({
            "name": "named-tool-secret-9Q",
            "description": format!("Send {secret} as the key."),
            "inputSchema": {
                "type": "object",
                "properties": {"named-tool-secret-9Q": {"type": "string"}, "path": {"type": "string"}},
                "required": ["named-tool-secret-9Q", "path"]
            }
        });
        let prompt = json!({"name": "p", "arguments": [{"name": "named-tool-secret-9Q"}]});
        let template = json!({"uriTemplate": "mem://named-tool-secret-9Q/{id}", "name": "t"});
        let mut tools = vec![plain.clone(), leaky];
        let mut prompts = vec![prompt];
        let mut templates = vec![template];
        let screened = screen_definitions("defs-server", Catalog::Tool, &mut tools);
        let flagged = screened.flagged.clone();
        screened.commit();
        let p = screen_definitions("defs-server", Catalog::Prompt, &mut prompts);
        assert_eq!(p.flagged.len(), 1);
        p.commit();
        let t = screen_definitions("defs-server", Catalog::Template, &mut templates);
        assert_eq!(t.flagged.len(), 1);
        t.commit();
        assert_eq!(tools[0], plain, "a definition with no secret is untouched");
        assert_eq!(flagged.len(), 1, "{flagged:?}");
        let name = tools[1]["name"].as_str().unwrap().to_string();
        assert!(
            name.starts_with("[secret:NAMED~")
                && name.len() == "[secret:NAMED~1234567890abcdef]".len(),
            "{name}"
        );
        assert_eq!(
            flagged[0],
            (
                name.clone(),
                vec!["API_KEY".to_string(), "NAMED".to_string()]
            )
        );
        let advertised = tools[1].to_string() + &prompts[0].to_string() + &templates[0].to_string();
        assert!(
            !advertised.contains("named-tool-secret") && !advertised.contains(secret),
            "{advertised}"
        );
        // A description gets the plain marker; only identifiers are aliased.
        assert_eq!(tools[1]["description"], "Send [secret:API_KEY] as the key.");
        let properties = tools[1]["inputSchema"]["properties"].as_object().unwrap();
        let arg = properties
            .keys()
            .find(|k| k.starts_with("[secret:NAMED~"))
            .unwrap()
            .clone();
        assert_eq!(
            tools[1]["inputSchema"]["required"],
            json!([arg.clone(), "path"])
        );
        // Routing: the client calls the advertised name with advertised keys; the
        // server gets its own text back.
        assert_eq!(
            upstream_identifier("defs-server", Catalog::Tool, &name),
            "named-tool-secret-9Q"
        );
        assert_eq!(
            upstream_arguments(
                "defs-server",
                Catalog::Tool,
                json!({arg.clone(): "x", "path": "/a"})
            ),
            json!({"named-tool-secret-9Q": "x", "path": "/a"})
        );
        let prompt_arg = prompts[0]["arguments"][0]["name"].as_str().unwrap();
        assert_eq!(
            upstream_arguments("defs-server", Catalog::Prompt, json!({prompt_arg: "v"})),
            json!({"named-tool-secret-9Q": "v"})
        );
        let template_uri = templates[0]["uriTemplate"]
            .as_str()
            .unwrap()
            .replace("{id}", "42");
        assert_eq!(
            upstream_identifier("defs-server", Catalog::Resource, &template_uri),
            "mem://named-tool-secret-9Q/42"
        );
        // Another server, or another catalog of this one, never resolves it.
        assert_eq!(
            upstream_identifier("other-server", Catalog::Tool, &name),
            name
        );
        assert_eq!(
            upstream_identifier("defs-server", Catalog::Prompt, &name),
            name
        );
        assert_eq!(
            upstream_identifier("defs-server", Catalog::Tool, "Tools-Read"),
            "Tools-Read"
        );
    }

    #[test]
    fn identifier_aliases_are_unique_stable_scoped_and_replaced_with_their_catalog() {
        use serde_json::json;
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!("tp-alias-{}", std::process::id()));
        let _data = crate::registry::DataDirOverride::set(&dir);
        // Two values under one key name (two servers' TOKEN).
        register("alias-a", "TOKEN", "alpha-secret-111A");
        register("alias-b", "TOKEN", "beta-secret-222B");
        let catalog = || {
            vec![
                json!({"name": "use-alpha-secret-111A", "description": "first alpha-secret-111A"}),
                json!({"name": "use-beta-secret-222B"}),
            ]
        };
        let mut items = catalog();
        screen_definitions("alias-srv", Catalog::Tool, &mut items).commit();
        let (one, two) = (
            items[0]["name"].as_str().unwrap().to_string(),
            items[1]["name"].as_str().unwrap().to_string(),
        );
        assert_ne!(one, two, "two originals never share an advertised name");
        assert_eq!(
            upstream_identifier("alias-srv", Catalog::Tool, &one),
            "use-alpha-secret-111A"
        );
        assert_eq!(
            upstream_identifier("alias-srv", Catalog::Tool, &two),
            "use-beta-secret-222B"
        );
        // Descriptions are never kept in the mapping.
        assert!(
            !recorded_originals("alias-srv")
                .iter()
                .any(|o| o.contains("first")),
            "{:?}",
            recorded_originals("alias-srv")
        );
        // Stable across a restart: a fresh mapping gives the same aliases.
        *IDENTITIES.lock().unwrap() = None;
        let mut again = catalog();
        screen_definitions("alias-srv", Catalog::Tool, &mut again).commit();
        assert_eq!(again[0]["name"], one.as_str());
        assert_eq!(again[1]["name"], two.as_str());
        // Scoped by server: the same original elsewhere gets its own alias.
        let mut elsewhere = catalog();
        screen_definitions("alias-other", Catalog::Tool, &mut elsewhere).commit();
        assert_ne!(elsewhere[0]["name"], one.as_str());
        // Replaced with its catalog: once the tool is gone, its alias no longer
        // resolves; a screening that is not committed changes nothing.
        let mut smaller = vec![catalog().remove(1)];
        let pending = screen_definitions("alias-srv", Catalog::Tool, &mut smaller);
        assert_eq!(
            upstream_identifier("alias-srv", Catalog::Tool, &one),
            "use-alpha-secret-111A"
        );
        pending.commit();
        assert_eq!(upstream_identifier("alias-srv", Catalog::Tool, &one), one);
        assert_eq!(
            upstream_identifier("alias-srv", Catalog::Tool, &two),
            "use-beta-secret-222B"
        );
    }

    #[test]
    fn outgoing_messages_carry_no_secret_the_client_did_not_send() {
        use serde_json::json;
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!("tp-outgoing-{}", std::process::id()));
        let _data = crate::registry::DataDirOverride::set(&dir);
        register("srv-a", "A_KEY", "outgoing-a-secret-1Qz");
        register("srv-b", "B_KEY", "outgoing-b-secret-2Wx");
        // Server A's result holds B's secret and A's own; the client sent A's.
        let request = json!({"name": "a__read", "arguments": {"q": "outgoing-a-secret-1Qz"}});
        let mut response = json!({"result": {"content": [{"type": "text",
            "text": "got outgoing-a-secret-1Qz and outgoing-b-secret-2Wx; Tools-Read ok"}]}});
        assert!(redact_outgoing(&mut response, Some(&request)));
        assert_eq!(
            response["result"]["content"][0]["text"],
            "got outgoing-a-secret-1Qz and [secret:B_KEY]; Tools-Read ok"
        );
        // A notification has no request: nothing is exempt.
        let note =
            json!({"method": "notifications/message", "params": {"data": "outgoing-a-secret-1Qz"}});
        let out = redact_outgoing_json(note.to_string());
        assert!(
            out.contains("[secret:A_KEY]") && !out.contains("outgoing-a-secret"),
            "{out}"
        );
        let clean = r#"{"method":"notifications/progress"}"#.to_string();
        assert_eq!(redact_outgoing_json(clean.clone()), clean);
    }

    #[test]
    fn hex_and_base32_match_in_any_case() {
        let m = m(&[
            ("HEX", "a1b2c3d4e5f6a7b8c9d0"),
            ("B32", "JBSWY3DPEHPK3PXP"),
            ("PLAIN", "Plain-Secret-Value-1"),
        ]);
        assert_eq!(
            m.redact_text("id A1b2C3d4E5f6A7b8C9D0."),
            "id [secret:HEX]."
        );
        assert_eq!(m.redact_text("otp jbswy3dpEHPK3pxp"), "otp [secret:B32]");
        let hex: String = "Plain-Secret-Value-1"
            .bytes()
            .map(|b| format!("{b:02x}"))
            .collect();
        let mixed: String = hex
            .chars()
            .enumerate()
            .map(|(i, c)| {
                if i % 2 == 0 {
                    c.to_ascii_uppercase()
                } else {
                    c
                }
            })
            .collect();
        assert_eq!(
            m.redact_text(&format!("dump {mixed}")),
            "dump [secret:PLAIN]"
        );
        // Case still matters for a value that is not hex or base32.
        assert_eq!(
            m.redact_text("PLAIN-secret-VALUE-1x"),
            "PLAIN-secret-VALUE-1x"
        );
    }

    #[test]
    fn a_secret_split_across_neighbouring_strings_is_redacted() {
        // Each piece is one character class, so no fragment window sees it alone.
        let secret = "abcdefghijkl1234567890";
        let m = m(&[("SPLIT", secret)]);
        let mut v = serde_json::json!({
            "parts": ["key: abcdefghijkl", "1234567890 (wrapped)"],
            "obj": {"head": "abcdefghijkl", "tail": "1234567890"}
        });
        assert!(m.redact_value(&mut v));
        assert_eq!(
            v["parts"],
            serde_json::json!(["key: [secret:SPLIT]", " (wrapped)"])
        );
        assert_eq!(v["obj"]["head"], "[secret:SPLIT]");
        assert_eq!(v["obj"]["tail"], "");
        let mut s = Stream::lines();
        let mut out = s.push_with(&m, "x abcdefghijkl\n");
        out.push_str(&s.push_with(&m, "1234567890 y\n"));
        out.push_str(&s.finish_with(&m));
        assert_eq!(out, "x [secret:SPLIT]\n y\n");
        let mut s = Stream::lines();
        let mut out = s.push_with(&m, "abcdefghijkl\n999");
        out.push_str(&s.finish_with(&m));
        assert_eq!(out, "abcdefghijkl\n999");
    }

    #[test]
    fn a_secret_split_across_text_leaves_at_any_nesting_is_redacted() {
        let m = m(&[("KEY", "sk-ZebraSecret987")]);
        let mut v = serde_json::json!([{"text": "sk-Zebra"}, {"text": "Secret987"}]);
        assert!(m.redact_value(&mut v));
        assert_eq!(
            v,
            serde_json::json!([{"text": "[secret:KEY]"}, {"text": ""}])
        );
        // Three pieces at different depths with a number between them, and text
        // around the secret kept in place.
        let mut v = serde_json::json!({
            "content": [
                {"type": "text", "text": "token sk-Ze"},
                {"n": 4, "inner": {"text": "braSec"}},
                {"text": "ret987 done"}
            ]
        });
        assert!(m.redact_value(&mut v));
        let leaves: Vec<&str> = vec![
            v["content"][0]["text"].as_str().unwrap(),
            v["content"][1]["inner"]["text"].as_str().unwrap(),
            v["content"][2]["text"].as_str().unwrap(),
        ];
        assert_eq!(leaves, vec!["token [secret:KEY]", "", " done"]);
        assert_eq!(v["content"][0]["type"], "text");
        assert_eq!(v["content"][1]["n"], 4);
        // An unrelated neighbour is never consumed.
        let mut v = serde_json::json!([{"text": "sk-Zebra"}, {"text": "Other"}]);
        m.redact_value(&mut v);
        assert_eq!(v[1]["text"], "Other");
    }

    #[test]
    fn encoded_spellings_of_a_secret_are_replaced_where_they_are_written() {
        let m = m(&[("ENC", "sk-EncodedSecret42")]);
        let spellings = [
            r"sk-EncodedSecret42",
            r"sk-EncodedSecret42",
            "%73%6B-Encoded%53ecret42",
            "%73%6b%2dEncodedSecret%34%32",
            "sk&#45;Encoded&#x53;ecret42",
            "sk&#45Encoded&#X53;ecret42",
            "&#115;&#107;-EncodedSecret42",
            "%26%23115%3Bk-EncodedSecret42",
            "&#37;73k-EncodedSecret42",
            "sk-&amp;#69;ncodedSecret42",
            r"sk-\\u0045ncodedSecret42",
            r"%5Cu0073k-EncodedSecret42",
            r"sk-&#69;%6EcodedSecret42",
        ];
        for spelling in spellings {
            let line = format!("before {spelling} after");
            assert!(m.may_contain(&line), "fast reject passed {line}");
            assert_eq!(
                m.redact_text(&line),
                "before [secret:ENC] after",
                "{spelling}"
            );
            let json = serde_json::json!({ "text": line }).to_string();
            assert_eq!(
                m.redact_line(&json),
                serde_json::json!({ "text": "before [secret:ENC] after" }).to_string(),
                "{spelling}"
            );
        }
        // The existing encodings still apply.
        let b64 = base64::engine::general_purpose::STANDARD.encode("sk-EncodedSecret42");
        assert_eq!(m.redact_text(&format!("x {b64} y")), "x [secret:ENC] y");
        let hex: String = "sk-EncodedSecret42"
            .bytes()
            .map(|b| format!("{b:02X}"))
            .collect();
        assert_eq!(m.redact_text(&format!("x {hex} y")), "x [secret:ENC] y");
        // Encoded text without the secret is untouched.
        for plain in ["a &amp; b %20 \\u0041 &#65; 100%", "&lt;tag&gt; %E2%82%AC"] {
            assert_eq!(m.redact_text(plain), plain);
        }
    }

    /// Random per-character spellings: every output, decoded, is free of the
    /// secret, and secret-free text comes back byte for byte.
    #[test]
    fn randomly_encoded_secrets_never_survive_and_plain_text_is_untouched() {
        let secret = "sk-Mixed9Secret-Value7";
        let m = m(&[("MIX", secret)]);
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move |n: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % n
        };
        let hex = |b: u8, upper: bool| {
            if upper {
                format!("{b:02X}")
            } else {
                format!("{b:02x}")
            }
        };
        for case in 0..2000 {
            let chars: Vec<char> = secret.chars().collect();
            let mut pieces: Vec<String> = Vec::new();
            for &c in &chars {
                let b = c as u8;
                let upper = next(2) == 0;
                let piece = match next(10) {
                    0 | 1 => c.to_string(),
                    2 => format!("\\u{}", hex(0, upper) + &hex(b, upper)),
                    3 => format!("%{}", hex(b, upper)),
                    4 => format!("&#{b};"),
                    5 => format!("&#{}{};", if upper { "X" } else { "x" }, hex(b, upper)),
                    6 => format!("&#{b}"),
                    // Nested once, both ways.
                    7 => format!("%26%23{b}%3B"),
                    8 => format!("&#37;{}", hex(b, upper)),
                    _ => format!("%5Cu00{}", hex(b, upper)),
                };
                pieces.push(piece);
            }
            // `&#83` without `;` must not run into a following digit.
            for i in 0..pieces.len().saturating_sub(1) {
                let open = pieces[i].starts_with("&#")
                    && !pieces[i].ends_with(';')
                    && !pieces[i].starts_with("&#x")
                    && !pieces[i].starts_with("&#X");
                if open && pieces[i + 1].starts_with(|c: char| c.is_ascii_digit()) {
                    pieces[i].push(';');
                }
            }
            let encoded = pieces.concat();
            let line = format!("case {case}: {encoded} end");
            assert!(m.may_contain(&line), "{line}");
            let out = m.redact_text(&line).into_owned();
            let decoded = crate::secret_normalize::normalize(&out).text;
            assert!(
                !out.contains(secret) && !decoded.contains(secret),
                "{line} -> {out}"
            );
            assert!(
                out.starts_with(&format!("case {case}: ")) && out.ends_with(" end"),
                "{out}"
            );
            // Secret-free text with the same introducers.
            let alphabet: Vec<char> = "abc019;#&%\\uxX -_".chars().collect();
            let plain: String = (0..40)
                .map(|_| alphabet[next(alphabet.len() as u64) as usize])
                .collect();
            assert_eq!(m.redact_text(&plain), plain.as_str());
            assert_eq!(m.redact_line(&plain), plain.as_str());
        }
    }

    #[test]
    fn a_matcher_that_cannot_be_built_withholds_text_instead_of_passing_it() {
        FAIL_BUILDS.with(|f| f.set(true));
        let m = m(&[("KEY", "sk-UnbuiltSecret77")]);
        FAIL_BUILDS.with(|f| f.set(false));
        assert!(m.is_broken());
        let line = "error: sk-UnbuiltSecret77 rejected";
        assert_eq!(m.redact_text(line), WITHHELD);
        assert_eq!(m.redact_fragment(line), WITHHELD);
        assert_eq!(m.redact_line(&format!("{line}\n")), format!("{WITHHELD}\n"));
        let mut v = serde_json::json!({"text": line, "n": 3});
        assert!(m.redact_value(&mut v));
        // Keys can carry a secret too, so they are withheld as well.
        let out = v.to_string();
        assert!(
            out.contains(WITHHELD) && !out.contains("Unbuilt") && !out.contains("rejected"),
            "{out}"
        );
        assert!(m.may_contain("anything"));
        assert!(!m.truncate_chars(line, 200).contains("Unbuilt"));
        // A buildable set is unaffected.
        assert!(!self::m(&[("KEY", "sk-UnbuiltSecret77")]).is_broken());
    }

    #[test]
    fn status_names_numeric_secrets_too_short_to_replace_as_numbers() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!("tp-short-num-{}", std::process::id()));
        let _data = crate::registry::DataDirOverride::set(&dir);
        register("pin-server", "PIN", "4821");
        register("pin-server", "ACCOUNT", "48213377");
        let notes = status_notes().join("\n");
        let line = notes
            .lines()
            .find(|l| l.starts_with("numeric secret(s)"))
            .unwrap_or_else(|| panic!("{notes}"));
        assert!(line.contains("pin-server/PIN"), "{line}");
        assert!(
            !line.contains("ACCOUNT") && !line.contains("4821"),
            "{line}"
        );
    }

    #[test]
    fn outgoing_redaction_counts_each_secret_it_replaced_by_name() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!("tp-counted-{}", std::process::id()));
        let _data = crate::registry::DataDirOverride::set(&dir);
        register("count-server", "API_KEY", "sk-CountedSecret321");
        register("count-server", "DB_PASS", "hunter2-Counted-pass");
        let mut result = serde_json::json!({
            "content": [
                {"type": "text", "text": "key sk-CountedSecret321 and again sk-CountedSecret321"},
                {"type": "text", "text": "db hunter2-Counted-pass; old [secret:OTHER] stays"}
            ]
        });
        let counts = redact_outgoing_counted(&mut result, None);
        assert_eq!(
            counts,
            [("API_KEY".to_string(), 2), ("DB_PASS".to_string(), 1)]
                .into_iter()
                .collect()
        );
        assert!(!result.to_string().contains("Counted"));
        // The client's own value echoed back is neither replaced nor counted.
        let mut echo = serde_json::json!({"text": "you sent sk-CountedSecret321"});
        let request = serde_json::json!({"arguments": {"key": "sk-CountedSecret321"}});
        assert!(redact_outgoing_counted(&mut echo, Some(&request)).is_empty());
    }

    #[test]
    fn an_eleven_character_password_inside_basic_auth_base64_is_found() {
        let password = "Tq8#mZr2!Lp";
        let m = m(&[("PASS", password)]);
        for user in ["dave", "x", "service-account"] {
            let blob =
                base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
            let line = format!("Authorization: Basic {blob}");
            let out = m.redact_text(&line);
            assert!(out.contains("[secret:PASS]"), "{user}: {out}");
        }
    }

    /// The scrub never follows a link out of the data directory, leaves no
    /// hard-linked copy holding the old text, and never widens a file's mode.
    #[cfg(unix)]
    #[test]
    fn the_scrub_stays_inside_the_data_dir_and_never_widens_a_file() {
        use std::os::unix::fs::PermissionsExt as _;
        let _lock = crate::registry::data_dir_test_lock();
        let root = std::env::temp_dir().join(format!("tp-scrub-links-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("data");
        let outside = root.join("outside");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let secret = "sk-LinkedScrubSecret55";
        let m = Matcher::new(&[("KEY", secret)]);
        // gateway.log is a symlink to a file outside the data directory.
        let target = outside.join("notes.txt");
        std::fs::write(&target, format!("mine {secret}\n")).unwrap();
        std::os::unix::fs::symlink(&target, dir.join("gateway.log")).unwrap();
        // audit.jsonl has a second hard link elsewhere.
        let audit = dir.join("audit.jsonl");
        std::fs::write(&audit, format!("{{\"error\":\"{secret}\"}}\n")).unwrap();
        let other_name = outside.join("audit-copy.jsonl");
        std::fs::hard_link(&audit, &other_name).unwrap();
        // security.jsonl is read-only for its owner.
        let security = dir.join("security.jsonl");
        std::fs::write(&security, format!("{{\"detail\":\"{secret}\"}}\n")).unwrap();
        std::fs::set_permissions(&security, std::fs::Permissions::from_mode(0o400)).unwrap();

        let skipped_link = scrub_file_with(&m, &dir.join("gateway.log"));
        let result = scrub_each(&m, &dir, |_| {});
        let target_text = std::fs::read_to_string(&target).unwrap();
        let link_kept = std::fs::symlink_metadata(dir.join("gateway.log"))
            .unwrap()
            .file_type()
            .is_symlink();
        let copy = std::fs::read_to_string(&other_name).unwrap();
        let audit_text = std::fs::read_to_string(&audit).unwrap();
        let notes = SCRUB_BLOCKED
            .lock()
            .unwrap()
            .get(&scope_of(&dir))
            .cloned()
            .unwrap_or_default();
        let mode = std::fs::metadata(&security).unwrap().permissions().mode() & 0o777;
        let security_text = std::fs::read_to_string(&security).unwrap();
        let _ = std::fs::set_permissions(&security, std::fs::Permissions::from_mode(0o600));
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            skipped_link.is_err() && result.is_err(),
            "a skipped link is not a clean scrub"
        );
        assert_eq!(
            target_text,
            format!("mine {secret}\n"),
            "a file outside was rewritten"
        );
        assert!(link_kept, "the link was replaced");
        // A file with another hard link is never changed: that would change the
        // file outside too. The cleanup stays unfinished and status says why.
        let original = format!("{{\"error\":\"{secret}\"}}\n");
        assert_eq!(audit_text, original, "a multi-link file was changed");
        assert_eq!(copy, original, "a file outside was changed through a link");
        assert!(
            notes
                .iter()
                .any(|n| n.starts_with("audit.jsonl has other hard links")),
            "{notes:?}"
        );
        assert!(!security_text.contains(secret), "{security_text}");
        assert_eq!(mode, 0o400, "mode changed to {mode:o}");
    }

    /// A mask a crash left journaled is never finished through a hard link
    /// to a file outside: the journal stays and the cleanup is unfinished.
    #[cfg(unix)]
    #[test]
    fn recovery_through_a_hard_link_is_reported_and_changes_nothing() {
        let _lock = crate::registry::data_dir_test_lock();
        let root = std::env::temp_dir().join(format!("tp-recover-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("data");
        let outside = root.join("outside");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let _data = crate::registry::DataDirOverride::set(&dir);
        let secret = "sk-RecoverLinkSecret77";
        register("recover-srv", "KEY", secret);
        let log = dir.join("daemon.log");
        let text = format!("mine {secret}\n");
        std::fs::write(&log, &text).unwrap();
        crate::scrub_io::journal_for_test(&dir, &log, &[(5, secret.len() as u64)]);
        let other = outside.join("notes.txt");
        std::fs::hard_link(&log, &other).unwrap();
        let result = scrub_data_dir(&dir);
        let other_text = std::fs::read_to_string(&other).unwrap();
        let journal_kept = std::fs::read_dir(&dir).unwrap().flatten().any(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(".mask-journal-")
        });
        let marker = dir.join(SCRUB_MARKER).exists();
        let notes = SCRUB_BLOCKED
            .lock()
            .unwrap()
            .get(&scope_of(&dir))
            .cloned()
            .unwrap_or_default();
        let _ = std::fs::remove_dir_all(&root);
        assert!(result.is_err(), "{result:?}");
        assert_eq!(other_text, text, "a file outside was masked");
        assert!(journal_kept, "the journal was dropped");
        assert!(!marker, "the cleanup was marked done");
        assert!(
            notes
                .iter()
                .any(|n| n.starts_with("daemon.log has other hard links")),
            "{notes:?}"
        );
    }

    #[test]
    fn status_reports_stderr_that_is_not_relayed() {
        let silenced = crate::daemon_log::StderrRedaction::Silenced {
            reason: "relay pipe unavailable".to_string(),
        };
        crate::daemon_log::set_stderr_redaction_state_for_test(silenced);
        let notes = status_notes().join("\n");
        crate::daemon_log::set_stderr_redaction_state_for_test(
            crate::daemon_log::StderrRedaction::NotInstalled,
        );
        assert!(
            notes.contains("stderr is discarded") && notes.contains("relay pipe unavailable"),
            "{notes}"
        );
    }

    #[test]
    fn concurrent_registrations_never_deadlock_and_all_are_covered() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!("tp-concurrent-{}", std::process::id()));
        let _data = crate::registry::DataDirOverride::set(&dir);
        let scope = scope_now();
        let (tx, rx) = std::sync::mpsc::channel();
        for t in 0..8 {
            let (tx, scope) = (tx.clone(), scope.clone());
            std::thread::spawn(move || {
                for i in 0..25 {
                    let value = format!("concurrent-{t}-{i}-Qx7v9Lm2");
                    add(
                        &scope,
                        &format!("conc-srv/K{t}_{i}"),
                        &format!("K{t}_{i}"),
                        &value,
                    );
                    let text = format!("saw {value}");
                    let covered = current_in(&scope)
                        .is_some_and(|m| !m.redact_text(&text).contains("Qx7v9Lm2"));
                    let _ = tx.send(covered);
                }
            });
        }
        drop(tx);
        let results: Vec<bool> = (0..200)
            .map(|_| {
                rx.recv_timeout(std::time::Duration::from_secs(60))
                    .expect("no deadlock")
            })
            .collect();
        assert!(
            results.iter().all(|&c| c),
            "each value is redacted once add returns"
        );
    }

    #[test]
    fn an_exemption_covers_only_the_exact_value_the_client_sent() {
        use serde_json::json;
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!("tp-exempt-{}", std::process::id()));
        let _data = crate::registry::DataDirOverride::set(&dir);
        // Same conventional key name on two servers (or old and new values).
        register("srv-a", "API_KEY", "sk-AlphaSecret123");
        register("srv-b", "API_KEY", "sk-BetaHidden456");
        // A secret that contains the sent value, and one contained in another.
        register("srv-c", "LONGER", "sk-AlphaSecret123XYZ");
        register("srv-d", "OUTER", "wrap-InnerValue-889-end");
        register("srv-d", "INNER", "InnerValue-889");
        let request =
            json!({"arguments": {"q": "sk-AlphaSecret123", "w": "wrap-InnerValue-889-end"}});
        let mut response = json!({"text": "a sk-AlphaSecret123 b sk-BetaHidden456 c sk-AlphaSecret123XYZ d InnerValue-889 e wrap-InnerValue-889-end"});
        assert!(redact_outgoing(&mut response, Some(&request)));
        assert_eq!(
            response["text"],
            "a sk-AlphaSecret123 b [secret:API_KEY] c [secret:LONGER,API_KEY] d [secret:INNER] e wrap-InnerValue-889-end"
        );
    }

    #[test]
    fn a_line_or_case_folded_copy_of_a_secret_exempts_nothing() {
        use serde_json::json;
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!("tp-exempt-line-{}", std::process::id()));
        let _data = crate::registry::DataDirOverride::set(&dir);
        let key = "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEAx1y2z3PrivateBody\n-----END RSA PRIVATE KEY-----";
        register("srv-pem", "KEY", key);
        register("srv-case", "MIXED", "Mixed-Case-Secret-91");
        for q in ["-----BEGIN RSA PRIVATE KEY-----", "mixed-case-secret-91"] {
            let request = json!({"arguments": {"q": q}});
            let mut response = json!({"text": format!("{key} and Mixed-Case-Secret-91")});
            redact_outgoing(&mut response, Some(&request));
            let text = response["text"].as_str().unwrap();
            assert!(!text.contains("PrivateBody"), "{q}: {text}");
            assert!(!text.contains("Mixed-Case-Secret-91"), "{q}: {text}");
            let mut s = Stream::whole_for(exempt_values(Some(&request)));
            let out = s.push(&format!("{key} Mixed-Case-Secret-91")) + &s.finish();
            assert!(!out.contains("PrivateBody"), "{q}: {out}");
            assert!(!out.contains("Mixed-Case-Secret-91"), "{q}: {out}");
        }
        // The armor lines are public: other text that has them is kept.
        let m = m(&[("KEY", key)]);
        let other = "-----BEGIN RSA PRIVATE KEY-----\nunrelated\n-----END RSA PRIVATE KEY-----";
        assert_eq!(m.redact_text(other), other);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_duplicate_registration_waits_until_its_value_is_covered() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!("tp-dup-{}", std::process::id()));
        let _data = crate::registry::DataDirOverride::set(&dir);
        let scope = scope_now();
        // Thread A is held inside its first build until B has finished.
        let gate = Arc::new(std::sync::Barrier::new(2));
        let release = Arc::new(std::sync::Barrier::new(2));
        let (g, r) = (Arc::clone(&gate), Arc::clone(&release));
        let first = Arc::new(AtomicBool::new(true));
        *COVER_HOOK.lock().unwrap() = Some(Arc::new(move || {
            if first.swap(false, Ordering::SeqCst) {
                g.wait();
                r.wait();
            }
        }));
        let value = "duplicate-value-7Rk2Pq9";
        let a = {
            let scope = scope.clone();
            std::thread::spawn(move || add(&scope, "dup-srv/K", "K", value))
        };
        gate.wait();
        // B registers the same value while A has not built anything yet.
        add(&scope, "dup-srv/K", "K", value);
        let covered = current_in(&scope)
            .is_some_and(|m| !m.redact_text(&format!("x {value}")).contains("7Rk2Pq9"));
        release.wait();
        a.join().unwrap();
        *COVER_HOOK.lock().unwrap() = None;
        assert!(
            covered,
            "a duplicate registration returned before its value was covered"
        );
    }

    #[test]
    fn only_numeric_secrets_of_six_digits_or_more_replace_numbers() {
        let m = m(&[("PORT", "8443"), ("PIN", "482193"), ("RATE", "1.50")]);
        let out = m
            .redact_line(r#"{"port":8443,"pin":482193,"rate":1.50,"text":"pin 482193 port 8443"}"#);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v["port"], 8443,
            "a short numeric secret leaves numbers alone"
        );
        assert_eq!(v["rate"], 1.50);
        assert_eq!(v["pin"], "[secret:PIN]");
        assert_eq!(
            v["text"], "pin [secret:PIN] port [secret:PORT]",
            "text still redacts"
        );
    }

    #[test]
    fn twenty_secrets_fit_in_two_mebibytes() {
        let set: Vec<(String, String)> = (0..20)
            .map(|i| {
                (
                    format!("KEY_{i}"),
                    format!("sk-{i:02}-Zq8Lm3Vx7Rt2Pw9Hn4Bc6Jd1Ks5"),
                )
            })
            .collect();
        let m = Matcher::new(&set);
        assert!(
            m.memory_usage() < 2 * 1024 * 1024,
            "{} bytes",
            m.memory_usage()
        );
    }

    #[test]
    fn a_value_registered_after_the_main_build_merges_with_it() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!("tp-layers-{}", std::process::id()));
        let _data = crate::registry::DataDirOverride::set(&dir);
        register("layers-srv", "A", "xxxxyyyy1-layer");
        // Let the background build take A into the main matcher.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let built = REGISTRY
                .read()
                .unwrap()
                .as_ref()
                .and_then(|m| m.get(&scope_now()))
                .is_some_and(|r| r.main_len == r.secrets.len());
            if built || std::time::Instant::now() > deadline {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // B overlaps A and is only in the delta right after it registers: the two
        // are still matched as one set, leaving no tail of either.
        register("layers-srv", "B", "yyyy1-layerzzzz");
        assert_eq!(redact_text("<xxxxyyyy1-layerzzzz>"), "<[secret:A,B]>");
    }

    #[test]
    fn overlapping_secrets_leave_no_tail_of_either() {
        let m = m(&[("A", "xxxxyyyy1"), ("B", "yyyy1zzzz")]);
        let out = m.redact_text("<xxxxyyyy1zzzz>");
        assert_eq!(out, "<[secret:A,B]>");
        // One secret containing another: the longer wins, nothing leaks.
        let m2 = Matcher::new(&[("SHORTER", "abcdef12"), ("LONGER", "zzabcdef1234")]);
        let out = m2.redact_text("v=zzabcdef1234;");
        assert!(!out.contains("1234") && !out.contains("abcdef"), "{out}");
    }

    #[test]
    fn several_secrets_in_one_text() {
        let m = m(&[("ONE", "first-secret-111"), ("TWO", "second-secret-222")]);
        assert_eq!(
            m.redact_text("a first-secret-111 b second-secret-222 c first-secret-111"),
            "a [secret:ONE] b [secret:TWO] c [secret:ONE]"
        );
    }

    #[test]
    fn short_secret_is_redacted_only_as_a_whole_token() {
        let m = m(&[("PIN", "4821")]);
        assert_eq!(m.redact_text("pin=4821."), "pin=[secret:PIN].");
        assert_eq!(m.redact_text("4821"), "[secret:PIN]");
        assert_eq!(m.redact_text("Bearer 4821"), "Bearer [secret:PIN]");
        assert_eq!(m.redact_text("id 948210 and x4821"), "id 948210 and x4821");
    }

    #[test]
    fn truncation_never_leaves_a_partial_secret() {
        let secret = "tok_ABCDEFGHIJKLMNOPQRSTUVWX";
        let m = m(&[("TOKEN", secret)]);
        for lead in 570..600 {
            let text = format!("{}{secret} tail tail", "e".repeat(lead));
            let out = m.truncate_chars(&text, 600);
            assert!(out.chars().count() <= 600);
            assert!(
                !out.contains("tok_"),
                "lead {lead}: {}",
                &out[lead.min(out.len())..]
            );
            assert!(!out.contains("ABCDEF"), "lead {lead}");
        }
        // Text an upstream already cut: the leftover ends of a secret go too.
        let out = m.truncate_chars("QRSTUVWX] request failed near tok_ABCDEFGHIJ", 600);
        assert_eq!(out, "[secret:TOKEN]] request failed near [secret:TOKEN]");
        // A secret past the cut is cut away; a huge error is bounded.
        let huge = format!("{}{secret}", "e".repeat(5_000_000));
        assert_eq!(m.truncate_chars(&huge, 600), "e".repeat(600));
    }

    #[test]
    fn fragments_lose_a_dangling_prefix_or_suffix() {
        let secret = "tok_ABCDEFGHIJKLMNOPQRSTUVWX";
        let m = m(&[("TOKEN", secret)]);
        let out = m.redact_fragment("error near tok_ABCDEFGH");
        assert_eq!(out, "error near [secret:TOKEN]");
        let out = m.redact_fragment("QRSTUVWX was echoed");
        assert_eq!(out, "[secret:TOKEN] was echoed");
        // A short coincidental overlap is not a secret.
        assert_eq!(m.redact_fragment("x tok_"), "x tok_");
    }

    #[test]
    fn json_lines_stay_valid_and_other_numbers_are_untouched() {
        let m = m(&[("PIN", "1234"), ("Q", "quote\"and\\slash-secret")]);
        let line = serde_json::json!({"durationMs":12345,"error":"bad pin 1234 and quote\"and\\slash-secret"}).to_string();
        let out = m.redact_line(&line);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["durationMs"], 12345);
        assert_eq!(v["error"], "bad pin [secret:PIN] and [secret:Q]");
        let clean = serde_json::json!({"durationMs":12345,"ok":true}).to_string() + "\n";
        assert!(matches!(m.redact_line(&clean), Cow::Borrowed(_)));
        // Torn JSON is redacted as text, in escaped form too.
        let torn = r#"{"error":"quote\"and\\slash-secret"#;
        assert!(!m.redact_line(torn).contains("slash-secret"));
    }

    #[test]
    fn oauth_state_and_auth_headers_register_their_credentials() {
        let mut found = Vec::new();
        let state = serde_json::json!({"access_token":"at-123456789","refresh_token":"rt-987654321","token_endpoint":"https://idp.example.invalid/token","token_type":"Bearer","client_id":"cid"});
        secret_fields(&state, None, &mut found);
        let names: Vec<&str> = found.iter().map(|(k, _)| k.as_str()).collect();
        assert!(names.contains(&"access_token") && names.contains(&"refresh_token"));
        assert!(!names.contains(&"token_endpoint") && !names.contains(&"token_type"));
    }

    #[test]
    fn concurrent_scrubs_of_one_directory_rewrite_each_log_once() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!("tp-scrub-race-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let _data = crate::registry::DataDirOverride::set(&dir);
        let secret = "race-scrub-secret-9d2b4e";
        let leaked = |i: usize| {
            serde_json::json!({"ok":false,"i":i,"error":format!("echo {secret}")}).to_string()
        };
        let body: String = (0..2000).map(|i| leaked(i) + "\n").collect();
        std::fs::write(dir.join("audit.jsonl"), &body).unwrap();
        std::fs::write(dir.join("security.jsonl"), &body).unwrap();
        register("race-server", "RACE_KEY", secret);
        let workers: Vec<_> = (0..4)
            .map(|_| {
                let dir = dir.clone();
                std::thread::spawn(move || scrub_data_dir(&dir).unwrap())
            })
            .collect();
        let rewritten: usize = workers.into_iter().map(|w| w.join().unwrap()).sum();
        assert_eq!(rewritten, 2, "each log rewritten by exactly one scrubber");
        for name in ["audit.jsonl", "security.jsonl"] {
            let text = std::fs::read_to_string(dir.join(name)).unwrap();
            assert!(!text.contains(secret));
            assert_eq!(text.lines().count(), 2000, "no line lost or duplicated");
            assert!(text
                .lines()
                .all(|l| serde_json::from_str::<Value>(l).is_ok()));
        }
        let marker = std::fs::read_to_string(dir.join(SCRUB_MARKER)).unwrap();
        assert_eq!(marker.matches("race-server/RACE_KEY").count(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_retirement_scrub_keeps_the_value_until_cleanup_succeeds() {
        crate::secrets::tests::with_isolated_vault(|| {
            let dir = crate::registry::conduit_dir().unwrap();
            let old = "retired-secret-old-7Jc4Qa";
            crate::secrets::set_secret("retire-srv", "RETIRE_KEY", old).unwrap();
            // A log the scrub cannot read: the cleanup fails.
            std::fs::create_dir_all(dir.join("audit.jsonl")).unwrap();
            std::fs::write(dir.join("gateway.log"), "unrelated\n").unwrap();
            crate::secrets::delete_secret("retire-srv", "RETIRE_KEY").unwrap();
            assert_eq!(crate::secrets::get_secret("retire-srv", "RETIRE_KEY"), None);
            assert_eq!(
                crate::secrets::retired_scrubs_pending(),
                1,
                "kept for cleanup"
            );
            // A later start: the log is readable again and holds the old value.
            std::fs::remove_dir_all(dir.join("audit.jsonl")).unwrap();
            std::fs::write(
                dir.join("audit.jsonl"),
                format!("{{\"error\":\"saw {old}\"}}\n"),
            )
            .unwrap();
            assert_eq!(crate::secrets::retry_retired_scrubs(), 0);
            let audit = std::fs::read_to_string(dir.join("audit.jsonl")).unwrap();
            assert!(
                !audit.contains(old) && audit.contains("[secret:RETIRE_KEY]"),
                "{audit}"
            );
            assert_eq!(
                crate::secrets::retired_scrubs_pending(),
                0,
                "dropped once clean"
            );
        });
    }

    #[test]
    fn a_value_retired_while_a_retry_runs_is_kept() {
        crate::secrets::tests::with_isolated_vault(|| {
            let dir = crate::registry::conduit_dir().unwrap();
            std::fs::write(dir.join("gateway.log"), "unrelated\n").unwrap();
            // A value whose cleanup now succeeds, left by an earlier run.
            crate::secrets::set_secret("first-srv", "FIRST_KEY", "retired-first-9Kw2").unwrap();
            std::fs::create_dir_all(dir.join("audit.jsonl")).unwrap();
            crate::secrets::delete_secret("first-srv", "FIRST_KEY").unwrap();
            std::fs::remove_dir_all(dir.join("audit.jsonl")).unwrap();
            assert_eq!(crate::secrets::retired_scrubs_pending(), 1);
            // Another process retires a second value while the retry scrubs.
            crate::secrets::set_secret("second-srv", "SECOND_KEY", "retired-second-4Mx7").unwrap();
            let left = crate::secrets::tests::with_retry_hook(
                || {
                    let dir = crate::registry::conduit_dir().unwrap();
                    std::fs::create_dir_all(dir.join("audit.jsonl")).unwrap();
                    crate::secrets::delete_secret("second-srv", "SECOND_KEY").unwrap();
                    std::fs::remove_dir_all(dir.join("audit.jsonl")).unwrap();
                },
                crate::secrets::retry_retired_scrubs,
            );
            assert_eq!(left, 1, "the first value is clean now");
            assert_eq!(
                crate::secrets::retired_scrubs_pending(),
                1,
                "the value retired meanwhile was lost"
            );
        });
    }

    #[test]
    fn a_value_that_cannot_be_kept_for_cleanup_is_not_replaced_or_deleted() {
        crate::secrets::tests::with_isolated_vault(|| {
            let dir = crate::registry::conduit_dir().unwrap();
            let old = "retired-secret-kept-3Vb8Ny";
            crate::secrets::set_secret("keep-srv", "KEEP_KEY", old).unwrap();
            // The cleanup fails (a log it cannot read), and so does saving the
            // value for a later one.
            std::fs::create_dir_all(dir.join("audit.jsonl")).unwrap();
            std::fs::write(dir.join("gateway.log"), "unrelated\n").unwrap();
            let deleted = crate::secrets::tests::with_failed_write("retired-secret-values", || {
                crate::secrets::delete_secret("keep-srv", "KEEP_KEY")
            });
            let replaced =
                crate::secrets::tests::with_failed_write("retired-secret-values", || {
                    crate::secrets::set_secret("keep-srv", "KEEP_KEY", "retired-secret-new-5Tq1")
                });
            assert!(deleted.is_err(), "the delete went on");
            assert!(replaced.is_err(), "the replace went on");
            assert_eq!(
                crate::secrets::get_secret("keep-srv", "KEEP_KEY").as_deref(),
                Some(old),
                "the only copy of the old value was lost"
            );
            assert_eq!(crate::secrets::retired_scrubs_pending(), 0);
        });
    }

    /// Failure mode: a failed read of the stored value was ignored, so the
    /// value was replaced or deleted with no scrub and no copy kept for one.
    #[test]
    fn a_value_whose_read_fails_is_not_replaced_or_deleted() {
        crate::secrets::tests::with_isolated_vault(|| {
            let old = "unread-secret-kept-8Hc2Wq";
            crate::secrets::set_secret("unread-srv", "UNREAD_KEY", old).unwrap();
            let (deleted, replaced) = crate::secrets::tests::with_failed_read("UNREAD_KEY", || {
                (
                    crate::secrets::delete_secret("unread-srv", "UNREAD_KEY"),
                    crate::secrets::set_secret(
                        "unread-srv",
                        "UNREAD_KEY",
                        "unread-secret-new-2Lp7",
                    ),
                )
            });
            assert!(deleted.is_err(), "the delete went on");
            assert!(replaced.is_err(), "the replace went on");
            assert_eq!(
                crate::secrets::get_secret("unread-srv", "UNREAD_KEY").as_deref(),
                Some(old)
            );
            // Deleting a value that is not there still works.
            assert!(crate::secrets::delete_secret("unread-srv", "ABSENT_KEY").is_ok());
        });
    }

    /// Failure mode: a stored value that is damaged (a broken chunk manifest,
    /// a missing chunk, an undecodable item) can never be read back, so it has
    /// nothing to scrub or keep, yet its replace and delete were refused: the
    /// user could never fix or remove it.
    #[test]
    fn a_damaged_stored_value_can_still_be_replaced_and_deleted() {
        crate::secrets::tests::with_isolated_vault(|| {
            crate::secrets::set_secret("damaged-srv", "DAMAGED_KEY", "damaged-old-3Pq8Ln").unwrap();
            let replaced = crate::secrets::tests::with_damaged_read("DAMAGED_KEY", || {
                crate::secrets::set_secret("damaged-srv", "DAMAGED_KEY", "damaged-new-7Ws2Kd")
            });
            assert_eq!(replaced, Ok(()));
            assert_eq!(
                crate::secrets::get_secret("damaged-srv", "DAMAGED_KEY").as_deref(),
                Some("damaged-new-7Ws2Kd")
            );
            let deleted = crate::secrets::tests::with_damaged_read("DAMAGED_KEY", || {
                crate::secrets::delete_secret("damaged-srv", "DAMAGED_KEY")
            });
            assert_eq!(deleted, Ok(()));
            assert_eq!(
                crate::secrets::get_secret("damaged-srv", "DAMAGED_KEY"),
                None
            );
            assert_eq!(crate::secrets::retired_scrubs_pending(), 0);
        });
    }

    /// Failure mode: the retired list itself was registered as a secret on
    /// every save and load, a new registry entry and matcher rebuild each time.
    #[test]
    fn the_retired_list_is_not_registered_as_a_secret() {
        crate::secrets::tests::with_isolated_vault(|| {
            let dir = crate::registry::conduit_dir().unwrap();
            let old = "retired-listed-value-6Rk3";
            crate::secrets::set_secret("list-srv", "LIST_KEY", old).unwrap();
            // The cleanup fails (a log it cannot read), so the value is retired.
            std::fs::create_dir_all(dir.join("audit.jsonl")).unwrap();
            std::fs::write(dir.join("gateway.log"), "unrelated\n").unwrap();
            crate::secrets::set_secret("list-srv", "LIST_KEY", "retired-listed-new-1Qa9").unwrap();
            assert_eq!(crate::secrets::retired_scrubs_pending(), 1);
            // A load as the retry runs it, then the count of registry entries
            // for the list itself.
            crate::secrets::retry_retired_scrubs();
            let listed = REGISTRY
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .and_then(|map| map.get(&scope_now()))
                .map_or(0, |reg| {
                    reg.secrets
                        .iter()
                        .filter(|(id, _, _)| id.ends_with("/retired-secret-values"))
                        .count()
                });
            assert_eq!(listed, 0, "the retired list was registered");
        });
    }

    #[test]
    fn a_recorded_id_does_not_skip_a_value_this_process_knows() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!("tp-scrub-rerun-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let _data = crate::registry::DataDirOverride::set(&dir);
        // Another process scrubbed this id, for the value it knew then.
        std::fs::write(
            dir.join(SCRUB_MARKER),
            r#"{"v":1,"scrubbed":["rerun-srv/RERUN_KEY"]}"#,
        )
        .unwrap();
        let value = "rerun-new-value-5Tg8Lm";
        std::fs::write(dir.join("gateway.log"), format!("saw {value}\n")).unwrap();
        register("rerun-srv", "RERUN_KEY", value);
        assert_eq!(scrub_data_dir(&dir).unwrap(), 1);
        assert_eq!(
            std::fs::read_to_string(dir.join("gateway.log")).unwrap(),
            "saw [secret:RERUN_KEY]\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The pending marker is never written through a link someone planted.
    #[cfg(unix)]
    #[test]
    fn the_pending_scrub_marker_never_writes_through_a_link() {
        let _lock = crate::registry::data_dir_test_lock();
        let root = std::env::temp_dir().join(format!("tp-pending-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (dir, outside) = (root.join("data"), root.join("outside"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let _data = crate::registry::DataDirOverride::set(&dir);
        std::fs::write(dir.join("gateway.log"), "saw pending-link-value-8Hq3Zt\n").unwrap();
        let (sym_target, hard_target) = (outside.join("notes.txt"), outside.join("copy.txt"));
        std::fs::write(&sym_target, "keep me\n").unwrap();
        std::fs::write(&hard_target, "keep me too\n").unwrap();
        let mut kept = Vec::new();
        let links: [fn(&Path, &Path) -> std::io::Result<()>; 2] = [
            |t, at| std::os::unix::fs::symlink(t, at),
            |t, at| std::fs::hard_link(t, at),
        ];
        for (i, link) in links.iter().enumerate() {
            let target = if i == 0 { &sym_target } else { &hard_target };
            let _ = std::fs::remove_file(dir.join(SCRUB_PENDING_FILE));
            link(target, &dir.join(SCRUB_PENDING_FILE)).unwrap();
            SCRUB_ENABLED.store(true, Ordering::Release);
            register(
                "pending-srv",
                "PENDING_KEY",
                &format!("pending-link-value-8Hq3Z{i}"),
            );
            SCRUB_ENABLED.store(false, Ordering::Release);
            flush_pending_scrub(std::time::Duration::from_secs(10));
            kept.push(std::fs::read_to_string(target).unwrap());
        }
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(kept, ["keep me\n", "keep me too\n"]);
    }

    #[test]
    fn a_queued_scrub_is_durable_and_drained_on_exit() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!("tp-scrub-exit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let _data = crate::registry::DataDirOverride::set(&dir);
        let value = "exit-drained-value-2Pw9Xs";
        std::fs::write(dir.join("gateway.log"), format!("saw {value}\n")).unwrap();
        SCRUB_ENABLED.store(true, Ordering::Release);
        register("exit-srv", "EXIT_KEY", value);
        SCRUB_ENABLED.store(false, Ordering::Release);
        assert!(
            dir.join(SCRUB_PENDING_FILE).exists(),
            "owed scrub is on disk"
        );
        flush_pending_scrub(std::time::Duration::from_secs(10));
        assert_eq!(
            std::fs::read_to_string(dir.join("gateway.log")).unwrap(),
            "saw [secret:EXIT_KEY]\n"
        );
        assert!(!dir.join(SCRUB_PENDING_FILE).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn configured_secrets_are_known_before_any_launch() {
        crate::secrets::tests::with_isolated_vault(|| {
            let server: crate::registry::ServerEntry = serde_json::from_value(serde_json::json!({
                "id": "preload-srv",
                "name": "preload-srv",
                "transport": "stdio",
                "command": "true",
                "args": [],
                "env": [{"key": "INLINE_PRELOAD", "secret": true, "value": "preload-inline-6Vb3Nk"}],
                "source": "manual",
                "disabledTools": []
            }))
            .unwrap();
            let mut reg = crate::registry::Registry::default();
            reg.servers.push(server);
            crate::registry::save(&reg).unwrap();
            assert_eq!(
                redact_text("x preload-inline-6Vb3Nk"),
                "x preload-inline-6Vb3Nk"
            );
            crate::secret_refs::preload_inline_secrets();
            assert_eq!(
                redact_text("x preload-inline-6Vb3Nk"),
                "x [secret:INLINE_PRELOAD]"
            );
        });
    }

    #[test]
    fn an_empty_directory_is_left_untouched() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!("tp-scrub-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let _data = crate::registry::DataDirOverride::set(&dir);
        register("empty-server", "EMPTY_KEY", "empty-dir-secret-77aa");
        assert_eq!(scrub_data_dir(&dir).unwrap(), 0);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn replacing_or_deleting_a_secret_scrubs_its_old_value_first() {
        crate::secrets::tests::with_isolated_vault(|| {
            let dir = crate::registry::conduit_dir().unwrap();
            let old = "rotating-secret-old-1a2b3c";
            let new = "rotating-secret-new-4d5e6f";
            crate::secrets::set_secret("rotate-srv", "ROTATE_KEY", old).unwrap();
            // A copy something unaware of it (an older build) left behind.
            let line = |v: &str| {
                serde_json::json!({"ok":false,"error":format!("rejected {v}")}).to_string() + "\n"
            };
            std::fs::write(dir.join("audit.jsonl"), line(old)).unwrap();
            crate::secrets::set_secret("rotate-srv", "ROTATE_KEY", new).unwrap();
            let audit = std::fs::read_to_string(dir.join("audit.jsonl")).unwrap();
            assert!(!audit.contains(old), "{audit}");
            assert!(audit.contains("rejected [secret:ROTATE_KEY]"), "{audit}");

            std::fs::write(dir.join("gateway.log"), format!("saw {new}\n")).unwrap();
            crate::secrets::delete_secret("rotate-srv", "ROTATE_KEY").unwrap();
            let log = std::fs::read_to_string(dir.join("gateway.log")).unwrap();
            assert_eq!(log, "saw [secret:ROTATE_KEY]\n");
            assert_eq!(
                crate::secrets::get_secret_result("rotate-srv", "ROTATE_KEY").unwrap(),
                None
            );
        });
    }

    #[test]
    fn scrub_rewrites_existing_files_once() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!("tp-scrub-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let _data = crate::registry::DataDirOverride::set(&dir);
        let secret = "scrub-unit-secret-5f1c9a7e";
        let audit = dir.join("audit.jsonl");
        let leaked =
            serde_json::json!({"ok":false,"error":format!("echo {secret}"),"durationMs":5})
                .to_string();
        std::fs::write(&audit, format!("{leaked}\n{{\"ok\":true}}\n")).unwrap();
        std::fs::write(dir.join("gateway.log"), format!("line with {secret}\n")).unwrap();
        // The daemon holds daemon.log open as its stderr: it must be masked in place.
        let daemon_log = dir.join("daemon.log");
        std::fs::write(&daemon_log, format!("1 pid=9 panic: {secret}\n")).unwrap();
        let mut held = crate::registry::open_append_private(&daemon_log).unwrap();
        // A tool catalog an older build cached with an echoed secret in it.
        let cache = dir.join("tool-cache.json");
        std::fs::write(
            &cache,
            serde_json::json!({"version":4,"tools":[{"name":"t","description":format!("uses {secret}")}]})
                .to_string(),
        )
        .unwrap();
        // The OAuth debug log and a held definition in the quarantine store.
        std::fs::write(dir.join("oauth-debug.log"), format!("url ?key={secret}\n")).unwrap();
        let quarantine = dir.join("quarantine-v2-work.json");
        std::fs::write(
            &quarantine,
            serde_json::json!({"s__scrub-unit":{"definition":{"name":"scrub-unit","description":format!("k {secret}")}}})
                .to_string(),
        )
        .unwrap();
        register("unit-scrub-server", "SCRUB_KEY", secret);
        assert_eq!(scrub_data_dir(&dir).unwrap(), 6);
        assert_eq!(
            std::fs::read_to_string(dir.join("oauth-debug.log")).unwrap(),
            "url ?key=[secret:SCRUB_KEY]\n"
        );
        let held_defs: Value =
            serde_json::from_str(&std::fs::read_to_string(&quarantine).unwrap()).unwrap();
        let def = &held_defs["s__scrub-unit"]["definition"];
        assert_eq!(def["description"], "k [secret:SCRUB_KEY]");
        assert_eq!(
            def["name"], "scrub-unit",
            "a piece of a secret in a name is not redacted"
        );
        let cached: Value =
            serde_json::from_str(&std::fs::read_to_string(&cache).unwrap()).unwrap();
        assert_eq!(cached["tools"][0]["description"], "uses [secret:SCRUB_KEY]");
        std::io::Write::write_all(&mut held, b"after scrub\n").unwrap();
        let daemon_text = std::fs::read_to_string(&daemon_log).unwrap();
        assert!(!daemon_text.contains(secret), "{daemon_text}");
        assert!(
            daemon_text.starts_with("1 pid=9 panic: [secret:SCRUB_KEY]*")
                && daemon_text.ends_with("\nafter scrub\n"),
            "same file, same length, writer unaffected: {daemon_text}"
        );
        let after = std::fs::read_to_string(&audit).unwrap();
        assert!(!after.contains(secret));
        assert!(after.contains("echo [secret:SCRUB_KEY]"));
        assert!(after.ends_with("{\"ok\":true}\n"));
        assert!(!std::fs::read_to_string(dir.join("gateway.log"))
            .unwrap()
            .contains(secret));
        // The marker holds names, never the value, and makes the next start a no-op.
        let marker = std::fs::read_to_string(dir.join(SCRUB_MARKER)).unwrap();
        assert!(marker.contains("unit-scrub-server/SCRUB_KEY") && !marker.contains(secret));
        // A second pass in the same process scrubs only for values learned since.
        // (Tests elsewhere that store a keychain value without the data-dir lock
        // can register into this directory meanwhile; then a pass is owed.)
        let known = || {
            REGISTRY
                .read()
                .unwrap()
                .as_ref()
                .and_then(|m| m.get(&scope_of(&dir)))
                .map_or(0, |r| r.secrets.len())
        };
        let covered = SCRUBBED.lock().unwrap().get(&scope_of(&dir)).copied();
        let before = known();
        std::fs::write(&audit, format!("{leaked}\n")).unwrap();
        let again = scrub_data_dir(&dir).unwrap();
        if covered == Some(before) && known() == before {
            assert_eq!(again, 0);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_value_split_over_many_strings_is_found_in_lines_results_definitions_and_scrubs() {
        use serde_json::json;
        let secret = "sk-AbC123xyZ";
        let m = m(&[("KEY", secret)]);
        // A log line: edited in place, still valid JSON, every other byte kept.
        let line = r#"{"parts": ["sk-","AbC","123","xyZ"], "n": 1}"#;
        let with_ending = format!("{line}\n");
        let out = m.redact_line(&with_ending);
        assert_eq!(
            out,
            "{\"parts\": [\"[secret:KEY]\",\"\",\"\",\"\"], \"n\": 1}\n"
        );
        assert!(serde_json::from_str::<Value>(out.trim_end()).is_ok());
        assert!(
            !m.redact_definition_line(line).contains("AbC"),
            "catalog line"
        );
        assert!(!redact_outgoing_json_with(&m, line).contains("AbC"));
        // A result, whole forms only: pieces under one key with other fields
        // between them.
        let mut v = json!({"content": [
            {"type": "text", "text": "sk-"}, {"type": "text", "text": "AbC"},
            {"type": "text", "text": "123"}, {"type": "text", "text": "xyZ tail"}
        ]});
        assert!(m.value_with(&mut v, false, None));
        assert_eq!(v["content"][0]["text"], "[secret:KEY]");
        assert_eq!(v["content"][3]["text"], " tail");
        // A definition: its descriptions are one text, like a result.
        let mut ids = IdBuilder::new("split-srv", Catalog::Tool);
        let mut tool = json!({"name": "t", "inputSchema": {"type": "object", "properties": {
            "a": {"description": "use sk-"}, "b": {"description": "AbC"},
            "c": {"description": "123xyZ now"}
        }}});
        m.screen_definition(&mut tool, &mut ids);
        assert_eq!(ids.found, vec!["KEY".to_string()]);
        let props = &tool["inputSchema"]["properties"];
        assert_eq!(props["a"]["description"], "use [secret:KEY]");
        assert_eq!(props["c"]["description"], " now");
        // A file scrub: JSON leaves, and a value wrapped over text lines.
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!("tp-split-scrub-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let audit = dir.join("audit.jsonl");
        std::fs::write(&audit, format!("{line}\n{{\"ok\":\"plain\"}}\n")).unwrap();
        let log = dir.join("gateway.log");
        std::fs::write(&log, "start\nkey sk-\nAbC\n123xyZ end\nlast\n").unwrap();
        let changed = (scrub_file_with(&m, &audit), scrub_file_with(&m, &log));
        let (audit_text, log_text) = (
            std::fs::read_to_string(&audit).unwrap(),
            std::fs::read_to_string(&log).unwrap(),
        );
        let _ = std::fs::remove_dir_all(&dir);
        assert!(matches!(changed, (Ok(true), Ok(true))), "{changed:?}");
        assert_eq!(
            audit_text,
            "{\"parts\": [\"[secret:KEY]\",\"\",\"\",\"\"], \"n\": 1}\n{\"ok\":\"plain\"}\n"
        );
        assert_eq!(log_text, "start\nkey [secret:KEY]\n\n end\nlast\n");
    }

    fn redact_outgoing_json_with(m: &Matcher, json: &str) -> String {
        m.redact_definition_line(json).into_owned()
    }

    #[test]
    fn every_decoding_level_is_searched_and_unmatched_bytes_are_kept() {
        let m = m(&[("PCT", "ab%41cd!"), ("FORM", "abc def")]);
        // The first decoding of the echo is the value as registered.
        assert_eq!(m.redact_text("x ab%2541cd! y"), "x [secret:PCT] y");
        // Bytes around a hit are left as written, encoded or not.
        assert_eq!(
            m.redact_text("%2541 then ab%2541cd! then %41"),
            "%2541 then [secret:PCT] then %41"
        );
        assert_eq!(m.redact_text("q=%61bc+def&r=1"), "q=[secret:FORM]&r=1");
        // Inside a JSON string inside a JSON string in a log line, encoded
        // twice: four levels.
        let inner = serde_json::json!({"k": "ab%2541cd!"}).to_string();
        let line = serde_json::json!({"text": inner, "n": "keep%2541"}).to_string();
        let out = m.redact_line(&line);
        assert!(!out.contains("2541cd"), "{out}");
        assert!(out.contains("keep%2541"), "{out}");
        assert!(serde_json::from_str::<Value>(&out).is_ok(), "{out}");
    }

    #[test]
    fn a_secret_wrapped_over_many_short_lines_or_chunks_is_one_span() {
        let secret = "sk-AbC123xyZ";
        let m = m(&[("KEY", secret)]);
        let run = |chunks: &[&str]| {
            let mut s = Stream::lines();
            let mut out = String::new();
            for c in chunks {
                out.push_str(&s.push_with(&m, c));
            }
            out + &s.finish_with(&m)
        };
        assert_eq!(run(&["sk-\n", "AbC\n", "123xyZ\n"]), "[secret:KEY]\n\n\n");
        assert_eq!(
            run(&["a s", "k-A", "b", "C1\r\n2", "3x", "yZ b\n"]),
            "a [secret:KEY]\r\n b\n"
        );
        let text = "err: sk-AbC123xyZ done\n";
        let chars: Vec<String> = text.chars().map(String::from).collect();
        let pieces: Vec<&str> = chars.iter().map(String::as_str).collect();
        assert_eq!(run(&pieces), "err: [secret:KEY] done\n");
        // The excerpt of a child's stderr uses the same stream.
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!("tp-wrap-{}", std::process::id()));
        let _data = crate::registry::DataDirOverride::set(&dir);
        register("wrap-srv", "KEY", secret);
        assert_eq!(
            redact_excerpt("a sk-\nAbC\n123xyZ b\n"),
            "a [secret:KEY]\n\n b\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_end_of_a_stream_keeps_a_short_tail_and_removes_a_cut_secret() {
        let m = m(&[("KEY", "sk-AbC123xyZ")]);
        let end = |text: &str| {
            let mut s = Stream::lines();
            let out = s.push_with(&m, text);
            out + &s.finish_with(&m)
        };
        // Shorter than any piece the dangling-edge rule removes: kept.
        assert_eq!(end("ends with sk-"), "ends with sk-");
        // A longer start of the secret is removed.
        assert_eq!(end("ends with sk-AbC12"), "ends with [secret:KEY]");
        assert_eq!(end("ordinary words\n"), "ordinary words\n");
    }

    #[test]
    fn the_exemption_is_the_registered_value_in_any_spelling() {
        use serde_json::json;
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!("tp-exempt-id-{}", std::process::id()));
        let _data = crate::registry::DataDirOverride::set(&dir);
        register("srv-x", "TOKEN", "Exempt-Secret-42x");
        register("srv-y", "TOKEN", "Other-Secret-77q");
        // The client sent its value encoded: an echo of it is its own.
        let request = json!({"arguments": {"q": "Exempt%2DSecret%2D42x"}});
        let mut response =
            json!({"text": "got Exempt-Secret-42x, Exempt%2DSecret%2D42x and Other-Secret-77q"});
        redact_outgoing(&mut response, Some(&request));
        assert_eq!(
            response["text"],
            "got Exempt-Secret-42x, Exempt%2DSecret%2D42x and [secret:TOKEN]"
        );
        // It sent the value as written: an encoded echo is exempt too.
        let request = json!({"arguments": {"q": "Exempt-Secret-42x"}});
        let mut response = json!({"text": "as Exempt%2DSecret%2D42x"});
        assert!(!redact_outgoing(&mut response, Some(&request)));
        // A progress stream of that request keeps it the same way.
        let mut s = Stream::whole_for(exempt_values(Some(&request)));
        let out = s.push("echo Exempt-Secret-42x and Other-Secret-77q.") + &s.finish();
        assert_eq!(out, "echo Exempt-Secret-42x and [secret:TOKEN].");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn aliases_have_sixteen_hex_digits_and_a_colliding_one_is_never_advertised() {
        use serde_json::json;
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!("tp-alias16-{}", std::process::id()));
        let _data = crate::registry::DataDirOverride::set(&dir);
        register("alias16", "NAMED", "collide-secret-77Q");
        let tools = || {
            vec![
                json!({"name": "a-collide-secret-77Q"}),
                json!({"name": "b-collide-secret-77Q"}),
            ]
        };
        let mut items = tools();
        let screened = screen_definitions("alias16", Catalog::Tool, &mut items);
        assert_eq!(items.len(), 2);
        for item in &items {
            let name = item["name"].as_str().unwrap();
            let suffix = name.split('~').nth(1).unwrap().trim_end_matches(']');
            assert_eq!(suffix.len(), 16, "{name}");
            assert!(suffix.bytes().all(|b| b.is_ascii_hexdigit()), "{name}");
        }
        assert!(screened.refused.is_empty());
        // Two identifiers whose aliases collide: the second is not advertised,
        // and the first keeps its alias.
        ALIAS_HEX_FOR_TEST.with(|h| *h.borrow_mut() = Some("0".repeat(64)));
        let mut items = tools();
        let screened = screen_definitions("alias16", Catalog::Tool, &mut items);
        ALIAS_HEX_FOR_TEST.with(|h| *h.borrow_mut() = None);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(items.len(), 1, "{items:?}");
        assert_eq!(items[0]["name"], "a-[secret:NAMED~0000000000000000]");
        assert_eq!(screened.refused.len(), 1);
        assert!(
            !screened.refused[0].contains("collide-secret"),
            "{:?}",
            screened.refused
        );
        screened.commit();
    }

    #[test]
    fn without_a_matcher_logs_and_messages_keep_their_structure() {
        use serde_json::json;
        FAIL_BUILDS.with(|f| f.set(true));
        let m = m(&[("KEY", "sk-UnbuiltSecret77")]);
        FAIL_BUILDS.with(|f| f.set(false));
        let line = r#"{"error":"sk-UnbuiltSecret77 rejected","code":5,"weird key!":"x"}"#;
        let out = m.redact_line(line);
        let v: Value = serde_json::from_str(&out).expect("valid JSON");
        assert_eq!(
            v,
            json!({"error": WITHHELD, "code": null, "withheld": WITHHELD})
        );
        let response = json!({"jsonrpc": "2.0", "id": 7, "result": {"text": "sk-UnbuiltSecret77"}});
        assert_eq!(
            withheld_message(&response),
            json!({"jsonrpc": "2.0", "id": 7, "error": {"code": -32603, "message": WITHHELD_RESPONSE}})
        );
        let note = json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": {"message": "sk-UnbuiltSecret77"}});
        let out = withheld_message(&note).to_string();
        assert!(
            out.contains("notifications/message") && !out.contains("Unbuilt"),
            "{out}"
        );
    }

    #[test]
    fn a_jwt_is_matched_by_signature_side_pieces_never_by_its_header() {
        let jwt = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c";
        let m = m(&[("JWT", jwt)]);
        // Another token from the same issuer shares the header: untouched.
        let other =
            "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiJvdGhlciJ9.c2lnbmF0dXJlLW9mLWFub3RoZXI";
        assert_eq!(m.redact_text(other), other);
        // The token itself, and a piece of its signature, are replaced.
        assert_eq!(
            m.redact_text(&format!("Bearer {jwt}")),
            "Bearer [secret:JWT]"
        );
        assert_eq!(
            m.redact_text("tail: wpMeJf36POk6yJV end"),
            "tail: [secret:JWT] end"
        );
    }

    /// One random secret, split into 1 to 6 pieces at random points, each piece
    /// encoded at 0 to 2 levels on its own and set in random innocent text,
    /// through all three sinks: a file scrub (pieces in the string values of a
    /// JSON line, and wrapped over text lines), an outgoing result (pieces in
    /// text leaves under one key, other fields between them) and the stderr
    /// stream (pieces on lines, written in random chunks). No sink may emit a
    /// form of the secret that decodes back to it, and the innocent text must
    /// come back byte for byte.
    ///
    /// Out of reach by design: a piece encoded in base64 or hex on its own (only
    /// the whole value has those forms), a split across a key and a value or
    /// across two JSON records, a split between leaves under different key
    /// names with other text leaves between them, more than four decoding
    /// levels, and a case change of one piece of a value that is not hex or
    /// base32.
    #[test]
    fn a_split_and_encoded_secret_never_leaves_any_sink_and_innocent_text_is_kept() {
        let mut state: u64 = 0x2545_F491_4F6C_DD1D;
        let mut next = move |n: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % n
        };
        const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789-_";
        const WORDS: &[&str] = &[
            "alpha", "beta", "gamma", "delta", "note", "value", "result", "path", "ok", "line",
        ];
        let encode = |c: char, level: u64, form: u64| -> String {
            let once = |text: &str, form: u64| -> String {
                text.chars()
                    .map(|c| match form {
                        0 => format!("%{:02X}", c as u32),
                        1 => format!("\\u{:04x}", c as u32),
                        _ => format!("&#{};", c as u32),
                    })
                    .collect()
            };
            match level {
                0 => c.to_string(),
                1 => once(&c.to_string(), form),
                _ => once(&once(&c.to_string(), form), (form + 1) % 3),
            }
        };
        let decodes_to = |out: &str, secret: &str| {
            out.contains(secret)
                || crate::secret_normalize::levels(out)
                    .iter()
                    .any(|l| l.text.contains(secret))
        };
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!("tp-soundness-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for case in 0..500 {
            let len = 12 + next(13) as usize;
            let secret: String = (0..len)
                .map(|_| ALPHABET[next(ALPHABET.len() as u64) as usize] as char)
                .collect();
            let m = Matcher::new(&[("S", secret.as_str())]);
            let n = 1 + next(6) as usize;
            let mut cuts: Vec<usize> = (0..n - 1)
                .map(|_| 1 + next(len as u64 - 1) as usize)
                .collect();
            cuts.sort_unstable();
            cuts.dedup();
            let mut bounds = vec![0];
            bounds.extend(cuts);
            bounds.push(len);
            let chars: Vec<char> = secret.chars().collect();
            let pieces: Vec<String> = bounds
                .windows(2)
                .map(|w| {
                    let level = next(3);
                    let form = next(3);
                    chars[w[0]..w[1]]
                        .iter()
                        .map(|&c| encode(c, level, form))
                        .collect()
                })
                .collect();
            let word = |next: &mut dyn FnMut(u64) -> u64| WORDS[next(WORDS.len() as u64) as usize];
            let before = format!("{} {} ", word(&mut next), word(&mut next));
            let after = format!(" {} {}", word(&mut next), word(&mut next));
            let ctx = format!("case {case} secret {secret} pieces {pieces:?}");

            // Outgoing result: pieces in "text" leaves, with "type" between.
            let mut texts: Vec<String> = pieces.clone();
            texts[0] = format!("{before}{}", texts[0]);
            let last = texts.len() - 1;
            texts[last] = format!("{}{after}", texts[last]);
            let content: Vec<Value> = texts
                .iter()
                .map(|t| serde_json::json!({"type": "text", "text": t}))
                .collect();
            let mut result = serde_json::json!({"content": content, "note": "innocent words"});
            m.value_with(&mut result, false, None);
            let joined: String = result["content"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| c["text"].as_str().unwrap().to_string())
                .collect();
            assert!(!decodes_to(&joined, &secret), "result: {joined} {ctx}");
            assert!(
                joined.starts_with(&before) && joined.ends_with(&after),
                "result: {joined} {ctx}"
            );
            assert_eq!(result["note"], "innocent words");

            // Stderr stream: one piece per line, written in random chunks.
            let stderr = format!("{before}{}{after}\nnext line\n", pieces.join("\n"));
            let mut stream = Stream::lines();
            let mut out = String::new();
            let bytes = stderr.as_bytes();
            let mut at = 0;
            while at < bytes.len() {
                let mut to = (at + 1 + next(9) as usize).min(bytes.len());
                while !stderr.is_char_boundary(to) {
                    to += 1;
                }
                out.push_str(&stream.push_with(&m, &stderr[at..to]));
                at = to;
            }
            out.push_str(&stream.finish_with(&m));
            let flat = out.replace('\n', "");
            assert!(!decodes_to(&flat, &secret), "stderr: {out:?} {ctx}");
            assert!(
                out.starts_with(&before) && out.ends_with(&format!("{after}\nnext line\n")),
                "stderr: {out:?} {ctx}"
            );
            assert_eq!(
                out.matches('\n').count(),
                stderr.matches('\n').count(),
                "{ctx}"
            );

            // File scrub: the pieces as string values of a JSON line, and the
            // same text wrapped over the lines of a text log.
            let values: Vec<Value> = texts.iter().map(|t| Value::String(t.clone())).collect();
            let line = serde_json::json!({"parts": values, "id": case}).to_string();
            let audit = dir.join("audit.jsonl");
            std::fs::write(&audit, format!("{line}\n{{\"plain\":\"innocent\"}}\n")).unwrap();
            let log = dir.join("gateway.log");
            std::fs::write(&log, &stderr).unwrap();
            scrub_file_with(&m, &audit).unwrap();
            scrub_file_with(&m, &log).unwrap();
            let audit_text = std::fs::read_to_string(&audit).unwrap();
            let log_text = std::fs::read_to_string(&log).unwrap();
            let first = audit_text.lines().next().unwrap();
            let parsed: Value =
                serde_json::from_str(first).unwrap_or_else(|e| panic!("{e}: {first} {ctx}"));
            let joined: String = parsed["parts"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p.as_str().unwrap().to_string())
                .collect();
            assert!(!decodes_to(&joined, &secret), "scrub json: {first} {ctx}");
            assert!(
                joined.starts_with(&before) && joined.ends_with(&after),
                "scrub json: {first} {ctx}"
            );
            assert!(audit_text.ends_with("{\"plain\":\"innocent\"}\n"), "{ctx}");
            assert!(
                !decodes_to(&log_text.replace('\n', ""), &secret),
                "scrub log: {log_text:?} {ctx}"
            );
            assert!(
                log_text.starts_with(&before)
                    && log_text.ends_with(&format!("{after}\nnext line\n")),
                "scrub log: {log_text:?} {ctx}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_short_secret_at_the_end_of_one_string_is_a_whole_word() {
        let m = m(&[("PIN", "ab12")]);
        let mut v = serde_json::json!({"a": "code ab12", "b": "next"});
        assert!(m.redact_value(&mut v));
        assert_eq!(v["a"], "code [secret:PIN]");
        assert_eq!(v["b"], "next");
        let line = r#"{"a":"code ab12","b":"next"}"#;
        assert_eq!(
            m.redact_line(line),
            r#"{"a":"code [secret:PIN]","b":"next"}"#
        );
        // Inside a word it is still not one.
        assert_eq!(m.redact_text("xab12y"), "xab12y");
    }

    /// The benchmark's rebuild wait: false while the build is still due, true
    /// once the main matcher covers every registered value.
    #[test]
    fn the_main_build_wait_returns_once_every_value_is_covered() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!("tp-main-wait-{}", std::process::id()));
        let _data = crate::registry::DataDirOverride::set(&dir);
        for i in 0..50 {
            register(
                "wait-srv",
                &format!("K{i}"),
                &format!("main-wait-value-{i:03}-Xq7"),
            );
        }
        assert!(wait_for_main_build(std::time::Duration::from_secs(30)));
        {
            let guard = REGISTRY.read().unwrap();
            let reg = guard.as_ref().unwrap().get(&scope_now()).unwrap();
            assert_eq!(reg.main_len, reg.secrets.len());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Failure mode: a request the gateway sends to a client (sampling,
    /// elicitation, roots) was edited in place as one line, id included, so an
    /// id equal to a secret came back changed and the client's answer could
    /// not be matched to it.
    #[test]
    fn a_request_to_the_client_keeps_its_id_and_redacts_its_params() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!("tp-push-id-{}", std::process::id()));
        let _data = crate::registry::DataDirOverride::set(&dir);
        register("push-srv", "PIN", "482193");
        register("push-srv", "KEY", "sk-PushBodySecret77");
        for id in [
            serde_json::json!(482193),
            serde_json::json!("sk-PushBodySecret77"),
        ] {
            let request = serde_json::json!({
                "jsonrpc": "2.0", "id": id, "method": "sampling/createMessage",
                "params": {"messages": [{"content": {"type": "text", "text": "key sk-PushBodySecret77 pin 482193"}}]}
            });
            let out: Value =
                serde_json::from_str(&redact_outgoing_json(request.to_string())).unwrap();
            assert_eq!(out["id"], id);
            assert_eq!(out["jsonrpc"], "2.0");
            assert_eq!(
                out["params"]["messages"][0]["content"]["text"],
                "key [secret:KEY] pin [secret:PIN]"
            );
        }
        // A notification has no id to keep, and is redacted as before.
        let note = r#"{"jsonrpc":"2.0","method":"notifications/message","params":{"data":"sk-PushBodySecret77"}}"#;
        assert_eq!(
            redact_outgoing_json(note.to_string()),
            r#"{"jsonrpc":"2.0","method":"notifications/message","params":{"data":"[secret:KEY]"}}"#
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_reply_is_redacted_in_its_result_and_error_never_its_id() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!("tp-reply-id-{}", std::process::id()));
        let _data = crate::registry::DataDirOverride::set(&dir);
        register("reply-srv", "PIN", "482193");
        register("reply-srv", "KEY", "sk-ReplyBodySecret88");
        for id in [
            serde_json::json!(482193),
            serde_json::json!("sk-ReplyBodySecret88"),
        ] {
            let mut ok = serde_json::json!({
                "jsonrpc": "2.0", "id": id,
                "result": {"content": [{"type": "text", "text": "pin 482193 key sk-ReplyBodySecret88"}]}
            });
            assert!(redact_reply(&mut ok, None));
            assert_eq!(ok["id"], id);
            assert_eq!(
                ok["result"]["content"][0]["text"],
                "pin [secret:PIN] key [secret:KEY]"
            );
            let mut failed = serde_json::json!({
                "jsonrpc": "2.0", "id": id,
                "error": {"code": -32000, "message": "bad key sk-ReplyBodySecret88"}
            });
            assert!(redact_reply(&mut failed, None));
            assert_eq!(failed["id"], id);
            assert_eq!(failed["error"]["message"], "bad key [secret:KEY]");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_text_line_that_starts_with_a_bracket_is_searched_as_text() {
        let secret = "sk-SECRETVALUE1234";
        let m = m(&[("TOKEN", secret)]);
        let stamped = format!("[2026-11-15 10:00:00] upstream said token {secret} rejected\n");
        for line in [
            stamped.clone(),
            format!("{{{secret}}}"),
            format!("[warn] {secret}"),
        ] {
            let out = m.redact_line(&line);
            assert!(!out.contains(secret), "{line} gave {out}");
            assert!(out.contains("[secret:TOKEN]"), "{out}");
        }
        // JSON is still edited in place.
        assert_eq!(
            m.redact_line(&format!(r#"{{"a":[1,true,null,"{secret}"]}}"#)),
            r#"{"a":[1,true,null,"[secret:TOKEN]"]}"#
        );
        // The rewrite scrub and the in-place mask of daemon.log.
        let mut out = Vec::new();
        let mut scrub = ScrubLines::new(&m, false);
        scrub.feed(Some(stamped.as_bytes()), &mut out);
        scrub.feed(None, &mut out);
        let out = String::from_utf8(out).unwrap();
        assert!(!out.contains(secret), "{out}");
        assert!(out.starts_with("[2026-11-15 10:00:00] upstream"), "{out}");
        let dir = std::env::temp_dir().join(format!("tp-bracket-mask-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("daemon.log");
        std::fs::write(&log, &stamped).unwrap();
        let masked = mask_locked(&m, &dir, &log).unwrap();
        let text = std::fs::read_to_string(&log).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(masked);
        assert!(!text.contains(secret), "{text}");
        assert_eq!(text.len(), stamped.len());
    }

    #[test]
    fn a_secret_in_a_json_key_is_redacted_whatever_the_values_hold() {
        let m = m(&[("TOKEN", "abababab")]);
        for line in [
            r#"{"abababab":"ab"}"#,
            r#"{"abababab":1}"#,
            r#"{"abababab":"abababab"}"#,
        ] {
            let out = m.redact_line(line);
            assert!(!out.contains("abababab"), "{line} gave {out}");
            assert!(serde_json::from_str::<Value>(&out).is_ok(), "{out}");
            // The rewrite scrub takes the same path.
            let mut scrubbed = Vec::new();
            let mut scrub = ScrubLines::new(&m, false);
            scrub.feed(Some(line.as_bytes()), &mut scrubbed);
            scrub.feed(None, &mut scrubbed);
            let scrubbed = String::from_utf8(scrubbed).unwrap();
            assert!(!scrubbed.contains("abababab"), "{line} scrubbed {scrubbed}");
        }
        assert!(m.redact_line(r#"{"abababab":"ab"}"#).contains(r#""ab""#));
        assert_eq!(
            m.redact_line(r#"{"k":"abababab"}"#),
            r#"{"k":"[secret:TOKEN]"}"#
        );
    }

    /// Best of three runs, so one slow scheduling slice does not decide it.
    fn best_of_three(mut run: impl FnMut()) -> std::time::Duration {
        (0..3)
            .map(|_| {
                let start = std::time::Instant::now();
                run();
                start.elapsed()
            })
            .min()
            .unwrap()
    }

    #[test]
    fn many_distinct_json_keys_cost_linear_work() {
        let m = m(&[("KEY", "sk-ManyKeysSecret42")]);
        let object = |n: usize| {
            let map: serde_json::Map<String, Value> = (0..n)
                .map(|i| (format!("key{i}"), Value::String(format!("value {i}"))))
                .collect();
            Value::Object(map)
        };
        let (small, large) = (object(2_000), object(20_000));
        let (small_line, large_line) = (small.to_string(), large.to_string());
        let line = |text: &str| {
            best_of_three(|| {
                std::hint::black_box(m.redact_line(text));
            })
        };
        let parsed = |v: &Value| {
            best_of_three(|| {
                let mut v = v.clone();
                std::hint::black_box(m.redact_value(&mut v));
            })
        };
        // Linear work is ten times as much; the old per-class scan was 100.
        let (a, b) = (line(&small_line), line(&large_line));
        assert!(b < a * 40, "JSON line: 2,000 keys {a:?}, 20,000 keys {b:?}");
        let (a, b) = (parsed(&small), parsed(&large));
        assert!(b < a * 40, "parsed: 2,000 keys {a:?}, 20,000 keys {b:?}");
    }

    #[test]
    fn the_held_tail_of_a_long_secret_is_found_in_linear_work() {
        // No secret in the text: every char of the window is a candidate tail.
        let text: String = (0..65_536)
            .map(|i| {
                if i % 61 == 60 {
                    ' '
                } else {
                    (b'a' + (i % 26) as u8) as char
                }
            })
            .collect();
        let secret = |n: usize| -> String {
            (0..n)
                .map(|i| b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"[(i * 7 + 3) % 36] as char)
                .collect()
        };
        let cost = |n: usize| {
            let m = Matcher::new(&[("K", secret(n).as_str())]);
            best_of_three(|| {
                std::hint::black_box(m.hold_from(&text));
            })
        };
        // The window grows with the secret: linear is ten times, a copy of
        // each tail a hundred.
        let (a, b) = (cost(1_000), cost(10_000));
        assert!(b < a * 40, "1,000 chars {a:?}, 10,000 chars {b:?}");
    }

    #[test]
    fn many_leaves_holding_a_secret_cost_linear_work() {
        let m = m(&[("TOKEN", "sk-SECRETVALUE1234")]);
        let doc = |n: usize| {
            let items: Vec<Value> = (0..n)
                .map(|_| Value::from("x sk-SECRETVALUE1234 y"))
                .collect();
            serde_json::json!({ "content": items })
        };
        let (small, large) = (doc(4_000), doc(40_000));
        let (small_line, large_line) = (small.to_string(), large.to_string());
        let parsed = |v: &Value| {
            best_of_three(|| {
                let mut v = v.clone();
                assert!(m.redact_value(&mut v));
            })
        };
        let line = |text: &str| {
            best_of_three(|| {
                assert!(!m.redact_line(text).contains("SECRETVALUE"));
            })
        };
        // Linear work is ten times as much; a scan of every span per leaf
        // (or every leaf per span) is a hundred.
        let (a, b) = (parsed(&small), parsed(&large));
        assert!(
            b < a * 40,
            "parsed: 4,000 leaves {a:?}, 40,000 leaves {b:?}"
        );
        let (a, b) = (line(&small_line), line(&large_line));
        assert!(
            b < a * 40,
            "JSON line: 4,000 leaves {a:?}, 40,000 leaves {b:?}"
        );
    }

    #[test]
    fn line_endings_after_a_held_start_keep_their_place_when_it_is_no_secret() {
        let m = m(&[("TOKEN", "sk-SECRETVALUE1234")]);
        let run = |pieces: &[&str]| {
            let mut s = Stream::lines();
            let mut out = String::new();
            for p in pieces {
                out.push_str(&s.push_with(&m, p));
            }
            out + &s.finish_with(&m)
        };
        // "s" is held until "!" shows it is not the secret: nothing after it
        // goes out first.
        let blank = "\n".repeat(65_536);
        assert!(
            run(&["s", &blank, "!"]) == format!("s{blank}!"),
            "reordered"
        );
        let one_by_one: Vec<&str> = std::iter::once("s")
            .chain(std::iter::repeat_n("\n", 65_536))
            .chain(std::iter::once("!"))
            .collect();
        assert!(run(&one_by_one) == format!("s{blank}!"), "reordered");
        // Mixed endings keep their exact bytes.
        let mixed = format!(
            "{}{}{}\r\r",
            "\r\n".repeat(1_000),
            "\n".repeat(1_000),
            "\r\n".repeat(1_000)
        );
        assert!(
            run(&["x s", &mixed, "!"]) == format!("x s{mixed}!"),
            "bytes changed"
        );
        // Endings that alternate kind make a run each time: bounded, and every
        // byte of them is still written.
        let mut s = Stream::lines();
        let mut out = s.push_with(&m, "x sk-SECRE");
        let mut most = 0;
        for _ in 0..10_000 {
            out.push_str(&s.push_with(&m, "\n\r\r"));
            most = most.max(s.held_bytes());
        }
        out.push_str(&s.finish_with(&m));
        assert!(most <= 4 * MAX_HELD_RUNS, "held {most} bytes");
        assert_eq!(out.matches('\n').count(), 10_000);
        assert_eq!(out.matches('\r').count(), 20_000);
        assert!(!out.contains("sk-SECRE"), "{}", &out[..40]);
    }

    /// A tool cache write in flight is not scrubbed: its writer renames it into
    /// place, and the scrub's lock on it was left behind after data removal.
    #[test]
    fn the_scrub_skips_temp_files_of_writes_in_flight() {
        let dir = std::env::temp_dir().join(format!("tp-scrub-tmp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cache = format!("tool-cache-v2-{}.json", "c".repeat(64));
        for name in [cache.clone(), format!("{cache}.7842.4.conduit-tmp")] {
            std::fs::write(dir.join(name), "{}").unwrap();
        }
        let files = persisted_log_files(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(files.contains(&dir.join(&cache)));
        assert!(
            !files
                .iter()
                .any(|path| path.to_string_lossy().ends_with(".conduit-tmp")),
            "{files:?}"
        );
    }

    /// A temp copy a crashed writer left could hold a secret the scrub skips:
    /// the scrub deletes it, with its lock, and keeps a copy still in flight.
    #[test]
    fn the_scrub_deletes_temp_copies_a_crashed_writer_left() {
        let dir = std::env::temp_dir().join(format!("tp-scrub-orphan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cache = format!("tool-cache-v2-{}.json", "c".repeat(64));
        // No process has this pid (above Linux's pid_max and any Windows pid in use here).
        let gone = 4_194_399u32;
        let orphan = format!("{cache}.{gone}.4.conduit-tmp");
        let live = format!("{cache}.{}.5.conduit-tmp", std::process::id());
        // A running pid on a copy far older than any write: the pid was reused.
        let reused = format!("{cache}.{}.6.conduit-tmp", std::process::id());
        let other = format!("registry.json.{gone}.1.conduit-tmp");
        // No scrubbed file itself, so the scrub stops after the cleanup and
        // takes no lock of its own here.
        for name in [&orphan, &format!("{orphan}.lock"), &live, &reused, &other] {
            std::fs::write(dir.join(name), "sk-OrphanedTempSecret1").unwrap();
        }
        let hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        std::fs::File::options()
            .write(true)
            .open(dir.join(&reused))
            .unwrap()
            .set_modified(hour_ago)
            .unwrap();
        assert_eq!(scrub_data_dir(&dir).unwrap(), 0);
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        let _ = std::fs::remove_dir_all(&dir);
        let mut expected = vec![live, other];
        expected.sort();
        assert_eq!(left, expected);
    }

    /// A copy the cleanup cannot delete keeps the scrub unfinished.
    #[cfg(unix)]
    #[test]
    fn a_temp_copy_that_cannot_be_deleted_leaves_the_scrub_unfinished() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("tp-scrub-stuck-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let orphan = format!(
            "tool-cache-v2-{}.json.4194399.4.conduit-tmp",
            "c".repeat(64)
        );
        std::fs::write(dir.join(&orphan), "sk-OrphanedTempSecret1").unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let result = scrub_data_dir(&dir);
        let still_there = dir.join(&orphan).exists();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        // Root may delete it anyway; then the scrub may finish.
        if still_there {
            assert!(result.is_err(), "{result:?}");
        }
    }

    #[test]
    fn blank_lines_after_a_live_prefix_cost_linear_work_and_are_capped() {
        let m = m(&[("KEY", "sk-AbC123xyZ")]);
        // Pushes of sixteen blank lines: 32 KiB, then 320 KiB, past the cap.
        let blank_lines = |n: usize| {
            let lines = "\n".repeat(16);
            best_of_three(|| {
                let mut s = Stream::lines();
                let mut out = s.push_with(&m, "x sk-AbC");
                for _ in 0..n {
                    out.push_str(&s.push_with(&m, &lines));
                }
                std::hint::black_box(out + &s.finish_with(&m));
            })
        };
        let (a, b) = (blank_lines(2_000), blank_lines(20_000));
        assert!(b < a * 40, "2,000 pushes {a:?}, 20,000 pushes {b:?}");

        // Past the cap the separators are written and only the live prefix
        // stays held, so the secret is still matched whole when it ends.
        let mut s = Stream::lines();
        let mut out = s.push_with(&m, "x sk-AbC");
        let mut most = 0;
        for _ in 0..MAX_HELD_BYTES + 10 {
            out.push_str(&s.push_with(&m, "\n"));
            most = most.max(s.held_bytes());
        }
        out.push_str(&s.push_with(&m, "123xyZ end\n"));
        out.push_str(&s.finish_with(&m));
        assert!(most <= MAX_HELD_BYTES, "held {most} bytes");
        assert!(
            !out.contains("sk-AbC") && !out.contains("123xyZ"),
            "{}",
            out.replace('\n', "")
        );
        assert_eq!(out.replace('\n', ""), "x [secret:KEY] end");
        assert_eq!(out.matches('\n').count(), MAX_HELD_BYTES + 11);
        // A secret with no fragment window over its end: the rest is gone too.
        let m2 = Matcher::new(&[("PASS", "correcthorsebatterystaple")]);
        let mut s = Stream::lines();
        let mut out = s.push_with(&m2, "token correcthorse");
        for _ in 0..70 {
            out.push_str(&s.push_with(&m2, &"\n".repeat(1_000)));
        }
        out.push_str(&s.push_with(&m2, "batterystaple end\n"));
        out.push_str(&s.finish_with(&m2));
        assert_eq!(out.replace('\n', ""), "token [secret:PASS] end");
        assert_eq!(out.matches('\n').count(), 70_001);

        // Within the cap a wrapped secret is still one span.
        let mut s = Stream::lines();
        let mut out = s.push_with(&m, "x sk-AbC");
        for _ in 0..1_000 {
            out.push_str(&s.push_with(&m, "\n"));
        }
        out.push_str(&s.push_with(&m, "123xyZ\n"));
        out.push_str(&s.finish_with(&m));
        assert_eq!(out, format!("x [secret:KEY]{}", "\n".repeat(1_001)));
    }
}
