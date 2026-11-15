//! Result-shaping: keep oversized tool results from blowing the model's context
//! within a byte budget. Retained bodies can be paged with `toolport_fetch_result`
//! until they expire. Memory holds the recent ones; an entry too large for memory,
//! or pushed out by newer ones, moves to an encrypted file in the data dir and stays
//! fetchable there for the rest of its time. Each read extends that time, up to a
//! fixed lifetime. Only a result too large for the disk cap too, or one that could
//! not be stored, gets no cursor, and the truncated reply then says which.
//!
//! This is the "other half" of the token story: lazy discovery trims tool
//! DEFINITION bloat; this trims tool RESULT bloat (a 10k-row response that would
//! otherwise sit in context). The gateway is the one place that can impose it
//! across every server, including legacy APIs with no native pagination.

use crate::session_store::SessionStore;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Results whose serialized size exceeds this get shaped. Generous on purpose, so
/// only genuinely large results are touched. Override with `TOOLPORT_RESULT_BUDGET`
/// (legacy: `CONDUIT_RESULT_BUDGET`)
/// (bytes); set it to 0 to disable shaping entirely.
pub const DEFAULT_BUDGET_BYTES: usize = 48 * 1024;

/// How long a cached full result stays fetchable after it was stored or last read.
/// Each fetch starts the clock again, so a model paging a large result keeps it.
const CACHE_TTL: Duration = Duration::from_secs(15 * 60);

/// The longest a result stays fetchable from when it was first stored, however often
/// it is read.
const MAX_RESULT_LIFETIME: Duration = Duration::from_secs(2 * 60 * 60);

/// Cap on the number of cached shaped results. A burst of large tool calls would
/// otherwise grow process memory without bound between TTL sweeps. Oldest
/// entries (by insertion time) are evicted first.
const MAX_CACHE_ENTRIES: usize = 64;

/// Cap on estimated retained bytes, including parsed JSON allocations. Evict
/// oldest until a new body fits. Over-cap entries fall back to text alone; if
/// that still exceeds the cap, no cursor is offered.
const MAX_CACHE_BYTES: usize = 64 * 1024 * 1024;

/// How far memory may grow past the caps above when older results cannot move to
/// disk. They stay in memory rather than lose a cursor already handed out; past these
/// a new result gets no cursor instead.
const HARD_MAX_CACHE_ENTRIES: usize = 2 * MAX_CACHE_ENTRIES;
const HARD_MAX_CACHE_BYTES: usize = 2 * MAX_CACHE_BYTES;

pub fn resolve_budget(value: Option<&str>) -> (usize, Option<String>) {
    match value {
        Some(v) => match v.trim().parse::<usize>() {
            Ok(value) => (value, None),
            Err(_) => (
                DEFAULT_BUDGET_BYTES,
                Some(format!(
                    "toolport: invalid TOOLPORT_RESULT_BUDGET/CONDUIT_RESULT_BUDGET value '{v}', falling back to default budget"
                )),
            ),
        },
        None => (DEFAULT_BUDGET_BYTES, None),
    }
}

/// Resolve the byte budget from the env override, falling back to the default.
/// 0 disables shaping (every result is treated as under budget).
pub fn budget() -> (usize, Option<String>) {
    let value = crate::brand::env_var("TOOLPORT_RESULT_BUDGET", "CONDUIT_RESULT_BUDGET");

    resolve_budget(value.as_deref())
}

struct Cached {
    server: Option<String>,
    body: String,
    structured: Option<Value>,
    projections_unavailable: bool,
    /// Estimated retained allocations, computed once at insertion.
    size: usize,
    /// The client the result belongs to (a registered HTTP client's label), or None
    /// for the single-tenant stdio process. Only this client may fetch it back.
    owner: Option<String>,
    /// For a discovery result (a tool definition from search), the exposed tool
    /// names it describes and the gateway host it came from. Fetching it rechecks
    /// that each tool is still visible.
    describes: Vec<String>,
    realm: usize,
    /// When the result was first stored; [`MAX_RESULT_LIFETIME`] runs from here.
    first_stored: Instant,
}

impl Cached {
    fn past_lifetime(&self) -> bool {
        self.past_lifetime_at(Instant::now())
    }

    fn past_lifetime_at(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.first_stored) >= max_result_lifetime()
    }
}

#[cfg(test)]
thread_local! {
    static LIFETIMES_FOR_TESTS: std::cell::Cell<Option<(Duration, Duration)>> =
        const { std::cell::Cell::new(None) };
}

/// Shorten the spill TTL and the lifetime cap on this test thread, so their clocks
/// run in milliseconds.
#[cfg(test)]
fn set_lifetimes_for_tests(lifetimes: Option<(Duration, Duration)>) {
    LIFETIMES_FOR_TESTS.with(|cell| cell.set(lifetimes));
}

/// [`CACHE_TTL`], for spilled results.
fn spill_ttl() -> Duration {
    #[cfg(test)]
    if let Some((ttl, _)) = LIFETIMES_FOR_TESTS.with(std::cell::Cell::get) {
        return ttl;
    }
    CACHE_TTL
}

fn max_result_lifetime() -> Duration {
    #[cfg(test)]
    if let Some((_, max)) = LIFETIMES_FOR_TESTS.with(std::cell::Cell::get) {
        return max;
    }
    MAX_RESULT_LIFETIME
}

/// The bounded stash of shaped results, keyed by cursor. P1.2: the lifetime rules
/// live on a [`SessionStore`] rather than a bare `HashMap` -- entries expire
/// `CACHE_TTL` after they were stashed or last read (and never later than
/// `MAX_RESULT_LIFETIME` after they were stashed), and an arrival past `MAX_CACHE_ENTRIES` or
/// `MAX_CACHE_BYTES` evicts the oldest.
fn cache() -> &'static Mutex<SessionStore<Cached>> {
    static C: OnceLock<Mutex<SessionStore<Cached>>> = OnceLock::new();
    // The store's own cap is the hard one: `evict_to_fit` keeps to the soft caps, and
    // the store must never drop an entry that could not move to disk.
    C.get_or_init(|| Mutex::new(SessionStore::new(CACHE_TTL, HARD_MAX_CACHE_ENTRIES)))
}

fn next_cursor() -> String {
    static N: AtomicU64 = AtomicU64::new(1);
    format!("r{}", N.fetch_add(1, Ordering::Relaxed))
}

static TRIM_PENDING: AtomicBool = AtomicBool::new(false);

fn sweep(store: &mut SessionStore<Cached>) {
    sweep_memory(store);
    spill::reap_expired();
}

fn sweep_memory(store: &mut SessionStore<Cached>) {
    sweep_memory_at(store, Instant::now());
}

/// [`sweep_memory`] as of `now`.
fn sweep_memory_at(store: &mut SessionStore<Cached>, now: Instant) {
    let before = store.weight(|c| c.size);
    let removed = store.reap_expired_at(now) + store.remove_where(|_, c| c.past_lifetime_at(now));
    if removed > 0 && before >= 1024 * 1024 {
        TRIM_PENDING.store(true, Ordering::Relaxed);
    }
}

/// Remove sealed result spills left by gateways that have exited. Called once at
/// gateway start-up, off the request path.
pub fn remove_stale_spills() {
    spill::remove_stale_at_startup();
}

/// Remove this process's spilled results. Every orderly exit runs this through
/// [`crate::telemetry::shutdown`].
pub fn release_spills() {
    spill::release();
}

/// Called by every gateway runtime's background maintenance tick. Trim outside the
/// cache lock and off the request path.
pub fn maintain_cache(idle_transition: bool) {
    {
        let mut store = cache().lock().unwrap_or_else(|e| e.into_inner());
        sweep(&mut store);
    }
    if TRIM_PENDING.swap(false, Ordering::Relaxed) || idle_transition {
        #[cfg(all(target_os = "linux", target_env = "gnu"))]
        // SAFETY: glibc's allocator synchronizes trimming with other allocations.
        unsafe {
            libc::malloc_trim(0);
        }
    }
}

// glibc chunks need alignment and a header; other allocators are conservatively
// charged the same overhead. Capacity matters even when the buffer is short.
fn allocation_size(capacity: usize) -> usize {
    if capacity == 0 {
        0
    } else {
        capacity.saturating_add(15) / 16 * 16 + 16
    }
}

fn value_heap_size(value: &Value) -> usize {
    match value {
        Value::String(s) => allocation_size(s.capacity()),
        Value::Array(a) => {
            allocation_size(a.capacity() * std::mem::size_of::<Value>())
                + a.iter().map(value_heap_size).sum::<usize>()
        }
        Value::Object(o) => {
            // std's B-tree nodes reserve eleven slots, even for a tiny object.
            // Non-root nodes have at least five keys. Charge the upper node count
            // and internal-node edges, not just the occupied key/value slots.
            let nodes = if o.len() < 11 {
                usize::from(!o.is_empty())
            } else {
                (o.len() - 1) / 5 + 1
            };
            let node_bytes = 11 * (std::mem::size_of::<String>() + std::mem::size_of::<Value>())
                + 16
                + 12 * std::mem::size_of::<usize>();
            nodes * allocation_size(node_bytes)
                + o.iter()
                    .map(|(key, value)| allocation_size(key.capacity()) + value_heap_size(value))
                    .sum::<usize>()
        }
        _ => 0,
    }
}

fn retained_size(body: &String, structured: Option<&Value>) -> usize {
    std::mem::size_of::<Cached>()
        + allocation_size(body.capacity())
        + structured.map(value_heap_size).unwrap_or(0)
}

/// Bound memory: move the oldest results to disk until the entry count and total bytes
/// leave room for a `new_entry_size`-byte result. Each entry's `size` is precomputed, so
/// this sum is O(n) adds, not O(n) JSON re-serializations, on every iteration. A result
/// that cannot be written stays in memory, so a cursor already issued is never lost,
/// and the caps are then soft up to the hard ones. Returns whether the new result fits
/// under the hard caps.
fn evict_to_fit(store: &mut SessionStore<Cached>, new_entry_size: usize) -> bool {
    let mut unspilled = Vec::new();
    let mut unspilled_bytes = 0;
    while !store.is_empty()
        && (store.len() + unspilled.len() >= MAX_CACHE_ENTRIES
            || store.weight(|c| c.size) + unspilled_bytes + new_entry_size > MAX_CACHE_BYTES)
    {
        // An entry pushed out of memory moves to disk for the rest of its time.
        let Some(oldest) = store.pop_oldest() else {
            break;
        };
        if !oldest.value.describes.is_empty() {
            // A discovery result is dropped, not kept: search makes a fresh one.
            TRIM_PENDING.store(true, Ordering::Relaxed);
            continue;
        }
        match spill::keep(&oldest.key, &oldest.value, oldest.last_seen) {
            // An expired entry is simply gone; nobody can fetch it any more.
            Ok(()) | Err(Refusal::Expired) => TRIM_PENDING.store(true, Ordering::Relaxed),
            Err(_) => {
                unspilled_bytes += oldest.value.size;
                unspilled.push(oldest);
            }
        }
    }
    if !unspilled.is_empty() {
        spill::report_kept_in_memory(unspilled.len());
        for entry in unspilled {
            store.put_back(entry);
        }
    }
    store.len() < HARD_MAX_CACHE_ENTRIES
        && store.weight(|c| c.size) + new_entry_size <= HARD_MAX_CACHE_BYTES
}

/// Why a result got no cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refusal {
    /// Larger than the disk cap, so it cannot be kept anywhere.
    TooLarge,
    /// It would fit, but storing it failed: no data dir, no sealing key, a failed
    /// write, or memory at its hard cap with results that could not move to disk.
    Unstored,
    /// Its time was already up.
    Expired,
}

fn retain_in(store: &mut SessionStore<Cached>, cursor: &str, entry: Cached) -> Result<(), Refusal> {
    if entry.size > MAX_CACHE_BYTES {
        // Too large to hold in memory: keep it, structured part included, on disk.
        return spill::keep(cursor, &entry, Instant::now());
    }
    sweep(store);
    if !evict_to_fit(store, entry.size) {
        // Memory is at its hard cap with results that could not move to disk. This
        // one can still go to disk if the failure has passed.
        return spill::keep(cursor, &entry, Instant::now());
    }
    store.insert(cursor, entry);
    Ok(())
}

fn retain(cursor: &str, entry: Cached) -> Result<(), Refusal> {
    let size = entry.size;
    let mut store = cache().lock().unwrap_or_else(|e| e.into_inner());
    let retained = retain_in(&mut store, cursor, entry);
    if size >= 1024 * 1024 {
        TRIM_PENDING.store(true, Ordering::Relaxed);
    }
    retained
}

// Process conformance tests seed an already expired entry, then observe a tick
// without fetching or inserting again. These hooks are absent from shipped builds.
#[cfg(feature = "test-support")]
pub fn seed_cache_maintenance_probe() {
    let mut store = cache().lock().unwrap_or_else(|e| e.into_inner());
    *store = SessionStore::new(Duration::ZERO, MAX_CACHE_ENTRIES);
    store.insert(
        "maintenance-probe",
        Cached {
            server: None,
            body: "x".repeat(1024 * 1024),
            structured: None,
            projections_unavailable: false,
            size: 1024 * 1024,
            owner: None,
            describes: Vec::new(),
            realm: 0,
            first_stored: Instant::now(),
        },
    );
}

#[cfg(feature = "test-support")]
pub fn cache_maintenance_probe_complete() -> bool {
    cache().lock().unwrap_or_else(|e| e.into_inner()).is_empty()
        && !TRIM_PENDING.load(Ordering::Relaxed)
}

/// Concatenate the model-facing text of an MCP tool result's content blocks, then
/// fold in `structuredContent` so nothing is lost when the structured payload is
/// the bloat.
fn extract_body(result: &Value) -> (String, usize) {
    let mut out = String::new();
    if let Some(blocks) = result.get("content").and_then(|c| c.as_array()) {
        for b in blocks {
            if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(t);
            }
        }
    }
    // The body's share of the serialized result is its JSON-encoded size, not its raw
    // size: escaping (backslashes, quotes, control characters) belongs to the body.
    let mut source_bytes = result
        .get("content")
        .and_then(|c| c.as_array())
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .map(json_string_len)
                .sum()
        })
        .unwrap_or(0);
    if let Some(sc) = result.get("structuredContent") {
        let structured_text = serde_json::to_string(sc).unwrap_or_default();
        source_bytes += structured_text.len() + usize::from(!out.is_empty());
        // Some servers return the same JSON as text and structuredContent. Keep
        // the original text bytes and the separately cached typed projection,
        // without making every page repeat that JSON a second time. Only a
        // single text block can qualify; mixed/multiple blocks remain lossless.
        let single_text = result
            .get("content")
            .and_then(Value::as_array)
            .is_some_and(|blocks| blocks.len() == 1 && blocks[0]["type"] == "text");
        let duplicate = single_text
            && (out == structured_text
                || serde_json::from_str::<Value>(&out)
                    .ok()
                    .is_some_and(|text| text == *sc));
        if !duplicate {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&structured_text);
        }
    }
    (out, source_bytes)
}

