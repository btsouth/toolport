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
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};
use base64::Engine as _;
use serde_json::Value;

/// Secrets shorter than this (in chars) are redacted only as a whole token.
pub const SHORT_SECRET_CHARS: usize = 6;
/// Upper bound on remembered values. OAuth access tokens rotate, so a long-lived
/// daemon would otherwise grow this forever; the oldest values drop first.
const MAX_SECRETS: usize = 4096;
/// Records which secrets (by `server/key`, never value) the scrub has covered.
const SCRUB_MARKER: &str = "secret-scrub-v1.json";
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
    exact: std::collections::HashMap<String, usize>,
}

/// Index `keys` (one per pattern) into an automaton over the distinct keys and,
/// per distinct key, the patterns that have it.
fn edge_index(keys: Vec<(String, usize)>) -> Option<(AhoCorasick, Vec<Vec<usize>>)> {
    let mut distinct: Vec<String> = Vec::new();
    let mut owners: Vec<Vec<usize>> = Vec::new();
    for (key, pattern) in keys {
        match distinct.iter().position(|k| *k == key) {
            Some(i) => owners[i].push(pattern),
            None => {
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

fn build(patterns: &[String]) -> Option<AhoCorasick> {
    if patterns.is_empty() {
        return None;
    }
    // A full DFA: every log line is scanned, and with every form and fragment of
    // every secret the default NFA would be several times slower. Falls back to
    // the default if the DFA would exceed the crate's size limit.
    AhoCorasickBuilder::new()
        .match_kind(MatchKind::Standard)
        .kind(Some(aho_corasick::AhoCorasickKind::DFA))
        .build(patterns)
        .or_else(|_| {
            AhoCorasickBuilder::new()
                .match_kind(MatchKind::Standard)
                .build(patterns)
        })
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
        Self {
            names,
            raw: build(&raw_texts),
            raw_patterns,
            json: build(&json_texts),
            longest_chars,
            heads,
            tails,
            exact,
        }
    }

    /// Heap bytes held by the compiled automata, for the performance notes.
    pub fn memory_usage(&self) -> usize {
        let edge = |e: &Option<(AhoCorasick, Vec<Vec<usize>>)>| {
            e.as_ref().map_or(0, |(ac, _)| ac.memory_usage())
        };
        self.raw.as_ref().map_or(0, AhoCorasick::memory_usage)
            + self.json.as_ref().map_or(0, AhoCorasick::memory_usage)
            + edge(&self.heads)
            + edge(&self.tails)
    }

    pub fn is_empty(&self) -> bool {
        self.raw.is_none()
    }

    fn marker(&self, names: &[usize]) -> String {
        let mut listed: Vec<&str> = Vec::new();
        for &n in names {
            let name = self.names[n].as_str();
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
        let Some(ac) = &self.raw else {
            return Vec::new();
        };
        let mut hits: Vec<(usize, usize, usize, bool)> = Vec::new();
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
        merge(hits)
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
        merge(hits)
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
                edge(&mut spans, start, text.len(), name);
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
                edge(&mut spans, 0, end, name);
            }
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

    /// Redact then cut to `max_chars`, so a cut can never leave part of a secret.
    /// Scans only the window that can reach the kept prefix, so a multi-megabyte
    /// error costs no more than a short one.
    pub fn truncate_chars(&self, text: &str, max_chars: usize) -> String {
        let window_chars = max_chars.saturating_add(self.longest_chars);
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
        exempt: Option<&HashSet<usize>>,
    ) -> Option<String> {
        let mut spans = self.spans_with(text, fragments);
        if let Some(exempt) = exempt {
            spans.retain(|(_, _, names)| !names.iter().all(|n| exempt.contains(n)));
        }
        (!spans.is_empty()).then(|| self.apply(text, &spans))
    }

    /// [`Self::redact_value`] with the [`Self::text_with`] options.
    fn value_with(
        &self,
        value: &mut Value,
        fragments: bool,
        exempt: Option<&HashSet<usize>>,
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
                .filter(|name| !exempt.is_some_and(|e| e.contains(name)))
            {
                Some(name) => {
                    *value = Value::String(self.marker(&[name]));
                    true
                }
                None => false,
            },
            Value::Array(items) => items.iter_mut().fold(false, |changed, item| {
                self.value_with(item, fragments, exempt) | changed
            }),
            Value::Object(map) => {
                let mut changed = false;
                for v in map.values_mut() {
                    changed |= self.value_with(v, fragments, exempt);
                }
                changed | self.redact_keys(map, exempt)
            }
            _ => false,
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

    /// `text` with whole secret forms replaced (never fragments), recording each
    /// replaced span as `(marker, original text)` and the whole string as
    /// `(new, old)`.
    fn screen_text(&self, text: &str, record: &mut Vec<(String, String)>) -> Option<String> {
        let spans = self.spans_with(text, false);
        if spans.is_empty() {
            return None;
        }
        for (start, end, names) in &spans {
            record.push((self.marker(names), text[*start..*end].to_string()));
        }
        let new = self.apply(text, &spans);
        if text.len() <= MAX_RECORDED_IDENTIFIER {
            record.push((new.clone(), text.to_string()));
        }
        Some(new)
    }

    /// Screen one advertised definition (tool, prompt, resource or template) by
    /// whole forms only. Fragment windows are for error text; in a definition they
    /// would rename ordinary identifiers (`Tools-Read` is a piece of
    /// `sk-Tools-ReadSecret123`), change pins and break calls.
    fn screen_definition(&self, value: &mut Value, record: &mut Vec<(String, String)>) -> bool {
        match value {
            Value::String(s) => match self.screen_text(s, record) {
                Some(new) => {
                    *s = new;
                    true
                }
                None => false,
            },
            Value::Number(n) => match self.number_secret(n) {
                Some(name) => {
                    *value = Value::String(self.marker(&[name]));
                    true
                }
                None => false,
            },
            Value::Array(items) => items.iter_mut().fold(false, |changed, item| {
                self.screen_definition(item, record) | changed
            }),
            Value::Object(map) => {
                let mut changed = false;
                for v in map.values_mut() {
                    changed |= self.screen_definition(v, record);
                }
                let renamed: Vec<(String, String)> = map
                    .keys()
                    .filter_map(|key| self.screen_text(key, record).map(|new| (key.clone(), new)))
                    .collect();
                for (old, new) in renamed {
                    let Some(v) = map.remove(&old) else { continue };
                    let mut key = new.clone();
                    let mut n = 2;
                    while map.contains_key(&key) {
                        key = format!("{new}#{n}");
                        n += 1;
                    }
                    if key != new {
                        record.push((key.clone(), old));
                    }
                    map.insert(key, v);
                    changed = true;
                }
                changed
            }
            _ => false,
        }
    }

    /// The secret a JSON number spells, by its text or, for an integral value
    /// written another way (`482193.0`, `4.82193e5`), by its integer text.
    fn number_secret(&self, n: &serde_json::Number) -> Option<usize> {
        if self.exact.is_empty() {
            return None;
        }
        if let Some(&name) = self.exact.get(&n.to_string()) {
            return Some(name);
        }
        let f = n.as_f64()?;
        (f.fract() == 0.0 && f.abs() < 9.0e15)
            .then(|| self.exact.get(&format!("{}", f as i64)).copied())
            .flatten()
    }

    /// Replace object keys that contain a whole secret form (never a fragment: a
    /// key is an identifier). Two keys that become the same text are kept apart
    /// with `#2`, `#3`, so no value is overwritten and the result stays a valid,
    /// unambiguous object.
    fn redact_keys(
        &self,
        map: &mut serde_json::Map<String, Value>,
        exempt: Option<&HashSet<usize>>,
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
        if self.exact.is_empty() {
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
                let name = self.exact.get(token).copied().or_else(|| {
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
        let Some(json) = &self.json else {
            return Cow::Borrowed(line);
        };
        let body = line.trim_end_matches(['\n', '\r']);
        let looks_json = body.trim_start().starts_with(['{', '[']);
        if looks_json {
            if !json.is_match(body) {
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
        let Some(ac) = &self.json else {
            return raw;
        };
        if !ac.is_match(raw.as_ref()) {
            return raw;
        }
        let mut hits = Vec::new();
        for m in ac.find_overlapping_iter(raw.as_ref()) {
            let pattern = &self.raw_patterns[m.pattern().as_usize()].1;
            if pattern.fragment && !fragments {
                continue;
            }
            if pattern.whole_token {
                let text = raw.as_ref();
                let before = text[..m.start()].chars().next_back();
                let after = text[m.end()..].chars().next();
                if before.is_some_and(is_token_char) || after.is_some_and(is_token_char) {
                    continue;
                }
            }
            hits.push((m.start(), m.end(), pattern.name, pattern.fragment));
        }
        let spans = merge(hits);
        if spans.is_empty() {
            return raw;
        }
        Cow::Owned(self.apply(raw.as_ref(), &spans))
    }

    /// Cheap pre-check: could `text` hold any registered form, raw or JSON-escaped?
    /// False positives are fine (short secrets ignore token boundaries here).
    pub fn may_contain(&self, text: &str) -> bool {
        self.raw.as_ref().is_some_and(|a| a.is_match(text))
            || self.json.as_ref().is_some_and(|a| a.is_match(text))
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
    secrets: Vec<(String, String, String)>,
    /// Compiled on first use after a change, so a burst of registrations (every
    /// server's secrets at startup) costs one build, not one per secret.
    matcher: Option<Arc<Matcher>>,
    /// `(id, value)` pairs [`register`] has fully processed, so the repeat reads
    /// (remote servers read their credential from the vault on every call) cost one
    /// read-locked lookup instead of re-deriving forms.
    seen: HashSet<(String, String)>,
    /// Values matched whole but never by fragment (see [`Matcher::new_with`]).
    whole_only: HashSet<String>,
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

fn current_in(scope: &Scope) -> Option<Arc<Matcher>> {
    if !ACTIVE.load(Ordering::Relaxed) {
        return None;
    }
    if let Some(matcher) = REGISTRY
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()?
        .get(scope)?
        .matcher
        .clone()
    {
        return Some(matcher);
    }
    let mut guard = REGISTRY
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let reg = guard.as_mut()?.get_mut(scope)?;
    let matcher = reg.matcher.get_or_insert_with(|| {
        let pairs: Vec<(&str, &str)> = reg
            .secrets
            .iter()
            .map(|(_, n, v)| (n.as_str(), v.as_str()))
            .collect();
        Arc::new(Matcher::new_with(&pairs, &reg.whole_only))
    });
    Some(Arc::clone(matcher))
}

fn current() -> Option<Arc<Matcher>> {
    if !ACTIVE.load(Ordering::Relaxed) {
        return None;
    }
    current_in(&scope_now())
}

/// Match `value` whole only (see [`Matcher::new_with`]). Call before [`add`].
fn mark_whole_only(scope: &Scope, value: &str) {
    let mut guard = REGISTRY
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let reg = guard
        .get_or_insert_with(Default::default)
        .entry(scope.clone())
        .or_insert_with(|| Registry {
            secrets: Vec::new(),
            matcher: None,
            seen: HashSet::new(),
            whole_only: HashSet::new(),
        });
    if reg.whole_only.insert(value.to_string()) {
        reg.matcher = None;
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
    let mut guard = REGISTRY
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let reg = guard
        .get_or_insert_with(Default::default)
        .entry(scope.clone())
        .or_insert_with(|| Registry {
            secrets: Vec::new(),
            matcher: None,
            seen: HashSet::new(),
            whole_only: HashSet::new(),
        });
    if reg
        .secrets
        .iter()
        .any(|(i, n, v)| i == id && n == name && v == value)
    {
        return false;
    }
    reg.secrets
        .push((id.to_string(), name.to_string(), value.to_string()));
    if reg.secrets.len() > MAX_SECRETS {
        let excess = reg.secrets.len() - MAX_SECRETS;
        reg.secrets.drain(..excess);
        reg.seen.clear();
    }
    reg.matcher = None;
    ACTIVE.store(true, Ordering::Release);
    true
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
        Some(m) => match m.redact_text(text) {
            Cow::Borrowed(_) => Cow::Borrowed(text),
            Cow::Owned(s) => Cow::Owned(s),
        },
        None => Cow::Borrowed(text),
    }
}

/// [`Matcher::redact_fragment`] against every registered secret.
pub fn redact_fragment(text: &str) -> Cow<'_, str> {
    match current() {
        Some(m) => match m.redact_fragment(text) {
            Cow::Borrowed(_) => Cow::Borrowed(text),
            Cow::Owned(s) => Cow::Owned(s),
        },
        None => Cow::Borrowed(text),
    }
}

/// Redact, then keep at most `max_chars`. See [`Matcher::truncate_chars`].
pub fn truncate_chars(text: &str, max_chars: usize) -> String {
    match current() {
        Some(m) => m.truncate_chars(text, max_chars),
        None => text.chars().take(max_chars).collect(),
    }
}

/// Longest whole string kept for restoring an identifier exactly; longer text
/// (a description) is never an identifier a client sends back.
const MAX_RECORDED_IDENTIFIER: usize = 1024;

/// Per server, advertised text to the text the server actually advertised, for
/// identifiers [`screen_definitions`] replaced. Memory only: it holds values the
/// registry holds anyway, and lets a call name the upstream tool, prompt,
/// resource or argument even though clients only ever saw the marker.
type Identities = std::collections::HashMap<String, std::collections::HashMap<String, String>>;
static IDENTITIES: Mutex<Option<Identities>> = Mutex::new(None);

/// Screen definitions a server advertised (tools, prompts, resources or
/// templates): any configured secret value in them is replaced (so neither
/// clients, the search index, pins nor the tool cache see it) and each affected
/// item is reported as `(advertised name, secret names)`. Identifiers that were
/// replaced stay callable through [`upstream_identifier`] and
/// [`upstream_arguments`]. Definitions with no secret are left byte for byte.
pub fn screen_definitions(server_id: &str, items: &mut [Value]) -> Vec<(String, Vec<String>)> {
    let Some(matcher) = current() else {
        return Vec::new();
    };
    let mut flagged = Vec::new();
    let mut record = Vec::new();
    for item in items.iter_mut() {
        let mut found = HashSet::new();
        matcher.secrets_in(item, &mut found);
        if found.is_empty() {
            continue;
        }
        matcher.screen_definition(item, &mut record);
        let name = ["name", "uri", "uriTemplate"]
            .iter()
            .find_map(|k| item.get(*k).and_then(Value::as_str))
            .unwrap_or("")
            .to_string();
        let mut names: Vec<String> = found
            .into_iter()
            .map(|n| matcher.names[n].clone())
            .collect();
        names.sort();
        names.dedup();
        flagged.push((name, names));
    }
    if !record.is_empty() {
        let mut guard = IDENTITIES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let map = guard
            .get_or_insert_with(Default::default)
            .entry(server_id.to_string())
            .or_default();
        for (advertised, upstream) in record {
            map.entry(advertised).or_insert(upstream);
        }
    }
    flagged
}

/// The identifier `server_id` advertised for `advertised` (a tool or prompt
/// name, a resource URI or an argument name a client sends back). Unchanged
/// unless [`screen_definitions`] replaced a secret in it. A URI expanded from a
/// screened template gets each marker put back.
pub fn upstream_identifier<'a>(server_id: &str, advertised: &'a str) -> Cow<'a, str> {
    if !advertised.contains("[secret:") {
        return Cow::Borrowed(advertised);
    }
    let guard = IDENTITIES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(map) = guard.as_ref().and_then(|m| m.get(server_id)) else {
        return Cow::Borrowed(advertised);
    };
    if let Some(upstream) = map.get(advertised) {
        return Cow::Owned(upstream.clone());
    }
    let mut out = advertised.to_string();
    for (marker, upstream) in map {
        if marker.starts_with("[secret:") && marker.ends_with(']') && out.contains(marker.as_str())
        {
            out = out.replace(marker.as_str(), upstream);
        }
    }
    Cow::Owned(out)
}

/// `arguments` with object keys a client copied from a screened schema renamed
/// back to the keys `server_id` advertised.
pub fn upstream_arguments(server_id: &str, arguments: Value) -> Value {
    fn walk(server_id: &str, value: Value) -> Value {
        match value {
            Value::Object(map) => Value::Object(
                map.into_iter()
                    .map(|(k, v)| {
                        let key = upstream_identifier(server_id, &k).into_owned();
                        (key, walk(server_id, v))
                    })
                    .collect(),
            ),
            Value::Array(items) => {
                Value::Array(items.into_iter().map(|v| walk(server_id, v)).collect())
            }
            other => other,
        }
    }
    let screened = IDENTITIES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .is_some_and(|m| m.contains_key(server_id));
    if !screened {
        return arguments;
    }
    walk(server_id, arguments)
}

/// [`Matcher::redact_definition_line`] against every registered secret.
pub fn redact_definition_line(line: &str) -> Cow<'_, str> {
    match current() {
        Some(m) => match m.redact_definition_line(line) {
            Cow::Borrowed(_) => Cow::Borrowed(line),
            Cow::Owned(s) => Cow::Owned(s),
        },
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
    current().is_some_and(|m| m.redact_value(value))
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

fn redact_line_with(matcher: Option<Arc<Matcher>>, line: &str) -> Cow<'_, str> {
    match matcher {
        Some(m) => match m.redact_line(line) {
            Cow::Borrowed(_) => Cow::Borrowed(line),
            Cow::Owned(s) => Cow::Owned(s),
        },
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
        "team-activity.jsonl",
        "gateway.log",
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
                // Tool catalogs, which hold upstream definition text.
                "tool-cache",
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
    let Some(matcher) = current_in(&scope_for_path(path)) else {
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
        .is_some_and(|n| n.starts_with("tool-cache"));
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
    // One scrub per data directory at a time, across every process. The marker is
    // read, the logs rewritten and the marker updated under this one lock, so a
    // second process waits, then finds the work recorded and skips it.
    let _scrub = crate::registry::lock_at_for(&dir.join("secret-scrub"), SCRUB_LOCK_WAIT)
        .map_err(std::io::Error::other)?;
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
    let pending = {
        let guard = REGISTRY
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(reg) = guard.as_ref().and_then(|map| map.get(&scope_of(dir))) else {
            return Ok(0);
        };
        let pending: Vec<(String, String, String)> = reg
            .secrets
            .iter()
            .filter(|(id, _, _)| !done.contains(id))
            .cloned()
            .collect();
        pending
    };
    if pending.is_empty() {
        return Ok(0);
    }
    let Some(matcher) = current_in(&scope_of(dir)) else {
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
    Ok(rewritten)
}

/// Scrub every log for one value right now, whatever the marker says: called just
/// before Toolport replaces or deletes a stored secret, the last moment the old
/// value is known. The value stays registered, so this process keeps redacting it.
/// Synchronous (a read of each log, a rewrite only on a hit) and serialized with
/// every other scrub of the directory.
pub fn scrub_value_now(owner: &str, key: &str, value: &str) {
    register(owner, key, value);
    let Some(dir) = scrub_dir_now() else {
        return;
    };
    if !persisted_log_files(&dir).iter().any(|path| path.is_file()) {
        return;
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
    if let Err(error) = result {
        eprintln!("toolport: scrub before retiring secret '{key}' failed: {error}");
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
        assert_eq!(d["rate"], "[secret:RATE]", "source text of a float");
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
        let mut items = vec![plain.clone(), leaky, prompt, template];
        let flagged = screen_definitions("defs-server", &mut items);
        assert_eq!(items[0], plain, "a definition with no secret is untouched");
        assert_eq!(flagged.len(), 3, "{flagged:?}");
        assert_eq!(
            flagged[0],
            (
                "[secret:NAMED]".to_string(),
                vec!["API_KEY".to_string(), "NAMED".to_string()]
            )
        );
        let advertised = items[1].to_string() + &items[2].to_string() + &items[3].to_string();
        assert!(
            !advertised.contains("named-tool-secret") && !advertised.contains(secret),
            "{advertised}"
        );
        assert_eq!(items[1]["description"], "Send [secret:API_KEY] as the key.");
        assert_eq!(
            items[1]["inputSchema"]["required"],
            json!(["[secret:NAMED]", "path"])
        );
        // Routing: the client calls the advertised name with advertised keys; the
        // server gets its own text back.
        assert_eq!(
            upstream_identifier("defs-server", "[secret:NAMED]"),
            "named-tool-secret-9Q"
        );
        assert_eq!(
            upstream_arguments("defs-server", json!({"[secret:NAMED]": "x", "path": "/a"})),
            json!({"named-tool-secret-9Q": "x", "path": "/a"})
        );
        assert_eq!(
            upstream_identifier("defs-server", "mem://[secret:NAMED]/42"),
            "mem://named-tool-secret-9Q/42"
        );
        // Another server never gets this server's text.
        assert_eq!(
            upstream_identifier("other-server", "[secret:NAMED]"),
            "[secret:NAMED]"
        );
        assert_eq!(
            upstream_identifier("defs-server", "Tools-Read"),
            "Tools-Read"
        );
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
        register("unit-scrub-server", "SCRUB_KEY", secret);
        assert_eq!(scrub_data_dir(&dir).unwrap(), 4);
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
        std::fs::write(&audit, format!("{leaked}\n")).unwrap();
        assert_eq!(scrub_data_dir(&dir).unwrap(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
