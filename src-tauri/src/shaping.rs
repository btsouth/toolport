//! Result-shaping: keep oversized tool results from blowing the model's context
//! within a byte budget. Retained bodies can be paged with `toolport_fetch_result`
//! until expiry or eviction. Very large entries retain only text when that fits;
//! structured projections are then unavailable. If even text exceeds the cache
//! cap, the truncated reply asks for a narrower call and offers no cursor.
//!
//! This is the "other half" of the token story: lazy discovery trims tool
//! DEFINITION bloat; this trims tool RESULT bloat (a 10k-row response that would
//! otherwise sit in context). The gateway is the one place that can impose it
//! across every server, including legacy APIs with no native pagination.

use crate::session_store::SessionStore;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// Results whose serialized size exceeds this get shaped. Generous on purpose, so
/// only genuinely large results are touched. Override with `TOOLPORT_RESULT_BUDGET`
/// (legacy: `CONDUIT_RESULT_BUDGET`)
/// (bytes); set it to 0 to disable shaping entirely.
pub const DEFAULT_BUDGET_BYTES: usize = 48 * 1024;

/// How long a cached full result stays fetchable.
const CACHE_TTL: Duration = Duration::from_secs(15 * 60);

/// Cap on the number of cached shaped results. A burst of large tool calls would
/// otherwise grow process memory without bound between TTL sweeps. Oldest
/// entries (by insertion time) are evicted first.
const MAX_CACHE_ENTRIES: usize = 64;

/// Cap on estimated retained bytes, including parsed JSON allocations. Evict
/// oldest until a new body fits. Over-cap entries fall back to text alone; if
/// that still exceeds the cap, no cursor is offered.
const MAX_CACHE_BYTES: usize = 64 * 1024 * 1024;

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
}

/// The bounded stash of shaped results, keyed by cursor. P1.2: the lifetime rules
/// live on a [`SessionStore`] rather than a bare `HashMap` -- entries expire
/// `CACHE_TTL` after they were stashed, and an arrival past `MAX_CACHE_ENTRIES` or
/// `MAX_CACHE_BYTES` evicts the oldest.
fn cache() -> &'static Mutex<SessionStore<Cached>> {
    static C: OnceLock<Mutex<SessionStore<Cached>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(SessionStore::new(CACHE_TTL, MAX_CACHE_ENTRIES)))
}

fn next_cursor() -> String {
    static N: AtomicU64 = AtomicU64::new(1);
    format!("r{}", N.fetch_add(1, Ordering::Relaxed))
}

static TRIM_PENDING: AtomicBool = AtomicBool::new(false);

fn sweep(store: &mut SessionStore<Cached>) {
    let before = store.weight(|c| c.size);
    if store.reap_expired() > 0 && before >= 1024 * 1024 {
        TRIM_PENDING.store(true, Ordering::Relaxed);
    }
}

/// Called by every gateway runtime's background maintenance tick. Reads never
/// extend the insertion-based TTL. Trim outside the cache lock and off the request path.
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

/// Bound memory: evict oldest until the entry count and total bytes leave room for
/// a `new_entry_size`-byte result. Each entry's `size` is precomputed, so this sum
/// is O(n) adds, not O(n) JSON re-serializations, on every iteration.
fn evict_to_fit(store: &mut SessionStore<Cached>, new_entry_size: usize) {
    while !store.is_empty()
        && (store.len() >= MAX_CACHE_ENTRIES
            || store.weight(|c| c.size) + new_entry_size > MAX_CACHE_BYTES)
    {
        if !store.remove_oldest() {
            break;
        }
        TRIM_PENDING.store(true, Ordering::Relaxed);
    }
}

fn retain_in(store: &mut SessionStore<Cached>, cursor: &str, mut entry: Cached) -> bool {
    if entry.size > MAX_CACHE_BYTES && entry.structured.is_some() {
        entry.structured = None;
        entry.projections_unavailable = true;
        entry.size = retained_size(&entry.body, None);
    }
    if entry.size > MAX_CACHE_BYTES {
        return false;
    }
    sweep(store);
    evict_to_fit(store, entry.size);
    store.insert(cursor, entry);
    true
}

fn retain(cursor: &str, entry: Cached) -> bool {
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
    let mut source_bytes = out.len();
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
        } else if let Some(array) = current.as_array() {
            let index = segment.parse::<usize>().ok()?;
            current = array.get(index)?;
        } else {
            return None;
        }
    }

    Some(current)
}