/// Bytes `s` takes as a JSON string, quotes included, as serde_json writes it.
fn json_string_len(s: &str) -> usize {
    2 + s.chars().map(json_char_len).sum::<usize>()
}

fn json_char_len(ch: char) -> usize {
    match ch {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
        ch if (ch as u32) < 0x20 => 6,
        ch => ch.len_utf8(),
    }
}

/// The longest char-boundary prefix of `s` whose JSON-encoded length (without
/// quotes) is at most `max_bytes`. Heavily escaped text takes more room in the result
/// than its raw bytes, so the head is measured the way the result is.
fn head_within_encoded_bytes(s: &str, max_bytes: usize) -> &str {
    let mut used = 0;
    for (i, ch) in s.char_indices() {
        used += json_char_len(ch);
        if used > max_bytes {
            return &s[..i];
        }
    }
    s
}

/// A short preview of a caller-supplied value for an error message, so an error never
/// echoes an argument of any size back into the model's context.
fn preview(value: &str) -> String {
    const LIMIT: usize = 48;
    let mut chars = value.chars();
    let head: String = chars.by_ref().take(LIMIT).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

fn value_size(value: &Value) -> usize {
    serde_json::to_string(value).map(|s| s.len()).unwrap_or(0)
}

fn text_result(text: String, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

fn project<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = value;

    for segment in path.split('.') {
        if let Some(object) = current.as_object() {
            current = object.get(segment)?;
        } else {
            let array = current.as_array()?;
            let index = segment.parse::<usize>().ok()?;
            current = array.get(index)?;
        }
    }

    Some(current)
}

/// True if every content block is text, so the text projection in [`extract_body`]
/// represents the result losslessly. A block with no `text` field is non-text
/// (image, audio, resource, resource_link); shaping would silently drop it, so such
/// results are left whole.
fn is_text_representable(result: &Value) -> bool {
    match result.get("content").and_then(|c| c.as_array()) {
        Some(blocks) => blocks
            .iter()
            .all(|b| b.get("text").and_then(|t| t.as_str()).is_some()),
        None => true,
    }
}

/// If `result` serializes to more than `budget` bytes, attempt to retain its body,
/// replace it with a truncated head and retention notice, and return `true` (shaped).
/// A `budget` of 0 disables shaping. The full body stays fetchable via
/// [`fetch_result`] until it expires (see [`CACHE_TTL`] and [`MAX_RESULT_LIFETIME`]).
pub fn shape_result(result: &mut Value, budget: usize, owner: Option<&str>) -> bool {
    shape_result_preserving_prefix(result, budget, owner, 0)
}

/// Shape a result while retaining at least `min_head_bytes` from the start of
/// its text projection. If the protected prefix and marker cannot fit, leave the
/// result whole instead of silently truncating recovery-critical data.
pub fn shape_result_preserving_prefix(
    result: &mut Value,
    budget: usize,
    owner: Option<&str>,
    min_head_bytes: usize,
) -> bool {
    shape_inner(result, budget, owner, min_head_bytes, Vec::new(), 0)
}

/// [`shape_result`] for a discovery result: a definition of the exposed tools in
/// `describes`, from the gateway host identified by `realm` (any stable nonzero
/// key). [`fetch_result_visible`] serves its pages only while every one of them is
/// still visible to the caller, and [`forget_discovery`] drops it when policy or
/// scope changes.
pub fn shape_discovery_result(
    result: &mut Value,
    budget: usize,
    owner: Option<&str>,
    describes: Vec<String>,
    realm: usize,
) -> bool {
    shape_inner(result, budget, owner, 0, describes, realm)
}

/// Drop the discovery results of host `realm` that describe a tool `stale` names,
/// so a definition the caller can no longer see does not stay in memory until the
/// TTL. Search makes a fresh one on demand. Returns how many were dropped.
///
/// `stale` runs with the cache unlocked: callers may hold other locks (a registry
/// publish holds the registry), and a predicate taking a lock under this one could
/// invert the order with [`fetch_result_visible`].
pub fn forget_discovery(realm: usize, stale: impl Fn(&str) -> bool) -> usize {
    let mut candidates: Vec<(String, Vec<String>)> = Vec::new();
    // A read-only pass: the closure records each candidate and removes nothing.
    cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove_where(|cursor, cached| {
            if cached.realm == realm && !cached.describes.is_empty() {
                candidates.push((cursor.to_string(), cached.describes.clone()));
            }
            false
        });
    let stale: Vec<(String, Vec<String>)> = candidates
        .into_iter()
        .filter(|(_, describes)| describes.iter().any(|tool| stale(tool)))
        .collect();
    if stale.is_empty() {
        return 0;
    }
    let mut store = cache().lock().unwrap_or_else(|e| e.into_inner());
    stale
        .into_iter()
        .filter(|(cursor, describes)| {
            // Only the entry that was judged: cursors are never reused, but check.
            store
                .peek(cursor, |cached| &cached.describes == describes)
                .unwrap_or(false)
                && store.remove(cursor).is_some()
        })
        .count()
}

fn shape_inner(
    result: &mut Value,
    budget: usize,
    owner: Option<&str>,
    min_head_bytes: usize,
    describes: Vec<String>,
    realm: usize,
) -> bool {
    if budget == 0 {
        return false;
    }
    let server = result
        .pointer("/_meta/app.toolport~1provenance/server")
        .and_then(Value::as_str)
        .map(str::to_string);
    let size = serde_json::to_string(result).map(|s| s.len()).unwrap_or(0);
    if size <= budget {
        return false;
    }

    let (body, _) = extract_body(result);
    if body.is_empty() {
        // Nothing textual to cut: a result of only non-text blocks passes whole.
        return false;
    }
    let structured = result.get("structuredContent").cloned();
    let total = body.chars().count();
    let is_error = result
        .get("isError")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let slots = layout(result, &body);

    let cursor = next_cursor();
    let cached_body = body.clone();
    let new_entry_size = retained_size(&cached_body, structured.as_ref());
    let body_bytes = body.len();
    let retained = retain(
        &cursor,
        Cached {
            server: server.clone(),
            body: cached_body,
            structured,
            projections_unavailable: false,
            size: new_entry_size,
            owner: owner.map(str::to_string),
            describes,
            realm,
            first_stored: Instant::now(),
        },
    );
    let marker = |head_chars: usize| -> String {
        if let Err(refusal) = retained {
            not_retained_notice(body_bytes, refusal)
        } else {
            format!(
                "\n\n[Toolport shaped this result: ~{} KB exceeds {} KB budget. Showing first {} of {} \
                 characters. Continue with toolport_fetch_result {{\"cursor\":\"{}\",\"offset\":{}}}. \
                 If the cursor expired, re-run the original tool call.]",
                size / 1024,
                budget / 1024,
                head_chars,
                total,
                cursor,
                head_chars
            )
        }
    };
    let decline = || {
        cache()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&cursor);
        spill::forget(&cursor);
        false
    };

    // Shaping rewrites the text and stashes `structuredContent`. Everything else is
    // carried across as the server sent it: non-text blocks in their places, `_meta`
    // and any other top-level field. `shown` is the body byte where the visible text
    // stops; text slots that start at or after it are left out (their text is in the
    // cache), and the marker follows the last shown text.
    let build = |shown: usize, marker: &str| -> Value {
        let last_text = slots
            .iter()
            .rposition(
                |slot| matches!(slot, Slot::Text { start, .. } if *start < shown || *start == 0),
            )
            .unwrap_or(0);
        let mut content = Vec::new();
        for (index, slot) in slots.iter().enumerate() {
            match slot {
                Slot::Keep(block) => content.push(block.clone()),
                Slot::Text { start, end, extra } => {
                    if index > last_text {
                        continue;
                    }
                    let mut text = body[*start..(*end).min(shown).max(*start)].to_string();
                    if index == last_text {
                        text.push_str(marker);
                    }
                    let mut block = extra.clone();
                    block.insert("type".into(), json!("text"));
                    block.insert("text".into(), Value::String(text));
                    content.push(Value::Object(block));
                }
            }
        }
        let mut shaped = json!({ "content": content, "isError": is_error });
        if let (Some(src), Some(dst)) = (result.as_object(), shaped.as_object_mut()) {
            for (key, value) in src {
                if matches!(key.as_str(), "content" | "structuredContent" | "isError") {
                    continue;
                }
                dst.insert(key.clone(), value.clone());
            }
        }
        if let Some(server) = &server {
            crate::integrity::label_untrusted_result(server, &mut shaped);
        }
        shaped
    };

    // Fit by calculation, not by trial. A probe with every text slot present but
    // emptied measures the envelope, the non-text blocks and each text block's shell.
    // Showing text slots 0..=k (the last one cut) then costs the probe, minus the
    // shells of the slots after k, plus the encoded text shown, plus the marker. The
    // marker is measured at its widest (head = total), so the real one never costs
    // more.
    let widest = marker(total);
    let marker_cost = json_string_len(&widest) - 2;
    // usize::MAX shows every slot, empty ones included, so blocks line up with slots.
    let mut emptied = build(usize::MAX, "");
    if let Some(blocks) = emptied["content"].as_array_mut() {
        for (block, slot) in blocks.iter_mut().zip(slots.iter()) {
            if matches!(slot, Slot::Text { .. }) {
                block["text"] = json!("");
            }
        }
    }
    let empty_size = value_size(&emptied);
    // Bytes each text slot's text adds, and each slot's empty shell plus its comma.
    let texts: Vec<(usize, usize, usize, usize)> = slots
        .iter()
        .zip(emptied["content"].as_array().into_iter().flatten())
        .filter_map(|(slot, block)| match slot {
            Slot::Text { start, end, .. } => Some((
                *start,
                *end,
                json_string_len(&body[*start..*end]) - 2,
                value_size(block) + 1,
            )),
            Slot::Keep(_) => None,
        })
        .collect();
    let mut later_shells: usize = texts.iter().map(|text| text.3).sum();
    let mut earlier_text = 0;
    let mut shown = 0;
    for (index, (start, end, cost, shell)) in texts.iter().enumerate() {
        later_shells -= shell;
        let fixed = empty_size - later_shells + earlier_text + marker_cost;
        let Some(room) = budget.checked_sub(fixed) else {
            // Even with this slot empty the envelope, kept blocks and marker do not
            // fit. Showing less text cannot help, so leave the result whole.
            if index == 0 {
                return decline();
            }
            break;
        };
        if *cost <= room && index + 1 < texts.len() {
            earlier_text += cost;
            shown = *end;
            continue;
        }
        shown = start + head_within_encoded_bytes(&body[*start..*end], room).len();
        break;
    }
    // The protected prefix must be visible, or the result is left whole.
    if shown < min_head_bytes.min(body.len()) {
        return decline();
    }
    let head_chars = body[..shown].chars().count();
    let shaped = build(shown, &marker(head_chars));
    // The calculation is exact; this guard only keeps `true` meaning "fits" if a
    // future change to the layout breaks it.
    if value_size(&shaped) > budget {
        debug_assert!(false, "shaping calculation overshot the budget");
        return decline();
    }

    *result = shaped;
    if size >= 1024 * 1024 {
        TRIM_PENDING.store(true, Ordering::Relaxed);
    }
    true
}

/// One place in a shaped result's content: a non-text block kept as sent, or a text
/// block showing `body[start..end]` (with its own extra fields, such as annotations).
enum Slot {
    Keep(Value),
    Text {
        start: usize,
        end: usize,
        extra: serde_json::Map<String, Value>,
    },
}

/// Where each block's text sits in [`extract_body`]'s body. An all-text result
/// becomes one text slot over the whole body, as it always has. A result with non-text
/// blocks keeps every block in its place, with one text slot per text block, and
/// typed content not repeated in the text as a final text slot.
fn layout(result: &Value, body: &str) -> Vec<Slot> {
    if is_text_representable(result) {
        return vec![Slot::Text {
            start: 0,
            end: body.len(),
            extra: serde_json::Map::new(),
        }];
    }
    let mut slots = Vec::new();
    let mut at = 0;
    for block in result["content"].as_array().into_iter().flatten() {
        match block.get("text").and_then(Value::as_str) {
            Some(text) => {
                // extract_body puts a newline before a text block once it has text.
                if at > 0 {
                    at += 1;
                }
                let mut extra = block.as_object().cloned().unwrap_or_default();
                extra.remove("type");
                extra.remove("text");
                slots.push(Slot::Text {
                    start: at,
                    end: at + text.len(),
                    extra,
                });
                at += text.len();
            }
            None => slots.push(Slot::Keep(block.clone())),
        }
    }
    if at < body.len() {
        let start = if at > 0 { at + 1 } else { at };
        slots.push(Slot::Text {
            start,
            end: body.len(),
            extra: serde_json::Map::new(),
        });
    }
    slots
}

fn not_retained_notice(body_bytes: usize, refusal: Refusal) -> String {
    let mb = body_bytes as f64 / 1_000_000.0;
    match refusal {
        Refusal::TooLarge => format!(
            "\n\n[Toolport: full result ({mb:.1} MB) was too large to keep for paging, even \
             on disk. Re-run the tool with narrower arguments.]"
        ),
        Refusal::Unstored | Refusal::Expired => format!(
            "\n\n[Toolport: full result ({mb:.1} MB) could not be stored for paging (a disk \
             or key failure). Re-run the tool, or narrow the call.]"
        ),
    }
}

/// Stash a Toolport-authored payload and return an admitted cursor readable through
/// `toolport_fetch_result`, with the same owner scoping, TTL, and bounds as shaped
/// results. This lets a small marker in a tool result point at bulkier material
/// without inflating the result it rides on. `None` means the caller must report
/// that paging is unavailable instead of advertising a fetch marker.
pub fn stash_payload(
    body: String,
    structured: Option<Value>,
    owner: Option<&str>,
) -> Option<String> {
    let cursor = next_cursor();
    let size = retained_size(&body, structured.as_ref());
    let retained = retain(
        &cursor,
        Cached {
            server: None,
            body,
            structured,
            projections_unavailable: false,
            size,
            owner: owner.map(str::to_string),
            describes: Vec::new(),
            realm: 0,
            first_stored: Instant::now(),
        },
    );
    retained.is_ok().then_some(cursor)
}

