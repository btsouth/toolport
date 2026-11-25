//! `toolport usage`: a local, offline token and cache profiler across coding agents.
//!
//! Reads Claude Code transcripts (`<profile>/projects/**/*.jsonl`), Codex rollouts
//! (`<codex home>/sessions/**/*.jsonl`), OpenCode's JSON message storage when present, and
//! Toolport's own `audit.jsonl`. Each transcript is streamed line by line on a worker pool
//! and reduced to a small [`FileSummary`]; summaries are cached by path, size and
//! modification time, so a repeat run only reads what changed.
//!
//! Attribution is residency, not volume: a tool result of T tokens that stays in context for
//! N more model turns costs T x N input tokens, which is what the bill and the context window
//! actually see. A compaction (context dropping by half or more, or an explicit compaction
//! record) ends every residency.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::BufRead as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use super::format::{self, Align, Severity, Style};
use super::{Args, CliError, Ctx, EXIT_OK};

/// Bump when [`FileSummary`] or the parsers change meaning, so old cache entries are ignored.
const CACHE_VERSION: u32 = 5;
/// A session with this many calls to one MCP server is a burst: a Code Mode candidate.
pub const BURST_CALLS: u64 = 9;
/// An image that stays in context this many turns is lingering.
const LINGER_TURNS: u64 = 20;
/// Characters per token for text estimates. Close for English and code; images are sized
/// from their dimensions instead.
const CHARS_PER_TOKEN: u64 = 4;
/// A cache rewrite smaller than this is ordinary prefix growth, not a miss.
const MISS_MIN_TOKENS: u64 = 4096;

// ---------------------------------------------------------------------------------------------
// Sources
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Client {
    Claude,
    Codex,
    Opencode,
}

impl Client {
    pub fn label(self) -> &'static str {
        match self {
            Client::Claude => "Claude Code",
            Client::Codex => "Codex",
            Client::Opencode => "OpenCode",
        }
    }
}

/// Where transcripts live. Built from Toolport's client discovery in production and by hand in
/// tests.
#[derive(Debug, Clone, Default)]
pub struct Sources {
    pub claude: Vec<PathBuf>,
    pub codex: Vec<PathBuf>,
    pub opencode: Vec<PathBuf>,
    pub audit: Option<PathBuf>,
}

impl Sources {
    /// Every Claude Code profile and Codex home Toolport knows about: `CLAUDE_CONFIG_DIR`,
    /// `~/.claude`, `~/.claude2`-style profiles, `CODEX_HOME`, and homes other tools point
    /// agents at (T3 Code instances), the same set the Clients and Skills pages list.
    pub fn discover(clients: &[crate::clients::DetectedClient]) -> Self {
        let mut sources = Sources {
            audit: crate::audit::audit_path(),
            ..Default::default()
        };
        let home = crate::clients::home();
        if let Some(env) = crate::skills::Env::current() {
            let mut claude = env.claude_profiles.clone();
            claude.push(env.claude.clone());
            for dir in claude {
                push_unique(&mut sources.claude, dir);
            }
            push_unique(&mut sources.codex, env.codex.clone());
        }
        for client in clients {
            let kind = client.id.split('@').next().unwrap_or(&client.id);
            let path = Path::new(&client.config_path);
            let Some(parent) = path.parent().filter(|_| path.is_absolute()) else {
                continue;
            };
            match kind {
                "claude-code" => {
                    let dir = if home
                        .as_ref()
                        .is_some_and(|h| path == h.join(".claude.json"))
                    {
                        home.as_ref().unwrap().join(".claude")
                    } else {
                        parent.to_path_buf()
                    };
                    push_unique(&mut sources.claude, dir);
                }
                "codex" => push_unique(&mut sources.codex, parent.to_path_buf()),
                _ => {}
            }
        }
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| home.as_ref().map(|h| h.join(".local").join("share")));
        if let Some(data) = data {
            push_unique(&mut sources.opencode, data.join("opencode"));
        }
        sources.claude.retain(|d| d.join("projects").is_dir());
        sources
            .codex
            .retain(|d| d.join("sessions").is_dir() || d.join("archived_sessions").is_dir());
        sources.opencode.retain(|d| d.is_dir());
        sources
    }
}

fn push_unique(list: &mut Vec<PathBuf>, dir: PathBuf) {
    let canon = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
    if !list
        .iter()
        .any(|d| std::fs::canonicalize(d).unwrap_or_else(|_| d.clone()) == canon)
    {
        list.push(dir);
    }
}

/// One unit of parsing and caching: a transcript file, or an OpenCode session folder.
/// For a folder, `size`, `mtime_ms` and `files` cover the message files inside it, so a
/// message rewritten in place invalidates the cached summary.
#[derive(Debug, Clone)]
struct Unit {
    client: Client,
    root: PathBuf,
    path: PathBuf,
    size: u64,
    mtime_ms: u64,
    files: u64,
}

fn mtime_ms(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn walk_jsonl(dir: &Path, since_ms: u64, depth: usize, out: &mut Vec<(PathBuf, u64, u64)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if kind.is_dir() {
            if depth > 0 {
                walk_jsonl(&path, since_ms, depth - 1, out);
            }
        } else if kind.is_file() && path.extension().is_some_and(|e| e == "jsonl") {
            if let Ok(meta) = entry.metadata() {
                let m = mtime_ms(&meta);
                if m >= since_ms {
                    out.push((path, meta.len(), m));
                }
            }
        }
    }
}