/// The longest char-boundary prefix of `s` whose UTF-8 length is at most
/// `max_bytes`. Truncating by char COUNT alone would let a multi-byte body (CJK,
/// emoji) emit several times the byte budget; this honors the byte budget exactly
/// while never splitting a code point.
fn head_within_bytes(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = 0;
    for (i, ch) in s.char_indices() {
        let next = i + ch.len_utf8();
        if next > max_bytes {
            break;
        }
        end = next;
    }
    &s[..end]
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

/// If `result` serializes to more than `budget` bytes, cache its full body, replace
/// it with a truncated head and retention notice, and return `true` (shaped).
/// A `budget` of 0 disables shaping. The full body stays fetchable via
/// [`fetch_result`] while retained within the memory cap and insertion TTL.
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
    if budget == 0 {
        return false;
    }
    let server = result
        .pointer("/_meta/app.toolport~1provenance/server")
        .and_then(Value::as_str)
        .map(str::to_string);
    // Callers add the notice after caching, so reserve its exact serialized cost.
    let budget = if let Some(server) = &server {
        let mut notice = serde_json::json!({"content":[]});
        crate::integrity::label_untrusted_result(server, &mut notice);
        let reserve = value_size(&notice["content"][0]) + 1;
        budget.saturating_sub(reserve)
    } else {
        budget
    };
    if budget == 0 {
        return false;
    }
    let size = serde_json::to_string(result).map(|s| s.len()).unwrap_or(0);
    if size <= budget {
        return false;
    }

    // Only shape text-representable results. Pass through non-text blocks and
    // results dominated by envelope fields that the text body cannot capture.
    let (body, source_bytes) = extract_body(result);
    if !is_text_representable(result) || source_bytes < size / 2 {
        return false;
    }
    let structured = result.get("structuredContent").cloned();

    let total = body.chars().count();
    // Envelope fields carried across (see below) are part of the shaped result, so
    // they have to come out of the budget too. Without this, preserving a large
    // `_meta` could push a "shaped" result back over the limit and `true` would no
    // longer mean "fits" (#511 review).
    let preserved_bytes: usize = result
        .as_object()
        .map(|obj| {
            obj.iter()
                .filter(|(key, _)| {
                    !matches!(key.as_str(), "content" | "structuredContent" | "isError")
                })
                .map(|(key, value)| key.len() + value_size(value) + 4)
                .sum()
        })
        .unwrap_or(0);
    // If the preserved envelope alone meets the budget, no head size makes the
    // shaped result fit. Leave it unshaped, as with the other cases where shaping
    // cannot honour its contract.
    if preserved_bytes >= budget {
        return false;
    }
    // Reserve room for the marker, then show as much of the body head as fits the
    // BYTE budget (not a char count, or multi-byte text would blow past it).
    //
    // No lower floor here. A `.max(256)` floor is what let a large envelope push
    // a "shaped" result back over the budget while still returning `true`: the
    // floor won whenever `budget - reserve - preserved` fell below it. The final
    // fit-check below is what actually enforces the contract; this is only the
    // starting estimate.
    let min_head_bytes = min_head_bytes.min(body.len());
    let mut head_byte_limit = budget
        .saturating_sub(512 + preserved_bytes)
        .max(min_head_bytes);
    let head = head_within_bytes(&body, head_byte_limit).to_string();
    let is_error = result
        .get("isError")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let cursor = next_cursor();
    let new_entry_size = retained_size(&body, structured.as_ref());

    let body_bytes = body.len();
    let retained = retain(
        &cursor,
        Cached {
            server: result
                .pointer("/_meta/app.toolport~1provenance/server")
                .and_then(Value::as_str)
                .map(str::to_string),
            body,
            structured,
            projections_unavailable: false,
            size: new_entry_size,
            owner: owner.map(str::to_string),
        },
    );

    // Build the shaped result for a given head, then measure it. The marker's own
    // length varies with the head length it reports, and the preserved envelope is
    // whatever the server sent, so the only reliable way to honour the budget is
    // to measure the finished value rather than predict it.
    let build = |head: &str| -> Value {
        let head_chars = head.chars().count();
        let marker = if retained {
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
        } else {
            not_retained_notice(body_bytes)
        };
        // Shaping deliberately rewrites `content` and stashes `structuredContent`
        // in the cache when admitted. Every other top-level
        // field belongs to the downstream server, not to us: `_meta`, and whatever
        // a future revision or extension adds. Carry them across so shaping stays a
        // truncation of the body rather than a rewrite of the envelope (SOU-444).
        let mut shaped = text_result(format!("{head}{marker}"), is_error);
        if let (Some(src), Some(dst)) = (result.as_object(), shaped.as_object_mut()) {
            for (key, value) in src {
                if matches!(key.as_str(), "content" | "structuredContent" | "isError") {
                    continue;
                }
                dst.insert(key.clone(), value.clone());
            }
        }
        if let Some(server) = &server {
            crate::integrity::label_untrusted_result_with_notice(server, &mut shaped, false);
        }
        shaped
    };

    let mut head = head;
    let mut shaped = build(&head);
    // Shrink until it fits. Each pass subtracts the exact overage, so this
    // converges immediately in practice; the bound is a guard, not a search.
    for _ in 0..4 {
        let shaped_size = serde_json::to_string(&shaped).map(|s| s.len()).unwrap_or(0);
        if shaped_size <= budget || head.is_empty() {
            break;
        }
        let overage = shaped_size - budget;
        head_byte_limit = head_byte_limit
            .saturating_sub(overage.max(1))
            .max(min_head_bytes);
        head = head_within_bytes(&head, head_byte_limit).to_string();
        shaped = build(&head);
    }

    // An empty head that still overflows means the marker and envelope alone
    // exceed the budget, so no truncation can honour the contract. Returning
    // `true` there would be the same false claim the head floor used to make.
    // Remove any admitted entry when we decline to shape.
    if serde_json::to_string(&shaped).map(|s| s.len()).unwrap_or(0) > budget {
        cache()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&cursor);
        return false;
    }

    *result = shaped;
    if size >= 1024 * 1024 {
        TRIM_PENDING.store(true, Ordering::Relaxed);
    }
    true
}

