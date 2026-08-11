//! Result-shaping: keep oversized tool results from blowing the model's context
//! WITHOUT losing data. When a downstream tool returns a result larger than the
//! byte budget, the full body is cached in-process and the model gets a truncated
//! head plus a Toolport-stamped marker carrying a cursor. `toolport_fetch_result`
//! pages through the cached full result. Lossless: nothing is dropped, only
//! deferred, and the full data stays retrievable.
//!
//! This is the "other half" of the token story: lazy discovery trims tool
//! DEFINITION bloat; this trims tool RESULT bloat (a 10k-row response that would
//! otherwise sit in context). The gateway is the one place that can impose it
//! across every server, including legacy APIs with no native pagination.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Results whose serialized size exceeds this get shaped. Generous on purpose, so
/// only genuinely large results are touched. Override with `TOOLPORT_RESULT_BUDGET`
/// (legacy: `CONDUIT_RESULT_BUDGET`)
/// (bytes); set it to 0 to disable shaping entirely.
pub const DEFAULT_BUDGET_BYTES: usize = 48 * 1024;

/// How long a cached full result stays fetchable.
const CACHE_TTL: Duration = Duration::from_secs(15 * 60);

/// Cap on the number of cached shaped results. A burst of large tool calls would
/// otherwise grow process memory without bound between lazy TTL sweeps. Oldest
/// entries (by insertion time) are evicted first.
const MAX_CACHE_ENTRIES: usize = 64;

/// Cap on total cached body bytes. Evict oldest until a new body fits, or the
/// cache is empty (then one over-cap body is kept rather than dropping the result
/// the caller just produced).
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
    body: String,
    structured: Option<Value>,
    /// The entry's total serialized size (`body` + structured JSON), computed once at
    /// insert. The eviction loop sums this across entries on every oversized call, so
    /// caching it avoids re-serializing every structured payload on each iteration.
    size: usize,
    at: Instant,
    /// The client the result belongs to (a registered HTTP client's label), or None
    /// for the single-tenant stdio process. Only this client may fetch it back.
    owner: Option<String>,
}

fn cache() -> &'static Mutex<HashMap<String, Cached>> {
    static C: OnceLock<Mutex<HashMap<String, Cached>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

fn next_cursor() -> String {
    static N: AtomicU64 = AtomicU64::new(1);
    format!("r{}", N.fetch_add(1, Ordering::Relaxed))
}

fn sweep(map: &mut HashMap<String, Cached>) {
    map.retain(|_, c| c.at.elapsed() < CACHE_TTL);
}

/// Bound memory: evict oldest until the entry count and total bytes leave room for
/// a `new_entry_size`-byte result (or the cache empties, keeping one over-cap
/// result). Each entry's `size` is precomputed, so this sum is O(n) adds, not O(n)
/// JSON re-serializations, on every iteration.
fn evict_to_fit(map: &mut HashMap<String, Cached>, new_entry_size: usize) {
    while !map.is_empty()
        && (map.len() >= MAX_CACHE_ENTRIES
            || map.values().map(|c| c.size).sum::<usize>() + new_entry_size > MAX_CACHE_BYTES)
    {
        let Some(oldest) = map.iter().min_by_key(|(_, c)| c.at).map(|(k, _)| k.clone()) else {
            break;
        };
        map.remove(&oldest);
    }
}

/// Concatenate the model-facing text of an MCP tool result's content blocks, then
/// fold in `structuredContent` so nothing is lost when the structured payload is
/// the bloat.
fn extract_body(result: &Value) -> String {
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
    if let Some(sc) = result.get("structuredContent") {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&serde_json::to_string(sc).unwrap_or_default());
    }
    out
}