fn units(sources: &Sources, filter: &BTreeSet<Client>, since_ms: u64) -> Vec<Unit> {
    let mut out = Vec::new();
    let mut add = |client: Client, root: &Path, dir: PathBuf, depth: usize| {
        let mut files = Vec::new();
        walk_jsonl(&dir, since_ms, depth, &mut files);
        for (path, size, mtime_ms) in files {
            out.push(Unit {
                client,
                root: root.to_path_buf(),
                path,
                size,
                mtime_ms,
                files: 1,
            });
        }
    };
    if filter.contains(&Client::Claude) {
        for root in &sources.claude {
            add(Client::Claude, root, root.join("projects"), 4);
        }
    }
    if filter.contains(&Client::Codex) {
        for root in &sources.codex {
            add(Client::Codex, root, root.join("sessions"), 5);
            add(Client::Codex, root, root.join("archived_sessions"), 5);
        }
    }
    if filter.contains(&Client::Opencode) {
        for root in &sources.opencode {
            let messages = root.join("storage").join("message");
            for entry in std::fs::read_dir(&messages).into_iter().flatten().flatten() {
                let Ok(meta) = entry.metadata() else { continue };
                if !meta.is_dir() {
                    continue;
                }
                // Editing a message in place leaves the folder's own time alone (and NTFS
                // flushes folder timestamps lazily); the newest message, total size and
                // count catch edits, additions and removals deterministically.
                let (mut files, mut size, mut newest) = (0, 0, 0);
                for file in std::fs::read_dir(entry.path())
                    .into_iter()
                    .flatten()
                    .flatten()
                {
                    let Ok(m) = file.metadata() else { continue };
                    if m.is_file() {
                        files += 1;
                        size += m.len();
                        newest = newest.max(mtime_ms(&m));
                    }
                }
                if files == 0 || newest < since_ms {
                    continue;
                }
                out.push(Unit {
                    client: Client::Opencode,
                    root: root.clone(),
                    path: entry.path(),
                    size,
                    mtime_ms: newest,
                    files,
                });
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Per-file summary
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Tokens {
    pub turns: u64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
}

impl Tokens {
    fn add(&mut self, other: &Tokens) {
        self.turns += other.turns;
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write_5m += other.cache_write_5m;
        self.cache_write_1h += other.cache_write_1h;
    }
    fn context(&self) -> u64 {
        self.input + self.cache_read + self.cache_write_5m + self.cache_write_1h
    }
}

/// What one transcript contributes. Small, serializable and free of content: counts, token
/// figures, tool categories and MCP server names only.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FileSummary {
    pub first_ms: u64,
    pub last_ms: u64,
    pub turns: u64,
    pub models: BTreeMap<String, Tokens>,
    /// Sum of the context each main-thread model turn carried.
    pub context_sum: u64,
    pub context_max: u64,
    /// Residency of the startup floor (the first turn's context: system prompt, tool
    /// definitions, rules files, the first message).
    pub floor_sum: u64,
    pub floor: u64,
    /// Residency in token-turns per source category.
    pub resident: BTreeMap<String, u64>,
    /// Tokens produced per category (each result counted once).
    pub produced: BTreeMap<String, u64>,
    pub tool_calls: BTreeMap<String, u64>,
    pub mcp_calls: BTreeMap<String, u64>,
    pub images: u64,
    pub image_tokens: u64,
    pub images_lingering: u64,
    pub image_linger_turns: u64,
    pub dup_reads: u64,
    pub dup_read_tokens: u64,
    pub misses_idle: u64,
    pub misses_idle_tokens: u64,
    pub misses_break: u64,
    pub misses_break_tokens: u64,
    pub compactions: u64,
    pub malformed_lines: u64,
    /// A Claude Code subagent transcript: its own context, but not a user session.
    #[serde(default)]
    pub subagent: bool,
}

impl FileSummary {
    /// Add another summary of the same transcript (another day of it).
    fn merge(&mut self, o: FileSummary) {
        fn add_map(into: &mut BTreeMap<String, u64>, from: BTreeMap<String, u64>) {
            for (k, v) in from {
                *into.entry(k).or_default() += v;
            }
        }
        if o.first_ms != 0 && (self.first_ms == 0 || o.first_ms < self.first_ms) {
            self.first_ms = o.first_ms;
        }
        self.last_ms = self.last_ms.max(o.last_ms);
        self.turns += o.turns;
        for (model, t) in o.models {
            self.models.entry(model).or_default().add(&t);
        }
        self.context_sum += o.context_sum;
        self.context_max = self.context_max.max(o.context_max);
        self.floor_sum += o.floor_sum;
        self.floor = self.floor.max(o.floor);
        add_map(&mut self.resident, o.resident);
        add_map(&mut self.produced, o.produced);
        add_map(&mut self.tool_calls, o.tool_calls);
        add_map(&mut self.mcp_calls, o.mcp_calls);
        self.images += o.images;
        self.image_tokens += o.image_tokens;
        self.images_lingering += o.images_lingering;
        self.image_linger_turns += o.image_linger_turns;
        self.dup_reads += o.dup_reads;
        self.dup_read_tokens += o.dup_read_tokens;
        self.misses_idle += o.misses_idle;
        self.misses_idle_tokens += o.misses_idle_tokens;
        self.misses_break += o.misses_break;
        self.misses_break_tokens += o.misses_break_tokens;
        self.compactions += o.compactions;
        self.malformed_lines += o.malformed_lines;
        self.subagent |= o.subagent;
    }
}

const DAY_MS: u64 = 86_400_000;

/// One transcript's summary per UTC day its records fall on, so a `--days` window counts
/// the turns inside it, not every turn of a file that was touched inside it.
pub type DaySummaries = Vec<(u32, FileSummary)>;

/// Builds [`DaySummaries`] while a transcript is read. Records without a timestamp count
/// toward the day of the record before them.
struct Days {
    map: BTreeMap<u32, FileSummary>,
    day: u32,
    subagent: bool,
}

impl Days {
    fn new(subagent: bool) -> Self {
        Days {
            map: BTreeMap::new(),
            day: 0,
            subagent,
        }
    }

    fn at(&mut self, ts: u64) -> &mut FileSummary {
        if ts > 0 {
            self.day = (ts / DAY_MS) as u32;
        }
        let subagent = self.subagent;
        self.map.entry(self.day).or_insert_with(|| FileSummary {
            subagent,
            ..Default::default()
        })
    }

    fn finish(mut self) -> DaySummaries {
        // Anything before the first timestamp belongs to the first day that has one.
        if let Some(early) = self.map.remove(&0) {
            match self.map.values_mut().next() {
                Some(first) => first.merge(early),
                None => {
                    self.map.insert(0, early);
                }
            }
        }
        self.map.into_iter().collect()
    }
}

/// The part of a transcript inside the window starting at `since_ms`, by whole UTC days.
/// A transcript without timestamps (day 0) is kept whole; its file time already matched.
pub fn in_window(days: &DaySummaries, since_ms: u64) -> FileSummary {
    let first = (since_ms / DAY_MS) as u32;
    let mut out = FileSummary::default();
    for (day, s) in days {
        if *day == 0 || *day >= first {
            out.merge(s.clone());
        }
    }
    out
}

/// Running state for one context stream while its transcript is read.
#[derive(Default)]
struct Stream {
    turns: u64,
    prev_ctx: u64,
    prev_ts: u64,
    last_write_1h: bool,
    floor: u64,
    /// Set by a compaction: the next turn's context is the new floor.
    refloor: bool,
    live: BTreeMap<String, u64>,
    images: Vec<(u64, u64)>,
}

impl Stream {
    /// One model request. `ctx` is its full input (uncached + cache read + cache write).
    fn turn(&mut self, s: &mut FileSummary, ts: u64, ctx: u64, read: u64, write: u64, w1h: bool) {
        if ctx == 0 {
            return;
        }
        let compacted = self.prev_ctx > 20_000 && ctx * 2 < self.prev_ctx;
        if compacted {
            self.compact(s);
        } else if self.turns > 0 && read * 2 < self.prev_ctx && write > MISS_MIN_TOKENS {
            let ttl = if self.last_write_1h {
                3_600_000
            } else {
                300_000
            };
            if ts > 0 && self.prev_ts > 0 && ts.saturating_sub(self.prev_ts) > ttl {
                s.misses_idle += 1;
                s.misses_idle_tokens += write;
            } else {
                s.misses_break += 1;
                s.misses_break_tokens += write;
            }
        }
        if write > 0 {
            self.last_write_1h = w1h;
        }
        if self.turns == 0 {
            self.floor = ctx;
            s.floor = s.floor.max(ctx);
        } else if self.refloor {
            // After a compaction the summary replaces the history; growth from here on is
            // what later tools and messages add, not part of the old floor.
            self.floor = ctx;
        }
        self.refloor = false;
        let floor = self.floor.min(ctx);
        s.floor_sum += floor;
        let live: u64 = self.live.values().sum();
        let room = ctx - floor;
        let scale = if live > room && live > 0 {
            room as f64 / live as f64
        } else {
            1.0
        };
        for (cat, tokens) in &self.live {
            *s.resident.entry(cat.clone()).or_default() += (*tokens as f64 * scale) as u64;
        }
        s.context_sum += ctx;
        s.context_max = s.context_max.max(ctx);
        s.turns += 1;
        self.turns += 1;
        self.prev_ctx = ctx;
        if ts > 0 {
            self.prev_ts = ts;
        }
    }

    fn result(&mut self, s: &mut FileSummary, cat: &str, tokens: u64) {
        if tokens == 0 {
            return;
        }
        *self.live.entry(cat.to_string()).or_default() += tokens;
        *s.produced.entry(cat.to_string()).or_default() += tokens;
    }

    fn image(&mut self, s: &mut FileSummary, tokens: u64) {
        s.images += 1;
        s.image_tokens += tokens;
        self.images.push((tokens, self.turns));
        self.result(s, "image", tokens);
    }

    fn compact(&mut self, s: &mut FileSummary) {
        s.compactions += 1;
        self.finish_images(s);
        self.live.clear();
        self.refloor = true;
    }

    fn finish_images(&mut self, s: &mut FileSummary) {
        for (_, start) in self.images.drain(..) {
            let turns = self.turns.saturating_sub(start);
            if turns >= LINGER_TURNS {
                s.images_lingering += 1;
                s.image_linger_turns += turns;
            }
        }
    }

    fn finish(mut self, s: &mut FileSummary) {
        self.finish_images(s);
    }
}

fn stamp(s: &mut FileSummary, ts: u64) {
    if ts == 0 {
        return;
    }
    if s.first_ms == 0 || ts < s.first_ms {
        s.first_ms = ts;
    }
    s.last_ms = s.last_ms.max(ts);
}

/// `2026-11-20T05:16:35.011Z` (or with an offset) to epoch milliseconds.
pub fn parse_iso_ms(text: &str) -> u64 {
    let b = text.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || b[13] != b':' {
        return 0;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> { text.get(r)?.parse().ok() };
    let (Some(y), Some(mo), Some(d), Some(h), Some(mi), Some(se)) = (
        num(0..4),
        num(5..7),
        num(8..10),
        num(11..13),
        num(14..16),
        num(17..19),
    ) else {
        return 0;
    };
    let mut ms = 0i64;
    let mut rest = &text[19..];
    if let Some(frac) = rest.strip_prefix('.') {
        let digits: String = frac.chars().take_while(|c| c.is_ascii_digit()).collect();
        rest = &frac[digits.len()..];
        let padded = format!("{digits:0<3}");
        ms = padded[..3].parse().unwrap_or(0);
    }
    let mut offset = 0i64;
    if let Some(sign @ ('+' | '-')) = rest.chars().next() {
        let parts: Vec<&str> = rest[1..].split(':').collect();
        let oh: i64 = parts
            .first()
            .and_then(|p| p.get(..2)?.parse().ok())
            .unwrap_or(0);
        let om: i64 = parts
            .get(1)
            .and_then(|p| p.parse().ok())
            .or_else(|| parts.first().and_then(|p| p.get(2..4)?.parse().ok()))
            .unwrap_or(0);
        offset = (oh * 60 + om) * 60_000 * if sign == '+' { 1 } else { -1 };
    }
    // Howard Hinnant's days_from_civil.
    let y = if mo <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let total = ((days * 24 + h) * 60 + mi) * 60_000 + se * 1000 + ms - offset;
    total.max(0) as u64
}

fn text_tokens(raw_len: usize) -> u64 {
    (raw_len as u64).div_ceil(CHARS_PER_TOKEN)
}

/// Image size in tokens from its pixel dimensions, the way Claude bills vision input:
/// (width x height) / 750 after scaling the long edge to at most 1568 px. Unknown formats count
/// as a typical screenshot.
pub fn image_tokens(base64_data: &str) -> u64 {
    use base64::Engine as _;
    let head: String = base64_data
        .chars()
        .filter(|c| !c.is_whitespace())
        .take(48 * 1024)
        .collect();
    let head = &head[..head.len() / 4 * 4];
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(head)
        .unwrap_or_default();
    match image_size(&bytes) {
        Some((w, h)) if w > 0 && h > 0 => {
            let (mut w, mut h) = (w as f64, h as f64);
            let long = w.max(h);
            if long > 1568.0 {
                w *= 1568.0 / long;
                h *= 1568.0 / long;
            }
            // Claude also caps area near 1.15 megapixels.
            let area = (w * h).min(1_150_000.0);
            ((area / 750.0).ceil() as u64).max(1)
        }
        _ => 1_200,
    }
}

fn image_size(b: &[u8]) -> Option<(u32, u32)> {
    let be32 = |i: usize| Some(u32::from_be_bytes(b.get(i..i + 4)?.try_into().ok()?));
    let le16 = |i: usize| Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?) as u32);
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some((be32(16)?, be32(20)?));
    }
    if b.starts_with(b"GIF8") {
        return Some((le16(6)?, le16(8)?));
    }
    if b.len() > 30 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        let le24 = |i: usize| {
            Some(u32::from_le_bytes([
                *b.get(i)?,
                *b.get(i + 1)?,
                *b.get(i + 2)?,
                0,
            ]))
        };
        return match &b[12..16] {
            b"VP8X" => Some((le24(24)? + 1, le24(27)? + 1)),
            b"VP8 " => Some((le16(26)? & 0x3fff, le16(28)? & 0x3fff)),
            b"VP8L" => {
                let v = u32::from_le_bytes(b.get(21..25)?.try_into().ok()?);
                Some(((v & 0x3fff) + 1, ((v >> 14) & 0x3fff) + 1))
            }
            _ => None,
        };
    }
    if b.starts_with(&[0xff, 0xd8]) {
        let mut i = 2;
        while i + 9 < b.len() {
            if b[i] != 0xff {
                i += 1;
                continue;
            }
            let marker = b[i + 1];
            let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
            if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
                let h = u16::from_be_bytes([b[i + 5], b[i + 6]]) as u32;
                let w = u16::from_be_bytes([b[i + 7], b[i + 8]]) as u32;
                return Some((w, h));
            }
            i += 2 + len;
        }
    }
    None
}

/// `mcp__server__tool` to `server`, normalized for comparing with configured names.
pub fn mcp_server_of(tool: &str) -> Option<String> {
    let rest = tool.strip_prefix("mcp__")?;
    let (server, _) = rest.split_once("__")?;
    (!server.is_empty()).then(|| normalize_server(server))
}

/// Lowercase, with every run of other characters as one `_`, the way clients form tool names.
pub fn normalize_server(name: &str) -> String {
    let mut out = String::new();
    let mut gap = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            if gap && !out.is_empty() {
                out.push('_');
            }
            gap = false;
            out.push(c.to_ascii_lowercase());
        } else {
            gap = true;
        }
    }
    out
}

/// The source category a Claude Code tool's result is filed under.
fn claude_category(name: &str, input: Option<&RawValue>) -> String {
    if name == "mcp__toolport__toolport_call_tool" {
        #[derive(Deserialize)]
        struct Call<'a> {
            #[serde(borrow)]
            name: Option<std::borrow::Cow<'a, str>>,
        }
        if let Some(inner) = input
            .and_then(|i| serde_json::from_str::<Call>(i.get()).ok())
            .and_then(|c| c.name)
        {
            if let Some((server, _)) = inner.split_once("__") {
                return format!("mcp:{}", normalize_server(server));
            }
        }
        return "mcp:toolport".into();
    }
    if let Some(server) = mcp_server_of(name) {
        return format!("mcp:{server}");
    }
    match name {
        "Bash" | "BashOutput" | "KillShell" | "PowerShell" => "bash",
        "Read" | "NotebookRead" => "read",
        "Grep" | "Glob" | "LS" | "ToolSearch" => "search",
        "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => "edit",
        "WebFetch" | "WebSearch" => "web",
        "Task" | "Agent" => "subagent",
        _ => "other",
    }
    .to_string()
}

// --- Claude Code -------------------------------------------------------------------------

#[derive(Deserialize)]
struct ClaudeLine<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<std::borrow::Cow<'a, str>>,
    #[serde(borrow)]
    timestamp: Option<std::borrow::Cow<'a, str>>,
    #[serde(rename = "isSidechain")]
    sidechain: Option<bool>,
    #[serde(rename = "isCompactSummary")]
    compact_summary: Option<bool>,
    #[serde(borrow)]
    subtype: Option<std::borrow::Cow<'a, str>>,
    #[serde(borrow)]
    message: Option<ClaudeMessage<'a>>,
}

#[derive(Deserialize)]
struct ClaudeMessage<'a> {
    #[serde(borrow)]
    id: Option<std::borrow::Cow<'a, str>>,
    #[serde(borrow)]
    model: Option<std::borrow::Cow<'a, str>>,
    usage: Option<ClaudeUsage>,
    #[serde(borrow)]
    content: Option<&'a RawValue>,
}

