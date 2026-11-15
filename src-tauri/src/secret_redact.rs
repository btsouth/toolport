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
    whole_token: bool,
    /// A [`fragment_windows`] window, not a whole value or encoding of one.
    fragment: bool,
}

/// An immutable set of secrets compiled for matching. Pure, so the matching rules
/// are unit-testable without touching the process-global registry.
pub struct Matcher {
    names: Vec<String>,
    /// Raw-text forms, for log text and JSON string values after parsing.
    raw: Option<AhoCorasick>,
    raw_patterns: Vec<(String, Pattern)>,
    /// Forms matched in any ASCII case: the hex of every secret, and a secret
    /// that is itself hex or base32, which tools print in either case or mixed.
    ci: Option<AhoCorasick>,
    ci_patterns: Vec<Pattern>,
    /// The same forms as they appear inside a serialized JSON string, used to decide
    /// cheaply whether a JSON line needs parsing at all.
    json: Option<AhoCorasick>,
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
    exact: HashMap<String, usize>,
    /// Every whole form, sorted (raw), and every case-free form lowercased and
    /// sorted: what [`Matcher::hold_from`] checks a stream's tail against.
    prefixes: Vec<String>,
    ci_prefixes: Vec<String>,
    /// Longest entry of either list, in bytes.
    prefix_reach: usize,
    /// A larger matcher searched together with this one, its names numbered
    /// after this one's: the process registry's main matcher, while this one
    /// holds only the values registered since it was built. Hits from both are
    /// merged in one pass, exactly as if they were one matcher. Never nested.
    extra: Option<Arc<Matcher>>,
}