/// Return the next slice of a cached shaped result, by cursor + character offset.
/// `len` of 0 means "use the current budget" (the whole rest when it is 0). This is
/// the code-mode read: the slice stays in the sandbox, so it is not byte-bounded.
pub fn fetch_result(
    cursor: &str,
    offset: usize,
    len: usize,
    requester: Option<&str>,
    projection: Option<&str>,
) -> Value {
    fetch_result_bounded(cursor, offset, len, requester, projection, None, &|_| true)
}

/// [`fetch_result`], refusing a discovery result whose tool `visible` no longer
/// admits (quarantined, disabled, or outside the caller's scope since it was
/// stashed). The refused entry is dropped. Results of tool calls carry no tool
/// list and page as before: `visible` is never asked about them, and they stay
/// readable to their caller after a policy change, because the caller already
/// received that output.
///
/// `visible` runs with the cache unlocked, so it may take other locks (a script's
/// live check reads the registry, which a registry publish holds while it calls
/// [`forget_discovery`]). The entry is read again afterwards; one that went away
/// or changed in between gets the "unknown or expired" answer.
pub fn fetch_result_visible(
    cursor: &str,
    offset: usize,
    len: usize,
    requester: Option<&str>,
    projection: Option<&str>,
    visible: &dyn Fn(&str) -> bool,
) -> Value {
    fetch_result_bounded(cursor, offset, len, requester, projection, None, visible)
}

/// The model-facing `toolport_fetch_result`. Every reply's text, footer included, fits
/// `budget` UTF-8 bytes, so it also fits `budget` characters as Claude Code counts them
/// (UTF-16 units never outnumber UTF-8 bytes) and the `anthropic/maxResultSizeChars`
/// hint Toolport derives from the budget holds. A page stops at `len` characters or the
/// byte bound, whichever comes first. A projection larger than the budget is shaped like
/// any other result: a head plus a cursor for paging the projected JSON. `budget` 0
/// never shapes, so pages run to the end and projections return whole.
pub fn fetch_result_for_model(
    cursor: &str,
    offset: usize,
    len: usize,
    requester: Option<&str>,
    projection: Option<&str>,
    budget: usize,
) -> Value {
    fetch_result_for_model_visible(cursor, offset, len, requester, projection, budget, &|_| {
        true
    })
}

/// [`fetch_result_for_model`], rechecking a discovery result with `visible` as
/// [`fetch_result_visible`] does.
pub fn fetch_result_for_model_visible(
    cursor: &str,
    offset: usize,
    len: usize,
    requester: Option<&str>,
    projection: Option<&str>,
    budget: usize,
    visible: &dyn Fn(&str) -> bool,
) -> Value {
    let mut result = fetch_result_bounded(
        cursor,
        offset,
        len,
        requester,
        projection,
        Some(budget),
        visible,
    );
    if let Some(path) = projection.filter(|_| result["isError"] != true) {
        // A projection cut to the budget is a fragment of its JSON. Say so, inside the
        // budget, so a model never parses the head as the whole value.
        let notice = format!(
            "\n[Toolport: the text above is the start of projection \"{}\", not complete \
             JSON. Page the rest with the cursor above.]",
            preview(path)
        );
        let reserve = json_string_len(&notice);
        if budget > reserve
            && shape_result(&mut result, budget - reserve, requester)
            && result["content"][0]["text"].is_string()
        {
            let text = result["content"][0]["text"]
                .as_str()
                .unwrap_or("")
                .to_string();
            result["content"][0]["text"] = Value::String(text + &notice);
        }
    }
    result
}

/// The same answer for a missing, expired or foreign cursor, so a scoped client
/// cannot probe which cursors exist.
fn unknown_cursor(cursor: &str) -> Value {
    text_result(
        format!(
            "[Toolport: cursor \"{}\" is unknown or expired. Re-run the original tool call \
             to get a fresh result.]",
            preview(cursor)
        ),
        true,
    )
}

enum Read {
    /// In memory for this requester; its TTL starts again.
    Found,
    /// In memory but not readable: another owner's, or past its lifetime.
    Refused,
    /// Not in memory; it may be on disk.
    Absent,
}

/// Look `cursor` up in memory for `requester`. A read extends the entry's TTL, up to
/// [`MAX_RESULT_LIFETIME`] from when it was first stored.
fn read_in(store: &mut SessionStore<Cached>, cursor: &str, requester: Option<&str>) -> Read {
    read_in_at(store, cursor, requester, Instant::now())
}

/// [`read_in`] as of `now`.
fn read_in_at(
    store: &mut SessionStore<Cached>,
    cursor: &str,
    requester: Option<&str>,
    now: Instant,
) -> Read {
    match store.get_at(cursor, now) {
        Some(c) if c.owner.as_deref() == requester && !c.past_lifetime_at(now) => {}
        Some(_) => return Read::Refused,
        None => return Read::Absent,
    }
    store.with_at(cursor, now, |_| ());
    Read::Found
}

fn fetch_result_bounded(
    cursor: &str,
    offset: usize,
    len: usize,
    requester: Option<&str>,
    projection: Option<&str>,
    model_budget: Option<usize>,
    visible: &dyn Fn(&str) -> bool,
) -> Value {
    // Scope: a cached result is readable only by the client that stashed it. Owner
    // must be a stable principal (e.g. client:{id}), never a shared display label
    // (SOU-324). A mismatch returns the SAME "unknown or expired" answer as a
    // missing cursor, so a scoped client can't probe which cursors exist. The
    // stash is process-global; without this check one HTTP client could read
    // another's result by guessing the sequential `r{n}` cursor.
    let owned = |c: &Cached| c.owner.as_deref() == requester;
    let describes = {
        let mut store = cache().lock().unwrap_or_else(|e| e.into_inner());
        sweep(&mut store);
        match store.get(cursor) {
            Some(c) if owned(c) => c.describes.clone(),
            Some(_) => return unknown_cursor(cursor),
            // What went to disk is never a discovery result (see `spill::keep`).
            None => Vec::new(),
        }
    };
    // The visibility check runs unlocked (see above).
    if !describes.iter().all(|tool| visible(tool)) {
        let mut store = cache().lock().unwrap_or_else(|e| e.into_inner());
        if store
            .peek(cursor, |c| owned(c) && c.describes == describes)
            .unwrap_or(false)
        {
            store.remove(cursor);
        }
        return text_result(
            format!(
                "[Toolport: cursor \"{}\" describes a tool that is no longer available \
                 to this client. Search again for the current tools.]",
                preview(cursor)
            ),
            true,
        );
    }
    let mut store = cache().lock().unwrap_or_else(|e| e.into_inner());
    let spilled;
    let c = match read_in(&mut store, cursor, requester) {
        Read::Found
            if store
                .peek(cursor, |c| c.describes == describes)
                .unwrap_or(false) =>
        {
            store.get(cursor).expect("read_in found it")
        }
        Read::Found | Read::Refused => return unknown_cursor(cursor),
        Read::Absent if describes.is_empty() => match spill::load(cursor, requester) {
            Some(entry) => {
                spilled = entry;
                &spilled
            }
            None => return unknown_cursor(cursor),
        },
        _ => return unknown_cursor(cursor),
    };
    if let Some(path) = projection {
        let structured = match &c.structured {
            Some(value) => value,
            None => {
                return text_result(
                    if c.projections_unavailable {
                        "[Toolport: projections aren't available for this very large result. Page the text or narrow the call.]"
                    } else {
                        "[Toolport: this cached result has no structuredContent.]"
                    }.to_string(),
                    true,
                );
            }
        };

        let value = match project(structured, path) {
            Some(value) => value,
            None => {
                return text_result(
                    format!("[Toolport: projection \"{}\" not found.]", preview(path)),
                    true,
                );
            }
        };

        let mut result = text_result(serde_json::to_string(value).unwrap_or_default(), false);
        if let Some(server) = &c.server {
            crate::integrity::label_untrusted_result(server, &mut result);
        }
        return result;
    }
    let total = c.body.chars().count();
    if offset >= total {
        return text_result(
            format!(
                "[Toolport: offset {offset} is at or past the end of the result ({total} \
                 characters). Nothing more to read.]"
            ),
            false,
        );
    }
    let len = match (len, model_budget) {
        (0, Some(0)) => usize::MAX,
        (0, Some(budget)) => budget,
        (0, None) => {
            let (budget, warning) = budget();
            if let Some(msg) = warning {
                eprintln!("{msg}");
            }
            if budget == 0 {
                usize::MAX
            } else {
                budget
            }
        }
        (len, _) => len,
    };
    // A model-facing page stops before the whole reply, serialized, passes the budget:
    // its text is charged as it encodes, and the envelope and the widest footer this
    // page could carry come off the top.
    let max_page_bytes = model_budget.filter(|&budget| budget > 0).map(|budget| {
        let widest = format!(
            "\n\n[Toolport: characters {total}..{total} of {total}. This page stopped at the \
             result budget. {total} remain, call toolport_fetch_result with offset={total} \
             for the next slice.]"
        );
        let mut envelope = text_result(widest, false);
        if let Some(server) = &c.server {
            crate::integrity::label_untrusted_result(server, &mut envelope);
        }
        budget.saturating_sub(value_size(&envelope)).max(1)
    });
    // saturating_add: a client-supplied `len` near usize::MAX must not overflow
    // `offset + len`. On debug that panics; on release it wraps to `end < offset`,
    // and the byte-mapping below then slices `body[start_byte..end_byte]` with
    // start > end - a panic that, on the stdio transport (no catch_unwind), takes
    // down the whole gateway. Saturating clamps `end` to `total` instead.
    let mut end = offset.saturating_add(len).min(total);
    // Map the character window [offset, end) to byte offsets in a single pass, so a
    // page read never allocates a Vec<char> of the whole (possibly multi-MB) body.
    // `end == total` leaves end_byte at the body's byte length (the loop never yields
    // char index `total`), so the last page runs cleanly to the end. A byte bound ends
    // the page early, but never before its first character, so paging always advances.
    let mut start_byte = c.body.len();
    let mut end_byte = c.body.len();
    let mut cut_by_budget = false;
    let mut page_bytes = 0;
    for (char_idx, (byte_idx, ch)) in c.body.char_indices().enumerate() {
        if char_idx == offset {
            start_byte = byte_idx;
        }
        if char_idx >= offset {
            page_bytes += json_char_len(ch);
        }
        let over_bytes = char_idx > offset && max_page_bytes.is_some_and(|max| page_bytes > max);
        if char_idx == end || over_bytes {
            cut_by_budget = over_bytes && char_idx < end;
            end = char_idx;
            end_byte = byte_idx;
            break;
        }
    }
    let slice = c.body[start_byte..end_byte].to_string();
    let remaining = total - end;
    let footer = if remaining > 0 && cut_by_budget {
        format!(
            "\n\n[Toolport: characters {offset}..{end} of {total}. This page stopped at the \
             result budget. {remaining} remain, call toolport_fetch_result with offset={end} \
             for the next slice.]"
        )
    } else if remaining > 0 {
        format!(
            "\n\n[Toolport: characters {offset}..{end} of {total}. {remaining} remain, call \
             toolport_fetch_result with offset={end} for the next slice.]"
        )
    } else {
        format!("\n\n[Toolport: end of result ({total} characters).]")
    };
    let mut result = text_result(format!("{slice}{footer}"), false);
    if let Some(server) = &c.server {
        crate::integrity::label_untrusted_result(server, &mut result);
    }
    result
}