fn value_size(value: &Value) -> usize { 
    serde_json::to_string(value) .map(|s| s.len()) .unwrap_or(0) 
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
/// it with a truncated head + a stamped cursor marker, and return `true` (shaped).
/// A `budget` of 0 disables shaping. Lossless: the full body stays fetchable via
/// [`fetch_result`].
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
    let size = serde_json::to_string(result).map(|s| s.len()).unwrap_or(0);
    if size <= budget {
        return false;
    }

    // Only shape what we can represent losslessly as a text head. If the result has
    // non-text blocks, or its size is dominated by non-body envelope (the text
    // projection captures under half the bytes), shaping would drop data and its
    // "nothing was lost" claim would be false. Pass those through untouched.
    let body = extract_body(result);
    if !is_text_representable(result) || body.len() < size / 2 {
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
                .filter(|(key, _)| !matches!(key.as_str(), "content" | "structuredContent" | "isError"))
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
    let new_entry_size = body.len() + structured.as_ref().map(value_size).unwrap_or(0);

    // Build the shaped result for a given head, then measure it. The marker's own
    // length varies with the head length it reports, and the preserved envelope is
    // whatever the server sent, so the only reliable way to honour the budget is
    // to measure the finished value rather than predict it.
    let build = |head: &str| -> Value {
        let head_chars = head.chars().count();
        let marker = format!(
            "\n\n[Toolport shaped this result: it was ~{} KB, larger than the {} KB context \
             budget. Showing the first {} of {} characters. The rest is held temporarily, call \
             toolport_fetch_result with {{\"cursor\":\"{}\",\"offset\":{}}} to read it. If that \
             later reports the cursor expired, just re-run this tool call for a fresh result.]",
            size / 1024,
            budget / 1024,
            head_chars,
            total,
            cursor,
            head_chars
        );
        // Shaping deliberately rewrites `content` and stashes `structuredContent`
        // in the cache (both retrievable via the cursor). Every other top-level
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
        head = head_within_bytes(&body, head_byte_limit).to_string();
        shaped = build(&head);
    }

    // An empty head that still overflows means the marker and envelope alone
    // exceed the budget, so no truncation can honour the contract. Returning
    // `true` there would be the same false claim the head floor used to make.
    // Bail BEFORE caching, so a result we decline to shape leaves no orphaned
    // cursor entry behind.
    if serde_json::to_string(&shaped).map(|s| s.len()).unwrap_or(0) > budget {
        return false;
    }

    // Only now stash the full body: the cursor in the marker above is live from
    // here on.
    {
        let mut map = cache().lock().unwrap_or_else(|e| e.into_inner());
        sweep(&mut map);

        evict_to_fit(&mut map, new_entry_size);

        map.insert(
            cursor.clone(),
            Cached {
                body,
                structured,
                size: new_entry_size,
                at: Instant::now(),
                owner: owner.map(str::to_string),
            },
        );
    }

    *result = shaped;
    true
}

/// Return the next slice of a cached shaped result, by cursor + character offset.
/// `len` of 0 means "use the current budget".
pub fn fetch_result(cursor: &str, offset: usize, len: usize, requester: Option<&str>, projection: Option<&str>,) -> Value {
    let mut map = cache().lock().unwrap_or_else(|e| e.into_inner());
    sweep(&mut map);
    // Scope: a cached result is readable only by the client that stashed it. Owner
    // must be a stable principal (e.g. client:{id}), never a shared display label
    // (SOU-324). A mismatch returns the SAME "unknown or expired" answer as a
    // missing cursor, so a scoped client can't probe which cursors exist. The
    // stash is process-global; without this check one HTTP client could read
    // another's result by guessing the sequential `r{n}` cursor.
    let c = match map.get(cursor) {
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
                "[Toolport: this cached result has no structuredContent.]".to_string(),
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

    return text_result(
        serde_json::to_string(value).unwrap_or_default(),
        false,
    );
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
    text_result(format!("{slice}{footer}"), false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn big_text_result(n: usize) -> Value {
        json!({ "content": [{ "type": "text", "text": "x".repeat(n) }], "isError": false })
    }

    #[test]
    fn under_budget_is_untouched() {
        let mut r = big_text_result(100);
        assert!(!shape_result(&mut r, 1024, None));
        assert_eq!(r["content"][0]["text"].as_str().unwrap().len(), 100);
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
            fetch_result(&cursor, 0, 100, Some("mallory"),None)["isError"].as_bool(),
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
        assert!(kept.get("io.modelcontextprotocol/protocolVersion").is_none());
        assert!(kept.get("io.modelcontextprotocol/clientInfo").is_none());
        assert!(kept.get("io.modelcontextprotocol/clientCapabilities").is_none());
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
        assert!(params["_meta"].get("io.modelcontextprotocol/clientInfo").is_none());

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
        for meta_len in [0, 500, 1_000, 2_000, 3_000, 3_400, 3_600, 3_800, 4_000, 4_100] {
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
        assert_eq!(r["_meta"]["com.example/ctx"].as_str().map(str::len), Some(2_000));
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

        assert_eq!(r["_meta"]["io.modelcontextprotocol/serverInfo"]["name"], "srv");
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

    // A cache entry with a given recorded `size` and age, without allocating a body
    // of that size: the eviction loop reads `Cached.size`, never the body itself, so
    // multi-megabyte entries cost a few bytes here. `secs_ago` fixes the insertion
    // order deterministically rather than relying on clock resolution between calls.
    fn cached_entry(size: usize, secs_ago: u64) -> Cached {
        Cached {
            body: String::new(),
            structured: None,
            size,
            at: Instant::now() - Duration::from_secs(secs_ago),
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
        let mut map: HashMap<String, Cached> = HashMap::new();
        map.insert("oldest".to_string(), cached_entry(HUGE, 60));
        map.insert("middle".to_string(), cached_entry(HUGE, 40));
        map.insert("newest".to_string(), cached_entry(HUGE, 20));
        assert!(map.len() < MAX_CACHE_ENTRIES, "must exercise the byte cap, not the entry cap");

        let new_entry_size = 1024 * 1024;
        evict_to_fit(&mut map, new_entry_size);
        map.insert("incoming".to_string(), cached_entry(new_entry_size, 0));

        let total: usize = map.values().map(|c| c.size).sum();
        assert!(
            total <= MAX_CACHE_BYTES,
            "cached bytes grew to {total}, past the {MAX_CACHE_BYTES} cap"
        );
        // Oldest-by-insertion-time goes first, and only as far as needed.
        assert!(!map.contains_key("oldest"), "the oldest entry must be evicted first");
        assert!(map.contains_key("middle"), "eviction must stop once the new body fits");
        assert!(map.contains_key("newest"));
        assert!(map.contains_key("incoming"));
    }

    #[test]
    fn cache_keeps_one_over_cap_body_rather_than_dropping_it() {
        // Documented behaviour (see MAX_CACHE_BYTES): evict until it fits OR the
        // cache is empty, so a single body larger than the cap is still retained
        // rather than dropping the result the caller just produced.
        let mut map: HashMap<String, Cached> = HashMap::new();
        map.insert("stale".to_string(), cached_entry(1024, 60));

        evict_to_fit(&mut map, MAX_CACHE_BYTES + 1);
        assert!(map.is_empty(), "everything older must be evicted to make room");

        // Eviction stops at an empty cache, so the caller's own over-cap result is
        // still inserted rather than discarded. It is over the cap by construction;
        // the next oversized call is what evicts it.
        map.insert("incoming".to_string(), cached_entry(MAX_CACHE_BYTES + 1, 0));
        assert!(
            map.contains_key("incoming"),
            "an over-cap body is kept, not dropped, when it is the only entry"
        );
        let mut map2 = map;
        evict_to_fit(&mut map2, 1);
        assert!(
            map2.is_empty(),
            "the retained over-cap entry must be evicted by the next arrival"
        );
    }

    #[test]
    fn cached_size_records_body_plus_structured_bytes() {
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
        let entry = map.get(&cursor).expect("the shaped result is cached under its cursor");
        assert_eq!(
            entry.size,
            entry.body.len() + entry.structured.as_ref().map(value_size).unwrap_or(0),
            "recorded size must cover the body and the stashed structuredContent"
        );
        assert!(entry.size >= 8_000, "recorded size was {}", entry.size);
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
        assert!(text.contains("\"cursor\":\"r"), "the failure must stay pageable");
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

        let projected = fetch_result(
            &cursor,
            0,
            0,
            None,
            Some("data.users.1.profile.age"),
        );

        assert!(!projected["isError"].as_bool().unwrap());

        let text = projected["content"][0]["text"].as_str().unwrap();
        assert_eq!(text, "40");
    }
}