#[derive(Deserialize, Default)]
struct ClaudeUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    cache_creation: Option<CacheCreation>,
}

#[derive(Deserialize, Default)]
struct CacheCreation {
    #[serde(default)]
    ephemeral_5m_input_tokens: u64,
    #[serde(default)]
    ephemeral_1h_input_tokens: u64,
}

#[derive(Deserialize)]
struct Block<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<std::borrow::Cow<'a, str>>,
    #[serde(borrow)]
    id: Option<std::borrow::Cow<'a, str>>,
    #[serde(borrow)]
    name: Option<std::borrow::Cow<'a, str>>,
    #[serde(borrow)]
    input: Option<&'a RawValue>,
    #[serde(borrow)]
    tool_use_id: Option<std::borrow::Cow<'a, str>>,
    #[serde(borrow)]
    content: Option<&'a RawValue>,
    #[serde(borrow)]
    text: Option<&'a RawValue>,
    #[serde(borrow)]
    source: Option<ImageSource<'a>>,
}

#[derive(Deserialize)]
struct ImageSource<'a> {
    #[serde(borrow)]
    data: Option<std::borrow::Cow<'a, str>>,
}

#[derive(Deserialize)]
struct FileInput<'a> {
    #[serde(borrow)]
    file_path: Option<std::borrow::Cow<'a, str>>,
    offset: Option<serde_json::Value>,
    limit: Option<serde_json::Value>,
}

struct PendingTool {
    category: String,
    read_key: Option<String>,
    duplicate: bool,
}

/// Claude Code's main thread and its sidechains are separate contexts.
#[derive(Default)]
struct ClaudeState {
    main: Stream,
    side: Stream,
    seen: HashMap<String, (bool, u64)>,
    pending: HashMap<String, PendingTool>,
    reads: HashMap<String, u64>,
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    memchr::memmem::find(hay, needle).is_some()
}

fn read_lines(path: &Path, mut f: impl FnMut(&[u8])) {
    let Ok(file) = std::fs::File::open(path) else {
        return;
    };
    let mut reader = std::io::BufReader::with_capacity(1 << 20, file);
    let mut buf = Vec::with_capacity(64 * 1024);
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let line = buf.strip_suffix(b"\n").unwrap_or(&buf);
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                if !line.is_empty() {
                    f(line);
                }
            }
        }
    }
}

pub fn parse_claude(path: &Path) -> DaySummaries {
    let mut days = Days::new(path.components().any(|c| c.as_os_str() == "subagents"));
    let mut st = ClaudeState::default();
    read_lines(path, |line| {
        if !(contains(line, b"\"assistant\"")
            || contains(line, b"tool_result")
            || contains(line, b"compact"))
        {
            return;
        }
        let Ok(parsed) = serde_json::from_slice::<ClaudeLine>(line) else {
            days.at(0).malformed_lines += 1;
            return;
        };
        let ts = parsed.timestamp.as_deref().map(parse_iso_ms).unwrap_or(0);
        claude_line(days.at(ts), &mut st, parsed);
    });
    let s = days.at(0);
    st.main.finish(s);
    st.side.finish(s);
    days.finish()
}