fn not_retained_notice(body_bytes: usize) -> String {
    format!(
        "\n\n[Toolport: full result ({:.1} MB) was too large to keep for paging. Re-run the tool with narrower arguments.]",
        body_bytes as f64 / 1_000_000.0
    )
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
        },
    );
    retained.then_some(cursor)
}

/// Return the next slice of a cached shaped result, by cursor + character offset.
/// `len` of 0 means "use the current budget".
pub fn fetch_result(
    cursor: &str,
    offset: usize,
    len: usize,
    requester: Option<&str>,
    projection: Option<&str>,
) -> Value {
    let mut store = cache().lock().unwrap_or_else(|e| e.into_inner());
    sweep(&mut store);
    // Scope: a cached result is readable only by the client that stashed it. Owner
    // must be a stable principal (e.g. client:{id}), never a shared display label
    // (SOU-324). A mismatch returns the SAME "unknown or expired" answer as a
    // missing cursor, so a scoped client can't probe which cursors exist. The
    // stash is process-global; without this check one HTTP client could read
    // another's result by guessing the sequential `r{n}` cursor.
    let c = match store.get(cursor) {
        Some(c) if c.owner.as_deref() == requester => c,
        _ => {
            return text_result(
                format!(
                    "[Toolport: cursor \"{cursor}\" is unknown or expired. Re-run the original \
                     tool call to get a fresh result.]"
                ),
                true,
            );
        }
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
                    format!("[Toolport: projection \"{path}\" not found.]"),
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
    let len = if len == 0 {
        let (budget, warning) = budget();
        if let Some(msg) = warning {
            eprintln!("{msg}");
        }
        budget
    } else {
        len
    };
    // saturating_add: a client-supplied `len` near usize::MAX must not overflow
    // `offset + len`. On debug that panics; on release it wraps to `end < offset`,
    // and the byte-mapping below then slices `body[start_byte..end_byte]` with
    // start > end - a panic that, on the stdio transport (no catch_unwind), takes
    // down the whole gateway. Saturating clamps `end` to `total` instead.
    let end = offset.saturating_add(len).min(total);
    // Map the character window [offset, end) to byte offsets in a single pass, so a
    // page read never allocates a Vec<char> of the whole (possibly multi-MB) body.
    // `end == total` leaves end_byte at the body's byte length (the loop never yields
    // char index `total`), so the last page runs cleanly to the end.
    let mut start_byte = c.body.len();
    let mut end_byte = c.body.len();
    for (char_idx, (byte_idx, _)) in c.body.char_indices().enumerate() {
        if char_idx == offset {
            start_byte = byte_idx;
        }
        if char_idx == end {
            end_byte = byte_idx;
            break;
        }
    }
    let slice = c.body[start_byte..end_byte].to_string();
    let remaining = total - end;
    let footer = if remaining > 0 {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn big_text_result(n: usize) -> Value {
        json!({ "content": [{ "type": "text", "text": "x".repeat(n) }], "isError": false })
    }

    #[test]
    fn fetched_pages_and_projections_keep_the_real_server() {
        let mut result = serde_json::json!({"content":[{"type":"text", "text":"x".repeat(10_000)}], "structuredContent":{"value":7}});
        crate::integrity::label_untrusted_result_with_notice("github", &mut result, false);
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
            assert!(fetched["content"][1]["text"]
                .as_str()
                .unwrap()
                .contains("from github;"));
        }
        crate::integrity::label_untrusted_result("github", &mut result);
        assert!(
            value_size(&result) <= 2048,
            "notice must fit within the original budget"
        );
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
            crate::integrity::label_untrusted_result_with_notice("github", &mut result, false);
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

    #[test]
    fn cache_rejects_over_cap_entries_without_evicting_live_results() {
        let mut store = SessionStore::new(CACHE_TTL, MAX_CACHE_ENTRIES);
        store.insert("live", cached_entry(1024));
        assert!(!retain_in(
            &mut store,
            "oversize",
            cached_entry(MAX_CACHE_BYTES + 1)
        ));
        assert!(store.get("oversize").is_none());
        assert!(store.get("live").is_some());
        assert!(retain_in(
            &mut store,
            "at-cap",
            cached_entry(MAX_CACHE_BYTES)
        ));
        assert!(store.get("live").is_none());
        assert_eq!(store.weight(|c| c.size), MAX_CACHE_BYTES);
    }

    #[test]
    fn over_cap_payload_reports_no_cursor() {
        assert!(stash_payload(String::with_capacity(MAX_CACHE_BYTES), None, None).is_none());
    }

    #[test]
    fn over_cap_shaping_reports_unavailable_paging_without_a_cursor() {
        let mut result = json!({
            "content": [{"type": "text", "text": "x".repeat(MAX_CACHE_BYTES)}],
            "isError": false,
            "_meta": {"fixture": true},
        });
        assert!(shape_result(&mut result, 2048, None));
        assert!(value_size(&result) <= 2048);
        assert_eq!(result["isError"], false);
        assert_eq!(result["_meta"]["fixture"], true);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with("xxx"));
        assert!(text.ends_with("[Toolport: full result (67.1 MB) was too large to keep for paging. Re-run the tool with narrower arguments.]"));
        assert!(!text.contains("cursor"));
        assert!(!text.contains("toolport_fetch_result"));
        assert!(not_retained_notice(MAX_CACHE_BYTES).len() < 128);
    }

    #[test]
    fn over_cap_tree_retains_text_pages_but_refuses_projections() {
        let structured =
            json!({"rows": (0..100_000).map(|i| json!({"id": i, "ok": true})).collect::<Vec<_>>()});
        let text = structured.to_string();
        assert!(retained_size(&text, Some(&structured)) > MAX_CACHE_BYTES);
        assert!(retained_size(&text, None) < MAX_CACHE_BYTES);
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
        assert_eq!(projection, text_result("[Toolport: projections aren't available for this very large result. Page the text or narrow the call.]".into(), true));
        assert_eq!(
            fetch_result(&cursor, 0, 100, Some("bob"), Some("rows.0"))["isError"],
            true
        );
        let store = cache().lock().unwrap_or_else(|e| e.into_inner());
        let entry = store.get(&cursor).unwrap();
        assert!(entry.structured.is_none());
        assert!(entry.size <= MAX_CACHE_BYTES);
    }

    #[test]
    fn normal_first_reply_and_pages_preserve_wire_bytes() {
        let mut result = big_text_result(10_000);
        assert!(shape_result(&mut result, 2048, None));
        let cursor = cursor_of(&result);
        let expected = text_result(format!("{}\n\n[Toolport shaped this result: ~9 KB exceeds 2 KB budget. Showing first 1536 of 10000 characters. Continue with toolport_fetch_result {{\"cursor\":\"{}\",\"offset\":1536}}. If the cursor expired, re-run the original tool call.]", "x".repeat(1536), cursor), false);
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
}