/// `(start, end, name, fragment)`: one automaton hit.
type Hit = (usize, usize, usize, bool);

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
/// byte offset inside a longer base64 blob. Sixteen characters carry 96 bits of
/// the secret, so a coincidental match in unrelated base64 is not a real risk;
/// shorter inner strings would start matching random blobs. A secret too short
/// to reach it (under 13 bytes) is still matched in its whole-value encodings,
/// which is how an upstream that encodes the bare secret writes it.
pub const MIN_BASE64_INNER_CHARS: usize = 16;

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
fn base64_inner(value: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    for k in 0..3usize {
        let mut shifted = vec![0u8; k];
        shifted.extend_from_slice(value);
        let start = (8 * k).div_ceil(6);
        let end = 8 * (k + value.len()) / 6;
        if end <= start || end - start < MIN_BASE64_INNER_CHARS {
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

/// Length of the windows [`fragment_windows`] takes from a secret.
pub const FRAGMENT_CHARS: usize = 8;
/// Secrets at least this long are also matched by fragment.
pub const FRAGMENT_MIN_SECRET_CHARS: usize = 12;

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
fn fragment_windows(value: &str) -> Vec<String> {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() < FRAGMENT_MIN_SECRET_CHARS || is_container(value) {
        return Vec::new();
    }
    chars
        .windows(FRAGMENT_CHARS)
        .filter(|window| {
            let classes = [
                window.iter().any(|c| c.is_lowercase()),
                window.iter().any(|c| c.is_uppercase()),
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
        for line in value.lines().map(str::trim) {
            if line.chars().count() >= MIN_BASE64_INNER_CHARS {
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

fn variants(value: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let push = |v: String, out: &mut Vec<String>| {
        if !v.is_empty() && !out.contains(&v) {
            out.push(v);
        }
    };
    for form in text_forms(value) {
        push(form.clone(), &mut out);
        let percent = urlencoding::encode(&form).into_owned();
        push(percent.to_ascii_lowercase_hex(), &mut out);
        push(percent, &mut out);
        push(
            url::form_urlencoded::byte_serialize(form.as_bytes()).collect(),
            &mut out,
        );
        let hex: String = form.bytes().map(|b| format!("{b:02x}")).collect();
        push(hex.to_ascii_uppercase(), &mut out);
        push(hex, &mut out);
        let once = json_escaped(&form);
        push(json_escaped(&once), &mut out);
        push(once, &mut out);
        for engine in BASE64_ENGINES {
            push(engine.encode(form.as_bytes()), &mut out);
        }
    }
    for engine in BASE64_ENGINES {
        push(engine.encode(format!(":{value}")), &mut out);
    }
    for inner in base64_inner(value.as_bytes()) {
        push(inner, &mut out);
    }
    out
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
    AhoCorasickBuilder::new()
        .match_kind(MatchKind::Standard)
        .kind(Some(aho_corasick::AhoCorasickKind::ContiguousNFA))
        .build(patterns)
        .ok()
}

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
        let mut names: Vec<String> = Vec::new();
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
            // A short secret keeps the whole-token rule in every encoding: its hex or
            // base64 is long enough to look specific but is still only a few bytes.
            let short = value.chars().count() < SHORT_SECRET_CHARS;
            for variant in variants(value) {
                if seen.insert(variant.clone()) {
                    let whole_token = short || variant.chars().count() < SHORT_SECRET_CHARS;
                    raw_patterns.push((
                        variant,
                        Pattern {
                            name,
                            whole_token,
                            fragment: false,
                        },
                    ));
                }
            }
            let windows = if whole_only.contains(value) {
                Vec::new()
            } else {
                fragment_windows(value)
            };
            for window in windows {
                if seen.insert(window.clone()) {
                    raw_patterns.push((
                        window,
                        Pattern {
                            name,
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
                            whole_token: short,
                            fragment: false,
                        },
                    ));
                }
            }
        }
        let raw_texts: Vec<String> = raw_patterns.iter().map(|(p, _)| p.clone()).collect();
        let json_texts: Vec<String> = raw_texts.iter().map(|p| json_escaped(p)).collect();
        let longest_chars = raw_texts
            .iter()
            .map(|p| p.chars().count())
            .max()
            .unwrap_or(0);
        let edge_patterns = || {
            raw_patterns.iter().enumerate().filter(|(_, (text, meta))| {
                !meta.whole_token && !meta.fragment && text.chars().count() > SHORT_SECRET_CHARS
            })
        };
        let heads = edge_index(
            edge_patterns()
                .map(|(i, (text, _))| (text.chars().take(SHORT_SECRET_CHARS).collect(), i))
                .collect(),
        );
        let tails = edge_index(
            edge_patterns()
                .map(|(i, (text, _))| {
                    let skip = text.chars().count() - SHORT_SECRET_CHARS;
                    (text.chars().skip(skip).collect(), i)
                })
                .collect(),
        );
        let mut exact = std::collections::HashMap::new();
        for (name, value) in secrets {
            let value = value.as_ref();
            if value.trim().is_empty() {
                continue;
            }
            let marker = marker_name(name.as_ref());
            if let Some(n) = names.iter().position(|m| *m == marker) {
                exact.entry(value.to_string()).or_insert(n);
                exact.entry(value.trim().to_string()).or_insert(n);
            }
        }
        let ci_texts: Vec<String> = ci_patterns.iter().map(|(t, _)| t.clone()).collect();
        let ci = (!ci_texts.is_empty())
            .then(|| {
                AhoCorasickBuilder::new()
                    .match_kind(MatchKind::Standard)
                    .kind(Some(aho_corasick::AhoCorasickKind::ContiguousNFA))
                    .ascii_case_insensitive(true)
                    .build(&ci_texts)
                    .ok()
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
        Self {
            names,
            raw: build(&raw_texts),
            raw_patterns,
            ci,
            ci_patterns: ci_patterns.into_iter().map(|(_, p)| p).collect(),
            json: build(&json_texts),
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

    /// `extra`'s hits for `f`, with names shifted after this matcher's.
    fn extra_hits(&self, f: impl Fn(&Matcher) -> Vec<Hit>) -> Vec<Hit> {
        let base = self.base();
        self.extra.as_ref().map_or_else(Vec::new, |m| {
            f(m).into_iter()
                .map(|(s, e, n, frag)| (s, e, n + base, frag))
                .collect()
        })
    }

    fn longest(&self) -> usize {
        self.extra.as_ref().map_or(self.longest_chars, |m| {
            m.longest_chars.max(self.longest_chars)
        })
    }

    fn exact_get(&self, text: &str) -> Option<usize> {
        self.exact.get(text).copied().or_else(|| {
            let base = self.base();
            self.extra
                .as_ref()
                .and_then(|m| m.exact.get(text).map(|n| n + base))
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
            + self.json.as_ref().map_or(0, AhoCorasick::memory_usage)
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

    pub fn is_empty(&self) -> bool {
        self.raw.is_none() && self.extra.as_ref().is_none_or(|m| m.is_empty())
    }

    fn marker(&self, names: &[usize]) -> String {
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
    fn spans(&self, text: &str) -> Vec<(usize, usize, Vec<usize>)> {
        self.spans_with(text, true)
    }

    /// [`Self::spans`], optionally without fragment windows (whole forms only).
    fn spans_with(&self, text: &str, fragments: bool) -> Vec<(usize, usize, Vec<usize>)> {
        let mut hits = self.own_hits(text, fragments);
        hits.extend(self.extra_hits(|m| m.own_hits(text, fragments)));
        merge(hits)
    }

    fn own_hits(&self, text: &str, fragments: bool) -> Vec<Hit> {
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
                if before.is_some_and(is_token_char) || after.is_some_and(is_token_char) {
                    continue;
                }
            }
            hits.push((m.start(), m.end(), pattern.name, pattern.fragment));
        }
        if let Some(ci) = &self.ci {
            for m in ci.find_overlapping_iter(text) {
                let pattern = &self.ci_patterns[m.pattern().as_usize()];
                if pattern.whole_token {
                    let before = text[..m.start()].chars().next_back();
                    let after = text[m.end()..].chars().next();
                    if before.is_some_and(is_token_char) || after.is_some_and(is_token_char) {
                        continue;
                    }
                }
                hits.push((m.start(), m.end(), pattern.name, false));
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
    fn byte_spans(&self, bytes: &[u8]) -> Vec<(usize, usize, Vec<usize>)> {
        let mut hits = self.own_byte_hits(bytes);
        hits.extend(self.extra_hits(|m| m.own_byte_hits(bytes)));
        merge(hits)
    }

    fn own_byte_hits(&self, bytes: &[u8]) -> Vec<Hit> {
        let Some(ac) = &self.raw else {
            return Vec::new();
        };
        let token = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80;
        let mut hits = Vec::new();
        for m in ac.find_overlapping_iter(bytes) {
            let pattern = &self.raw_patterns[m.pattern().as_usize()].1;
            if pattern.whole_token {
                let before = m.start().checked_sub(1).map(|i| bytes[i]);
                let after = bytes.get(m.end()).copied();
                if before.is_some_and(token) || after.is_some_and(token) {
                    continue;
                }
            }
            hits.push((m.start(), m.end(), pattern.name, pattern.fragment));
        }
        if let Some(ci) = &self.ci {
            for m in ci.find_overlapping_iter(bytes) {
                let pattern = &self.ci_patterns[m.pattern().as_usize()];
                if pattern.whole_token {
                    let before = m.start().checked_sub(1).map(|i| bytes[i]);
                    let after = bytes.get(m.end()).copied();
                    if before.is_some_and(token) || after.is_some_and(token) {
                        continue;
                    }
                }
                hits.push((m.start(), m.end(), pattern.name, false));
            }
        }
        hits
    }

    /// Where a stream may be cut: the start of the earliest tail of `text` that is
    /// a proper prefix of a whole form (raw, or case-free in any case), moved back
    /// before any complete form that would straddle the cut. Text before it can
    /// be redacted and written now; the rest is held until the stream continues,
    /// so no piece of a secret is written before the stream shows whether the
    /// rest follows. `text.len()` when nothing needs holding.
    pub fn hold_from(&self, text: &str) -> usize {
        let mut cut = self.own_hold(text);
        if let Some(extra) = &self.extra {
            cut = cut.min(extra.own_hold(text));
        }
        if cut == text.len() {
            return cut;
        }
        // A complete form crossing the cut would be written in part.
        let spans = self.spans_with(text, false);
        loop {
            match spans.iter().find(|(s, e, _)| *s < cut && cut < *e) {
                Some((s, _, _)) => cut = *s,
                None => return cut,
            }
        }
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
        let mut lowered = String::new();
        for (i, _) in text[from..].char_indices() {
            let tail = &text[from + i..];
            if live(&self.prefixes, tail) {
                return from + i;
            }
            if !self.ci_prefixes.is_empty() {
                lowered.clear();
                lowered.push_str(tail);
                lowered.make_ascii_lowercase();
                if live(&self.ci_prefixes, &lowered) {
                    return from + i;
                }
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
        let mut spans = self.spans(text);
        let edge = |spans: &mut Vec<(usize, usize, Vec<usize>)>, start: usize, end: usize, n| {
            if start < end && !spans.iter().any(|(s, e, _)| *s <= start && end <= *e) {
                spans.push((start, end, vec![n]));
            }
        };
        for (start, end, name, _) in self
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
            .flat_map(|(s, e, names)| names.into_iter().map(move |n| (s, e, n, false)))
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
                        .then_some((start, meta.name))
                })
            });
            if let Some((start, name)) = found {
                out.push((start, text.len(), name, false));
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
                            .then_some((end, meta.name))
                    })
                })
                .max_by_key(|(end, _)| *end);
            if let Some((end, name)) = found {
                out.push((0, end, name, false));
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
    /// includes [`fragment_windows`] hits; a span whose secrets are all in
    /// `exempt` is left as it is.
    fn text_with(
        &self,
        text: &str,
        fragments: bool,
        exempt: Option<&HashSet<String>>,
    ) -> Option<String> {
        let mut spans = self.spans_with(text, fragments);
        if let Some(exempt) = exempt {
            // Exempt only where the exact value the client sent appears whole:
            // never a different secret that contains it or is contained in it.
            spans.retain(|(s, e, _)| !exempt.contains(&text[*s..*e]));
        }
        (!spans.is_empty()).then(|| self.apply(text, &spans))
    }

    /// [`Self::redact_value`] with the [`Self::text_with`] options: first the
    /// secrets that cross from one text leaf into the next, then each leaf.
    fn value_with(
        &self,
        value: &mut Value,
        fragments: bool,
        exempt: Option<&HashSet<String>>,
    ) -> bool {
        let across = self.redact_across_leaves(value, exempt);
        self.leaves_with(value, fragments, exempt) | across
    }

    fn leaves_with(
        &self,
        value: &mut Value,
        fragments: bool,
        exempt: Option<&HashSet<String>>,
    ) -> bool {
        match value {
            Value::String(s) => match self.text_with(s, fragments, exempt) {
                Some(new) => {
                    *s = new;
                    true
                }
                None => false,
            },
            // A number whose text is a registered value (a numeric PIN or account
            // number) becomes the marker string. Booleans and null carry no value.
            Value::Number(n) => match self
                .number_secret(n)
                .filter(|_| !exempt.is_some_and(|e| e.contains(&n.to_string())))
            {
                Some(name) => {
                    *value = Value::String(self.marker(&[name]));
                    true
                }
                None => false,
            },
            Value::Array(items) => items.iter_mut().fold(false, |changed, item| {
                self.leaves_with(item, fragments, exempt) | changed
            }),
            Value::Object(map) => {
                let mut changed = false;
                for v in map.values_mut() {
                    changed |= self.leaves_with(v, fragments, exempt);
                }
                changed | self.redact_keys(map, exempt)
            }
            _ => false,
        }
    }

    /// A secret split across the text leaves of one value, whatever the nesting
    /// (`[{"text":"sk-ab"},{"text":"cd12"}]`). Leaves are taken in document
    /// order, and object keys are in sorted order in a parsed value, so other
    /// fields can sit between two pieces (`"type":"text"` in every MCP content
    /// item). Two streams are searched: all string leaves, and the leaves under
    /// each key name (array elements share one name). A whole form that crosses
    /// a leaf boundary in either puts its marker where it starts and is removed
    /// from every later leaf of that stream it reaches. Numbers, booleans and
    /// null are not text and are skipped.
    fn redact_across_leaves(&self, value: &mut Value, exempt: Option<&HashSet<String>>) -> bool {
        fn collect<'v>(value: &'v mut Value, class: &str, out: &mut Vec<(String, &'v mut String)>) {
            match value {
                Value::String(s) => out.push((class.to_string(), s)),
                Value::Array(items) => items.iter_mut().for_each(|v| collect(v, "[]", out)),
                Value::Object(map) => map.iter_mut().for_each(|(k, v)| collect(v, k, out)),
                _ => {}
            }
        }
        let mut leaves = Vec::new();
        collect(value, "", &mut leaves);
        if leaves.iter().filter(|(_, l)| !l.is_empty()).count() < 2 {
            return false;
        }
        let mut streams: Vec<Vec<usize>> = vec![(0..leaves.len()).collect()];
        let mut classes: Vec<&str> = leaves.iter().map(|(c, _)| c.as_str()).collect();
        classes.sort_unstable();
        classes.dedup();
        if classes.len() > 1 {
            for class in classes {
                let members: Vec<usize> = (0..leaves.len())
                    .filter(|&i| leaves[i].0 == class && !leaves[i].1.is_empty())
                    .collect();
                if members.len() > 1 {
                    streams.push(members);
                }
            }
        }
        let mut changed = false;
        for stream in streams {
            let mut texts: Vec<&mut String> = Vec::with_capacity(stream.len());
            // Disjoint indices into `leaves`, borrowed mutably together.
            let mut rest: &mut [(String, &mut String)] = &mut leaves;
            let mut base = 0;
            for i in stream {
                let (_, tail) = std::mem::take(&mut rest).split_at_mut(i - base);
                let (one, tail) = tail.split_first_mut().expect("index in range");
                texts.push(&mut *one.1);
                rest = tail;
                base = i + 1;
            }
            changed |= self.redact_stream(&mut texts, exempt);
        }
        changed
    }

    /// One stream of [`Self::redact_across_leaves`].
    fn redact_stream(&self, leaves: &mut [&mut String], exempt: Option<&HashSet<String>>) -> bool {
        let mut joined = String::with_capacity(leaves.iter().map(|l| l.len()).sum());
        let mut starts = Vec::with_capacity(leaves.len());
        for leaf in leaves.iter() {
            starts.push(joined.len());
            joined.push_str(leaf);
        }
        // The last leaf that starts at or before `at` (an empty leaf yields to the
        // one after it).
        let leaf_of = |at: usize| starts.partition_point(|&s| s <= at) - 1;
        let spans: Vec<_> = self
            .spans_with(&joined, false)
            .into_iter()
            .filter(|(s, e, _)| {
                leaf_of(*s) != leaf_of(*e - 1)
                    && !exempt.is_some_and(|x| x.contains(&joined[*s..*e]))
            })
            .collect();
        if spans.is_empty() {
            return false;
        }
        for (i, leaf) in leaves.iter_mut().enumerate() {
            let (ls, le) = (starts[i], starts[i] + leaf.len());
            let mut out = String::new();
            let mut at = ls;
            let mut touched = false;
            for (s, e, names) in spans.iter().filter(|(s, e, _)| *s < le && *e > ls) {
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

    /// `a` and `b` with a secret that crosses from the end of `a` into the start
    /// of `b` replaced: whole forms only, `None` when nothing crosses.
    pub fn redact_pair(
        &self,
        a: &str,
        b: &str,
        exempt: Option<&HashSet<String>>,
    ) -> Option<(String, String)> {
        if a.is_empty() || b.is_empty() {
            return None;
        }
        let reach = self.longest() * 4;
        let mut from = a.len().saturating_sub(reach);
        while !a.is_char_boundary(from) {
            from += 1;
        }
        let mut to = reach.min(b.len());
        while !b.is_char_boundary(to) {
            to += 1;
        }
        let joined = format!("{}{}", &a[from..], &b[..to]);
        let seam = a.len() - from;
        let (start, end, names) =
            self.spans_with(&joined, false)
                .into_iter()
                .find(|(s, e, _)| {
                    *s < seam && *e > seam && !exempt.is_some_and(|x| x.contains(&joined[*s..*e]))
                })?;
        Some((
            format!("{}{}", &a[..from + start], self.marker(&names)),
            b[end - seam..].to_string(),
        ))
    }

    /// The registered values that occur whole, exactly as registered, in the
    /// strings, numbers and keys of `value` (a client's request): the identity
    /// of an exemption is the value itself, never its display name, so two
    /// secrets under one key name never exempt each other.
    fn values_in(&self, value: &Value, out: &mut HashSet<String>) {
        let text = |t: &str, out: &mut HashSet<String>| {
            for (s, e, _) in self.spans_with(t, false) {
                if self.exact_get(&t[s..e]).is_some() {
                    out.insert(t[s..e].to_string());
                }
            }
        };
        match value {
            Value::String(s) => text(s, out),
            Value::Number(n) => {
                if self.exact_get(&n.to_string()).is_some() {
                    out.insert(n.to_string());
                }
            }
            Value::Array(items) => items.iter().for_each(|v| self.values_in(v, out)),
            Value::Object(map) => {
                for (k, v) in map {
                    text(k, out);
                    self.values_in(v, out);
                }
            }
            _ => {}
        }
    }

    /// The secrets whose whole forms occur anywhere in `value` (strings, numbers
    /// and keys), by index.
    fn secrets_in(&self, value: &Value, out: &mut HashSet<usize>) {
        let text = |t: &str, out: &mut HashSet<usize>| {
            for (_, _, names) in self.spans_with(t, false) {
                out.extend(names);
            }
        };
        match value {
            Value::String(s) => text(s, out),
            Value::Number(n) => out.extend(self.number_secret(n)),
            Value::Array(items) => items.iter().for_each(|v| self.secrets_in(v, out)),
            Value::Object(map) => {
                for (k, v) in map {
                    text(k, out);
                    self.secrets_in(v, out);
                }
            }
            _ => {}
        }
    }

    /// An identifier (a tool or prompt name, a resource URI or template, an
    /// argument name) with whole secret forms replaced by aliased markers
    /// (`[secret:NAME~3f9a0c12]`), recorded in `ids` so a call can name the
    /// original. `None` when it holds no secret.
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
            let listed = self.marker(names);
            let base = &listed[..listed.len() - 1];
            let marker = ids.alias(part, text, i, base);
            pieces.push((marker.clone(), text[*start..*end].to_string()));
            out.push_str(&marker);
            at = *end;
        }
        out.push_str(&text[at..]);
        ids.record(part, &out, text, pieces);
        Some(out)
    }

    /// Text that is not an identifier (a description, a title, a default value):
    /// whole forms become plain markers and nothing is recorded.
    fn screen_plain(&self, text: &str) -> Option<String> {
        let spans = self.spans_with(text, false);
        (!spans.is_empty()).then(|| self.apply(text, &spans))
    }

    /// Screen one advertised definition (tool, prompt, resource or template) by
    /// whole forms only. Fragment windows are for error text; in a definition they
    /// would rename ordinary identifiers (`Tools-Read` is a piece of
    /// `sk-Tools-ReadSecret123`), change pins and break calls. Identifier fields
    /// get stable aliases ([`Self::screen_identifier`]); every other string gets
    /// plain markers.
    fn screen_definition(&self, item: &mut Value, ids: &mut IdBuilder) -> bool {
        let Value::Object(map) = item else {
            return self.screen_in(item, Role::Text, ids);
        };
        let catalog = ids.catalog;
        let mut changed = false;
        for (key, v) in map.iter_mut() {
            let role = match (catalog, key.as_str()) {
                (Catalog::Tool | Catalog::Prompt, "name") => Role::Name,
                (Catalog::Resource, "uri") | (Catalog::Template, "uriTemplate") => Role::Name,
                (Catalog::Tool, "inputSchema") => Role::Schema,
                (Catalog::Prompt, "arguments") => Role::PromptArguments,
                _ => Role::Text,
            };
            changed |= self.screen_in(v, role, ids);
        }
        changed | self.screen_keys(map, Role::Text, ids)
    }

    fn screen_in(&self, value: &mut Value, role: Role, ids: &mut IdBuilder) -> bool {
        match value {
            Value::String(s) => {
                let new = match role {
                    Role::Name => self.screen_identifier(s, Part::Name, ids),
                    Role::Argument => self.screen_identifier(s, Part::Argument, ids),
                    _ => self.screen_plain(s),
                };
                match new {
                    Some(new) => {
                        *s = new;
                        true
                    }
                    None => false,
                }
            }
            Value::Number(n) => match self.number_secret(n) {
                Some(name) => {
                    *value = Value::String(self.marker(&[name]));
                    true
                }
                None => false,
            },
            Value::Array(items) => {
                let inner = match role {
                    // `required: ["arg", ...]` names arguments.
                    Role::Required => Role::Argument,
                    Role::PromptArguments => Role::PromptArgument,
                    other => other,
                };
                items.iter_mut().fold(false, |changed, item| {
                    self.screen_in(item, inner, ids) | changed
                })
            }
            Value::Object(map) => {
                let mut changed = false;
                for (key, v) in map.iter_mut() {
                    let inner = match role {
                        Role::Schema if key == "required" => Role::Required,
                        Role::Schema => Role::Schema,
                        Role::PromptArgument if key == "name" => Role::Argument,
                        _ => Role::Text,
                    };
                    changed |= self.screen_in(v, inner, ids);
                }
                changed | self.screen_keys(map, role, ids)
            }
            _ => false,
        }
    }

    /// Object keys of a definition: in an input schema they are argument names
    /// (aliased); anywhere else plain markers, made unique with `#2`, `#3`.
    fn screen_keys(
        &self,
        map: &mut serde_json::Map<String, Value>,
        role: Role,
        ids: &mut IdBuilder,
    ) -> bool {
        let renamed: Vec<(String, String)> = map
            .keys()
            .filter_map(|key| {
                let new = if role == Role::Schema {
                    self.screen_identifier(key, Part::Argument, ids)
                } else {
                    self.screen_plain(key)
                };
                new.map(|new| (key.clone(), new))
            })
            .collect();
        let changed = !renamed.is_empty();
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
        changed
    }

    /// The secret a JSON number spells, by its text or, for an integral value
    /// written another way (`482193.0`, `4.82193e5`), by its integer text.
    fn number_secret(&self, n: &serde_json::Number) -> Option<usize> {
        if !self.has_exact() {
            return None;
        }
        let text = n.to_string();
        if let Some(name) = self.numeric_secret(&text) {
            return Some(name);
        }
        let f = n.as_f64()?;
        (f.fract() == 0.0 && f.abs() < 9.0e15)
            .then(|| self.numeric_secret(&format!("{}", f as i64)))
            .flatten()
    }

    /// A registered value spelled by a JSON number's text, if it has at least
    /// [`MIN_NUMERIC_SECRET_DIGITS`] digits. A shorter one (a port, a small id)
    /// would turn ordinary numbers into strings and change the types a model
    /// reads; those are named in status instead, and still redacted in text.
    fn numeric_secret(&self, text: &str) -> Option<usize> {
        (text.bytes().filter(u8::is_ascii_digit).count() >= MIN_NUMERIC_SECRET_DIGITS)
            .then(|| self.exact_get(text))
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
        let looks_json = body.trim_start().starts_with(['{', '[']);
        if looks_json {
            if !self.may_contain(body) {
                return Cow::Borrowed(line);
            }
            let numbers = self.redact_number_tokens(body);
            let source = numbers.as_deref().unwrap_or(body);
            if let Ok(mut value) = serde_json::from_str::<Value>(source) {
                if !self.value_with(&mut value, fragments, None) && numbers.is_none() {
                    return Cow::Borrowed(line);
                }
                let mut out = value.to_string();
                out.push_str(&line[body.len()..]);
                return Cow::Owned(out);
            }
        }
        // Text, or a torn JSON line. JSON escapes can hide a value from the raw
        // automaton, so try both forms.
        let raw = match self.text_with(line, fragments, None) {
            Some(new) => Cow::Owned(new),
            None => Cow::Borrowed(line),
        };
        let text = raw.as_ref();
        let mut hits = self.own_json_hits(text, fragments);
        hits.extend(self.extra_hits(|m| m.own_json_hits(text, fragments)));
        let spans = merge(hits);
        if spans.is_empty() {
            return raw;
        }
        Cow::Owned(self.apply(raw.as_ref(), &spans))
    }

    /// Hits of the JSON-escaped forms, for text that is not valid JSON.
    fn own_json_hits(&self, text: &str, fragments: bool) -> Vec<Hit> {
        let Some(ac) = &self.json else {
            return Vec::new();
        };
        let mut hits = Vec::new();
        for m in ac.find_overlapping_iter(text) {
            let pattern = &self.raw_patterns[m.pattern().as_usize()].1;
            if pattern.fragment && !fragments {
                continue;
            }
            if pattern.whole_token {
                let before = text[..m.start()].chars().next_back();
                let after = text[m.end()..].chars().next();
                if before.is_some_and(is_token_char) || after.is_some_and(is_token_char) {
                    continue;
                }
            }
            hits.push((m.start(), m.end(), pattern.name, pattern.fragment));
        }
        hits
    }

    /// Cheap pre-check: could `text` hold any registered form, raw or JSON-escaped?
    /// False positives are fine (short secrets ignore token boundaries here).
    pub fn may_contain(&self, text: &str) -> bool {
        self.raw.as_ref().is_some_and(|a| a.is_match(text))
            || self.json.as_ref().is_some_and(|a| a.is_match(text))
            || self.ci.as_ref().is_some_and(|a| a.is_match(text))
            || self.extra.as_ref().is_some_and(|m| m.may_contain(text))
    }
}

/// Merge overlapping hits into spans. A span is named after the whole-value
/// matches inside it; only a span found purely by fragment windows lists the
/// secrets those windows came from.
fn merge(mut hits: Vec<(usize, usize, usize, bool)>) -> Vec<(usize, usize, Vec<usize>)> {
    hits.sort_by_key(|(s, e, _, _)| (*s, std::cmp::Reverse(*e)));
    let mut spans: Vec<(usize, usize, Vec<usize>, Vec<usize>)> = Vec::new();
    for (s, e, n, fragment) in hits {
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

    fn may_contain(&self, text: &str) -> bool {
        self.0.iter().any(|m| m.may_contain(text))
    }

    fn hold_from(&self, text: &str) -> usize {
        self.0
            .iter()
            .map(|m| m.hold_from(text))
            .min()
            .unwrap_or(text.len())
    }

    fn redact_pair(&self, a: &str, b: &str) -> Option<(String, String)> {
        let (mut a, mut b) = (a.to_string(), b.to_string());
        let mut changed = false;
        for m in &self.0 {
            if let Some((na, nb)) = m.redact_pair(&a, &b, None) {
                (a, b) = (na, nb);
                changed = true;
            }
        }
        changed.then_some((a, b))
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
    fn screen(&self, item: &mut Value, ids: &mut IdBuilder) -> Vec<String> {
        let mut names = Vec::new();
        for m in &self.0 {
            let mut found = HashSet::new();
            m.secrets_in(item, &mut found);
            if found.is_empty() {
                continue;
            }
            names.extend(found.into_iter().map(|n| m.name(n).to_string()));
            m.screen_definition(item, ids);
        }
        names.sort();
        names.dedup();
        names
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
    REGISTRY
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()?
        .get(scope)?
        .layers()
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

/// [`Matcher::redact_pair`] against every registered secret: consecutive lines
/// of a stream, checked for a secret broken across the line break.
pub fn redact_pair(a: &str, b: &str) -> Option<(String, String)> {
    current()?.redact_pair(a, b)
}

/// [`Matcher::redact_fragment`] against every registered secret.
pub fn redact_fragment(text: &str) -> Cow<'_, str> {
    match current() {
        Some(m) => m.redact_fragment(text),
        None => Cow::Borrowed(text),
    }
}

/// Text that is an excerpt of a longer stream (the kept tail of a child's
/// stderr): a secret a tool wrapped across a line break is matched across it,
/// and a piece of one cut off at either end of the excerpt is removed.
pub fn redact_excerpt(text: &str) -> String {
    let Some(m) = current() else {
        return text.to_string();
    };
    let mut lines: Vec<String> = text.split_inclusive('\n').map(str::to_string).collect();
    for i in 1..lines.len() {
        let (before, after) = lines.split_at_mut(i);
        let (prev, next) = (&mut before[i - 1], &mut after[0]);
        let body = prev.trim_end_matches(['\n', '\r']);
        let ending = prev[body.len()..].to_string();
        if let Some((a, b)) = m.redact_pair(body, next) {
            *prev = format!("{a}{ending}");
            *next = b;
        }
    }
    m.redact_fragment(&lines.concat()).into_owned()
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
    key: [u8; 32],
    ids: CatalogIds,
    /// Each alias marker handed out, with the identifier and span it stands for.
    owners: HashMap<String, (Part, String, usize)>,
}

impl IdBuilder {
    fn new(server: &str, catalog: Catalog) -> Self {
        Self {
            server: server.to_string(),
            catalog,
            key: alias_key(),
            ids: CatalogIds::default(),
            owners: HashMap::new(),
        }
    }

    /// `base` (`[secret:NAME`) closed with an alias suffix: a keyed hash of the
    /// server, catalog, part, whole original identifier and span position, so the
    /// same original gets the same alias on every run, different originals never
    /// share one, and the suffix says nothing about the secret. Lengthened on
    /// the rare clash within this catalog.
    fn alias(&mut self, part: Part, original: &str, index: usize, base: &str) -> String {
        use hmac::{Hmac, Mac};
        let mut mac =
            Hmac::<sha2::Sha256>::new_from_slice(&self.key).expect("HMAC takes any key length");
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
        let mut digits = 8;
        loop {
            let marker = format!("{base}~{}]", &hex[..digits]);
            let owner = (part, original.to_string(), index);
            // One identifier listed twice (a key and in `required`) is the same
            // owner and gets the same alias.
            let clash = self.owners.get(&marker).is_some_and(|o| *o != owner);
            if !clash || digits == hex.len() {
                self.owners.insert(marker.clone(), owner);
                return marker;
            }
            digits += 8;
        }
    }

    fn record(
        &mut self,
        part: Part,
        advertised: &str,
        original: &str,
        pieces: Vec<(String, String)>,
    ) {
        let table = match part {
            Part::Name => &mut self.ids.names,
            Part::Argument => &mut self.ids.arguments,
        };
        table.insert(advertised.to_string(), original.to_string());
        self.ids.pieces.extend(pieces);
    }
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
pub fn screen_definitions(server_id: &str, catalog: Catalog, items: &mut [Value]) -> Screened {
    let mut ids = IdBuilder::new(server_id, catalog);
    let mut flagged = Vec::new();
    if let Some(matcher) = current() {
        for item in items.iter_mut() {
            let names = matcher.screen(item, &mut ids);
            if names.is_empty() {
                continue;
            }
            let name = ["name", "uri", "uriTemplate"]
                .iter()
                .find_map(|k| item.get(*k).and_then(Value::as_str))
                .unwrap_or("")
                .to_string();
            flagged.push((name, names));
        }
    }
    Screened {
        flagged,
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

/// [`Matcher::hold_from`] with the current secrets.
pub fn hold_from(text: &str) -> usize {
    match current() {
        Some(m) => m.hold_from(text),
        None => text.len(),
    }
}

/// Redaction for text written in chunks (a relayed stream, a child's stderr,
/// the messages of one progress stream): [`Self::push`] returns the part that
/// is safe to write now and holds the tail that may be the start of a secret,
/// until the next chunk shows whether the rest follows (the held text is then
/// matched together with it) or [`Self::finish`] ends the stream.
#[derive(Debug)]
pub struct Stream {
    held: String,
    fragments: bool,
}

impl Default for Stream {
    fn default() -> Self {
        Self::new()
    }
}

impl Stream {
    /// A log stream: fragment windows apply, as in [`redact_text`].
    pub fn new() -> Self {
        Self {
            held: String::new(),
            fragments: true,
        }
    }

    /// A stream of message text bound for a client: whole forms only, as in
    /// [`redact_outgoing`], so ordinary words in content are never renamed.
    pub fn whole() -> Self {
        Self {
            held: String::new(),
            fragments: false,
        }
    }

    /// `chunk` appended to the held tail; the redacted part to write now.
    pub fn push(&mut self, chunk: &str) -> String {
        self.held.push_str(chunk);
        let cut = hold_from(&self.held);
        let rest = self.held.split_off(cut);
        let ready = std::mem::replace(&mut self.held, rest);
        let Some(m) = current() else {
            return ready;
        };
        if self.fragments {
            m.redact_text(&ready).into_owned()
        } else {
            m.fold(&ready, |m, t| m.text_with(t, false, None))
                .into_owned()
        }
    }

    /// Whether a tail is being held.
    pub fn is_holding(&self) -> bool {
        !self.held.is_empty()
    }

    /// The end of the stream: the held tail, redacted, including a piece of a
    /// secret left dangling at its end.
    pub fn finish(&mut self) -> String {
        let held = std::mem::take(&mut self.held);
        redact_fragment(&held).into_owned()
    }
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
    m.value_with(value, false, request)
}

/// [`redact_outgoing`] for a serialized message with no client request behind
/// it. Unchanged (and unparsed) unless it may hold a registered form.
pub fn redact_outgoing_json(json: String) -> String {
    let Some(m) = current() else {
        return json;
    };
    if !m.may_contain(&json) {
        return json;
    }
    match serde_json::from_str::<Value>(&json) {
        Ok(mut value) => {
            if m.value_with(&mut value, false, None) {
                value.to_string()
            } else {
                json
            }
        }
        Err(_) => m.redact_definition_line(&json).into_owned(),
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
            let rotated = [
                "audit.jsonl.",
                "security.jsonl.",
                "gateway.log.",
                "inspect.jsonl.",
                // Tool catalogs and held (quarantined) definitions, which hold
                // upstream definition text.
                "tool-cache",
                "quarantine",
                "team-activity-",
            ]
            .iter()
            .any(|prefix| name.starts_with(prefix) && !name.ends_with(".lock"));
            if rotated && !files.contains(&path) {
                files.push(path);
            }
        }
    }
    files
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
/// are unaffected: nothing moves and the file is never replaced.
fn mask_in_place(matcher: &Matcher, path: &Path) -> std::io::Result<bool> {
    // The daemon's own stderr writes take no lock; this one keeps two scrubbers
    // (or a scrub and a delete-time scrub) from masking the same file at once.
    let _lock = crate::registry::lock_at(path).map_err(std::io::Error::other)?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true);
    // The live daemon holds this file as its stderr handle (append access, the
    // default read/write/delete sharing). Share all three so neither side blocks
    // the other; same-length writes never move what the daemon appends.
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_SHARE_READ: u32 = 0x1;
        const FILE_SHARE_WRITE: u32 = 0x2;
        const FILE_SHARE_DELETE: u32 = 0x4;
        options.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut &file, &mut bytes)?;
    let spans = matcher.byte_spans(&bytes);
    for (start, end, names) in &spans {
        let len = end - start;
        let marker = matcher.marker(names);
        let mut mask: Vec<u8> = if marker.len() <= len {
            marker.into_bytes()
        } else {
            Vec::new()
        };
        mask.resize(len, b'*');
        write_at(&file, &mask, *start as u64)?;
    }
    if !spans.is_empty() {
        file.sync_all()?;
    }
    Ok(!spans.is_empty())
}

#[cfg(unix)]
fn write_at(file: &std::fs::File, bytes: &[u8], offset: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(file, bytes, offset)
}

#[cfg(windows)]
fn write_at(file: &std::fs::File, mut bytes: &[u8], mut offset: u64) -> std::io::Result<()> {
    while !bytes.is_empty() {
        let n = std::os::windows::fs::FileExt::seek_write(file, bytes, offset)?;
        if n == 0 {
            return Err(std::io::ErrorKind::WriteZero.into());
        }
        bytes = &bytes[n..];
        offset += n as u64;
    }
    Ok(())
}

fn scrub_file_with(matcher: &Matcher, path: &Path) -> std::io::Result<bool> {
    if held_open(path) {
        return mask_in_place(matcher, path);
    }
    let _lock = crate::registry::lock_at(path).map_err(std::io::Error::other)?;
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    let text = String::from_utf8_lossy(&bytes);
    if !matcher.may_contain(&text) {
        return Ok(false);
    }
    // A tool catalog is screened like a live one (whole forms only) so a cached
    // identifier never differs from the advertised one.
    let catalog = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("tool-cache") || n.starts_with("quarantine"));
    let mut changed = false;
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let redacted = if catalog {
            matcher.redact_definition_line(line)
        } else {
            matcher.redact_line(line)
        };
        changed |= matches!(redacted, Cow::Owned(_));
        out.push_str(&redacted);
    }
    if !changed {
        return Ok(false);
    }
    // Tool catalogs are replaced whole by their writer without this lock. If one
    // was rewritten since the read, the newer file came from a build that already
    // redacts, so keep it rather than put back an older catalog.
    if std::fs::read(path).map(|now| now != bytes).unwrap_or(true) {
        return Ok(false);
    }
    crate::registry::atomic_write(path, &out).map_err(std::io::Error::other)?;
    Ok(true)
}

/// Scrub every log file in `dir` for secrets not yet scrubbed there, then record
/// their ids (names only, never values) in the marker so later starts skip the work.
pub fn scrub_data_dir(dir: &Path) -> std::io::Result<usize> {
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
            return Ok(0);
        };
        let pending: Vec<(String, String, String)> =
            reg.secrets.get(from..).unwrap_or_default().to_vec();
        (pending, reg.secrets.len())
    };
    if pending.is_empty() {
        let _ = std::fs::remove_file(dir.join(SCRUB_PENDING_FILE));
        return Ok(0);
    }
    let Some(matcher) = complete_in(&scope) else {
        return Ok(0);
    };
    let mut rewritten = 0;
    for path in persisted_log_files(dir) {
        if scrub_file_with(&matcher, &path)? {
            rewritten += 1;
        }
    }
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
    let short: Vec<String> = REGISTRY
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .and_then(|map| map.get(&scope_now()))
        .map(|reg| {
            let mut ids: Vec<String> = reg
                .secrets
                .iter()
                .filter(|(id, _, v)| {
                    !id.starts_with("__toolport_internal__/")
                        && v.trim().chars().count() < SHORT_SECRET_CHARS
                })
                .map(|(id, _, _)| id.clone())
                .collect();
            ids.sort();
            ids.dedup();
            ids
        })
        .unwrap_or_default();
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
        .and_then(|_scrub| {
            for path in persisted_log_files(&dir) {
                scrub_file_with(&matcher, &path)?;
            }
            Ok(())
        });
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
        let _ = std::fs::write(dir.join(SCRUB_PENDING_FILE), b"");
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
        assert!(base64_inner(b"short-Qm7").is_empty());
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
            name.starts_with("[secret:NAMED~") && name.len() == "[secret:NAMED~12345678]".len(),
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
        assert_eq!(
            m.redact_pair("x abcdefghijkl", "1234567890 y", None),
            Some(("x [secret:SPLIT]".to_string(), " y".to_string()))
        );
        assert_eq!(m.redact_pair("abcdefghijkl", "999", None), None);
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
            crate::secret_refs::preload_configured_secrets();
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
}