fn claude_line(s: &mut FileSummary, st: &mut ClaudeState, line: ClaudeLine) {
    let ts = line.timestamp.as_deref().map(parse_iso_ms).unwrap_or(0);
    // A subagent's own transcript is one coherent context even though it is a sidechain.
    let side = line.sidechain.unwrap_or(false) && !s.subagent;
    if line.compact_summary == Some(true) || line.subtype.as_deref() == Some("compact_boundary") {
        let stream = if side { &mut st.side } else { &mut st.main };
        if stream.turns > 0 && !stream.live.is_empty() {
            stream.compact(s);
            stream.prev_ctx = 0;
        }
        return;
    }
    let Some(message) = line.message else { return };
    match line.kind.as_deref() {
        Some("assistant") => {
            stamp(s, ts);
            if let (Some(id), Some(usage)) = (message.id.as_deref(), &message.usage) {
                let model = message.model.as_deref().unwrap_or("unknown");
                let (w5, w1) = match &usage.cache_creation {
                    Some(c) if c.ephemeral_5m_input_tokens + c.ephemeral_1h_input_tokens > 0 => {
                        (c.ephemeral_5m_input_tokens, c.ephemeral_1h_input_tokens)
                    }
                    _ => (usage.cache_creation_input_tokens, 0),
                };
                match st.seen.get_mut(id) {
                    Some((_, output)) => {
                        // Later lines of one message repeat its usage; keep the final output.
                        if usage.output_tokens > *output {
                            let extra = usage.output_tokens - *output;
                            *output = usage.output_tokens;
                            if model != "<synthetic>" {
                                s.models.entry(model.to_string()).or_default().output += extra;
                            }
                        }
                    }
                    None => {
                        st.seen.insert(id.to_string(), (side, usage.output_tokens));
                        if model != "<synthetic>" {
                            let t = s.models.entry(model.to_string()).or_default();
                            t.add(&Tokens {
                                turns: 1,
                                input: usage.input_tokens,
                                output: usage.output_tokens,
                                cache_read: usage.cache_read_input_tokens,
                                cache_write_5m: w5,
                                cache_write_1h: w1,
                            });
                            let ctx = usage.input_tokens + usage.cache_read_input_tokens + w5 + w1;
                            let stream = if side { &mut st.side } else { &mut st.main };
                            if side {
                                // Sidechains interleave subagents; count their tokens, not
                                // their residency.
                                stream.turns += 1;
                            } else {
                                stream.turn(
                                    s,
                                    ts,
                                    ctx,
                                    usage.cache_read_input_tokens,
                                    w5 + w1,
                                    w1 > 0,
                                );
                            }
                        }
                    }
                }
            }
            let Some(content) = message.content else {
                return;
            };
            let Ok(blocks) = serde_json::from_str::<Vec<Block>>(content.get()) else {
                return;
            };
            for block in blocks {
                if block.kind.as_deref() != Some("tool_use") {
                    continue;
                }
                let (Some(id), Some(name)) = (block.id.as_deref(), block.name.as_deref()) else {
                    continue;
                };
                if st.pending.contains_key(id) {
                    continue;
                }
                let category = claude_category(name, block.input);
                *s.tool_calls.entry(category.clone()).or_default() += 1;
                // On the day of the message that made the call, so a `--days` window only
                // counts the calls inside it.
                if let Some(server) = category.strip_prefix("mcp:") {
                    *s.mcp_calls.entry(server.to_string()).or_default() += 1;
                }
                let file = block
                    .input
                    .and_then(|i| serde_json::from_str::<FileInput>(i.get()).ok());
                let mut read_key = None;
                let mut duplicate = false;
                match name {
                    "Read" => {
                        if let Some(f) = &file {
                            if let Some(p) = f.file_path.as_deref() {
                                let key = format!(
                                    "{p}\u{1f}{}\u{1f}{}",
                                    f.offset.as_ref().map(|v| v.to_string()).unwrap_or_default(),
                                    f.limit.as_ref().map(|v| v.to_string()).unwrap_or_default()
                                );
                                let n = st.reads.entry(key.clone()).or_default();
                                *n += 1;
                                duplicate = *n > 1;
                                read_key = Some(key);
                            }
                        }
                    }
                    "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => {
                        if let Some(p) = file.as_ref().and_then(|f| f.file_path.as_deref()) {
                            let prefix = format!("{p}\u{1f}");
                            st.reads.retain(|k, _| !k.starts_with(&prefix));
                        }
                    }
                    _ => {}
                }
                st.pending.insert(
                    id.to_string(),
                    PendingTool {
                        category,
                        read_key,
                        duplicate,
                    },
                );
            }
        }
        Some("user") => {
            stamp(s, ts);
            let Some(content) = message.content else {
                return;
            };
            if !content.get().starts_with('[') {
                return;
            }
            let Ok(blocks) = serde_json::from_str::<Vec<Block>>(content.get()) else {
                return;
            };
            let stream = if side { &mut st.side } else { &mut st.main };
            for block in blocks {
                match block.kind.as_deref() {
                    Some("tool_result") => {
                        let pending = block
                            .tool_use_id
                            .as_deref()
                            .and_then(|id| st.pending.remove(id));
                        let category = pending
                            .as_ref()
                            .map(|p| p.category.clone())
                            .unwrap_or_else(|| "other".into());
                        let mut text = 0u64;
                        if let Some(c) = block.content {
                            let raw = c.get();
                            if raw.starts_with('[') {
                                for inner in
                                    serde_json::from_str::<Vec<Block>>(raw).unwrap_or_default()
                                {
                                    match inner.kind.as_deref() {
                                        Some("image") => {
                                            let data = inner
                                                .source
                                                .and_then(|s| s.data)
                                                .unwrap_or_default();
                                            stream.image(s, image_tokens(&data));
                                        }
                                        _ => {
                                            text += inner
                                                .text
                                                .map(|t| text_tokens(t.get().len()))
                                                .unwrap_or(0);
                                        }
                                    }
                                }
                            } else {
                                text += text_tokens(raw.len());
                            }
                        }
                        if let Some(p) = &pending {
                            if p.duplicate {
                                s.dup_reads += 1;
                                s.dup_read_tokens += text;
                            }
                            let _ = &p.read_key;
                        }
                        stream.result(s, &category, text);
                    }
                    Some("image") => {
                        let data = block.source.and_then(|s| s.data).unwrap_or_default();
                        stream.image(s, image_tokens(&data));
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

// --- Codex --------------------------------------------------------------------------------

#[derive(Deserialize)]
struct CodexLine<'a> {
    #[serde(borrow)]
    timestamp: Option<std::borrow::Cow<'a, str>>,
    #[serde(rename = "type", borrow)]
    kind: Option<std::borrow::Cow<'a, str>>,
    #[serde(borrow)]
    payload: Option<&'a RawValue>,
}

#[derive(Deserialize)]
struct CodexPayload<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<std::borrow::Cow<'a, str>>,
    #[serde(borrow)]
    name: Option<std::borrow::Cow<'a, str>>,
    #[serde(borrow)]
    namespace: Option<std::borrow::Cow<'a, str>>,
    #[serde(borrow)]
    call_id: Option<std::borrow::Cow<'a, str>>,
    #[serde(borrow)]
    input: Option<&'a RawValue>,
    #[serde(borrow)]
    arguments: Option<&'a RawValue>,
    #[serde(borrow)]
    output: Option<&'a RawValue>,
    #[serde(borrow)]
    model: Option<std::borrow::Cow<'a, str>>,
    info: Option<CodexInfo>,
}

#[derive(Deserialize)]
struct CodexInfo {
    total_token_usage: Option<CodexUsage>,
    last_token_usage: Option<CodexUsage>,
}

#[derive(Deserialize, Default, Clone)]
struct CodexUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    cached_input_tokens: u64,
    #[serde(default)]
    cache_write_input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    total_tokens: u64,
}

#[derive(Deserialize)]
struct CodexOutputPart<'a> {
    #[serde(rename = "type", borrow)]
    kind: Option<std::borrow::Cow<'a, str>>,
    #[serde(borrow)]
    text: Option<&'a RawValue>,
    #[serde(borrow)]
    image_url: Option<std::borrow::Cow<'a, str>>,
}

/// What a Codex call does, from its name, namespace and (for code mode `exec`) the tools its
/// script calls.
fn codex_category(
    name: &str,
    namespace: Option<&str>,
    input: Option<&str>,
) -> (String, Vec<String>) {
    if let Some(server) = namespace.and_then(|n| n.strip_prefix("mcp__")) {
        let server = normalize_server(server);
        return (format!("mcp:{server}"), vec![server]);
    }
    if let Some(server) = mcp_server_of(name) {
        return (format!("mcp:{server}"), vec![server]);
    }
    match name {
        "exec_command" | "shell" | "shell_command" | "local_shell" | "write_stdin" | "wait" => {
            ("bash".into(), Vec::new())
        }
        "apply_patch" => ("edit".into(), Vec::new()),
        "view_image" => ("image".into(), Vec::new()),
        "read_file" | "list_dir" | "grep_files" => ("read".into(), Vec::new()),
        "spawn_agent" | "send_message" | "followup_task" => ("subagent".into(), Vec::new()),
        "web_search" => ("web".into(), Vec::new()),
        "exec" => {
            let script = input.unwrap_or("");
            let mut servers = Vec::new();
            let mut rest = script;
            while let Some(i) = rest.find("mcp__") {
                let tail = &rest[i..];
                let end = tail
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .unwrap_or(tail.len());
                if let Some(server) = mcp_server_of(&tail[..end]) {
                    servers.push(server);
                }
                rest = &tail[end.max(5)..];
            }
            if let Some(first) = servers.first() {
                (format!("mcp:{first}"), servers)
            } else if script.contains("exec_command") || script.contains("shell") {
                ("bash".into(), Vec::new())
            } else if script.contains("apply_patch") {
                ("edit".into(), Vec::new())
            } else {
                ("other".into(), Vec::new())
            }
        }
        _ => ("other".into(), Vec::new()),
    }
}

pub fn parse_codex(path: &Path) -> DaySummaries {
    let mut days = Days::new(false);
    let mut stream = Stream::default();
    let mut model = String::from("unknown");
    let mut last_total = None::<u64>;
    let mut calls: HashMap<String, String> = HashMap::new();
    read_lines(path, |line| {
        if !(contains(line, b"token_count")
            || contains(line, b"turn_context")
            || contains(line, b"call")
            || contains(line, b"compacted"))
        {
            return;
        }
        let Ok(parsed) = serde_json::from_slice::<CodexLine>(line) else {
            days.at(0).malformed_lines += 1;
            return;
        };
        let ts = parsed.timestamp.as_deref().map(parse_iso_ms).unwrap_or(0);
        let s = days.at(ts);
        if parsed.kind.as_deref() == Some("compacted") {
            stream.compact(s);
            stream.prev_ctx = 0;
            return;
        }
        let Some(payload) = parsed.payload else {
            return;
        };
        let Ok(p) = serde_json::from_str::<CodexPayload>(payload.get()) else {
            return;
        };
        match (parsed.kind.as_deref(), p.kind.as_deref()) {
            (Some("turn_context"), _) => {
                if let Some(m) = p.model {
                    model = m.into_owned();
                }
            }
            (Some("event_msg"), Some("token_count")) => {
                let Some(info) = p.info else { return };
                let (Some(total), Some(last)) = (info.total_token_usage, info.last_token_usage)
                else {
                    return;
                };
                if last_total == Some(total.total_tokens) {
                    return;
                }
                last_total = Some(total.total_tokens);
                stamp(s, ts);
                let cached = last.cached_input_tokens.min(last.input_tokens);
                let write = last.cache_write_input_tokens;
                let fresh = last.input_tokens.saturating_sub(cached + write);
                s.models.entry(model.clone()).or_default().add(&Tokens {
                    turns: 1,
                    input: fresh,
                    output: last.output_tokens,
                    cache_read: cached,
                    cache_write_5m: write,
                    cache_write_1h: 0,
                });
                // OpenAI caches without an explicit write; a big uncached input after a turn
                // that was cached is what a cache miss looks like there.
                stream.turn(s, ts, last.input_tokens, cached, write.max(fresh), false);
            }
            (Some("response_item"), Some(kind)) => {
                stamp(s, ts);
                match kind {
                    "function_call" | "custom_tool_call" | "local_shell_call" => {
                        let name = p.name.as_deref().unwrap_or(if kind == "local_shell_call" {
                            "local_shell"
                        } else {
                            ""
                        });
                        let input = p.input.or(p.arguments).map(|r| {
                            serde_json::from_str::<String>(r.get())
                                .unwrap_or_else(|_| r.get().to_string())
                        });
                        let (category, servers) =
                            codex_category(name, p.namespace.as_deref(), input.as_deref());
                        *s.tool_calls.entry(category.clone()).or_default() += 1;
                        for server in servers {
                            *s.mcp_calls.entry(server).or_default() += 1;
                        }
                        if let Some(id) = p.call_id {
                            calls.insert(id.into_owned(), category);
                        }
                    }
                    "function_call_output"
                    | "custom_tool_call_output"
                    | "local_shell_call_output" => {
                        let category = p
                            .call_id
                            .as_deref()
                            .and_then(|id| calls.remove(id))
                            .unwrap_or_else(|| "other".into());
                        let Some(output) = p.output else { return };
                        let raw = output.get();
                        if raw.starts_with('[') {
                            let mut text = 0;
                            for part in serde_json::from_str::<Vec<CodexOutputPart>>(raw)
                                .unwrap_or_default()
                            {
                                match part.kind.as_deref() {
                                    Some("input_image") => {
                                        let url = part.image_url.unwrap_or_default();
                                        let data =
                                            url.split_once("base64,").map(|(_, d)| d).unwrap_or("");
                                        stream.image(s, image_tokens(data));
                                    }
                                    _ => {
                                        text += part
                                            .text
                                            .map(|t| text_tokens(t.get().len()))
                                            .unwrap_or(0)
                                    }
                                }
                            }
                            stream.result(s, &category, text);
                        } else {
                            stream.result(s, &category, text_tokens(raw.len()));
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    });
    stream.finish(days.at(0));
    days.finish()
}

// --- OpenCode ------------------------------------------------------------------------------

#[derive(Deserialize)]
struct OpencodeMessage {
    role: Option<String>,
    #[serde(rename = "modelID")]
    model: Option<String>,
    time: Option<OpencodeTime>,
    tokens: Option<OpencodeTokens>,
}

#[derive(Deserialize)]
struct OpencodeTime {
    created: Option<u64>,
}

#[derive(Deserialize, Default)]
struct OpencodeTokens {
    #[serde(default)]
    input: u64,
    #[serde(default)]
    output: u64,
    #[serde(default)]
    reasoning: u64,
    #[serde(default)]
    cache: OpencodeCache,
}

#[derive(Deserialize, Default)]
struct OpencodeCache {
    #[serde(default)]
    read: u64,
    #[serde(default)]
    write: u64,
}

/// One OpenCode session folder of `storage/message/<session>/*.json`. Best effort: a message
/// in a shape this doesn't know is skipped.
pub fn parse_opencode(dir: &Path) -> DaySummaries {
    let mut days = Days::new(false);
    let mut malformed = 0;
    let mut messages: Vec<OpencodeMessage> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| {
            let text = std::fs::read(e.path()).ok()?;
            match serde_json::from_slice::<OpencodeMessage>(&text) {
                Ok(m) => Some(m),
                Err(_) => {
                    malformed += 1;
                    None
                }
            }
        })
        .collect();
    messages.sort_by_key(|m| m.time.as_ref().and_then(|t| t.created).unwrap_or(0));
    let mut stream = Stream::default();
    for m in messages {
        let ts = m.time.as_ref().and_then(|t| t.created).unwrap_or(0);
        let s = days.at(ts);
        stamp(s, ts);
        if m.role.as_deref() != Some("assistant") {
            continue;
        }
        let Some(t) = m.tokens else { continue };
        let model = m.model.unwrap_or_else(|| "unknown".into());
        s.models.entry(model).or_default().add(&Tokens {
            turns: 1,
            input: t.input,
            output: t.output + t.reasoning,
            cache_read: t.cache.read,
            cache_write_5m: t.cache.write,
            cache_write_1h: 0,
        });
        stream.turn(
            s,
            ts,
            t.input + t.cache.read + t.cache.write,
            t.cache.read,
            t.cache.write,
            false,
        );
    }
    stream.finish(days.at(0));
    days.at(0).malformed_lines += malformed;
    days.finish()
}

// ---------------------------------------------------------------------------------------------
// Cache
// ---------------------------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Default)]
struct CacheFile {
    version: u32,
    files: BTreeMap<String, CacheEntry>,
}

#[derive(Serialize, Deserialize, Clone)]
struct CacheEntry {
    size: u64,
    mtime: u64,
    #[serde(default)]
    files: u64,
    days: DaySummaries,
}

/// The summary cache lives with other rebuildable caches (`TOOLPORT_CACHE_DIR`, else the OS
/// cache folder), never in Toolport's data folder: deleting it only costs one slower run.
fn cache_path() -> Option<PathBuf> {
    let dir = std::env::var_os("TOOLPORT_CACHE_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| dirs::cache_dir().map(|d| d.join("toolport")))?;
    Some(dir.join("usage-summaries.json"))
}

fn load_cache(path: Option<&Path>) -> CacheFile {
    path.and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice::<CacheFile>(&b).ok())
        .filter(|c| c.version == CACHE_VERSION)
        .unwrap_or_default()
}

fn save_cache(path: Option<&Path>, cache: &CacheFile) {
    let Some(path) = path else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(text) = serde_json::to_string(cache) {
        let _ = crate::registry::atomic_write(path, &text);
    }
}

// ---------------------------------------------------------------------------------------------
// Audit log
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
pub struct AuditStats {
    pub calls: BTreeMap<String, u64>,
    pub sessions: BTreeMap<String, BTreeSet<String>>,
    pub bursts: BTreeMap<String, u64>,
    pub response_bytes: BTreeMap<String, u64>,
    pub failures: BTreeMap<String, u64>,
}

#[derive(Deserialize)]
struct AuditRow<'a> {
    kind: Option<std::borrow::Cow<'a, str>>,
    #[serde(borrow)]
    server: Option<std::borrow::Cow<'a, str>>,
    #[serde(rename = "sessionId", borrow)]
    session: Option<std::borrow::Cow<'a, str>>,
    ts: Option<u64>,
    ok: Option<bool>,
    #[serde(rename = "responseBytes")]
    response_bytes: Option<u64>,
}

/// Downstream tool calls per server from Toolport's audit log since `since_ms`.
pub fn read_audit(path: &Path, since_ms: u64) -> AuditStats {
    let mut stats = AuditStats::default();
    let mut per_session: HashMap<(String, String), u64> = HashMap::new();
    read_lines(path, |line| {
        let Ok(row) = serde_json::from_slice::<AuditRow>(line) else {
            return;
        };
        if row.kind.is_some() || row.ts.unwrap_or(0) < since_ms {
            return;
        }
        let Some(server) = row.server.as_deref().filter(|s| *s != "toolport") else {
            return;
        };
        let server = normalize_server(server);
        *stats.calls.entry(server.clone()).or_default() += 1;
        if row.ok == Some(false) {
            *stats.failures.entry(server.clone()).or_default() += 1;
        }
        if let Some(bytes) = row.response_bytes {
            *stats.response_bytes.entry(server.clone()).or_default() += bytes;
        }
        if let Some(session) = row.session {
            stats
                .sessions
                .entry(server.clone())
                .or_default()
                .insert(session.to_string());
            *per_session
                .entry((server, session.into_owned()))
                .or_default() += 1;
        }
    });
    for ((server, _), n) in per_session {
        if n >= BURST_CALLS {
            *stats.bursts.entry(server).or_default() += 1;
        }
    }
    stats
}

// ---------------------------------------------------------------------------------------------
// Pricing
// ---------------------------------------------------------------------------------------------

/// List prices in dollars per million tokens: input, output, cache read. Cache writes bill at
/// 1.25x input (5 minute) and 2x input (1 hour). Anthropic first-party rates as of 2026-11;
/// other providers have no table here and show tokens only.
pub fn price(model: &str) -> Option<(f64, f64, f64)> {
    let m = model.to_ascii_lowercase();
    let table: &[(&str, (f64, f64, f64))] = &[
        ("fable-5-2", (8.0, 40.0, 0.20)),
        ("mythos-5-2", (8.0, 40.0, 0.20)),
        ("fable-5-1", (10.0, 50.0, 1.0)),
        ("mythos-5-1", (10.0, 50.0, 1.0)),
        ("fable-5", (10.0, 50.0, 1.0)),
        ("mythos-5", (10.0, 50.0, 1.0)),
        ("opus-5-5", (4.0, 20.0, 0.20)),
        ("opus-5", (5.0, 25.0, 0.50)),
        ("opus-4-8", (5.0, 25.0, 0.50)),
        ("opus-4-7", (5.0, 25.0, 0.50)),
        ("opus-4-6", (5.0, 25.0, 0.50)),
        ("opus-4-5", (5.0, 25.0, 0.50)),
        ("opus-4", (15.0, 75.0, 1.50)),
        ("sonnet-5-5", (2.0, 10.0, 0.10)),
        ("sonnet-5", (2.0, 10.0, 0.20)),
        ("sonnet-4", (3.0, 15.0, 0.30)),
        ("haiku-5-5", (0.10, 0.50, 0.01)),
        ("haiku-4-5", (1.0, 5.0, 0.10)),
        ("haiku", (0.80, 4.0, 0.08)),
    ];
    if !m.contains("claude") && !m.contains("fable") && !m.contains("mythos") {
        return None;
    }
    table
        .iter()
        .find(|(key, _)| m.contains(key))
        .map(|(_, p)| *p)
}

pub fn cost(model: &str, t: &Tokens) -> Option<f64> {
    let (input, output, read) = price(model)?;
    let m = 1_000_000.0;
    Some(
        t.input as f64 * input / m
            + t.output as f64 * output / m
            + t.cache_read as f64 * read / m
            + t.cache_write_5m as f64 * input * 1.25 / m
            + t.cache_write_1h as f64 * input * 2.0 / m,
    )
}

// ---------------------------------------------------------------------------------------------
// Report
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct UsageReport {
    pub days: u32,
    pub since_ms: u64,
    pub sources: Vec<SourceRow>,
    pub totals: Totals,
    pub models: Vec<ModelRow>,
    pub clients: Vec<ClientRow>,
    pub resident: Vec<Share>,
    pub startup: Vec<super::scan::BudgetRow>,
    pub cache: CacheHealth,
    pub waste: Waste,
    pub mcp: Vec<McpRow>,
    pub unused_servers: Vec<UnusedServer>,
    pub recommendations: Vec<Recommendation>,
    pub notes: Vec<String>,
    pub stats: RunStats,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRow {
    pub client: Client,
    pub root: String,
    pub sessions: usize,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Totals {
    pub sessions: usize,
    /// Subagent contexts (their turns are included in every total).
    pub subagents: usize,
    pub turns: u64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub mean_context: u64,
    pub max_context: u64,
    pub cache_read_share: f64,
    /// List-price equivalent for models with a price table. Not what a subscription bills.
    pub list_price_usd: Option<f64>,
    pub unpriced_turns: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRow {
    pub model: String,
    #[serde(flatten)]
    pub tokens: Tokens,
    pub list_price_usd: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientRow {
    pub client: Client,
    pub sessions: usize,
    pub turns: u64,
    pub mean_context: u64,
    pub cache_read_share: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Share {
    pub source: String,
    pub label: String,
    /// Token-turns resident in context.
    pub tokens: u64,
    pub share: f64,
    /// Tokens these results added, each counted once.
    pub produced: u64,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CacheHealth {
    pub idle_misses: u64,
    pub idle_tokens: u64,
    pub breaks: u64,
    pub break_tokens: u64,
    pub compactions: u64,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Waste {
    pub duplicate_reads: u64,
    pub duplicate_read_tokens: u64,
    pub images: u64,
    pub image_tokens: u64,
    pub lingering_images: u64,
    pub mean_linger_turns: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpRow {
    pub server: String,
    pub calls: u64,
    pub sessions: u64,
    pub bursts: u64,
    pub resident_share: f64,
    pub response_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnusedServer {
    pub name: String,
    pub configured_in: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Recommendation {
    pub id: &'static str,
    pub severity: Severity,
    pub title: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RunStats {
    pub files: usize,
    pub cached: usize,
    pub parsed: usize,
    pub bytes_read: u64,
    pub malformed_lines: u64,
    pub elapsed_ms: u64,
}

pub struct Options {
    pub days: u32,
    pub clients: BTreeSet<Client>,
    pub now_ms: u64,
    pub use_cache: bool,
    pub cache_path: Option<PathBuf>,
    pub progress: bool,
}

/// Servers configured somewhere this report can see calls for, by normalized name.
pub type Configured = BTreeMap<String, (String, BTreeSet<String>)>;

pub fn analyze(sources: &Sources, opts: &Options, configured: &Configured) -> UsageReport {
    let started = std::time::Instant::now();
    let since_ms = opts.now_ms.saturating_sub(opts.days as u64 * 86_400_000);
    let all = units(sources, &opts.clients, since_ms);
    let mut cache = if opts.use_cache {
        load_cache(opts.cache_path.as_deref())
    } else {
        CacheFile::default()
    };
    cache.version = CACHE_VERSION;
    let mut todo = Vec::new();
    let mut done: Vec<(Unit, DaySummaries)> = Vec::new();
    for unit in all {
        let key = unit.path.to_string_lossy().into_owned();
        match cache.files.get(&key) {
            Some(e) if e.size == unit.size && e.mtime == unit.mtime_ms && e.files == unit.files => {
                done.push((unit, e.days.clone()));
            }
            _ => todo.push(unit),
        }
    }
    let mut stats = RunStats {
        cached: done.len(),
        parsed: todo.len(),
        bytes_read: todo.iter().map(|u| u.size).sum(),
        ..Default::default()
    };
    let parsed = parse_all(todo, opts.progress);
    for (unit, summary) in &parsed {
        cache.files.insert(
            unit.path.to_string_lossy().into_owned(),
            CacheEntry {
                size: unit.size,
                mtime: unit.mtime_ms,
                files: unit.files,
                days: summary.clone(),
            },
        );
    }
    if opts.use_cache && !parsed.is_empty() {
        // Keep entries for a year so a wider --days stays fast; drop files that are gone.
        let keep = opts.now_ms.saturating_sub(366 * 86_400_000);
        cache
            .files
            .retain(|path, e| e.mtime >= keep && Path::new(path).exists());
        save_cache(opts.cache_path.as_deref(), &cache);
    }
    done.extend(parsed);
    let audit = sources
        .audit
        .as_deref()
        .filter(|p| p.is_file())
        .map(|p| read_audit(p, since_ms));
    let in_range = done
        .into_iter()
        .map(|(unit, days)| (unit, in_window(&days, since_ms)))
        .collect();
    let mut report = reduce(in_range, since_ms, audit.as_ref(), configured);
    report.days = opts.days;
    report.since_ms = since_ms;
    stats.files = stats.cached + stats.parsed;
    stats.malformed_lines = report.stats.malformed_lines;
    stats.elapsed_ms = started.elapsed().as_millis() as u64;
    report.stats = stats;
    for root in &sources.opencode {
        if !root.join("storage").join("message").is_dir() && root.join("opencode.db").is_file() {
            report.notes.push(format!(
                "OpenCode keeps its history in {} (SQLite), which this version doesn't read yet; OpenCode sessions are not counted.",
                format::home_relative(&root.join("opencode.db"))
            ));
        }
    }
    report
}

fn parse_all(todo: Vec<Unit>, progress: bool) -> Vec<(Unit, DaySummaries)> {
    if todo.is_empty() {
        return Vec::new();
    }
    let total = todo.len();
    let next = AtomicUsize::new(0);
    let finished = AtomicUsize::new(0);
    let results = Mutex::new(Vec::with_capacity(total));
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, 16)
        .min(total);
    // Biggest first, so one large active transcript doesn't finish last on its own.
    let mut todo = todo;
    todo.sort_by_key(|a| std::cmp::Reverse(a.size));
    let show = progress && total >= 20;
    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(unit) = todo.get(i) else { break };
                let summary = match unit.client {
                    Client::Claude => parse_claude(&unit.path),
                    Client::Codex => parse_codex(&unit.path),
                    Client::Opencode => parse_opencode(&unit.path),
                };
                results.lock().unwrap().push((unit.clone(), summary));
                let n = finished.fetch_add(1, Ordering::Relaxed) + 1;
                if show && (n.is_multiple_of(25) || n == total) {
                    eprint!("\r  reading transcripts {n}/{total}");
                }
            });
        }
    });
    if show {
        eprint!("\r\x1b[2K");
    }
    results.into_inner().unwrap()
}

fn label_for(source: &str) -> String {
    match source {
        "floor" => "Startup floor (system, tools, rules)".into(),
        "bash" => "Bash and shell output".into(),
        "read" => "Read: files".into(),
        "image" => "Read: screenshots and images".into(),
        "search" => "Search (Grep, Glob)".into(),
        "edit" => "Edits and patches".into(),
        "web" => "Web fetch and search".into(),
        "subagent" => "Subagent results".into(),
        "other" => "Other tools".into(),
        "conversation" => "Conversation and reasoning".into(),
        s => match s.strip_prefix("mcp:") {
            Some(server) => format!("MCP: {server}"),
            None => s.to_string(),
        },
    }
}

fn reduce(
    files: Vec<(Unit, FileSummary)>,
    since_ms: u64,
    audit: Option<&AuditStats>,
    configured: &Configured,
) -> UsageReport {
    let mut r = UsageReport::default();
    let mut models: BTreeMap<String, Tokens> = BTreeMap::new();
    let mut by_client: BTreeMap<Client, (usize, u64, u64, u64, u64)> = BTreeMap::new();
    let mut by_root: BTreeMap<(Client, PathBuf), usize> = BTreeMap::new();
    let mut resident: BTreeMap<String, u64> = BTreeMap::new();
    let mut produced: BTreeMap<String, u64> = BTreeMap::new();
    let mut floor = 0u64;
    let mut context = 0u64;
    let mut linger_turns = 0u64;
    let mut transcript_mcp: BTreeMap<String, (u64, u64, u64)> = BTreeMap::new();
    for (unit, s) in files {
        r.stats.malformed_lines += s.malformed_lines;
        if s.turns == 0 && s.models.is_empty() {
            continue;
        }
        if s.last_ms != 0 && s.last_ms < since_ms {
            continue;
        }
        let session = usize::from(!s.subagent);
        r.totals.sessions += session;
        r.totals.subagents += 1 - session;
        *by_root.entry((unit.client, unit.root.clone())).or_default() += session;
        let c = by_client.entry(unit.client).or_default();
        c.0 += session;
        c.1 += s.turns;
        c.2 += s.context_sum;
        for (model, t) in &s.models {
            models.entry(model.clone()).or_default().add(t);
            c.3 += t.cache_read;
            c.4 += t.context();
        }
        for (k, v) in &s.resident {
            *resident.entry(k.clone()).or_default() += v;
        }
        for (k, v) in &s.produced {
            *produced.entry(k.clone()).or_default() += v;
        }
        floor += s.floor_sum;
        context += s.context_sum;
        r.totals.turns += s.turns;
        r.totals.max_context = r.totals.max_context.max(s.context_max);
        r.cache.idle_misses += s.misses_idle;
        r.cache.idle_tokens += s.misses_idle_tokens;
        r.cache.breaks += s.misses_break;
        r.cache.break_tokens += s.misses_break_tokens;
        r.cache.compactions += s.compactions;
        r.waste.duplicate_reads += s.dup_reads;
        r.waste.duplicate_read_tokens += s.dup_read_tokens;
        r.waste.images += s.images;
        r.waste.image_tokens += s.image_tokens;
        r.waste.lingering_images += s.images_lingering;
        linger_turns += s.image_linger_turns;
        for (server, calls) in &s.mcp_calls {
            let e = transcript_mcp.entry(server.clone()).or_default();
            e.0 += calls;
            e.1 += session as u64;
            if *calls >= BURST_CALLS {
                e.2 += 1;
            }
        }
    }
    r.waste.mean_linger_turns = linger_turns
        .checked_div(r.waste.lingering_images)
        .unwrap_or(0);
    r.sources = by_root
        .into_iter()
        .map(|((client, root), sessions)| SourceRow {
            client,
            root: format::home_relative(&root),
            sessions,
        })
        .collect();
    // Totals and cost.
    let mut total_cost = 0.0;
    let mut priced = false;
    for (model, t) in &models {
        r.totals.input += t.input;
        r.totals.output += t.output;
        r.totals.cache_read += t.cache_read;
        r.totals.cache_write += t.cache_write_5m + t.cache_write_1h;
        let c = cost(model, t);
        match c {
            Some(c) => {
                total_cost += c;
                priced = true;
            }
            None => r.totals.unpriced_turns += t.turns,
        }
        r.models.push(ModelRow {
            model: model.clone(),
            tokens: t.clone(),
            list_price_usd: c,
        });
    }
    r.models
        .sort_by_key(|a| std::cmp::Reverse(a.tokens.context()));
    r.totals.list_price_usd = priced.then_some(total_cost);
    let all_input = r.totals.input + r.totals.cache_read + r.totals.cache_write;
    r.totals.cache_read_share = ratio(r.totals.cache_read, all_input);
    r.totals.mean_context = context.checked_div(r.totals.turns).unwrap_or(0);
    r.clients = by_client
        .into_iter()
        .map(|(client, (sessions, turns, ctx, read, all))| ClientRow {
            client,
            sessions,
            turns,
            mean_context: ctx.checked_div(turns).unwrap_or(0),
            cache_read_share: ratio(read, all),
        })
        .collect();
    // Residency shares.
    let attributed: u64 = floor + resident.values().sum::<u64>();
    let mut shares: Vec<Share> = resident
        .iter()
        .map(|(k, v)| Share {
            source: k.clone(),
            label: label_for(k),
            tokens: *v,
            share: ratio(*v, context),
            produced: produced.get(k).copied().unwrap_or(0),
        })
        .collect();
    shares.push(Share {
        source: "floor".into(),
        label: label_for("floor"),
        tokens: floor,
        share: ratio(floor, context),
        produced: 0,
    });
    let rest = context.saturating_sub(attributed);
    shares.push(Share {
        source: "conversation".into(),
        label: label_for("conversation"),
        tokens: rest,
        share: ratio(rest, context),
        produced: 0,
    });
    shares.retain(|s| s.tokens > 0);
    shares.sort_by_key(|a| std::cmp::Reverse(a.tokens));
    r.resident = shares;
    // MCP servers: transcript calls plus Toolport's audit log.
    let mut mcp: BTreeMap<String, McpRow> = BTreeMap::new();
    for (server, (calls, sessions, bursts)) in &transcript_mcp {
        mcp.insert(
            server.clone(),
            McpRow {
                server: server.clone(),
                calls: *calls,
                sessions: *sessions,
                bursts: *bursts,
                resident_share: ratio(
                    resident.get(&format!("mcp:{server}")).copied().unwrap_or(0),
                    context,
                ),
                response_bytes: None,
            },
        );
    }
    if let Some(a) = audit {
        for (server, calls) in &a.calls {
            let row = mcp.entry(server.clone()).or_insert_with(|| McpRow {
                server: server.clone(),
                calls: 0,
                sessions: 0,
                bursts: 0,
                resident_share: 0.0,
                response_bytes: None,
            });
            // Gateway calls appear in both; the larger count is the better one.
            row.calls = row.calls.max(*calls);
            row.sessions = row
                .sessions
                .max(a.sessions.get(server).map(|s| s.len() as u64).unwrap_or(0));
            row.bursts = row.bursts.max(a.bursts.get(server).copied().unwrap_or(0));
            row.response_bytes = a.response_bytes.get(server).copied();
        }
    }
    let mut mcp: Vec<McpRow> = mcp.into_values().collect();
    mcp.sort_by_key(|a| std::cmp::Reverse(a.calls));
    // Unused: configured where calls are visible, with sessions in the window, and no calls.
    if r.totals.sessions > 0 {
        let used: BTreeSet<&str> = mcp.iter().map(|m| m.server.as_str()).collect();
        r.unused_servers = configured
            .iter()
            .filter(|(norm, _)| !used.contains(norm.as_str()) && norm.as_str() != "toolport")
            .map(|(_, (name, clients))| UnusedServer {
                name: name.clone(),
                configured_in: clients.iter().cloned().collect(),
            })
            .collect();
    }
    r.mcp = mcp;
    r.recommendations = recommendations(&r);
    r
}

fn ratio(a: u64, b: u64) -> f64 {
    if b == 0 {
        0.0
    } else {
        a as f64 / b as f64
    }
}

fn recommendations(r: &UsageReport) -> Vec<Recommendation> {
    let mut out = Vec::new();
    let share = |source: &str| {
        r.resident
            .iter()
            .find(|s| s.source == source)
            .map(|s| s.share)
            .unwrap_or(0.0)
    };
    let bash = share("bash");
    if bash >= 0.15 {
        out.push(Recommendation {
            id: "bash-output",
            severity: Severity::Warn,
            title: format!("Shell output holds {} of resident context", format::percent(bash)),
            detail: "Trim it where it starts: rtk (github.com/rtk-ai/rtk) compacts common command output, and asking agents to pipe long output through head or a grep keeps logs out of context.".into(),
        });
    }
    if r.waste.lingering_images > 0 {
        out.push(Recommendation {
            id: "lingering-screenshots",
            severity: Severity::Warn,
            title: format!(
                "{} stayed in context for {} turns on average",
                format::plural(r.waste.lingering_images as usize, "screenshot", "screenshots"),
                r.waste.mean_linger_turns
            ),
            detail: "Start a fresh session after a visual check, or save screenshots to disk and Read only the one that matters.".into(),
        });
    }
    if r.cache.breaks > 0 {
        out.push(Recommendation {
            id: "cache-breaks",
            severity: Severity::Warn,
            title: format!(
                "{} rebuilt the cache mid-session ({} tokens rewritten)",
                format::plural(r.cache.breaks as usize, "turn", "turns"),
                format::compact(r.cache.break_tokens)
            ),
            detail: "Something changed the prompt prefix while the cache was still warm: a model or effort switch, a changing MCP tool list, or an edited rules file. Toolport's gateway keeps the tool list stable; run 'toolport scan' to check what each agent loads.".into(),
        });
    }
    if r.cache.idle_misses > 0 {
        out.push(Recommendation {
            id: "idle-expiry",
            severity: Severity::Info,
            title: format!(
                "{} resumed after the cache expired ({} tokens rewritten)",
                format::plural(r.cache.idle_misses as usize, "session", "sessions"),
                format::compact(r.cache.idle_tokens)
            ),
            detail: "Expected after a long pause. A fresh session with a short summary is cheaper than resuming a very large one.".into(),
        });
    }
    // Claude Code answers a repeat read of an unchanged file with a short stub, so the
    // count alone overstates it; suggest a rule only when the repeats cost real context.
    if r.waste.duplicate_reads > 0 && r.waste.duplicate_read_tokens >= 250_000 {
        out.push(Recommendation {
            id: "duplicate-reads",
            severity: Severity::Info,
            title: format!(
                "{} of unchanged files ({} tokens)",
                format::plural(r.waste.duplicate_reads as usize, "repeat read", "repeat reads"),
                format::compact(r.waste.duplicate_read_tokens)
            ),
            detail: "The same file and range was read again with no edit in between. A rules line like 'reuse earlier reads unless the file changed' helps.".into(),
        });
    }
    for m in r.mcp.iter().filter(|m| m.bursts > 0).take(3) {
        out.push(Recommendation {
            id: "code-mode",
            severity: Severity::Info,
            title: format!(
                "{} had {} of {}+ calls in one session",
                m.server,
                format::plural(m.bursts as usize, "burst", "bursts"),
                BURST_CALLS
            ),
            detail: "A Code Mode candidate: set TOOLPORT_DISCOVERY=code-mode (or clientDiscovery = \"code-mode\") so one script runs those calls and only the result enters context.".into(),
        });
    }
    if !r.unused_servers.is_empty() {
        out.push(Recommendation {
            id: "unused-servers",
            severity: Severity::Info,
            title: format!(
                "{} had no calls in {} days",
                format::plural(r.unused_servers.len(), "MCP server", "MCP servers"),
                r.days.max(1)
            ),
            detail: "Their tool definitions still load every session. Disable or remove them; 'toolport scan' shows where each one is configured.".into(),
        });
    }
    if let Some(top) = r.startup.first() {
        if top.total_tokens > 8_000 {
            out.push(Recommendation {
                id: "startup-tax",
                severity: Severity::Info,
                title: format!(
                    "{} loads {} tokens of rules and skill listings before the first prompt",
                    top.agent_name,
                    format::thousands(top.total_tokens)
                ),
                detail: "Every turn pays for them. Move rarely needed rules into skills, and turn off skills an agent never uses.".into(),
            });
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------------------------

pub fn render(r: &UsageReport, style: &Style) -> String {
    let mut out = String::new();
    let clients: Vec<String> = r
        .clients
        .iter()
        .map(|c| c.client.label().to_string())
        .collect();
    out.push_str(&format!(
        "{} {}\n",
        style.bold("toolport usage"),
        style.dim(&format!(
            "· last {} days · {}",
            r.days,
            if clients.is_empty() {
                "no transcripts".to_string()
            } else {
                clients.join(", ")
            }
        ))
    ));
    if r.totals.sessions == 0 {
        out.push_str(&format!(
            "\n  {} No agent sessions found in the last {} days.\n",
            style.mark(Severity::Info),
            r.days
        ));
        out.push_str(&format!(
            "  {}\n",
            style.dim("Looked for Claude Code (~/.claude*/projects), Codex (~/.codex/sessions) and OpenCode storage.")
        ));
        for note in &r.notes {
            out.push_str(&format!(
                "  {} {}\n",
                style.mark(Severity::Info),
                style.dim(note)
            ));
        }
        return out;
    }
    let t = &r.totals;
    let cost = match t.list_price_usd {
        Some(c) => format!("{} list-price equivalent", style.bold(&format::money(c))),
        None => style.dim("no list prices for these models"),
    };
    out.push_str(&format!(
        "\n  {}  {}  {}  {}\n",
        stat(style, &format::thousands(t.sessions as u64), "sessions"),
        stat(style, &format::compact(t.turns), "model turns"),
        stat(style, &format::compact(t.mean_context), "mean context/turn"),
        stat(
            style,
            &format::percent(t.cache_read_share),
            "of input from cache"
        ),
    ));
    out.push_str(&format!("  {cost}"));
    if t.unpriced_turns > 0 && t.list_price_usd.is_some() {
        out.push_str(&style.dim(&format!(
            "  ({} turns on models without a price table are not included)",
            format::compact(t.unpriced_turns)
        )));
    }
    out.push('\n');

    // Per-model table.
    out.push_str(&style.heading("Models"));
    let rows: Vec<Vec<String>> = r
        .models
        .iter()
        .take(6)
        .map(|m| {
            vec![
                m.model.clone(),
                format!("{} turns", format::compact(m.tokens.turns)),
                format!("in {}", format::compact(m.tokens.input)),
                format!("cache {}", format::compact(m.tokens.cache_read)),
                format!(
                    "write {}",
                    format::compact(m.tokens.cache_write_5m + m.tokens.cache_write_1h)
                ),
                format!("out {}", format::compact(m.tokens.output)),
                m.list_price_usd
                    .map(format::money)
                    .unwrap_or_else(|| style.dim("n/a")),
            ]
        })
        .collect();
    out.push_str(&format::table(
        &rows,
        &[
            Align::Left,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
            Align::Right,
        ],
        2,
    ));

    // Resident context.
    out.push_str(&style.heading(&format!(
        "What stays in context  {}",
        style.dim("share of every turn's input, by source")
    )));
    let max = r
        .resident
        .iter()
        .map(|s| s.share)
        .fold(0.0, f64::max)
        .max(0.0001);
    let rows: Vec<Vec<String>> = r
        .resident
        .iter()
        .take(12)
        .map(|s| {
            let colorize = |text: &str| match s.source.as_str() {
                "floor" => style.magenta(text),
                "conversation" => style.dim(text),
                src if src.starts_with("mcp:") => style.yellow(text),
                "image" => style.red(text),
                _ => style.cyan(text),
            };
            vec![
                format::truncate(&s.label, 38),
                colorize(&style.bar(s.share / max, 24)),
                format::percent(s.share),
                style.dim(&if s.produced > 0 {
                    format!("{} added", format::compact(s.produced))
                } else {
                    String::new()
                }),
            ]
        })
        .collect();
    out.push_str(&format::table(
        &rows,
        &[Align::Left, Align::Left, Align::Right, Align::Right],
        2,
    ));

    // Startup tax.
    if !r.startup.is_empty() {
        out.push_str(&style.heading(&format!(
            "Turn-0 startup tax  {}",
            style.dim("rules and skill listings loaded before the first prompt")
        )));
        let rows: Vec<Vec<String>> = r
            .startup
            .iter()
            .take(6)
            .map(|b| {
                vec![
                    b.agent_name.clone(),
                    format!("{} tok", format::thousands(b.total_tokens)),
                    style.dim(&format!(
                        "rules {} in {} · skills {} in {}",
                        format::compact(b.rules_tokens),
                        format::plural(b.rules_files.len(), "file", "files"),
                        format::compact(b.skills_tokens),
                        b.skills_loaded
                    )),
                ]
            })
            .collect();
        out.push_str(&format::table(
            &rows,
            &[Align::Left, Align::Right, Align::Left],
            2,
        ));
    }

    // Cache health.
    out.push_str(&style.heading("Cache health"));
    let c = &r.cache;
    let healthy = c.breaks == 0;
    out.push_str(&format!(
        "  {} {} from cache · {} mid-session {} · {} idle {} · {}\n",
        style.mark(if healthy {
            Severity::Ok
        } else {
            Severity::Warn
        }),
        format::percent(r.totals.cache_read_share),
        if c.breaks > 0 {
            style.yellow(&format::thousands(c.breaks))
        } else {
            c.breaks.to_string()
        },
        if c.breaks == 1 { "break" } else { "breaks" },
        format::thousands(c.idle_misses),
        if c.idle_misses == 1 {
            "expiry"
        } else {
            "expiries"
        },
        format::plural(c.compactions as usize, "compaction", "compactions"),
    ));
    let w = &r.waste;
    if w.images > 0 || w.duplicate_reads > 0 {
        out.push_str(&format!(
            "  {} {} ({} tokens) · {} lingering · {} repeat reads ({} tokens)\n",
            style.mark(Severity::Info),
            format::plural(w.images as usize, "image", "images"),
            format::compact(w.image_tokens),
            format::thousands(w.lingering_images),
            format::thousands(w.duplicate_reads),
            format::compact(w.duplicate_read_tokens),
        ));
    }

    // MCP.
    if !r.mcp.is_empty() || !r.unused_servers.is_empty() {
        out.push_str(&style.heading("MCP servers"));
        let rows: Vec<Vec<String>> = r
            .mcp
            .iter()
            .take(10)
            .map(|m| {
                vec![
                    m.server.clone(),
                    format!("{} calls", format::thousands(m.calls)),
                    format::plural(m.sessions as usize, "session", "sessions"),
                    if m.bursts > 0 {
                        style.yellow(&format::plural(m.bursts as usize, "burst", "bursts"))
                    } else {
                        style.dim("no bursts")
                    },
                    style.dim(&format!("{} of context", format::percent(m.resident_share))),
                ]
            })
            .collect();
        out.push_str(&format::table(
            &rows,
            &[
                Align::Left,
                Align::Right,
                Align::Right,
                Align::Left,
                Align::Left,
            ],
            2,
        ));
        if !r.unused_servers.is_empty() {
            let names: Vec<&str> = r.unused_servers.iter().map(|u| u.name.as_str()).collect();
            out.push_str(&format!(
                "  {} unused: {}\n",
                style.mark(Severity::Warn),
                format::truncate(&names.join(", "), 90)
            ));
        }
    }

    // Recommendations.
    if !r.recommendations.is_empty() {
        out.push_str(&style.heading("Recommendations"));
        for rec in &r.recommendations {
            out.push_str(&format!(
                "  {} {}\n    {}\n",
                style.mark(rec.severity),
                style.bold(&rec.title),
                style.dim(&rec.detail)
            ));
        }
    }
    for note in &r.notes {
        out.push_str(&format!(
            "\n  {} {}\n",
            style.mark(Severity::Info),
            style.dim(note)
        ));
    }
    out.push_str(&format!(
        "\n{}\n",
        style.dim(&format!(
            "{} transcripts ({} cached, {} read, {}) in {} ms · estimates: text at ~4 chars/token, cost at list price",
            format::thousands(r.stats.files as u64),
            format::thousands(r.stats.cached as u64),
            format::thousands(r.stats.parsed as u64),
            format::bytes(r.stats.bytes_read),
            format::thousands(r.stats.elapsed_ms)
        ))
    ));
    out
}

fn stat(style: &Style, value: &str, label: &str) -> String {
    format!("{} {}", style.bold(value), style.dim(label))
}

pub fn parse_clients(value: Option<&str>) -> Result<BTreeSet<Client>, CliError> {
    let all: BTreeSet<Client> = [Client::Claude, Client::Codex, Client::Opencode].into();
    match value.unwrap_or("all") {
        "all" => Ok(all),
        "claude" | "claude-code" => Ok([Client::Claude].into()),
        "codex" => Ok([Client::Codex].into()),
        "opencode" => Ok([Client::Opencode].into()),
        other => Err(CliError::new(format!("unknown client '{other}'"))
            .hint("use claude, codex, opencode or all")),
    }
}

/// Servers configured in Toolport and in the agents `--client` selected, whose calls this
/// report can see.
pub fn configured_servers(
    snapshot: &super::scan::Snapshot,
    clients: &BTreeSet<Client>,
) -> Configured {
    let mut out: Configured = BTreeMap::new();
    if let Ok(registry) = crate::registry::load() {
        for s in registry.servers.iter().filter(|s| s.enabled) {
            if crate::clients::is_gateway_server(s) {
                continue;
            }
            out.entry(normalize_server(&s.name))
                .or_insert_with(|| (s.name.clone(), BTreeSet::new()))
                .1
                .insert("Toolport".into());
        }
    }
    add_client_servers(&mut out, snapshot.present(), clients);
    out
}

/// The servers each selected agent configures. Another agent's servers are not unused just
/// because this report doesn't read that agent's transcripts.
fn add_client_servers<'a>(
    out: &mut Configured,
    detected: impl Iterator<Item = &'a crate::clients::DetectedClient>,
    clients: &BTreeSet<Client>,
) {
    for client in detected {
        let kind = match client.id.split('@').next().unwrap_or(&client.id) {
            "claude-code" => Client::Claude,
            "codex" => Client::Codex,
            "opencode" => Client::Opencode,
            _ => continue,
        };
        if !clients.contains(&kind) {
            continue;
        }
        for s in &client.servers {
            if crate::clients::detected_is_gateway(s) {
                continue;
            }
            out.entry(normalize_server(&s.name))
                .or_insert_with(|| (s.name.clone(), BTreeSet::new()))
                .1
                .insert(super::scan::display_name(client));
        }
    }
}

pub fn build(ctx: &Ctx, days: u32, clients: BTreeSet<Client>, use_cache: bool) -> UsageReport {
    let project = ctx.project_str();
    let snapshot = super::scan::Snapshot::read(project.as_deref());
    let sources = Sources::discover(&snapshot.clients);
    let configured = configured_servers(&snapshot, &clients);
    let opts = Options {
        days,
        clients,
        now_ms: super::now_ms(),
        use_cache,
        cache_path: cache_path(),
        progress: !ctx.json && std::io::IsTerminal::is_terminal(&std::io::stderr()),
    };
    let mut report = analyze(&sources, &opts, &configured);
    if let Some(inv) = &snapshot.inventory {
        report.startup = super::scan::budget(inv);
        report.recommendations = recommendations(&report);
    }
    report
}

pub fn command(ctx: &Ctx, args: &Args) -> Result<i32, CliError> {
    let days = args.number("days", 30, 3650)?;
    let clients = parse_clients(args.value("client"))?;
    let report = build(ctx, days, clients, !args.has("no-cache"));
    if ctx.json {
        ctx.emit_json(&report)?;
    } else {
        super::out(&render(&report, &ctx.style));
    }
    Ok(EXIT_OK)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_timestamps() {
        assert_eq!(parse_iso_ms("1970-01-01T00:00:00Z"), 0);
        assert_eq!(parse_iso_ms("1970-01-01T00:00:01.5Z"), 1500);
        assert_eq!(parse_iso_ms("2026-11-24T00:00:00.000Z"), 1_795_478_400_000);
        assert_eq!(
            parse_iso_ms("2026-11-24T01:00:00+01:00"),
            parse_iso_ms("2026-11-24T00:00:00Z")
        );
        assert_eq!(parse_iso_ms("garbage"), 0);
    }

    #[test]
    fn png_and_gif_dimensions_size_images() {
        use base64::Engine as _;
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend_from_slice(&1500u32.to_be_bytes());
        png.extend_from_slice(&750u32.to_be_bytes());
        let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
        assert_eq!(image_tokens(&b64), 1500);
        let gif = [b'G', b'I', b'F', b'8', b'9', b'a', 100, 0, 75, 0];
        let b64 = base64::engine::general_purpose::STANDARD.encode(gif);
        assert_eq!(image_tokens(&b64), 10);
        assert_eq!(image_tokens("not-base64!"), 1200);
    }

    #[test]
    fn server_names_normalize_like_tool_names() {
        assert_eq!(mcp_server_of("mcp__t3-code__send"), Some("t3_code".into()));
        assert_eq!(mcp_server_of("mcp__GitHub__x"), Some("github".into()));
        assert_eq!(mcp_server_of("Bash"), None);
        assert_eq!(normalize_server("My Server.v2"), "my_server_v2");
    }

    #[test]
    fn prices_cover_current_claude_models_only() {
        assert_eq!(price("claude-opus-5-5"), Some((4.0, 20.0, 0.20)));
        assert_eq!(price("claude-opus-4-1-20250805"), Some((15.0, 75.0, 1.5)));
        assert_eq!(price("claude-sonnet-4-6"), Some((3.0, 15.0, 0.3)));
        assert_eq!(price("gpt-6.1-sol"), None);
        let t = Tokens {
            turns: 1,
            input: 1_000_000,
            output: 1_000_000,
            cache_read: 1_000_000,
            cache_write_5m: 1_000_000,
            cache_write_1h: 1_000_000,
        };
        let c = cost("claude-opus-5-5", &t).unwrap();
        assert!((c - (4.0 + 20.0 + 0.2 + 5.0 + 8.0)).abs() < 1e-9, "{c}");
    }

    #[test]
    fn residency_and_cache_misses() {
        let mut s = FileSummary::default();
        let mut st = Stream::default();
        st.turn(&mut s, 1_000, 30_000, 0, 30_000, false);
        st.result(&mut s, "bash", 5_000);
        st.turn(&mut s, 2_000, 36_000, 30_000, 6_000, false);
        st.turn(&mut s, 3_000, 37_000, 36_000, 1_000, false);
        // Unexplained break: cache dropped within the TTL.
        st.turn(&mut s, 4_000, 38_000, 0, 38_000, false);
        // Idle expiry: two hours later.
        st.turn(&mut s, 4_000 + 7_200_000, 39_000, 0, 39_000, false);
        // Compaction ends the bash residency.
        st.turn(&mut s, 4_100 + 7_200_000, 8_000, 0, 8_000, false);
        st.finish(&mut s);
        assert_eq!(s.turns, 6);
        assert_eq!(s.resident["bash"], 5_000 * 4);
        assert_eq!(s.misses_break, 1);
        assert_eq!(s.misses_idle, 1);
        assert_eq!(s.compactions, 1);
        assert_eq!(s.floor, 30_000);
        assert!(s.floor_sum <= s.context_sum);
    }

    /// A fresh directory for one test's fixture files, outside any real data directory.
    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("toolport-usage-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_lines(path: &Path, lines: &[serde_json::Value]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let text: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        std::fs::write(path, text.join("\n") + "\n").unwrap();
    }

    fn options(clients: BTreeSet<Client>, cache_path: Option<PathBuf>) -> Options {
        Options {
            days: 30,
            clients,
            now_ms: crate::cli::now_ms(),
            use_cache: cache_path.is_some(),
            cache_path,
            progress: false,
        }
    }

    #[test]
    fn compaction_resets_the_floor() {
        let mut s = FileSummary::default();
        let mut st = Stream::default();
        st.turn(&mut s, 1_000, 30_000, 0, 30_000, false);
        st.result(&mut s, "bash", 2_000);
        st.turn(&mut s, 2_000, 32_000, 30_000, 2_000, false);
        // Compaction: the summary turn is the new floor.
        st.turn(&mut s, 3_000, 8_000, 0, 8_000, false);
        st.result(&mut s, "bash", 17_000);
        st.turn(&mut s, 4_000, 25_000, 8_000, 17_000, false);
        // An explicit boundary (Claude Code's compact_boundary) does the same.
        st.compact(&mut s);
        st.prev_ctx = 0;
        st.turn(&mut s, 5_000, 6_000, 0, 6_000, false);
        st.result(&mut s, "read", 4_000);
        st.turn(&mut s, 6_000, 10_000, 6_000, 4_000, false);
        st.finish(&mut s);
        assert_eq!(s.compactions, 2);
        // The startup floor stays the first turn's.
        assert_eq!(s.floor, 30_000);
        assert_eq!(s.floor_sum, 30_000 * 2 + 8_000 * 2 + 6_000 * 2);
        // 2k before compaction, then all 17k of growth after it, not hidden by the old floor.
        assert_eq!(s.resident["bash"], 2_000 + 17_000);
        assert_eq!(s.resident["read"], 4_000);
    }

    #[test]
    fn claude_mcp_calls_count_on_the_day_they_happened() {
        let dir = scratch("claude-mcp-days");
        let path = dir.join("s.jsonl");
        let call = |ts: Option<&str>, id: &str, tool: &str| {
            let mut line = serde_json::json!({
                "type": "assistant",
                "message": {
                    "id": id,
                    "model": "claude-opus-5-5",
                    "usage": {"input_tokens": 10, "cache_creation_input_tokens": 30_000, "output_tokens": 5},
                    "content": [{"type": "tool_use", "id": format!("t-{id}"), "name": tool, "input": {}}]
                }
            });
            if let Some(ts) = ts {
                line["timestamp"] = ts.into();
            }
            line
        };
        write_lines(
            &path,
            &[
                call(Some("2026-11-14T10:00:00Z"), "m1", "mcp__github__search"),
                call(Some("2026-11-24T10:00:00Z"), "m2", "mcp__github__search"),
                call(Some("2026-11-24T10:01:00Z"), "m3", "mcp__linear__list"),
                // No timestamp: the day of the record before it.
                call(None, "m4", "mcp__github__search"),
            ],
        );
        let days = parse_claude(&path);
        assert_eq!(days.len(), 2);
        let recent = in_window(&days, parse_iso_ms("2026-11-24T00:00:00Z"));
        assert_eq!(
            recent.mcp_calls,
            BTreeMap::from([("github".into(), 2), ("linear".into(), 1)])
        );
        let all = in_window(&days, parse_iso_ms("2026-11-01T00:00:00Z"));
        assert_eq!(all.mcp_calls["github"], 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn codex_mcp_calls_count_on_the_day_they_happened() {
        let dir = scratch("codex-mcp-days");
        let path = dir.join("rollout.jsonl");
        let call = |ts: Option<&str>, id: &str, name: &str, namespace: Option<&str>| {
            let mut line = serde_json::json!({
                "type": "response_item",
                "payload": {"type": "function_call", "name": name, "arguments": "{}", "call_id": id}
            });
            if let Some(ts) = ts {
                line["timestamp"] = ts.into();
            }
            if let Some(ns) = namespace {
                line["payload"]["namespace"] = ns.into();
            }
            line
        };
        write_lines(
            &path,
            &[
                call(
                    Some("2026-11-14T10:00:00Z"),
                    "c1",
                    "search",
                    Some("mcp__github"),
                ),
                call(
                    Some("2026-11-24T10:00:00Z"),
                    "c2",
                    "search",
                    Some("mcp__github"),
                ),
                call(None, "c3", "mcp__linear__list", None),
            ],
        );
        let days = parse_codex(&path);
        assert_eq!(days.len(), 2);
        let recent = in_window(&days, parse_iso_ms("2026-11-24T00:00:00Z"));
        assert_eq!(
            recent.mcp_calls,
            BTreeMap::from([("github".into(), 1), ("linear".into(), 1)])
        );
        let all = in_window(&days, parse_iso_ms("2026-11-01T00:00:00Z"));
        assert_eq!(all.mcp_calls["github"], 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn detected(id: &str, name: &str, servers: &[&str]) -> crate::clients::DetectedClient {
        crate::clients::DetectedClient {
            id: id.into(),
            name: name.into(),
            uses_connectors: false,
            discovery: crate::clients::discovery_capabilities(id),
            config_path: format!("/fixture/{id}.json"),
            config_exists: true,
            app_present: true,
            servers: servers
                .iter()
                .map(|s| crate::clients::McpServer {
                    name: (*s).into(),
                    transport: "stdio".into(),
                    command: Some(format!("/fixture/{s}")),
                    args: Vec::new(),
                    env_keys: Vec::new(),
                    url: None,
                })
                .collect(),
            plugin_servers: Vec::new(),
            gateway_installed: false,
            entry_state: crate::clients::GatewayEntryState::Absent,
            error: None,
            claude_profile: None,
            t3_code: Vec::new(),
            location_removable: false,
            location_kept_by_t3_code: false,
        }
    }

    #[test]
    fn client_filter_keeps_other_clients_servers_out_of_unused() {
        let dir = scratch("client-filter-unused");
        // One Claude Code session without MCP calls.
        write_lines(
            &dir.join("claude/projects/-work/s.jsonl"),
            &[serde_json::json!({
                "type": "assistant",
                "message": {
                    "id": "m1",
                    "model": "claude-opus-5-5",
                    "usage": {"input_tokens": 10, "cache_creation_input_tokens": 30_000, "output_tokens": 5},
                    "content": []
                }
            })],
        );
        let clients = [
            detected("claude-code", "Claude Code", &["alpha"]),
            detected("codex", "Codex", &["beta"]),
        ];
        let only_claude: BTreeSet<Client> = [Client::Claude].into();
        let mut configured = Configured::new();
        add_client_servers(&mut configured, clients.iter(), &only_claude);
        assert_eq!(configured.keys().collect::<Vec<_>>(), ["alpha"]);

        let sources = Sources {
            claude: vec![dir.join("claude")],
            ..Default::default()
        };
        let report = analyze(&sources, &options(only_claude, None), &configured);
        assert_eq!(report.totals.sessions, 1);
        let unused: Vec<&str> = report
            .unused_servers
            .iter()
            .map(|u| u.name.as_str())
            .collect();
        assert_eq!(unused, ["alpha"]);

        let mut every = Configured::new();
        let all: BTreeSet<Client> = [Client::Claude, Client::Codex, Client::Opencode].into();
        add_client_servers(&mut every, clients.iter(), &all);
        assert_eq!(every.keys().collect::<Vec<_>>(), ["alpha", "beta"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn opencode_cache_rereads_a_message_edited_in_place() {
        let dir = scratch("opencode-in-place");
        let root = dir.join("opencode");
        let session = root.join("storage/message/ses_1");
        std::fs::create_dir_all(&session).unwrap();
        let file = session.join("msg_1.json");
        let now = crate::cli::now_ms();
        let write = |output: u64, age_ms: u64| {
            let message = serde_json::json!({
                "role": "assistant",
                "modelID": "m",
                "time": {"created": now - 3_600_000},
                "tokens": {"input": 100, "output": output, "cache": {"read": 0, "write": 0}}
            });
            std::fs::write(&file, message.to_string()).unwrap();
            let at = std::time::UNIX_EPOCH + std::time::Duration::from_millis(now - age_ms);
            std::fs::File::options()
                .write(true)
                .open(&file)
                .unwrap()
                .set_modified(at)
                .unwrap();
        };
        write(5, 3_600_000);
        let sources = Sources {
            opencode: vec![root],
            ..Default::default()
        };
        let opts = options([Client::Opencode].into(), Some(dir.join("cache.json")));
        let first = analyze(&sources, &opts, &Configured::new());
        assert_eq!((first.stats.parsed, first.stats.cached), (1, 0));
        assert_eq!(first.totals.output, 5);
        let again = analyze(&sources, &opts, &Configured::new());
        assert_eq!((again.stats.parsed, again.stats.cached), (0, 1));

        // Rewritten in place with the same byte length: only the message timestamp changes.
        write(7, 1_800_000);
        let edited = analyze(&sources, &opts, &Configured::new());
        assert_eq!((edited.stats.parsed, edited.stats.cached), (1, 0));
        assert_eq!(edited.totals.output, 7);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