/// Results that left memory: too large for it, or pushed out by newer ones. Each is
/// one file under `result-spill/<pid>` in the data dir, written 0600 through
/// `atomic_write` and sealed with XChaCha20-Poly1305 under a key this process holds
/// only in memory. A file is unreadable once its process is gone, and nothing in it
/// is changed, so paging offsets stay exact. Files are removed when their entry
/// expires (the same rules as memory: 15 minutes from storing or the last read, two
/// hours at most), when the disk cap pushes them out, when
/// the gateway exits normally, and at the next start of any gateway for directories
/// whose process has exited. Usage counts what is on disk: a file whose removal
/// failed keeps its bytes counted until a retry removes it.
mod spill {
    use super::{max_result_lifetime, spill_ttl, Cached, Refusal, CACHE_TTL};
    use base64::Engine;
    use chacha20poly1305::aead::{Aead, KeyInit};
    use chacha20poly1305::{XChaCha20Poly1305, XNonce};
    use serde_json::{json, Value};
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};
    #[cfg(test)]
    use std::time::Duration;
    use std::time::Instant;

    /// Disk held by one gateway process's spilled results.
    pub(super) const MAX_SPILL_BYTES: usize = 512 * 1024 * 1024;

    #[cfg(test)]
    thread_local! {
        static CAP_FOR_TESTS: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
    }

    /// Lower the disk cap on this test thread, so the "too large even for disk" path
    /// runs without writing half a gigabyte.
    #[cfg(test)]
    pub(super) fn set_cap_for_tests(cap: Option<usize>) {
        CAP_FOR_TESTS.with(|cell| cell.set(cap));
    }

    #[cfg(test)]
    thread_local! {
        static FAIL_FOR_TESTS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    /// Make every spill on this test thread fail, as a full or broken disk would.
    #[cfg(test)]
    pub(super) fn fail_writes_for_tests(fail: bool) {
        FAIL_FOR_TESTS.with(|cell| cell.set(fail));
    }

    #[cfg(test)]
    thread_local! {
        static FAIL_REMOVE_FOR_TESTS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    /// Make every file removal on this test thread fail, as a locked file would.
    #[cfg(test)]
    fn fail_removals_for_tests(fail: bool) {
        FAIL_REMOVE_FOR_TESTS.with(|cell| cell.set(fail));
    }

    /// Remove a spill file. True once it is gone, including when it already was.
    fn remove_file(path: &std::path::Path) -> bool {
        #[cfg(test)]
        if FAIL_REMOVE_FOR_TESTS.with(std::cell::Cell::get) {
            return false;
        }
        match std::fs::remove_file(path) {
            Ok(()) => true,
            Err(error) => error.kind() == std::io::ErrorKind::NotFound,
        }
    }

    fn max_spill_bytes() -> usize {
        #[cfg(test)]
        if let Some(cap) = CAP_FOR_TESTS.with(std::cell::Cell::get) {
            return cap;
        }
        MAX_SPILL_BYTES
    }
    const NONCE_LEN: usize = 24;

    struct Spilled {
        path: PathBuf,
        server: Option<String>,
        owner: Option<String>,
        projections_unavailable: bool,
        bytes: usize,
        first_stored: Instant,
        /// Stored or last read; the TTL runs from here.
        touched: Instant,
    }

    impl Spilled {
        fn expired_at(&self, now: Instant) -> bool {
            now.saturating_duration_since(self.touched) >= spill_ttl()
                || now.saturating_duration_since(self.first_stored) >= max_result_lifetime()
        }
    }

    struct State {
        dir: Option<PathBuf>,
        /// None when no key could be generated; spilling is then off.
        cipher: Option<XChaCha20Poly1305>,
        entries: HashMap<String, Spilled>,
        /// Files of dropped entries whose removal failed. Their bytes stay counted
        /// against the disk cap until a later try removes them.
        orphans: Vec<(PathBuf, usize)>,
        /// [`LOCK_FILE`] in `dir`, held while this process may have files there.
        lock: Option<std::fs::File>,
    }

    /// Held (flock on Unix, LockFileEx on Windows) by the gateway that owns its
    /// directory. The start-up sweep never removes a directory whose lock is held.
    pub(super) const LOCK_FILE: &str = "owner.lock";

    static STATE: OnceLock<Mutex<State>> = OnceLock::new();

    fn state() -> &'static Mutex<State> {
        STATE.get_or_init(|| {
            let mut key = [0u8; 32];
            let key = getrandom::getrandom(&mut key).ok().map(|()| key);
            let state = State::new(spill_root(), key);
            if state.cipher.is_none() {
                report(KEY_FAILED_NOTE);
            }
            Mutex::new(state)
        })
    }

    /// Shown in `toolport_status` and Activity once a result had to stay in memory.
    pub(super) const KEPT_IN_MEMORY_NOTE: &str = "Some paged results stayed in memory \
        past the result cache cap because they could not be written to disk.";

    /// Results that could not move to disk stayed in memory. Logged once per run; the
    /// status note is kept once.
    pub(super) fn report_kept_in_memory(count: usize) {
        static LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            log(&format!("{KEPT_IN_MEMORY_NOTE} ({count} this time)"));
        }
        crate::daemon::add_status_note(KEPT_IN_MEMORY_NOTE);
    }

    /// Shown in `toolport_status` and Activity when spilling is off for this run.
    pub(super) const KEY_FAILED_NOTE: &str = "Paged results are kept in memory only: the \
        gateway could not generate a key to seal them on disk.";

    /// Record a spill problem in the gateway log and the status notes. Tests keep it
    /// out of the gateway log, which lives in the real data dir.
    fn report(note: &str) {
        log(note);
        crate::daemon::add_status_note(note);
    }

    fn log(message: &str) {
        #[cfg(not(test))]
        crate::gatewaylog::append(&format!("result spill: {message}"));
        #[cfg(test)]
        let _ = message;
    }

    impl State {
        /// The spill store under `root`, sealed with `key`. Stale directories under
        /// `root` are removed first. Without a key nothing is ever written: a fixed
        /// key would leave every file readable by anyone who knows it.
        fn new(root: Option<PathBuf>, key: Option<[u8; 32]>) -> State {
            let dir = root.map(|root| {
                remove_stale(&root);
                root.join(std::process::id().to_string())
            });
            State {
                dir,
                cipher: key.map(|key| XChaCha20Poly1305::new((&key).into())),
                entries: HashMap::new(),
                orphans: Vec::new(),
                lock: None,
            }
        }
    }

    fn spill_root() -> Option<PathBuf> {
        if cfg!(test) {
            // Unit tests never write into a real data dir.
            return Some(std::env::temp_dir().join("toolport-result-spill-tests"));
        }
        crate::registry::conduit_dir().map(|data| data.join("result-spill"))
    }

    /// At start: remove the spill directories of gateways that have exited (their key
    /// died with them). A gateway holds [`LOCK_FILE`] in its directory for as long as
    /// it runs, and the OS releases it when the process ends, so a directory whose
    /// lock can be taken is dead and one whose lock is held is left to its owner, which
    /// reaps its own files. This holds on Windows too, where no pid check is done.
    ///
    /// A directory with no lock file comes from an older gateway. It is removed when
    /// its pid is gone (Unix) and otherwise loses each file whose age, its last
    /// modified time, has passed the TTL. That gateway never extends a file's time.
    fn remove_stale(root: &std::path::Path) {
        let Ok(dirs) = std::fs::read_dir(root) else {
            return;
        };
        for dir in dirs.flatten() {
            match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(dir.path().join(LOCK_FILE))
            {
                Ok(lock) => {
                    if lock.try_lock().is_ok() {
                        drop(lock);
                        let _ = std::fs::remove_dir_all(dir.path());
                    }
                    // Held, or not lockable at all: its owner may still be serving it.
                    continue;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                // Present but unopenable (on Windows, a sharing violation): leave it.
                Err(_) => continue,
            }
            let pid = dir
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok());
            if pid.is_none_or(|pid| !process_alive(pid)) {
                let _ = std::fs::remove_dir_all(dir.path());
                continue;
            }
            for file in std::fs::read_dir(dir.path())
                .into_iter()
                .flatten()
                .flatten()
            {
                let expired = file
                    .metadata()
                    .and_then(|meta| meta.modified())
                    .ok()
                    .and_then(|modified| modified.elapsed().ok())
                    .is_none_or(|age| age >= CACHE_TTL);
                if expired {
                    let _ = std::fs::remove_file(file.path());
                }
            }
        }
    }

    fn process_alive(pid: u32) -> bool {
        if pid == std::process::id() {
            // A reused pid from an earlier run: its files are not ours to read.
            return false;
        }
        // Signal 0 on Unix; on Windows a process handle that is not yet signaled. An
        // inconclusive answer counts as running.
        crate::gateway_publish::pid_is_running(pid)
    }

    /// Keep `entry` on disk under `cursor`, its TTL running from `touched` (when it was
    /// stored or last read), or say why it cannot be kept.
    pub(super) fn keep(cursor: &str, entry: &Cached, touched: Instant) -> Result<(), Refusal> {
        // A discovery result stays in memory, where its visibility is checked on
        // every fetch and `forget_discovery` drops it. Pushed out of memory it is
        // dropped, and one too large for memory is not kept: search makes a fresh one.
        if !entry.describes.is_empty() {
            return Err(Refusal::TooLarge);
        }
        let mut state = state().lock().unwrap_or_else(|e| e.into_inner());
        state.keep(cursor, entry, touched)
    }

    impl State {
        fn keep(&mut self, cursor: &str, entry: &Cached, touched: Instant) -> Result<(), Refusal> {
            let state = self;
            #[cfg(test)]
            if FAIL_FOR_TESTS.with(std::cell::Cell::get) {
                return Err(Refusal::Unstored);
            }
            if touched.elapsed() >= spill_ttl() || entry.past_lifetime() {
                return Err(Refusal::Expired);
            }
            // No secret reaches the disk, sealed or not: the result is redacted with
            // every server's secrets first, so a fetch from disk returns it redacted.
            // Without a working matcher the result stays in memory.
            if crate::secret_redact::unavailable_reason().is_some() {
                return Err(Refusal::Unstored);
            }
            let mut plain = json!({ "body": entry.body, "structured": entry.structured });
            crate::secret_redact::redact_outgoing(&mut plain, None);
            let plain = plain.to_string();
            // Base64 of the sealed text is a third larger; check before sealing.
            if plain.len() / 3 * 4 > max_spill_bytes() {
                return Err(Refusal::TooLarge);
            }
            let (Some(dir), Some(cipher)) = (state.dir.clone(), state.cipher.as_ref()) else {
                return Err(Refusal::Unstored);
            };
            let mut nonce = [0u8; NONCE_LEN];
            if getrandom::getrandom(&mut nonce).is_err() {
                return Err(Refusal::Unstored);
            }
            let Ok(sealed) = cipher.encrypt(XNonce::from_slice(&nonce), plain.as_bytes()) else {
                return Err(Refusal::Unstored);
            };
            let mut bytes = nonce.to_vec();
            bytes.extend_from_slice(&sealed);
            let text = base64::engine::general_purpose::STANDARD.encode(bytes);
            if text.len() > max_spill_bytes() {
                return Err(Refusal::TooLarge);
            }
            state.reap();
            while state.used() + text.len() > max_spill_bytes() {
                let Some(oldest) = state
                    .entries
                    .iter()
                    .min_by_key(|(_, s)| s.touched)
                    .map(|(cursor, _)| cursor.clone())
                else {
                    break;
                };
                state.drop_entry(&oldest);
            }
            if state.used() + text.len() > max_spill_bytes() {
                // Files that would not delete still hold the room this one needs.
                return Err(Refusal::Unstored);
            }
            if state.claim_dir(&dir).is_err() {
                return Err(Refusal::Unstored);
            }
            let path = dir.join(format!("{cursor}.sealed"));
            if crate::registry::atomic_write(&path, &text).is_err() {
                return Err(Refusal::Unstored);
            }
            state.entries.insert(
                cursor.to_string(),
                Spilled {
                    path,
                    server: entry.server.clone(),
                    owner: entry.owner.clone(),
                    projections_unavailable: entry.projections_unavailable,
                    bytes: text.len(),
                    first_stored: entry.first_stored,
                    touched,
                },
            );
            Ok(())
        }
    }

    impl State {
        /// Bytes on disk: live entries plus files that would not delete.
        fn used(&self) -> usize {
            self.entries.values().map(|s| s.bytes).sum::<usize>()
                + self.orphans.iter().map(|(_, bytes)| bytes).sum::<usize>()
        }

        /// Drop `cursor` and its file. A file that will not delete stays counted.
        fn drop_entry(&mut self, cursor: &str) {
            if let Some(gone) = self.entries.remove(cursor) {
                if !remove_file(&gone.path) {
                    self.orphans.push((gone.path, gone.bytes));
                }
            }
        }

        /// Drop expired entries and retry removing files that would not delete.
        fn reap(&mut self) {
            self.reap_at(Instant::now());
        }

        fn reap_at(&mut self, now: Instant) {
            self.orphans.retain(|(path, _)| !remove_file(path));
            let expired: Vec<String> = self
                .entries
                .iter()
                .filter(|(_, spilled)| spilled.expired_at(now))
                .map(|(cursor, _)| cursor.clone())
                .collect();
            for cursor in expired {
                self.drop_entry(&cursor);
            }
        }

        /// Remove every file of this process and stop spilling: the process is exiting.
        fn release(&mut self) {
            let cursors: Vec<String> = self.entries.keys().cloned().collect();
            for cursor in cursors {
                self.drop_entry(&cursor);
            }
            self.orphans.retain(|(path, _)| !remove_file(path));
            self.lock = None;
            if let Some(dir) = self.dir.take() {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    }

    /// Remove this process's spilled results. A crash leaves them for the next
    /// gateway's start-up sweep; nothing can read them once this process is gone.
    pub(super) fn release() {
        if let Some(state) = STATE.get() {
            state.lock().unwrap_or_else(|e| e.into_inner()).release();
        }
    }

    impl State {
        /// Create `dir` for this user only and take its lock, once per process.
        fn claim_dir(&mut self, dir: &std::path::Path) -> std::io::Result<()> {
            if self.lock.is_some() && dir.is_dir() {
                return Ok(());
            }
            create_private_dir(dir)?;
            // Lock under a temporary name, then move it into place: a sweep that
            // found an unlocked `owner.lock` here would take this directory for dead.
            let staged = dir.join(format!("{LOCK_FILE}.{}", std::process::id()));
            let lock = std::fs::File::create(&staged)?;
            lock.try_lock().map_err(std::io::Error::from)?;
            std::fs::rename(&staged, dir.join(LOCK_FILE))?;
            self.lock = Some(lock);
            Ok(())
        }
    }

    /// Create `dir` and its parent readable by this user only. On Unix they are 0700
    /// (files are 0600 through `atomic_write`). On Windows each gets a protected DACL
    /// granting only the current user, inherited by every file created inside, so a
    /// spill never takes the data dir's broader ACL.
    fn create_private_dir(dir: &std::path::Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [dir, dir.parent().unwrap_or(dir)] {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
            }
        }
        #[cfg(windows)]
        {
            for path in [dir.parent().unwrap_or(dir), dir] {
                crate::clients::backup_permissions::secure(path).map_err(std::io::Error::other)?;
            }
        }
        Ok(())
    }

    /// The spilled entry for `cursor`, if it is still live and belongs to `requester`.
    pub(super) fn load(cursor: &str, requester: Option<&str>) -> Option<Cached> {
        // Nothing was spilled yet when the state does not exist: only `keep` opens
        // the spill directory, so a read never resolves the data directory.
        STATE
            .get()?
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .load(cursor, requester)
    }

    impl State {
        /// Read `cursor` back for `requester`. A read starts its TTL again, up to
        /// `MAX_RESULT_LIFETIME` from when it was first stored.
        fn load(&mut self, cursor: &str, requester: Option<&str>) -> Option<Cached> {
            self.load_at(cursor, requester, Instant::now())
        }

        /// [`State::load`] as of `now`.
        fn load_at(
            &mut self,
            cursor: &str,
            requester: Option<&str>,
            now: Instant,
        ) -> Option<Cached> {
            self.reap_at(now);
            let cipher = self.cipher.as_ref()?;
            let spilled = self.entries.get_mut(cursor)?;
            if spilled.owner.as_deref() != requester {
                return None;
            }
            let text = std::fs::read_to_string(&spilled.path).ok()?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(text)
                .ok()?;
            if bytes.len() < NONCE_LEN {
                return None;
            }
            let (nonce, sealed) = bytes.split_at(NONCE_LEN);
            let plain = cipher.decrypt(XNonce::from_slice(nonce), sealed).ok()?;
            let value: Value = serde_json::from_slice(&plain).ok()?;
            let structured = value.get("structured").filter(|v| !v.is_null()).cloned();
            spilled.touched = now;
            Some(Cached {
                server: spilled.server.clone(),
                body: value.get("body")?.as_str()?.to_string(),
                size: 0,
                structured,
                projections_unavailable: spilled.projections_unavailable,
                owner: spilled.owner.clone(),
                first_stored: spilled.first_stored,
                describes: Vec::new(),
                realm: 0,
            })
        }
    }

    /// Drop a spilled entry and its file (a shape that was declined after storing).
    pub(super) fn forget(cursor: &str) {
        if let Some(state) = STATE.get() {
            state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .drop_entry(cursor);
        }
    }

    pub(super) fn reap_expired() {
        if let Some(state) = STATE.get() {
            state.lock().unwrap_or_else(|e| e.into_inner()).reap();
        }
    }

    /// Remove spill directories that gateways which have exited left behind. The
    /// gateway runs this at start-up; otherwise the first spill of a run does it.
    pub(super) fn remove_stale_at_startup() {
        // Opening the state sweeps the root before this run has a directory there,
        // under the same once-only step every spill takes.
        let _ = state();
    }

    #[cfg(test)]
    pub(super) fn remove_stale_for_tests(root: &std::path::Path) {
        remove_stale(root);
    }

    #[cfg(test)]
    pub(super) fn spilled_path(cursor: &str) -> Option<PathBuf> {
        let state = state().lock().unwrap_or_else(|e| e.into_inner());
        state.entries.get(cursor).map(|s| s.path.clone())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn entry(body: &str) -> Cached {
            Cached {
                server: None,
                body: body.to_string(),
                structured: None,
                projections_unavailable: false,
                size: 0,
                owner: None,
                first_stored: Instant::now(),
                describes: Vec::new(),
                realm: 0,
            }
        }

        #[test]
        fn a_spilled_result_is_redacted_before_it_is_sealed() {
            let _data = crate::registry::DataDirTestEnv::new("spill-redact");
            let secret = "sk-SpilledResultSecret64";
            crate::secret_redact::register("spill-srv", "KEY", secret);
            let root = case_root("redact");
            let mut state = State::new(Some(root.clone()), Some([7; 32]));
            let mut item = entry(&format!("token {secret} end"));
            item.structured = Some(json!({ "k": secret }));
            assert!(state.keep("r1", &item, Instant::now()).is_ok());
            let loaded = state.load("r1", None).expect("kept on disk");
            let _ = std::fs::remove_dir_all(&root);
            assert_eq!(loaded.body, "token [secret:KEY] end");
            assert_eq!(loaded.structured, Some(json!({ "k": "[secret:KEY]" })));
        }

        /// A fresh spill root of this test's own, under the test spill root.
        fn case_root(name: &str) -> PathBuf {
            // Outside the shared test root, whose start-up sweep removes what it finds.
            let root = std::env::temp_dir()
                .join("toolport-result-spill-cases")
                .join(name);
            let _ = std::fs::remove_dir_all(&root);
            root
        }

        /// Sealed spill files under `root`, lock files aside.
        fn files_under(root: &std::path::Path) -> usize {
            std::fs::read_dir(root)
                .into_iter()
                .flatten()
                .flatten()
                .flat_map(|dir| {
                    std::fs::read_dir(dir.path())
                        .into_iter()
                        .flatten()
                        .flatten()
                })
                .filter(|file| file.path().extension().is_some_and(|ext| ext == "sealed"))
                .count()
        }

        #[test]
        fn spilling_is_off_when_the_sealing_key_cannot_be_made() {
            let root = case_root("no-key");
            let mut state = State::new(Some(root.clone()), None);
            assert_eq!(
                state.keep("r1", &entry("secret body"), Instant::now()),
                Err(Refusal::Unstored)
            );
            assert_eq!(
                files_under(&root),
                0,
                "a spill was written under a zero key"
            );
            // The same store with a key does spill, so the test exercises the key.
            let mut keyed = State::new(Some(root.clone()), Some([7; 32]));
            assert!(keyed
                .keep("r1", &entry("secret body"), Instant::now())
                .is_ok());
            assert_eq!(files_under(&root), 1);
            let _ = std::fs::remove_dir_all(&root);
        }

        #[test]
        fn a_spill_that_will_not_delete_keeps_its_bytes_counted() {
            let root = case_root("undeletable");
            let mut state = State::new(Some(root.clone()), Some([7; 32]));
            let body = "a".repeat(1_000);
            // Room for one sealed entry, not two.
            set_cap_for_tests(Some(2_000));
            assert!(state.keep("r1", &entry(&body), Instant::now()).is_ok());
            fail_removals_for_tests(true);
            let second = state.keep("r2", &entry(&body), Instant::now());
            fail_removals_for_tests(false);
            assert_eq!(
                second,
                Err(Refusal::Unstored),
                "budget freed by a failed delete"
            );
            assert_eq!(files_under(&root), 1);
            // The next spill retries the removal, and the budget comes back.
            assert!(state.keep("r3", &entry(&body), Instant::now()).is_ok());
            set_cap_for_tests(None);
            assert_eq!(files_under(&root), 1);
            let _ = std::fs::remove_dir_all(&root);
        }

        #[test]
        fn a_read_from_disk_extends_its_time_up_to_the_lifetime_cap() {
            let root = case_root("extend");
            let mut state = State::new(Some(root.clone()), Some([7; 32]));
            let secs = Duration::from_secs;
            super::super::set_lifetimes_for_tests(Some((secs(60), secs(100))));
            // The clock is passed in: each read happens at an exact time after storing,
            // so no read depends on how fast the test runs.
            let stored = Instant::now();
            let mut item = entry("body");
            item.first_stored = stored;
            assert!(state.keep("r1", &item, stored).is_ok());
            let first = state.load_at("r1", None, stored + secs(40)).is_some();
            // Past the TTL from storing, inside it from the read.
            let second = state.load_at("r1", None, stored + secs(80)).is_some();
            // Inside the TTL from the last read, past the lifetime cap.
            let third = state.load_at("r1", None, stored + secs(110)).is_some();
            super::super::set_lifetimes_for_tests(None);
            assert!(first);
            assert!(second, "a read did not extend it");
            assert!(!third, "a read extended it past the lifetime cap");
            let _ = std::fs::remove_dir_all(&root);
        }

        #[test]
        fn release_removes_this_process_spills_and_stops_spilling() {
            let root = case_root("release");
            let mut state = State::new(Some(root.clone()), Some([7; 32]));
            assert!(state.keep("r1", &entry("body"), Instant::now()).is_ok());
            let dir = root.join(std::process::id().to_string());
            // A spill takes the directory's lock, so another gateway's sweep keeps it.
            remove_stale(&root);
            assert_eq!(files_under(&root), 1);
            assert!(dir.join(LOCK_FILE).exists());
            state.release();
            assert!(!root.join(std::process::id().to_string()).exists());
            assert_eq!(
                state.keep("r2", &entry("body"), Instant::now()),
                Err(Refusal::Unstored)
            );
            assert_eq!(files_under(&root), 0);
            let _ = std::fs::remove_dir_all(&root);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn big_text_result(n: usize) -> Value {
        json!({ "content": [{ "type": "text", "text": "x".repeat(n) }], "isError": false })
    }

    #[test]
    fn fetched_pages_and_projections_keep_the_real_server() {
        let mut result = serde_json::json!({"content":[{"type":"text", "text":"x".repeat(10_000)}], "structuredContent":{"value":7}});
        crate::integrity::label_untrusted_result("github", &mut result);
        assert!(shape_result(&mut result, 2048, None));
        let text = result["content"][0]["text"].as_str().unwrap();
        let cursor = text
            .split("\"cursor\":\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap();
        for path in [None, Some("value")] {
            let fetched = fetch_result(cursor, 0, 100, None, path);
            assert_eq!(
                fetched["_meta"]["app.toolport/provenance"]["server"],
                "github"
            );
            assert_eq!(fetched["content"].as_array().unwrap().len(), 1);
        }
        crate::integrity::label_untrusted_result("github", &mut result);
        assert!(value_size(&result) <= 2048);
        let cursor = stash_payload("Toolport-owned data".into(), None, None).unwrap();
        let fetched = fetch_result(&cursor, 0, 100, None, None);
        assert!(fetched.get("_meta").is_none());
    }

    #[test]
    fn under_budget_is_untouched() {
        let mut r = big_text_result(100);
        assert!(!shape_result(&mut r, 1024, None));
        assert_eq!(r["content"][0]["text"].as_str().unwrap().len(), 100);
    }

    #[test]
    fn duplicate_json_is_paged_once_without_changing_text_or_typed_projection() {
        let structured = json!({"rows":[{"text":"é🙂".repeat(3000)}]});
        for pretty in [false, true] {
            let text = if pretty {
                serde_json::to_string_pretty(&structured).unwrap()
            } else {
                structured.to_string()
            };
            let mut result = json!({"content":[{"type":"text","text":text}], "structuredContent":structured,"isError":false,"extension":{"preserve":true}});
            crate::integrity::label_untrusted_result("github", &mut result);
            let original = serde_json::to_vec(&result).unwrap();
            let mut under = result.clone();
            assert!(!shape_result(
                &mut under,
                original.len() + 1024,
                Some("alice")
            ));
            assert_eq!(serde_json::to_vec(&under).unwrap(), original);
            assert!(shape_result(&mut result, 2048, Some("alice")));
            assert_eq!(result["extension"], json!({"preserve":true}));
            let cursor = cursor_of(&result);
            let fetched = fetch_result(&cursor, 0, usize::MAX, Some("alice"), None);
            assert_eq!(
                fetched["content"][0]["text"],
                format!(
                    "{text}\n\n[Toolport: end of result ({} characters).]",
                    text.chars().count()
                )
            );
            assert_eq!(
                fetched["_meta"]["app.toolport/provenance"]["trust"],
                "untrusted"
            );
            let projection = fetch_result(&cursor, 0, 0, Some("alice"), Some("rows"));
            assert_eq!(
                serde_json::from_str::<Value>(projection["content"][0]["text"].as_str().unwrap())
                    .unwrap(),
                structured["rows"]
            );
            assert_eq!(
                projection["_meta"]["app.toolport/provenance"]["server"],
                "github"
            );
            assert_eq!(
                fetch_result(&cursor, 0, 100, Some("bob"), None)["isError"],
                true
            );
        }
    }

    #[test]
    fn different_or_multiple_text_blocks_keep_both_representations() {
        for text in ["not JSON", r#"{"value":8}"#] {
            let result =
                json!({"content":[{"type":"text","text":text}],"structuredContent":{"value":7}});
            assert_eq!(extract_body(&result).0, format!("{text}\n{{\"value\":7}}"));
        }
        let multiple = json!({"content":[{"type":"text","text":"{\"value\":"},{"type":"text","text":"7}"}],"structuredContent":{"value":7}});
        assert_eq!(extract_body(&multiple).0, "{\"value\":\n7}\n{\"value\":7}");
    }

    #[test]
    fn over_budget_truncates_and_caches() {
        let mut r = big_text_result(10_000);
        assert!(shape_result(&mut r, 2048, None));
        let text = r["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("toolport_fetch_result"));
        assert!(text.len() < 10_000);
        // The marker carries a cursor that fetch_result can page.
        assert!(text.contains("\"cursor\":\"r"));
    }

    #[test]
    fn budget_zero_disables() {
        let mut r = big_text_result(10_000);
        assert!(!shape_result(&mut r, 0, None));
    }

    #[test]
    fn fetch_pages_the_remainder() {
        let mut r = big_text_result(10_000);
        shape_result(&mut r, 2048, None);
        // Pull the cursor back out of the marker.
        let text = r["content"][0]["text"].as_str().unwrap();
        let cursor = text
            .split("\"cursor\":\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .unwrap()
            .to_string();
        let more = fetch_result(&cursor, 1500, 500, None, None);
        let mt = more["content"][0]["text"].as_str().unwrap();
        assert!(mt.contains("of 10000"));
    }

    #[test]
    fn resolve_budget_cases() {
        assert_eq!(resolve_budget(Some("10000")), (10000, None));
        assert_eq!(resolve_budget(Some(" 20000 ")), (20000, None));
        assert_eq!(resolve_budget(None), (DEFAULT_BUDGET_BYTES, None));
        let (budget, warning) = resolve_budget(Some("invalid"));
        assert_eq!(budget, DEFAULT_BUDGET_BYTES);
        assert_eq!(
            warning.as_deref(),
            Some(
                "toolport: invalid TOOLPORT_RESULT_BUDGET/CONDUIT_RESULT_BUDGET value 'invalid', falling back to default budget"
            )
        );
    }

    #[test]
    fn fetch_unknown_cursor_is_an_error() {
        let v = fetch_result("nope", 0, 100, None, None);
        assert_eq!(v["isError"].as_bool(), Some(true));
    }

    // Pull the cursor back out of a shaped result's marker.
    fn cursor_of(r: &Value) -> String {
        r["content"][0]["text"]
            .as_str()
            .unwrap()
            .split("\"cursor\":\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .unwrap()
            .to_string()
    }

    #[test]
    fn fetch_is_scoped_to_the_owning_client() {
        let mut r = big_text_result(10_000);
        assert!(shape_result(&mut r, 2048, Some("alice")));
        let cursor = cursor_of(&r);
        // A different client (or an unattributed one) gets the same "unknown/expired"
        // answer as a missing cursor: no cross-tenant read, and no oracle for which
        // cursors exist. The stash is process-global, so in HTTP mode this is the only
        // thing stopping one client from reading another's result by guessing r{n}.
        assert_eq!(
            fetch_result(&cursor, 0, 100, Some("mallory"), None)["isError"].as_bool(),
            Some(true)
        );
        assert_eq!(
            fetch_result(&cursor, 0, 100, None, None)["isError"].as_bool(),
            Some(true)
        );
        // The owner still reads it.
        assert_ne!(
            fetch_result(&cursor, 0, 100, Some("alice"), None)["isError"].as_bool(),
            Some(true)
        );
    }

    #[test]
    fn fetch_scopes_by_stable_id_not_shared_display_label() {
        // SOU-324: stash owners must be client:{id}-style principals. Two
        // "Open WebUI" labels with different ids must not read each other.
        let mut r = big_text_result(10_000);
        assert!(shape_result(&mut r, 2048, Some("client:c1")));
        let cursor = cursor_of(&r);
        assert_eq!(
            fetch_result(&cursor, 0, 100, Some("client:c2"), None)["isError"].as_bool(),
            Some(true)
        );
        // A display label alone must never unlock a stash keyed by client id.
        assert_eq!(
            fetch_result(&cursor, 0, 100, Some("Open WebUI"), None)["isError"].as_bool(),
            Some(true)
        );
        assert_ne!(
            fetch_result(&cursor, 0, 100, Some("client:c1"), None)["isError"].as_bool(),
            Some(true)
        );
    }

    #[test]
    fn fetch_with_pathological_len_does_not_panic() {
        let mut r = big_text_result(10_000);
        shape_result(&mut r, 2048, None);
        let cursor = cursor_of(&r);
        // offset + len must saturate, not overflow into a start > end byte slice
        // (which panics, and on the stdio transport takes down the whole gateway).
        let v = fetch_result(&cursor, 5, usize::MAX, None, None);
        assert_ne!(v["isError"].as_bool(), Some(true));
        assert!(v["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("end of result"));
    }

    #[test]
    fn relayable_meta_keeps_unknown_and_drops_per_hop() {
        use crate::downstream::{relayable_meta, sanitize_forwarded_meta};

        let meta = json!({
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientInfo": { "name": "x" },
            "io.modelcontextprotocol/clientCapabilities": {},
            "progressToken": "p-1",
            "traceparent": "00-abc-def-01",
            "com.example/unknown": { "a": 1 }
        });
        let kept = relayable_meta(Some(&meta)).expect("some keys survive");
        assert_eq!(kept["traceparent"], "00-abc-def-01");
        assert_eq!(kept["com.example/unknown"]["a"], 1);
        assert!(kept
            .get("io.modelcontextprotocol/protocolVersion")
            .is_none());
        assert!(kept.get("io.modelcontextprotocol/clientInfo").is_none());
        assert!(kept
            .get("io.modelcontextprotocol/clientCapabilities")
            .is_none());
        // Relayed since SOU-444 part 2: the gateway now routes the resulting
        // `notifications/progress` back to the client that minted the token.
        assert_eq!(kept["progressToken"], "p-1");

        // Nothing relayable means no `_meta` at all, not an empty object, so the
        // request stays byte-identical to what Toolport sent before SOU-444.
        assert!(relayable_meta(Some(
            &json!({ "io.modelcontextprotocol/clientInfo": { "name": "x" } })
        ))
        .is_none());
        assert!(relayable_meta(None).is_none());

        // The wholesale-forward path (completion/complete) strips the same keys.
        let mut params = json!({
            "ref": { "type": "ref/prompt", "name": "p" },
            "_meta": { "io.modelcontextprotocol/clientInfo": { "name": "x" }, "keep": 1 }
        });
        sanitize_forwarded_meta(&mut params);
        assert_eq!(params["_meta"]["keep"], 1);
        assert!(params["_meta"]
            .get("io.modelcontextprotocol/clientInfo")
            .is_none());

        // ...and removes `_meta` entirely when nothing survives.
        let mut params = json!({
            "ref": {}, "_meta": { "io.modelcontextprotocol/protocolVersion": "2026-07-28" }
        });
        sanitize_forwarded_meta(&mut params);
        assert!(params.get("_meta").is_none());
    }

    #[test]
    fn shaped_results_fit_the_budget_across_envelope_sizes() {
        // A SWEEP, not a single point. The previous version of this test hardcoded
        // a 2 000-byte envelope, which sat inside the safe zone, while 3 600 B
        // returned `true` at 4 269 bytes against a 4 096 budget. Any single-point
        // test can land in a window like that; stepping across the range cannot.
        // Size of the marker plus JSON skeleton, measured rather than guessed.
        // Declining is legitimate only once the envelope plus this cannot fit.
        //
        // This bound is deliberately TIGHT. A generous one (600) let the test pass
        // against a head-size floor that gave up at a 3 600-byte envelope and
        // passed the whole 53 686-byte body through, where shrinking to fit
        // produces 4 009. Picking the threshold by measuring both implementations
        // is the only reason this catches it.
        const MARKER_RESERVE: usize = 450;

        let budget = 4096;
        for meta_len in [
            0, 500, 1_000, 2_000, 3_000, 3_400, 3_600, 3_800, 4_000, 4_100,
        ] {
            let mut r = json!({
                "content": [{ "type": "text", "text": "x".repeat(50_000) }],
                "isError": false,
                "_meta": { "com.example/ctx": "m".repeat(meta_len) }
            });
            let shaped = shape_result(&mut r, budget, None);
            let size = serde_json::to_string(&r).map(|s| s.len()).unwrap_or(0);

            if shaped {
                assert!(
                    size <= budget,
                    "`true` must mean it fits: {meta_len}B envelope produced {size} \
                     bytes against a {budget} budget"
                );
                // The envelope has to survive, otherwise "fits" was bought by
                // silently dropping the server's data.
                assert_eq!(
                    r["_meta"]["com.example/ctx"].as_str().map(str::len),
                    Some(meta_len),
                    "{meta_len}B envelope was dropped rather than preserved"
                );
            } else {
                // Declining is allowed ONLY when the envelope plus the marker
                // genuinely cannot fit. Otherwise declining is itself a
                // regression: the full 50 KB body reaches the model instead of a
                // shaped head. This is what catches a head-size floor that gives
                // up early rather than shrinking to fit.
                assert!(
                    meta_len + MARKER_RESERVE >= budget,
                    "{meta_len}B envelope leaves room for a head, so it should have \
                     been shaped rather than passed through whole"
                );
                // ...and an unshaped result must be left completely untouched.
                assert!(
                    r["content"][0]["text"].as_str().map(str::len) == Some(50_000),
                    "an unshaped result must be left alone, {meta_len}B case"
                );
            }
        }
    }

    #[test]
    fn preserved_envelope_fields_do_not_push_a_shaped_result_over_budget() {
        // Preserved fields are part of the shaped result, so they come out of the
        // same budget. Before this, a large `_meta` was copied in AFTER the head
        // was sized, so `true` could mean "shaped, and still oversized" (#511
        // review). The guarantee is that a `true` return fits.
        let budget = 4096;
        let mut r = json!({
            "content": [{ "type": "text", "text": "x".repeat(50_000) }],
            "isError": false,
            // Deliberately bulky: about half the budget on its own.
            "_meta": { "com.example/ctx": "m".repeat(2_000) }
        });
        assert!(shape_result(&mut r, budget, None));

        let size = serde_json::to_string(&r).map(|s| s.len()).unwrap_or(0);
        assert!(
            size <= budget,
            "a shaped result must fit the budget, got {size} bytes against {budget}"
        );
        // ...and the bulky field really was preserved, not dropped to make it fit.
        assert_eq!(
            r["_meta"]["com.example/ctx"].as_str().map(str::len),
            Some(2_000)
        );
    }

    #[test]
    fn shaping_preserves_meta_and_unknown_envelope_fields() {
        // Shaping truncates the BODY. Everything else in the envelope belongs to
        // the downstream server: `_meta`, and whatever a future revision or
        // extension adds. Dropping it would make Toolport a lossy proxy in the
        // result direction, the mirror of the request-side gap (SOU-444).
        let mut r = json!({
            "content": [{ "type": "text", "text": "x".repeat(5_000) }],
            "isError": false,
            "_meta": { "io.modelcontextprotocol/serverInfo": { "name": "srv", "version": "1" } },
            "somethingAFutureSpecAdded": { "keep": true }
        });
        assert!(shape_result(&mut r, 1024, None));

        assert_eq!(
            r["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
            "srv"
        );
        assert_eq!(r["somethingAFutureSpecAdded"]["keep"], true);
        // ...while the body really was shaped.
        assert!(r["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Toolport shaped this result"));
    }

    #[test]
    fn multibyte_head_respects_byte_budget() {
        // 3-byte chars: truncating by char COUNT would emit ~3x the budget in bytes.
        let mut r = json!({
            "content": [{ "type": "text", "text": "€".repeat(5_000) }],
            "isError": false
        });
        assert!(shape_result(&mut r, 2048, None));
        let text = r["content"][0]["text"].as_str().unwrap();
        let head = text.split("\n\n[Toolport shaped").next().unwrap();
        assert!(
            head.len() <= 2048,
            "head was {} bytes, over the 2048 budget",
            head.len()
        );
    }

    #[test]
    fn fetch_pages_multibyte_by_char_offset() {
        // The body is all 3-byte chars, so char offsets != byte offsets. The
        // single-pass byte mapping must slice on char boundaries and honor the
        // requested character window exactly.
        let mut r = json!({
            "content": [{ "type": "text", "text": "€".repeat(4_000) }],
            "isError": false
        });
        assert!(shape_result(&mut r, 2048, None));
        let text = r["content"][0]["text"].as_str().unwrap();
        let cursor = text
            .split("\"cursor\":\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .unwrap()
            .to_string();
        // Read 100 chars starting at char 1000 (byte 3000): all euros, none split.
        let page = fetch_result(&cursor, 1000, 100, None, None);
        let pt = page["content"][0]["text"].as_str().unwrap();
        let body = pt.split("\n\n[Toolport:").next().unwrap();
        assert_eq!(body.chars().filter(|&c| c == '€').count(), 100);
        assert!(pt.contains("of 4000"));
    }

    #[test]
    fn fetch_past_end_reports_nothing_more() {
        let mut r = big_text_result(10_000);
        shape_result(&mut r, 2048, None);
        let text = r["content"][0]["text"].as_str().unwrap();
        let cursor = text
            .split("\"cursor\":\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .unwrap()
            .to_string();
        let past = fetch_result(&cursor, 999_999, 100, None, None);
        let pt = past["content"][0]["text"].as_str().unwrap();
        assert!(pt.contains("past the end"));
        assert_eq!(past["isError"].as_bool(), Some(false));
    }

    #[test]
    fn non_text_result_is_not_shaped() {
        // A large image block would be dropped by shaping, so it must pass through.
        let mut r = json!({
            "content": [{ "type": "image", "data": "A".repeat(10_000), "mimeType": "image/png" }],
            "isError": false
        });
        assert!(!shape_result(&mut r, 2048, None));
        assert_eq!(r["content"][0]["type"].as_str(), Some("image"));
    }

    #[test]
    fn envelope_heavy_result_is_not_shaped() {
        // Size is dominated by a non-body field the text projection can't capture,
        // so shaping would lose it; leave the result whole.
        let mut r = json!({
            "content": [{ "type": "text", "text": "small" }],
            "annotations": { "blob": "Z".repeat(10_000) },
            "isError": false
        });
        assert!(!shape_result(&mut r, 2048, None));
        assert_eq!(r["content"][0]["text"].as_str(), Some("small"));
    }

    #[test]
    fn cache_is_bounded() {
        // Insert well past the cap; the cache must never exceed MAX_CACHE_ENTRIES.
        for _ in 0..(MAX_CACHE_ENTRIES + 20) {
            let mut r = big_text_result(5_000);
            shape_result(&mut r, 1024, None);
        }
        let map = cache().lock().unwrap_or_else(|e| e.into_inner());
        assert!(
            map.len() <= MAX_CACHE_ENTRIES,
            "cache grew to {} entries",
            map.len()
        );
    }

    // A cache entry with a given recorded `size`, without allocating a body of that
    // size: the eviction loop reads `Cached.size`, never the body itself, so
    // multi-megabyte entries cost a few bytes here. The store evicts in insertion
    // order, so the tests insert oldest first.
    fn cached_entry(size: usize) -> Cached {
        Cached {
            server: None,
            body: String::new(),
            structured: None,
            projections_unavailable: false,
            size,
            owner: None,
            describes: Vec::new(),
            realm: 0,
            first_stored: Instant::now(),
        }
    }

    #[test]
    fn cache_byte_cap_evicts_oldest_first() {
        // The ENTRY cap is covered by `cache_is_bounded`; this is the BYTE cap, which
        // a burst of a few huge results hits long before 64 entries. Sizes are
        // recorded, not allocated, so the 64 MiB path costs nothing to exercise.
        // Three of these sum to 72 MiB, past the 64 MiB cap; dropping the oldest
        // leaves 48 MiB, so exactly one eviction is required for a 1 MiB arrival.
        const HUGE: usize = 24 * 1024 * 1024;
        let mut store: SessionStore<Cached> =
            SessionStore::new(Duration::from_secs(900), MAX_CACHE_ENTRIES);
        store.insert("oldest", cached_entry(HUGE));
        store.insert("middle", cached_entry(HUGE));
        store.insert("newest", cached_entry(HUGE));
        assert!(
            store.len() < MAX_CACHE_ENTRIES,
            "must exercise the byte cap, not the entry cap"
        );

        let new_entry_size = 1024 * 1024;
        evict_to_fit(&mut store, new_entry_size);
        store.insert("incoming", cached_entry(new_entry_size));

        assert!(
            store.weight(|c| c.size) <= MAX_CACHE_BYTES,
            "cached bytes grew to {}, past the {MAX_CACHE_BYTES} cap",
            store.weight(|c| c.size)
        );
        // Oldest-by-insertion goes first, and only as far as needed.
        assert!(
            store.get("oldest").is_none(),
            "the oldest entry must be evicted first"
        );
        assert!(
            store.get("middle").is_some(),
            "eviction must stop once the new body fits"
        );
        assert!(store.get("newest").is_some());
        assert!(store.get("incoming").is_some());
    }

    // An object whose recorded size passes the memory cap without filling it: `json!`
    // would copy the string and lose its capacity, so build the value directly.
    fn big_object(extra: &[(&str, Value)]) -> Value {
        let mut object = serde_json::Map::new();
        object.insert(
            "big".into(),
            Value::String(String::with_capacity(MAX_CACHE_BYTES)),
        );
        for (key, value) in extra {
            object.insert((*key).into(), value.clone());
        }
        Value::Object(object)
    }

    #[test]
    fn over_cap_entries_go_to_disk_without_evicting_live_results() {
        let mut store = SessionStore::new(CACHE_TTL, MAX_CACHE_ENTRIES);
        store.insert("live", cached_entry(1024));
        assert!(retain_in(&mut store, "oversize", cached_entry(MAX_CACHE_BYTES + 1)).is_ok());
        assert!(store.get("oversize").is_none());
        assert!(spill::spilled_path("oversize").is_some());
        assert!(store.get("live").is_some());
        assert!(retain_in(&mut store, "at-cap", cached_entry(MAX_CACHE_BYTES)).is_ok());
        // The live entry made room and moved to disk; it was not dropped.
        assert!(store.get("live").is_none());
        assert!(spill::spilled_path("live").is_some());
        assert_eq!(store.weight(|c| c.size), MAX_CACHE_BYTES);
        for cursor in ["oversize", "live"] {
            spill::forget(cursor);
        }
    }

    #[test]
    fn an_issued_cursor_stays_in_memory_when_it_cannot_move_to_disk() {
        let mut store = SessionStore::new(CACHE_TTL, HARD_MAX_CACHE_ENTRIES);
        store.insert("issued", cached_entry(MAX_CACHE_BYTES));
        spill::fail_writes_for_tests(true);
        let next = retain_in(&mut store, "next", cached_entry(MAX_CACHE_BYTES));
        // Past the hard ceiling the new result is refused, never an issued one.
        let refused = retain_in(&mut store, "refused", cached_entry(1));
        spill::fail_writes_for_tests(false);
        assert!(next.is_ok());
        assert!(
            store.get("issued").is_some(),
            "an issued cursor was dropped"
        );
        assert!(store.get("next").is_some());
        assert_eq!(refused, Err(Refusal::Unstored));
        assert!(store.get("refused").is_none());
        assert!(crate::daemon::status_notes()
            .iter()
            .any(|note| note == spill::KEPT_IN_MEMORY_NOTE));
    }

    #[test]
    fn a_payload_past_the_disk_cap_reports_no_cursor() {
        spill::set_cap_for_tests(Some(1_024));
        let big = "z".repeat(4_096);
        let structured = big_object(&[]);
        assert!(stash_payload(big, Some(structured), None).is_none());
        spill::set_cap_for_tests(None);
    }

    #[test]
    fn an_over_cap_stash_keeps_its_text_and_projections_on_disk() {
        let structured = big_object(&[("small", json!(7))]);
        let cursor = stash_payload("retained text".into(), Some(structured), None).unwrap();
        assert!(spill::spilled_path(&cursor).is_some());
        assert_eq!(
            fetch_result(&cursor, 0, 100, None, None),
            text_result(
                "retained text\n\n[Toolport: end of result (13 characters).]".into(),
                false
            )
        );
        assert_eq!(
            fetch_result(&cursor, 0, 100, None, Some("small")),
            text_result("7".into(), false)
        );
        spill::forget(&cursor);
    }

    #[test]
    fn shaping_past_the_disk_cap_reports_unavailable_paging_without_a_cursor() {
        spill::set_cap_for_tests(Some(1_024));
        let mut result = json!({
            "content": [{"type": "text", "text": "x".repeat(MAX_CACHE_BYTES)}],
            "isError": false,
            "_meta": {"fixture": true},
        });
        assert!(shape_result(&mut result, 2048, None));
        spill::set_cap_for_tests(None);
        assert!(value_size(&result) <= 2048);
        assert_eq!(result["isError"], false);
        assert_eq!(result["_meta"]["fixture"], true);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("xxx"));
        assert!(text.ends_with("[Toolport: full result (67.1 MB) was too large to keep for paging, even on disk. Re-run the tool with narrower arguments.]"));
        assert!(!text.contains("cursor"));
        assert!(!text.contains("toolport_fetch_result"));
        for refusal in [Refusal::TooLarge, Refusal::Unstored] {
            assert!(not_retained_notice(MAX_CACHE_BYTES, refusal).len() < 160);
        }
    }

    #[test]
    fn a_result_that_could_not_be_stored_is_not_called_too_large() {
        spill::fail_writes_for_tests(true);
        let mut result = text_result("x".repeat(MAX_CACHE_BYTES), false);
        assert!(shape_result(&mut result, 2048, None));
        spill::fail_writes_for_tests(false);
        assert!(value_size(&result) <= 2048);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.ends_with("[Toolport: full result (67.1 MB) could not be stored for paging (a disk or key failure). Re-run the tool, or narrow the call.]"), "{}", &text[text.len() - 200..]);
        assert!(!text.contains("cursor"));
    }

    #[test]
    fn an_over_cap_tree_pages_and_projects_from_disk() {
        let structured =
            json!({"rows": (0..100_000).map(|i| json!({"id": i, "ok": true})).collect::<Vec<_>>()});
        let text = structured.to_string();
        assert!(retained_size(&text, Some(&structured)) > MAX_CACHE_BYTES);
        let mut result = json!({"structuredContent": structured});
        assert!(shape_result(&mut result, 2048, Some("alice")));
        let cursor = cursor_of(&result);
        let page = fetch_result(&cursor, text.len() - 100, 100, Some("alice"), None);
        assert_eq!(
            page,
            text_result(
                format!(
                    "{}\n\n[Toolport: end of result ({} characters).]",
                    &text[text.len() - 100..],
                    text.len()
                ),
                false
            )
        );
        let projection = fetch_result(&cursor, 0, 100, Some("alice"), Some("rows.0"));
        assert_eq!(
            projection,
            text_result("{\"id\":0,\"ok\":true}".into(), false)
        );
        assert_eq!(
            fetch_result(&cursor, 0, 100, Some("bob"), Some("rows.0"))["isError"],
            true
        );
        assert!(cache()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&cursor)
            .is_none());
        spill::forget(&cursor);
    }

    #[test]
    fn normal_first_reply_and_pages_preserve_wire_bytes() {
        let mut result = big_text_result(10_000);
        assert!(shape_result(&mut result, 2048, None));
        let cursor = cursor_of(&result);
        // The head fills the budget as far as the widest marker allows.
        let head = result["content"][0]["text"]
            .as_str()
            .unwrap()
            .chars()
            .take_while(|c| *c == 'x')
            .count();
        assert!(head > 1536, "head {head}");
        assert!(serde_json::to_vec(&result).unwrap().len() <= 2048);
        let expected = text_result(format!("{}\n\n[Toolport shaped this result: ~9 KB exceeds 2 KB budget. Showing first {head} of 10000 characters. Continue with toolport_fetch_result {{\"cursor\":\"{}\",\"offset\":{head}}}. If the cursor expired, re-run the original tool call.]", "x".repeat(head), cursor), false);
        assert_eq!(
            serde_json::to_vec(&result).unwrap(),
            serde_json::to_vec(&expected).unwrap()
        );
        let page = fetch_result(&cursor, 1536, 100, None, None);
        let expected = text_result(format!("{}\n\n[Toolport: characters 1536..1636 of 10000. 8364 remain, call toolport_fetch_result with offset=1636 for the next slice.]", "x".repeat(100)), false);
        assert_eq!(
            serde_json::to_vec(&page).unwrap(),
            serde_json::to_vec(&expected).unwrap()
        );
    }

    #[test]
    fn cached_size_records_retained_allocations() {
        // The eviction loop trusts `Cached.size` instead of re-serializing, so a
        // size recorded as 0 (or body-only) would silently disable the byte cap.
        let structured = json!({ "rows": ["y".repeat(3_000)] });
        let mut r = json!({
            "content": [{ "type": "text", "text": "x".repeat(5_000) }],
            "structuredContent": structured,
            "isError": false
        });
        assert!(shape_result(&mut r, 2048, None));
        let cursor = cursor_of(&r);

        let map = cache().lock().unwrap_or_else(|e| e.into_inner());
        let entry = map
            .get(&cursor)
            .expect("the shaped result is cached under its cursor");
        assert_eq!(
            entry.size,
            retained_size(&entry.body, entry.structured.as_ref()),
            "recorded size must cover the body and the stashed structuredContent"
        );
        assert!(entry.size >= 8_000, "recorded size was {}", entry.size);
    }

    #[test]
    fn tiny_objects_charge_the_reserved_btree_node() {
        let value = json!({"a": 1});
        assert!(
            value_heap_size(&value)
                >= 11 * (std::mem::size_of::<String>() + std::mem::size_of::<Value>())
        );
        let empty = json!({});
        assert_eq!(value_heap_size(&empty), 0);
    }

    #[test]
    fn small_json_members_charge_tree_allocations() {
        let structured =
            json!({"rows": (0..1000).map(|_| json!({"a": 1, "b": 2})).collect::<Vec<_>>()});
        let size = retained_size(&String::new(), Some(&structured));
        assert!(size > value_size(&structured) * 5);
        let mut store = SessionStore::new(CACHE_TTL, MAX_CACHE_ENTRIES);
        store.insert("first", cached_entry(MAX_CACHE_BYTES - size + 1));
        evict_to_fit(&mut store, size);
        assert!(store.is_empty(), "parsed allocations must trigger eviction");
    }

    #[test]
    fn retained_capacity_triggers_the_byte_cap() {
        let mut body = String::with_capacity(4096);
        body.push_str("short text");
        let size = retained_size(&body, None);
        assert!(
            size >= 4096,
            "retained capacity, not text length, must be charged"
        );
        let mut store = SessionStore::new(CACHE_TTL, MAX_CACHE_ENTRIES);
        store.insert("oldest", cached_entry(MAX_CACHE_BYTES - 4096));
        evict_to_fit(&mut store, size);
        assert!(
            store.is_empty(),
            "the retained buffer must trigger eviction"
        );
    }

    #[test]
    fn a_read_from_memory_extends_its_time_up_to_the_lifetime_cap() {
        let secs = Duration::from_secs;
        let mut store = SessionStore::new(secs(60), MAX_CACHE_ENTRIES);
        set_lifetimes_for_tests(Some((secs(60), secs(100))));
        // The clock is passed in: each read happens at an exact time after storing,
        // so no read depends on how fast the test runs.
        let item = cached_entry(1);
        let stored = item.first_stored;
        store.insert("paged", item);
        let read = |store: &mut SessionStore<Cached>, at| read_in_at(store, "paged", None, at);
        let first = matches!(read(&mut store, stored + secs(40)), Read::Found);
        // Past the TTL from storing, inside it from the read.
        let second = matches!(read(&mut store, stored + secs(80)), Read::Found);
        // Inside the TTL from the last read, past the lifetime cap.
        let third = matches!(read(&mut store, stored + secs(110)), Read::Refused);
        // Memory only: the shortened clocks must not reach the shared spill store.
        sweep_memory_at(&mut store, stored + secs(110));
        set_lifetimes_for_tests(None);
        assert!(first);
        assert!(second, "a read did not extend it");
        assert!(third, "a read extended it past the lifetime cap");
        assert!(
            store.is_empty(),
            "the sweep kept a result past its lifetime"
        );
    }

    #[test]
    fn sweep_releases_expired_results_without_a_fetch() {
        let mut store = SessionStore::new(Duration::ZERO, MAX_CACHE_ENTRIES);
        store.insert("expired", cached_entry(1024 * 1024));
        sweep(&mut store);
        assert!(store.is_empty());
        let mut live = SessionStore::new(CACHE_TTL, MAX_CACHE_ENTRIES);
        live.insert("live", cached_entry(1));
        sweep(&mut live);
        assert!(live.get("live").is_some());
    }

    #[test]
    fn shaping_preserves_is_error_on_oversized_failures() {
        // A failure big enough to shape is still a failure. Dropping `isError` here
        // would turn a downstream error into an apparent success for the model, and
        // the marker must survive too so the failure detail stays pageable.
        let mut r = json!({
            "content": [{ "type": "text", "text": "boom: ".to_string() + &"e".repeat(10_000) }],
            "isError": true
        });
        assert!(shape_result(&mut r, 2048, None));

        // The JSON value itself, so a dropped or nulled field fails rather than
        // silently reading as "not true".
        assert_eq!(
            r["isError"],
            json!(true),
            "shaped failure lost its isError flag: {}",
            r
        );
        let text = r["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Toolport shaped this result"));
        assert!(
            text.contains("\"cursor\":\"r"),
            "the failure must stay pageable"
        );
    }

    /// Text as Claude Code counts it: UTF-16 units across the text blocks.
    fn utf16_units(result: &Value) -> usize {
        result["content"]
            .as_array()
            .map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|block| block["text"].as_str())
                    .map(|text| text.encode_utf16().count())
                    .sum()
            })
            .unwrap_or(0)
    }

    #[test]
    fn text_of_any_escape_density_is_shaped_to_the_budget() {
        // Characters with every JSON encoding cost: plain, escaped as two bytes,
        // escaped as \uXXXX, multi-byte and astral.
        const PALETTE: [char; 8] = ['a', ' ', '\\', '"', '\n', '\u{1}', 'é', '😀'];
        let mut seed: u64 = 0x5eed_1234_abcd_0001;
        let mut next = move || {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (seed >> 33) as usize
        };
        for case in 0..60 {
            let budget = [2_000, 9_000, 30_000][case % 3];
            // Escape density from none to every character.
            let escaped_share = case % 11;
            let len = budget / 2 + next() % (budget * 3);
            let text: String = (0..len)
                .map(|_| {
                    if next() % 10 < escaped_share {
                        PALETTE[2 + next() % 4]
                    } else {
                        [PALETTE[0], PALETTE[1], PALETTE[6], PALETTE[7]][next() % 4]
                    }
                })
                .collect();
            let mut result = text_result(text.clone(), false);
            shape_result(&mut result, budget, None);
            assert!(
                utf16_units(&result) <= budget,
                "case {case}: {} units over {budget} (density {escaped_share})",
                utf16_units(&result)
            );
            assert!(serde_json::to_string(&result).unwrap().len() <= budget || text.is_empty());
            // The same text as a projection and as pages.
            let cursor = stash_payload(text.clone(), Some(json!({ "v": text })), None).unwrap();
            let projected = fetch_result_for_model(&cursor, 0, 0, None, Some("v"), budget);
            assert!(utf16_units(&projected) <= budget, "case {case} projection");
            let page = fetch_result_for_model(&cursor, 0, 0, None, None, budget);
            assert!(utf16_units(&page) <= budget, "case {case} page");
        }
    }

    #[test]
    fn a_result_is_left_whole_only_when_its_envelope_alone_cannot_fit() {
        // A small body under a _meta larger than the budget: no head size fits.
        let mut result = text_result("short".into(), false);
        result["_meta"] = json!({ "blob": "m".repeat(5_000) });
        let before = result.clone();
        assert!(!shape_result(&mut result, 2_000, None));
        assert_eq!(result, before);
        // A non-text block larger than the budget is never dropped, so it passes whole.
        let mut image = json!({ "content": [
            { "type": "image", "data": "i".repeat(5_000), "mimeType": "image/png" },
            { "type": "text", "text": "t".repeat(5_000) }
        ] });
        let before = image.clone();
        assert!(!shape_result(&mut image, 2_000, None));
        assert_eq!(image, before);
        // An envelope that outweighs the text but fits: the text is shaped around it.
        let mut heavy = text_result("t".repeat(1_200), false);
        heavy["_meta"] = json!({ "blob": "m".repeat(1_300) });
        assert!(shape_result(&mut heavy, 2_000, None));
        assert_eq!(heavy["_meta"]["blob"].as_str().unwrap().len(), 1_300);
        assert!(serde_json::to_string(&heavy).unwrap().len() <= 2_000);
    }

    #[test]
    fn a_small_image_does_not_exempt_large_text_from_shaping() {
        let text = "x".repeat(1_000_000);
        let mut result = json!({ "content": [
            { "type": "image", "data": "iVBORw0KGgo=", "mimeType": "image/png" },
            { "type": "text", "text": text }
        ] });
        assert!(shape_result(&mut result, 150_000, None));
        assert!(serde_json::to_string(&result).unwrap().len() <= 150_000);
        let blocks = result["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "image");
        assert_eq!(blocks[0]["data"], "iVBORw0KGgo=");
        let shown = blocks[1]["text"].as_str().unwrap();
        let cursor = shown
            .split("\"cursor\":\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap();
        let full = fetch_result(cursor, 0, usize::MAX, None, None);
        assert!(full["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with(&text));
    }

    #[test]
    fn mixed_results_are_shaped_in_order_within_the_budget() {
        let mut seed: u64 = 0x0dd5_eed5_0000_0042;
        let mut next = move || {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (seed >> 33) as usize
        };
        const PALETTE: [char; 6] = ['a', '\\', '"', '\n', '\u{1}', '😀'];
        for case in 0..80 {
            let budget = [3_000, 12_000, 40_000][case % 3];
            let mut blocks = Vec::new();
            let mut texts = Vec::new();
            let mut images = Vec::new();
            for slot in 0..(2 + next() % 5) {
                if next() % 3 == 0 {
                    let data = format!("img{case}-{slot}-{}", "A".repeat(next() % 300));
                    images.push(data.clone());
                    blocks.push(json!({ "type": "image", "data": data, "mimeType": "image/png" }));
                } else {
                    let len = next() % (budget * 2);
                    let text: String = (0..len).map(|_| PALETTE[next() % 6]).collect();
                    texts.push(text.clone());
                    blocks.push(json!({ "type": "text", "text": text }));
                }
            }
            let mut result = json!({ "content": blocks, "isError": false });
            let before = result.clone();
            let size = serde_json::to_string(&result).unwrap().len();
            let shaped = shape_result(&mut result, budget, None);
            if size <= budget || texts.iter().all(String::is_empty) {
                assert!(!shaped, "case {case}");
                assert_eq!(result, before, "case {case}");
                continue;
            }
            assert!(shaped, "case {case}: {size} bytes at {budget}");
            assert!(
                serde_json::to_string(&result).unwrap().len() <= budget,
                "case {case} over budget"
            );
            let out = result["content"].as_array().unwrap();
            // Every non-text block survives, in its original order.
            let kept: Vec<String> = out
                .iter()
                .filter(|b| b["type"] == "image")
                .map(|b| b["data"].as_str().unwrap().to_string())
                .collect();
            assert_eq!(kept, images, "case {case}");
            // Text blocks before an image stay before it: the shown text, read in
            // order, is a prefix of the original text joined in order.
            let joined = texts.join("\n");
            let mut shown = String::new();
            let mut cursor = None;
            for block in out.iter().filter(|b| b["type"] == "text") {
                let text = block["text"].as_str().unwrap();
                let (body, marker) = match text.find("\n\n[Toolport shaped this result") {
                    Some(at) => (&text[..at], Some(&text[at..])),
                    None => (text, None),
                };
                if !shown.is_empty() {
                    shown.push('\n');
                }
                shown.push_str(body);
                if let Some(marker) = marker {
                    cursor = marker
                        .split("\"cursor\":\"")
                        .nth(1)
                        .map(|rest| rest.split('"').next().unwrap().to_string());
                }
            }
            assert!(
                joined.starts_with(&shown),
                "case {case}: shown text is not a prefix"
            );
            let cursor = cursor.expect("a shaped mixed result carries a cursor");
            let full = fetch_result(&cursor, 0, usize::MAX, None, None);
            assert!(
                full["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .starts_with(&joined),
                "case {case}: the cursor does not hold the whole text"
            );
        }
    }

    #[test]
    fn results_pushed_out_of_memory_stay_fetchable_from_a_sealed_file() {
        let secret = "spill-plaintext-marker ".repeat(400);
        let mut first = text_result(secret.clone(), false);
        first["structuredContent"] = json!({ "rows": [secret.clone()] });
        assert!(shape_result(&mut first, 2_000, Some("spill-owner")));
        let text = first["content"][0]["text"].as_str().unwrap().to_string();
        let cursor = text
            .split("\"cursor\":\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap();
        // Push it out of memory with newer results.
        for _ in 0..(MAX_CACHE_ENTRIES + 4) {
            let mut r = big_text_result(5_000);
            shape_result(&mut r, 1_024, Some("spill-other"));
        }
        assert!(
            cache()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(cursor)
                .is_none(),
            "still in memory"
        );
        let path = spill::spilled_path(cursor).expect("moved to disk");
        let stored = std::fs::read_to_string(&path).unwrap();
        assert!(
            !stored.contains("spill-plaintext-marker"),
            "plaintext on disk"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        // Pages and projections read it back exactly, for its owner only.
        let page = fetch_result(cursor, 0, usize::MAX, Some("spill-owner"), None);
        assert!(page["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with(&secret));
        let rows = fetch_result(cursor, 0, 0, Some("spill-owner"), Some("rows.0"));
        assert_eq!(
            rows["content"][0]["text"],
            json!(serde_json::to_string(&secret).unwrap())
        );
        let foreign = fetch_result(cursor, 0, 0, Some("spill-other"), None);
        assert_eq!(foreign["isError"], true);
        spill::forget(cursor);
        assert!(!path.exists());
    }

    #[test]
    fn startup_removes_spills_of_exited_gateways_and_expired_files() {
        let root = std::env::temp_dir()
            .join("toolport-result-spill-cases")
            .join("startup");
        let dead = root.join("4294967294");
        let named = root.join("not-a-pid");
        let live = root.join(std::process::id().to_string());
        for dir in [&dead, &named, &live] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join("r1.sealed"), "x").unwrap();
        }
        spill::remove_stale_for_tests(&root);
        assert!(!dead.exists() && !named.exists());
        // This process's pid from an earlier run is not ours either.
        assert!(!live.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn startup_keeps_the_spills_of_a_gateway_that_holds_its_lock() {
        let root = std::env::temp_dir()
            .join("toolport-result-spill-cases")
            .join("held-lock");
        let _ = std::fs::remove_dir_all(&root);
        // A pid that no longer exists here (or, on Windows, is not checked), with a file
        // older than the TTL: only the lock says the gateway is still serving it.
        let held = root.join("4294967293");
        let released = root.join("4294967292");
        let mut locks = Vec::new();
        for dir in [&held, &released] {
            std::fs::create_dir_all(dir).unwrap();
            let file = std::fs::File::create(dir.join("r1.sealed")).unwrap();
            file.set_modified(std::time::SystemTime::now() - 2 * CACHE_TTL)
                .unwrap();
            let lock = std::fs::File::create(dir.join("owner.lock")).unwrap();
            locks.push(lock);
        }
        locks[0].try_lock().unwrap();
        assert_eq!(spill::LOCK_FILE, "owner.lock");
        drop(locks.pop());
        spill::remove_stale_for_tests(&root);
        assert!(
            held.join("r1.sealed").exists(),
            "a live gateway's spill was reaped"
        );
        assert!(!released.exists());
        drop(locks);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pages_of_dense_escapes_fit_the_budget_and_cover_every_character_once() {
        let body: String = "\u{1}\"\\a\u{7}é😀\n".repeat(4_000);
        for budget in [1_000, 4_000, 20_000] {
            let cursor = stash_payload(body.clone(), None, None).unwrap();
            let mut offset = 0;
            let mut read = String::new();
            loop {
                let page = fetch_result_for_model(&cursor, offset, 0, None, None, budget);
                let bytes = serde_json::to_string(&page).unwrap().len();
                assert!(
                    bytes <= budget,
                    "{bytes} bytes over {budget} at offset {offset}"
                );
                let text = page["content"][0]["text"].as_str().unwrap();
                let at = text.rfind("\n\n[Toolport: ").unwrap();
                read.push_str(&text[..at]);
                match text[at..].split("offset=").nth(1) {
                    Some(rest) => {
                        let next: usize = rest
                            .chars()
                            .take_while(char::is_ascii_digit)
                            .collect::<String>()
                            .parse()
                            .unwrap();
                        assert!(next > offset);
                        offset = next;
                    }
                    None => break,
                }
            }
            assert_eq!(read, body, "budget {budget}");
        }
    }

    #[test]
    fn error_replies_echo_only_a_preview_of_their_arguments() {
        let huge = "x".repeat(100_001);
        let unknown = fetch_result_for_model(&huge, 0, 0, None, None, 100_000);
        assert!(utf16_units(&unknown) < 400, "{}", utf16_units(&unknown));
        let cursor = stash_payload("body".into(), Some(json!({ "a": 1 })), None).unwrap();
        let missing = fetch_result_for_model(&cursor, 0, 0, None, Some(&huge), 100_000);
        assert!(utf16_units(&missing) < 400, "{}", utf16_units(&missing));
        assert!(missing["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("not found"));
    }

    #[test]
    fn model_pages_and_projections_fit_the_budget() {
        let body = "a😀".repeat(1_000);
        let rows = json!({ "rows": vec!["r".repeat(100); 100] });
        let cursor = stash_payload(body.clone(), Some(rows), None).unwrap();
        let text = |v: &Value| v["content"][0]["text"].as_str().unwrap().to_string();
        let budget = 1_000;
        let mut offset = 0;
        let mut paged = String::new();
        loop {
            let page = text(&fetch_result_for_model(
                &cursor, offset, 0, None, None, budget,
            ));
            assert!(page.len() <= budget, "{} bytes", page.len());
            let (slice, footer) = page.split_once("\n\n[Toolport: ").unwrap();
            paged.push_str(slice);
            offset += slice.chars().count();
            if footer.starts_with("end of result") {
                break;
            }
        }
        assert_eq!(paged, body);
        // An explicit len is still bounded for the model.
        let long = text(&fetch_result_for_model(
            &cursor,
            0,
            usize::MAX,
            None,
            None,
            budget,
        ));
        assert!(long.len() <= budget);
        // The footer says the budget cut the page, and how to continue.
        assert!(long.contains("stopped at the result budget"), "{long}");
        assert!(
            long.contains("toolport_fetch_result with offset="),
            "{long}"
        );
        // A page cut by its own len says nothing about the budget.
        let short = text(&fetch_result_for_model(&cursor, 0, 10, None, None, budget));
        assert!(!short.contains("result budget"), "{short}");
        // Budget 0 never shapes: the page is the rest of the body, never empty.
        let whole = text(&fetch_result_for_model(&cursor, 0, 0, None, None, 0));
        assert!(whole.starts_with(&body), "{}", &whole[..40]);
        // A projection over the budget comes back shaped, with a cursor to page it.
        let projected = fetch_result_for_model(&cursor, 0, 0, None, Some("rows"), budget);
        assert!(serde_json::to_string(&projected).unwrap().len() <= budget);
        assert!(text(&projected).contains("Toolport shaped this result"));
        // A model must not take the fragment for complete JSON.
        assert!(
            text(&projected).contains("not complete JSON"),
            "{}",
            text(&projected)
        );
        // A small projection is unchanged.
        let small = fetch_result_for_model(&cursor, 0, 0, None, Some("rows.0"), budget);
        assert_eq!(text(&small), format!("\"{}\"", "r".repeat(100)));
        // Code mode keeps reading whole slices.
        let sandbox = text(&fetch_result(&cursor, 0, usize::MAX, None, None));
        assert!(sandbox.starts_with(&body));
    }

    #[test]
    fn fetch_result_projection_returns_nested_field() {
        let mut r = json!({
            "content": [{
                "type": "text",
                "text": "x".repeat(4096)
            }],
            "structuredContent": {
                "data": {
                    "users": [
                        {
                            "profile": {
                                "name": "Alice",
                                "age": 30
                            }
                        },
                        {
                            "profile": {
                                "name": "Bob",
                                "age": 40
                            }
                        }
                    ]
                }
            },
            "isError": false
        });

        // Force shaping so the result is cached.
        assert!(shape_result(&mut r, 2048, None));

        let text = r["content"][0]["text"].as_str().unwrap();
        let cursor = text
            .split("\"cursor\":\"")
            .nth(1)
            .and_then(|s| s.split('"').next())
            .unwrap()
            .to_string();

        let projected = fetch_result(&cursor, 0, 0, None, Some("data.users.1.profile.age"));

        assert!(!projected["isError"].as_bool().unwrap());

        let text = projected["content"][0]["text"].as_str().unwrap();
        assert_eq!(text, "40");
    }

    /// R5-1: a discovery page fetch whose visibility check takes another lock (a
    /// script's live check reads the registry) must not hold the cache meanwhile,
    /// or it deadlocks against a registry publish, which holds the registry and
    /// calls [`forget_discovery`]. Barriers force the interleaving; a timeout
    /// reports the deadlock instead of hanging the run.
    #[test]
    fn page_fetch_visibility_check_runs_without_the_cache_lock() {
        use std::sync::{mpsc, Arc, Barrier, Mutex};
        let realm = 0x5e_f0_0d;
        let mut result = big_text_result(20_000);
        assert!(shape_discovery_result(
            &mut result,
            2048,
            Some("r5-owner"),
            vec!["t__x".to_string()],
            realm,
        ));
        let text = result["content"][0]["text"].as_str().unwrap();
        let cursor = text
            .split("\"cursor\":\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap()
            .to_string();
        // Stands in for `host.registry`.
        let registry = Arc::new(Mutex::new(()));
        let in_check = Arc::new(Barrier::new(2));
        let publishing = Arc::new(Barrier::new(2));
        let (done, finished) = mpsc::channel();
        let fetcher = {
            let (registry, in_check, publishing, done) = (
                Arc::clone(&registry),
                Arc::clone(&in_check),
                Arc::clone(&publishing),
                done.clone(),
            );
            std::thread::spawn(move || {
                let page = fetch_result_visible(&cursor, 0, 100, Some("r5-owner"), None, &|_| {
                    in_check.wait();
                    // The publisher holds the registry now.
                    publishing.wait();
                    let _registry = registry.lock().unwrap();
                    true
                });
                done.send(("fetch", page)).unwrap();
            })
        };
        let publisher = std::thread::spawn(move || {
            in_check.wait();
            let _registry = registry.lock().unwrap();
            publishing.wait();
            let dropped = forget_discovery(realm, |_| true);
            done.send(("publish", json!(dropped))).unwrap();
        });
        let mut seen = Vec::new();
        for _ in 0..2 {
            let (who, value) = finished
                .recv_timeout(std::time::Duration::from_secs(20))
                .unwrap_or_else(|_| {
                    panic!("deadlock: page fetch and registry publish, seen {seen:?}")
                });
            seen.push((who, value));
        }
        fetcher.join().unwrap();
        publisher.join().unwrap();
        let publish = seen.iter().find(|(who, _)| *who == "publish").unwrap();
        assert_eq!(publish.1, json!(1), "{seen:?}");
        // The publish dropped the entry while the check ran: the fetch reads it
        // again afterwards and answers as for any expired cursor.
        let fetch = seen.iter().find(|(who, _)| *who == "fetch").unwrap();
        let fetched = fetch.1["content"][0]["text"].as_str().unwrap();
        assert!(fetched.contains("unknown or expired"), "{fetched}");
        assert_eq!(fetch.1["isError"], true);
    }
}
