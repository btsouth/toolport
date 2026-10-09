//! Local MCP catalog exposure measurements and legacy estimates.
//!
//! Distinct lazy/grouped catalog exposures are counted once per session. The
//! telemetry writer tokenizes full/exposed arrays and discovery text offline.
//! The headline is their signed net; historical estimates stay separate.
//!
//! Old gateways retain ownership of `savings.jsonl` and `savings-v2.jsonl`.
//! Tokenized events go to `savings-v3.jsonl`. Its append and rotation
//! share a cross-process lock.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex, OnceLock};

pub const TOKENIZER: &str = "cl100k_base";

/// Owned by a client session, never shared across stdio/legacy HTTP sessions.
/// Sessionless HTTP uses its listener guard, keyed additionally by client label.
#[derive(Default)]
pub struct CatalogSession {
    seen: Mutex<std::collections::HashSet<[u8; 32]>>,
}

impl CatalogSession {
    fn first_exposure(
        &self,
        client: Option<&str>,
        full: &SurfaceSummary,
        exposed: &SerializedSurface,
    ) -> bool {
        let mut hash = Sha256::new();
        let client = client.unwrap_or("");
        hash.update((client.len() as u64).to_le_bytes());
        hash.update(client.as_bytes());
        hash.update(full.hash);
        hash.update(exposed.hash);
        self.seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(hash.finalize().into())
    }
}

/// Special-token-like strings in schemas are ordinary text. No runtime IO.
pub fn count_tokens(text: &str) -> u64 {
    static TOKENIZER: OnceLock<tiktoken_rs::CoreBPE> = OnceLock::new();
    TOKENIZER
        .get_or_init(|| tiktoken_rs::cl100k_base().expect("bundled cl100k_base vocabulary"))
        .encode_ordinary(text)
        .len() as u64
}

/// Rotate tokenized detail once it passes this size. Lifetime and daily aggregates
/// remain durable even if their own total eventually exceeds the detail budget.
const MAX_SAVINGS_BYTES: u64 = 1024 * 1024;
/// Recent detail lines kept on rotation; older lines collapse into one carry line
/// so the cumulative total survives trimming.
const KEEP_LINES: usize = 2000;

fn savings_path() -> Option<PathBuf> {
    // Legacy path. Older gateway processes may continue writing it after an
    // upgrade; the new implementation reads it but writes v2 elsewhere.
    Some(crate::registry::conduit_dir()?.join("savings.jsonl"))
}

fn v2_path() -> Option<PathBuf> {
    Some(crate::registry::conduit_dir()?.join("savings-v2.jsonl"))
}

fn v3_path() -> Option<PathBuf> {
    Some(crate::registry::conduit_dir()?.join("savings-v3.jsonl"))
}

/// Delete all savings logs, including carry-forward aggregates (called when the
/// user clears retained activity). Returns `Err` only on a real removal failure; a
/// missing file (nothing to clear) is success. Local and irreversible; the running
/// total resets to zero and the next serve starts a fresh file.
pub fn try_clear() -> std::io::Result<()> {
    // Write anything queued before deleting, so a queued line cannot reappear.
    if !crate::telemetry::flush() {
        return Err(std::io::Error::other(
            "Telemetry is still pending; retry clearing Activity",
        ));
    }
    let mut first_error = None;
    for path in [savings_path(), v2_path(), v3_path()].into_iter().flatten() {
        let _lock = match crate::registry::lock_at(&path) {
            Ok(lock) => lock,
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(io::Error::other(error));
                }
                continue;
            }
        };
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => {
                if first_error.is_none() {
                    first_error = Some(e);
                }
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn epoch_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

pub const ESTIMATE_METHOD: &str = "utf8_bytes_div_4";

/// Exact UTF-8 size of the serialized MCP `tools` array, including brackets and
/// commas. This is the canonical byte measurement for catalog surfaces.
pub fn surface_bytes<T: std::borrow::Borrow<Value>>(tools: impl IntoIterator<Item = T>) -> u64 {
    serialize_surface(tools, |_, _| {})
}

/// Serialize the array once while exposing each element's exact byte length to
/// attribution. This produces the same bytes as serde_json::to_vec(tools).
fn serialize_surface<T: std::borrow::Borrow<Value>>(
    tools: impl IntoIterator<Item = T>,
    on_tool: impl FnMut(&Value, u64),
) -> u64 {
    let mut writer = SurfaceWriter::default();
    write_surface(&mut writer, tools, on_tool);
    writer.len
}

pub fn estimated_tokens(bytes: u64) -> u64 {
    bytes.div_ceil(4)
}

/// Legacy reference estimate. Old logs summed individually serialized tools;
/// this test helper documents their historical arithmetic.
#[cfg(test)]
pub fn estimate_tokens(tools: &[Value]) -> u64 {
    let bytes: usize = tools
        .iter()
        .filter_map(|t| serde_json::to_string(t).ok())
        .map(|s| s.len())
        .sum();
    bytes.div_ceil(4) as u64
}

/// Digest, exact length and per-name attribution without retaining schema text.
#[derive(Debug)]
pub struct SurfaceSummary {
    pub hash: [u8; 32],
    pub bytes: u64,
    tools: BTreeMap<String, u64>,
    count: usize,
}

#[derive(Default)]
struct SurfaceWriter {
    text: Option<Vec<u8>>,
    hash: Option<Sha256>,
    len: u64,
}

impl Write for SurfaceWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let Some(text) = &mut self.text {
            text.extend_from_slice(bytes);
        }
        if let Some(hash) = &mut self.hash {
            hash.update(bytes);
        }
        self.len += bytes.len() as u64;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn write_surface<T: std::borrow::Borrow<Value>>(
    writer: &mut SurfaceWriter,
    tools: impl IntoIterator<Item = T>,
    mut on_tool: impl FnMut(&Value, u64),
) -> usize {
    writer.write_all(b"[").unwrap();
    let mut count = 0;
    for tool in tools {
        let tool = tool.borrow();
        if count > 0 {
            writer.write_all(b",").unwrap();
        }
        let start = writer.len;
        serde_json::to_writer(&mut *writer, tool).expect("serde_json::Value serializes");
        on_tool(tool, writer.len - start);
        count += 1;
    }
    writer.write_all(b"]").unwrap();
    count
}

impl SurfaceSummary {
    pub fn from_tools<T: std::borrow::Borrow<Value>>(tools: impl IntoIterator<Item = T>) -> Self {
        Self::build(&mut SurfaceWriter::default(), tools)
    }

    fn build<T: std::borrow::Borrow<Value>>(
        writer: &mut SurfaceWriter,
        tools: impl IntoIterator<Item = T>,
    ) -> Self {
        writer.hash = Some(Sha256::new());
        let mut sizes = BTreeMap::new();
        let count = write_surface(writer, tools, |tool, bytes| {
            if let Some(name) = tool.get("name").and_then(Value::as_str) {
                *sizes.entry(name.to_string()).or_default() += bytes;
            }
        });
        Self {
            hash: writer
                .hash
                .as_ref()
                .expect("summary hashes bytes")
                .clone()
                .finalize()
                .into(),
            bytes: writer.len,
            tools: sizes,
            count,
        }
    }

    pub fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self
                .tools
                .keys()
                .map(|name| name.capacity() + 128)
                .sum::<usize>()
    }
}

/// Immutable wire bytes with shared attribution metadata.
#[derive(Debug)]
pub struct SerializedSurface {
    pub json: Box<serde_json::value::RawValue>,
    pub summary: Arc<SurfaceSummary>,
}

impl std::ops::Deref for SerializedSurface {
    type Target = SurfaceSummary;

    fn deref(&self) -> &Self::Target {
        &self.summary
    }
}

impl SerializedSurface {
    pub fn new(tools: &[Value]) -> Self {
        Self::from_tools(tools)
    }

    pub fn from_tools<T: std::borrow::Borrow<Value>>(tools: impl IntoIterator<Item = T>) -> Self {
        let mut writer = SurfaceWriter {
            text: Some(Vec::new()),
            ..SurfaceWriter::default()
        };
        let summary = Arc::new(SurfaceSummary::build(&mut writer, tools));
        let text = String::from_utf8(writer.text.unwrap()).expect("JSON is UTF-8");
        Self {
            summary,
            json: serde_json::value::RawValue::from_string(text)
                .expect("serialized tools are valid JSON"),
        }
    }

    pub fn retained_bytes(&self) -> usize {
        self.json.get().len() + std::mem::size_of::<Self>() + self.summary.retained_bytes()
    }

    pub fn tool_count(&self) -> usize {
        self.count
    }
}

/// Compatibility entry point for callers without a cached surface.
pub fn record_catalog(
    session: &CatalogSession,
    mode: &str,
    client: Option<&str>,
    full: &[Value],
    exposed: &[Value],
    route: impl Fn(&str) -> Option<String>,
) {
    let full = SerializedSurface::new(full);
    record_catalog_surfaces(
        session,
        mode,
        client,
        &full,
        &SerializedSurface::new(exposed),
        || full.json.get().to_string(),
        route,
    );
}

/// Record cached surfaces once per client/session and pair of surface hashes.
/// Dedupe precedes attribution, text copies and offline tokenization.
pub fn record_catalog_surfaces(
    session: &CatalogSession,
    mode: &str,
    client: Option<&str>,
    full: &SurfaceSummary,
    exposed: &SerializedSurface,
    full_text: impl FnOnce() -> String,
    route: impl Fn(&str) -> Option<String>,
) {
    if !session.first_exposure(client, full, exposed) {
        return;
    }
    let mut by_server_bytes = BTreeMap::<String, u64>::new();
    for (name, bytes) in &full.tools {
        if !exposed.tools.contains_key(name) {
            if let Some(server) = route(name) {
                *by_server_bytes.entry(server).or_default() += bytes;
            }
        }
    }
    let full_bytes = full.bytes;
    let exposed_bytes = exposed.json.get().len() as u64;
    let avoided = full_bytes.saturating_sub(exposed_bytes);
    let extra = exposed_bytes.saturating_sub(full_bytes);
    let mut row = json!({
        "v": 3, "kind": "catalog_exposure", "ts": epoch_millis() as u64,
        "mode": mode, "fullToolCount": full.count, "exposedToolCount": exposed.count,
        "fullSurfaceBytes": full_bytes, "exposedSurfaceBytes": exposed_bytes,
        "avoidedSurfaceBytes": avoided,
        "extraExposedSurfaceBytes": extra,
        "surfaceDeltaBytes": full_bytes as i64 - exposed_bytes as i64,
        "tokenizer": TOKENIZER,
        "_fullText": full_text(), "_exposedText": exposed.json.get(),
        "byServerBytes": by_server_bytes,
    });
    if let Some(client) = client.filter(|client| !client.is_empty()) {
        row["client"] = json!(client);
    }
    append_line(&row);
}

/// Search content is measured after composing the text returned by tools/call.
pub fn record_discovery(text: &str, matched_schema_bytes: u64) {
    append_line(&json!({
        "v": 3, "kind": "discovery_response", "ts": epoch_millis() as u64,
        "responseContentBytes": text.len() as u64,
        "matchedSchemaBytes": matched_schema_bytes,
        "tokenizer": TOKENIZER, "_discoveryText": text,
    }));
}

/// Record one code-mode script run: the downstream round-trips it collapsed into a single
/// `run_script` call (`round_trips_saved` = calls - 1). Written to the v2 log,
/// tagged `kind:"orchestration"` and carrying `roundTripsSaved` so the reader
/// totals round trips separately from catalog estimates. `loads:0` keeps it out
/// of the list-serve count (a script run is not a `tools/list`). No-op when nothing was saved.
pub fn record_orchestration(round_trips_saved: u64) {
    if round_trips_saved == 0 {
        return;
    }
    append_line(&json!({
        "v": 2,
        "ts": epoch_millis() as u64,
        "kind": "orchestration",
        "roundTripsSaved": round_trips_saved,
        "loads": 0,
    }));
}

/// Append one JSON entry under the v2 lock. The lock covers both append and
/// any rotation; an old gateway never opens this versioned path.
fn append_line(entry: &Value) {
    if let Some(path) = v3_path() {
        crate::telemetry::record(
            &path,
            &entry.to_string(),
            crate::telemetry::Rotation::Savings {
                max_bytes: MAX_SAVINGS_BYTES,
                keep_lines: KEEP_LINES,
            },
        );
    }
}

/// Single-line wrapper used by the savings tests. Production writes go through
/// [`append_lines_at`] on the background telemetry writer.
#[cfg(test)]
fn append_line_at(
    path: &Path,
    entry: &Value,
    max_bytes: u64,
    keep_lines: usize,
    after_snapshot: Option<&mut dyn FnMut()>,
) -> Result<(), String> {
    let line = entry.to_string();
    append_lines_at_with_hook(
        path,
        std::slice::from_ref(&line),
        max_bytes,
        keep_lines,
        after_snapshot,
    )
    .map_err(|error| error.message)
}

/// Append a batch of already-serialized JSONL `lines` under ONE acquisition of the
/// path's cross-process lock, then apply the savings cap. Used by the background
/// telemetry writer ([`crate::telemetry`]) so neither the lock nor a fold-rotation
/// runs on the request thread.
pub(crate) fn append_lines_at(
    path: &Path,
    lines: &[String],
    max_bytes: u64,
    keep_lines: usize,
) -> Result<(), crate::telemetry::AppendError> {
    // Deferred texts exist only in the bounded in-memory queue, never on disk.
    let lines: Vec<String> = lines.iter().map(|line| tokenize_line(line)).collect();
    append_lines_at_with_hook(path, &lines, max_bytes, keep_lines, None)
}

fn tokenize_line(line: &str) -> String {
    let Ok(mut row) = serde_json::from_str::<Value>(line) else {
        return line.to_owned();
    };
    for (text_key, count_key) in [
        ("_fullText", "fullTokens"),
        ("_exposedText", "exposedTokens"),
        ("_discoveryText", "discoveryTokens"),
    ] {
        if let Some(text) = row.as_object_mut().and_then(|obj| obj.remove(text_key)) {
            if let Some(text) = text.as_str() {
                row[count_key] = json!(count_tokens(text));
            }
        }
    }
    if row["kind"] == "catalog_exposure" && row["tokenizer"] == TOKENIZER {
        let avoided = row["fullTokens"]
            .as_u64()
            .unwrap_or(0)
            .saturating_sub(row["exposedTokens"].as_u64().unwrap_or(0));
        // Team attribution remains catalog-only, apportioned by omitted bytes.
        let weights = row["byServerBytes"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        let total: u128 = weights
            .values()
            .map(|n| n.as_u64().unwrap_or(0) as u128)
            .sum();
        let mut remaining = avoided;
        let mut shares = BTreeMap::new();
        for (index, (server, weight)) in weights.iter().enumerate() {
            let share = if total == 0 {
                0
            } else if index + 1 == weights.len() {
                remaining
            } else {
                (avoided as u128 * weight.as_u64().unwrap_or(0) as u128 / total) as u64
            };
            remaining = remaining.saturating_sub(share);
            shares.insert(server.clone(), share);
        }
        row["byServer"] = json!(shares);
    }
    row.to_string()
}

fn append_lines_at_with_hook(
    path: &Path,
    lines: &[String],
    max_bytes: u64,
    keep_lines: usize,
    after_snapshot: Option<&mut dyn FnMut()>,
) -> Result<(), crate::telemetry::AppendError> {
    let _lock = crate::registry::lock_at(path)?;
    let mut file = crate::registry::open_append_private(path).map_err(|e| e.to_string())?;
    let mut bytes = String::new();
    for line in lines {
        bytes.push_str(line);
        if !line.ends_with('\n') {
            bytes.push('\n');
        }
    }
    file.write_all(bytes.as_bytes())
        .map_err(|e| e.to_string())?;
    let size = file
        .metadata()
        .map_err(|e| crate::telemetry::AppendError::after_append(e.to_string()))?
        .len();
    drop(file);
    if size > max_bytes {
        rotate_if_large(path, keep_lines, after_snapshot)
            .map_err(crate::telemetry::AppendError::after_append)?;
    }
    Ok(())
}

/// Collapse old lines into a single carry line once the log exceeds the cap, so
/// the running total is preserved while the file stays bounded.
fn rotate_if_large(
    path: &Path,
    keep_lines: usize,
    mut after_snapshot: Option<&mut dyn FnMut()>,
) -> Result<(), String> {
    let content = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() <= keep_lines {
        return Ok(());
    }
    let Ok(parsed): Result<Vec<Value>, _> = lines
        .iter()
        .map(|line| serde_json::from_str(line))
        .collect()
    else {
        // Preserve damaged history for explicit recovery instead of folding it
        // away during a later append.
        return Ok(());
    };
    if parsed.iter().any(|row| !row.is_object()) {
        return Ok(());
    }
    let mut details: Vec<Value> = parsed
        .iter()
        .filter(|row| row.get("kind").and_then(Value::as_str) != Some("team_daily"))
        .cloned()
        .collect();
    if details.len() <= keep_lines {
        return Ok(());
    }
    let retained = details.split_off(details.len() - keep_lines);
    let carry = fold(&details);
    let mut buckets = BTreeMap::<String, BTreeMap<String, u64>>::new();
    for row in parsed.iter().filter(|row| row["kind"] == "team_daily") {
        merge_team_bucket(&mut buckets, row);
    }
    for row in &details {
        if row["kind"] == "catalog_exposure" {
            merge_team_bucket(&mut buckets, row);
        }
    }
    if let Some(hook) = after_snapshot.as_mut() {
        hook();
    }
    let mut out = carry.to_string();
    out.push('\n');
    for (day, by_server) in buckets {
        out.push_str(
            &json!({"v":2,"kind":"team_daily","day":day,"byServer":by_server}).to_string(),
        );
        out.push('\n');
    }
    for row in retained {
        out.push_str(&row.to_string());
        out.push('\n');
    }
    // Atomic + unique temp: every client's gateway shares this file, so a
    // bespoke fixed temp name could let two rotations collide.
    crate::registry::atomic_write(path, &out)
}

fn merge_team_bucket(buckets: &mut BTreeMap<String, BTreeMap<String, u64>>, row: &Value) {
    let day = row
        .get("day")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            row.get("ts")
                .and_then(Value::as_u64)
                .map(crate::usage_report::utc_day)
        });
    let (Some(day), Some(by_server)) = (day, row.get("byServer").and_then(Value::as_object)) else {
        return;
    };
    let bucket = buckets.entry(day).or_default();
    for (server, value) in by_server {
        let total = bucket.entry(server.clone()).or_default();
        *total = total.saturating_add(value.as_u64().unwrap_or(0));
    }
}

/// Fold entries into a single carry record that the reader sums like any other
/// line: it preserves the saved total, the load count, the peak catalog, and the
/// earliest timestamp.
fn fold(entries: &[Value]) -> Value {
    let mut tokenized_loads = 0u64;
    let mut full_tokens = 0u64;
    let mut exposed_tokens = 0u64;
    let mut discovery_tokens = 0u64;
    let mut saved = 0u64;
    let mut v2_estimated = 0u64;
    let mut loads = 0u64;
    let mut peak = 0u64;
    let mut since = 0u64;
    let mut round_trips = 0u64;
    let mut full_bytes = 0u64;
    let mut exposed_bytes = 0u64;
    let mut avoided_bytes = 0u64;
    let mut extra_bytes = 0u64;
    let mut measured_loads = 0u64;
    let mut discovery_count = 0u64;
    let mut discovery_bytes = 0u64;
    let mut matched_schema_bytes = 0u64;
    let mut estimated_discovery_tokens = 0u64;
    let mut latest_catalog_ts = 0u64;
    let mut latest_full_tools = 0u64;
    let mut latest_exposed_tools = 0u64;
    let mut latest_full_bytes = 0u64;
    let mut latest_exposed_bytes = 0u64;
    for e in entries {
        let number = |key| e.get(key).and_then(Value::as_u64).unwrap_or(0);
        let kind = e.get("kind").and_then(Value::as_str);
        let is_carry = kind == Some("carry");
        let is_catalog = kind == Some("catalog_exposure");
        let is_discovery = kind == Some("discovery_response");
        if is_carry || e["tokenizer"] == TOKENIZER {
            full_tokens = full_tokens.saturating_add(number("fullTokens"));
            exposed_tokens = exposed_tokens.saturating_add(number("exposedTokens"));
            discovery_tokens = discovery_tokens.saturating_add(number("discoveryTokens"));
            tokenized_loads = tokenized_loads.saturating_add(if is_catalog {
                1
            } else {
                number("tokenizedLoads")
            });
        }
        // v1 rows and v1 carry records both store their estimate in `saved`.
        // v2 carry records store only that legacy component in `saved`.
        saved = saved.saturating_add(number("saved"));
        if is_catalog || is_carry {
            let estimate = number("estimatedTokensAvoided");
            v2_estimated = v2_estimated.saturating_add(estimate);
            full_bytes = full_bytes.saturating_add(number("fullSurfaceBytes"));
            exposed_bytes = exposed_bytes.saturating_add(number("exposedSurfaceBytes"));
            avoided_bytes = avoided_bytes.saturating_add(number("avoidedSurfaceBytes"));
            extra_bytes = extra_bytes.saturating_add(number("extraExposedSurfaceBytes"));
            measured_loads = measured_loads.saturating_add(if is_catalog {
                1
            } else {
                number("measuredLoads")
            });
            let candidate_ts = if is_catalog {
                number("ts")
            } else {
                number("latestCatalogTs")
            };
            if candidate_ts > 0 && candidate_ts >= latest_catalog_ts {
                latest_catalog_ts = candidate_ts;
                latest_full_tools = if is_catalog {
                    number("fullToolCount")
                } else {
                    number("latestFullToolCount")
                };
                latest_exposed_tools = if is_catalog {
                    number("exposedToolCount")
                } else {
                    number("latestExposedToolCount")
                };
                latest_full_bytes = if is_catalog {
                    number("fullSurfaceBytes")
                } else {
                    number("latestFullSurfaceBytes")
                };
                latest_exposed_bytes = if is_catalog {
                    number("exposedSurfaceBytes")
                } else {
                    number("latestExposedSurfaceBytes")
                };
            }
        }
        if is_discovery || is_carry {
            discovery_count = discovery_count.saturating_add(if is_discovery {
                1
            } else {
                number("discoveryCount")
            });
            discovery_bytes = discovery_bytes.saturating_add(number("responseContentBytes"));
            matched_schema_bytes =
                matched_schema_bytes.saturating_add(number("matchedSchemaBytes"));
            estimated_discovery_tokens =
                estimated_discovery_tokens.saturating_add(number("estimatedResponseTokens"));
        }
        // A normal list-serve line has no `loads` and counts as one; a carry line and an
        // orchestration line carry an explicit `loads` (the latter is 0), so neither a
        // rotation nor a code-mode run inflates the list-load count.
        loads = loads.saturating_add(
            e.get("loads")
                .and_then(Value::as_u64)
                .unwrap_or(if is_catalog || (kind.is_none()) { 1 } else { 0 }),
        );
        peak = peak.max(number("tools")).max(number("fullToolCount"));
        round_trips = round_trips.saturating_add(
            e.get("roundTripsSaved")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        );
        let ts = e.get("ts").and_then(Value::as_u64).unwrap_or(0);
        if ts > 0 && (since == 0 || ts < since) {
            since = ts;
        }
    }
    json!({ "v": 3, "kind": "carry",
        "fullTokens": full_tokens, "exposedTokens": exposed_tokens,
        "discoveryTokens": discovery_tokens, "tokenizedLoads": tokenized_loads, "ts": since, "saved": saved, "legacyTokensSaved": saved, "tools": peak, "loads": loads, "roundTripsSaved": round_trips,
        "fullSurfaceBytes": full_bytes, "exposedSurfaceBytes": exposed_bytes, "avoidedSurfaceBytes": avoided_bytes,
        "extraExposedSurfaceBytes": extra_bytes,
        "estimatedTokensAvoided": v2_estimated, "measuredLoads": measured_loads,
        "latestCatalogTs": latest_catalog_ts, "latestFullToolCount": latest_full_tools,
        "latestExposedToolCount": latest_exposed_tools,
        "latestFullSurfaceBytes": latest_full_bytes, "latestExposedSurfaceBytes": latest_exposed_bytes,
        "discoveryCount": discovery_count, "responseContentBytes": discovery_bytes,
        "matchedSchemaBytes": matched_schema_bytes, "estimatedResponseTokens": estimated_discovery_tokens })
}

/// Every savings line on disk, oldest first (bounded by rotation). Shared by the
/// in-app counter and the team usage rollup.
fn read_lines(path: Option<PathBuf>) -> io::Result<Vec<Value>> {
    let Some(path) = path else {
        return Ok(Vec::new());
    };
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    content
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            let row: Value = serde_json::from_str(line).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("line {}: {error}", index + 1),
                )
            })?;
            if !row.is_object() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("line {}: expected a telemetry object", index + 1),
                ));
            }
            Ok(row)
        })
        .collect()
}

pub fn try_entries() -> io::Result<Vec<Value>> {
    // Land this process's queued savings lines before reading so a caller never
    // misses its own writes.
    crate::telemetry::flush();
    let mut old = read_lines(savings_path())?;
    if old.iter().any(|row| row["v"] == 2) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected v2 telemetry in legacy savings.jsonl",
        ));
    }
    for path in [v2_path(), v3_path()].into_iter().flatten() {
        let _lock = crate::registry::lock_at(&path).map_err(io::Error::other)?;
        old.extend(read_lines(Some(path))?);
    }
    Ok(old)
}

pub fn entries() -> Vec<Value> {
    try_entries().unwrap_or_default()
}

/// Cumulative savings for the in-app counter.
pub fn summary() -> Value {
    aggregate(&entries())
}

pub fn try_summary() -> io::Result<Value> {
    Ok(aggregate(&try_entries()?))
}

/// Pure aggregation, split out so the math is testable without touching disk.
/// A normal line counts as one load; a carry line carries its own `loads`.
fn aggregate(entries: &[Value]) -> Value {
    let folded = fold(entries);
    let number = |key| folded[key].as_u64().unwrap_or(0) as i128;
    let delta = number("fullTokens") - number("exposedTokens");
    let signed = |n: i128| n.clamp(i64::MIN as i128, i64::MAX as i128) as i64;
    json!({
        "tokensSaved": signed(delta - number("discoveryTokens")),
        "catalogTokenDelta": signed(delta),
        "fullTokens": folded["fullTokens"],
        "exposedTokens": folded["exposedTokens"],
        "discoveryTokens": folded["discoveryTokens"],
        "tokenizedLoads": folded["tokenizedLoads"],
        "tokenizer": TOKENIZER,
        "listLoads": folded.get("loads").and_then(Value::as_u64).unwrap_or(0),
        "peakCatalog": folded.get("tools").and_then(Value::as_u64).unwrap_or(0),
        "sinceTs": folded.get("ts").and_then(Value::as_u64).unwrap_or(0),
        "roundTripsSaved": folded.get("roundTripsSaved").and_then(Value::as_u64).unwrap_or(0),
        "legacyEstimatedTokensAvoided": folded["legacyTokensSaved"],
        "measuredLoads": folded["measuredLoads"],
        "latestCatalogTs": folded["latestCatalogTs"],
        "latestFullToolCount": folded["latestFullToolCount"],
        "latestExposedToolCount": folded["latestExposedToolCount"],
        "latestFullSurfaceBytes": folded["latestFullSurfaceBytes"],
        "latestExposedSurfaceBytes": folded["latestExposedSurfaceBytes"],
        "fullSurfaceBytes": folded["fullSurfaceBytes"],
        "exposedSurfaceBytes": folded["exposedSurfaceBytes"],
        "avoidedSurfaceBytes": folded["avoidedSurfaceBytes"],
        "extraExposedSurfaceBytes": folded["extraExposedSurfaceBytes"],
        "surfaceDeltaBytes": folded["avoidedSurfaceBytes"].as_u64().unwrap_or(0) as i64 - folded["extraExposedSurfaceBytes"].as_u64().unwrap_or(0) as i64,
        "estimatedTokensAvoided": folded["estimatedTokensAvoided"],
        "estimateMethod": ESTIMATE_METHOD,
        "discoveryCount": folded["discoveryCount"],
        "discoveryResponseBytes": folded["responseContentBytes"],
        "matchedSchemaBytes": folded["matchedSchemaBytes"],
        "estimatedDiscoveryTokens": folded["estimatedResponseTokens"],
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn serialized_surface_keeps_exact_bytes_hash_and_repeat_credit() {
        let tools = vec![
            json!({"name":"s__read", "description":"é\nrecord", "inputSchema":{"type":"object"}}),
        ];
        let surface = SerializedSurface::new(&tools);
        assert_eq!(
            surface.json.get().as_bytes(),
            serde_json::to_vec(&tools).unwrap()
        );
        assert_eq!(
            surface.hash,
            <[u8; 32]>::from(Sha256::digest(surface.json.get().as_bytes()))
        );
        let streamed = SerializedSurface::from_tools(tools.clone());
        assert_eq!(streamed.json.get(), surface.json.get());
        assert_eq!(streamed.hash, surface.hash);
        assert_eq!(streamed.tools, surface.tools);
        let summary = SurfaceSummary::from_tools(&tools);
        assert_eq!(summary.hash, surface.hash);
        assert_eq!(summary.bytes, surface.json.get().len() as u64);
        assert_eq!(summary.tools, surface.tools);
        assert_eq!(summary.count, surface.count);
        assert_eq!(surface_bytes(&tools), summary.bytes);
        assert_eq!(
            estimated_tokens(summary.bytes),
            estimated_tokens(surface.json.get().len() as u64)
        );
        let session = CatalogSession::default();
        let exposed = SerializedSurface::new(&[]);
        assert!(session.first_exposure(Some("client"), &surface, &exposed));
        assert!(!session.first_exposure(Some("client"), &surface, &exposed));
        assert!(session.first_exposure(Some("other"), &surface, &exposed));
        let changed = SerializedSurface::new(&[
            json!({"name":"s__read", "inputSchema":{"type":"object","properties":{"new":{"type":"string"}}}}),
        ]);
        assert!(session.first_exposure(Some("client"), &changed, &exposed));
        assert!(session.first_exposure(Some("client"), &surface, &surface));
        // A hit must stop before resolving attribution or copying schema texts.
        record_catalog_surfaces(
            &session,
            "lazy",
            Some("client"),
            &surface,
            &exposed,
            || panic!("repeated exposure rebuilt text"),
            |_| panic!("repeated exposure was attributed again"),
        );
    }

    #[test]
    fn summary_only_catalog_exposure_keeps_identical_savings() {
        let _lock = crate::registry::data_dir_test_lock();
        let root =
            std::env::temp_dir().join(format!("toolport-summary-savings-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let _data = crate::registry::DataDirOverride::set(&root);
        try_clear().unwrap();
        let full = vec![
            json!({"name":"s__read", "description":"é\nrecord"}),
            json!({"name":"s__write", "inputSchema":{"type":"object"}}),
        ];
        let exposed = vec![json!({"name":"toolport_search_tools"})];
        record_catalog(
            &CatalogSession::default(),
            "lazy",
            Some("client"),
            &full,
            &exposed,
            |_| Some("s".into()),
        );
        let summary = SurfaceSummary::from_tools(&full);
        let session = CatalogSession::default();
        record_catalog_surfaces(
            &session,
            "lazy",
            Some("client"),
            &summary,
            &SerializedSurface::new(&exposed),
            || serde_json::to_string(&full).unwrap(),
            |_| Some("s".into()),
        );
        record_catalog_surfaces(
            &session,
            "lazy",
            Some("client"),
            &summary,
            &SerializedSurface::new(&exposed),
            || panic!("warm exposure rebuilt text"),
            |_| panic!("warm attribution"),
        );
        let mut rows = entries();
        assert_eq!(rows.len(), 2);
        for row in &mut rows {
            row.as_object_mut().unwrap().remove("ts");
        }
        assert_eq!(
            serde_json::to_vec(&rows[0]).unwrap(),
            serde_json::to_vec(&rows[1]).unwrap()
        );
        try_clear().unwrap();
        drop(_data);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rotation_failure_is_reported_and_keeps_history() {
        use crate::registry::tests::{with_atomic_failure, FailingAtomicWriteStep::*};
        let root = std::env::temp_dir().join(format!(
            "toolport-savings-rotate-failure-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("savings.jsonl");
        let old = "{\"v\":2,\"kind\":\"list\",\"tokensSaved\":5}\n";
        for step in [Permissions, Write, Rename] {
            std::fs::write(&path, old).unwrap();
            let result = with_atomic_failure(step, || {
                super::append_lines_at(&path, &[old.trim().into()], 1, 1)
            });
            assert!(result.is_err());
            let appended = serde_json::from_str::<serde_json::Value>(old).unwrap();
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                format!("{old}{appended}\n")
            );
        }
        super::append_lines_at(&path, &[old.trim().into()], 1, 1).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    use super::*;
    use fs2::FileExt;
    use std::collections::HashSet;

    #[test]
    fn tokenizer_matches_known_cl100k_vectors() {
        assert_eq!(count_tokens("hello world"), 2);
        assert_eq!(count_tokens("こんにちは世界"), 4);
        assert_eq!(count_tokens(""), 0);
        assert_eq!(count_tokens("<|endoftext|>"), 7);
    }

    #[test]
    fn concurrent_reloads_count_once_and_changes_and_sessions_count_separately() {
        let _guard = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!("toolport-net-savings-{}", std::process::id()));
        let _override = crate::registry::DataDirOverride::set(&dir);
        let session = std::sync::Arc::new(CatalogSession::default());
        let full = vec![json!({"name":"alpha__tool","description":"read a file".repeat(100)})];
        let exposed = vec![json!({"name":"toolport_search_tools"})];
        std::thread::scope(|scope| {
            for _ in 0..16 {
                let session = &session;
                let full = &full;
                let exposed = &exposed;
                scope.spawn(move || {
                    record_catalog(session, "lazy", Some("client"), full, exposed, |_| {
                        Some("alpha".into())
                    })
                });
            }
        });
        let full_count = count_tokens(&serde_json::to_string(&full).unwrap());
        let exposed_count = count_tokens(&serde_json::to_string(&exposed).unwrap());
        record_discovery("hello world", 0);
        let first = summary();
        assert_eq!(first["tokenizedLoads"], 1);
        assert_eq!(
            first["tokensSaved"],
            full_count as i64 - exposed_count as i64 - 2
        );
        assert_eq!(first["legacyEstimatedTokensAvoided"], 0);
        assert_eq!(first["estimatedTokensAvoided"], 0);
        let disk = std::fs::read_to_string(v3_path().unwrap()).unwrap();
        assert!(!disk.contains("_fullText"));
        assert!(!disk.contains("_discoveryText"));
        let mut changed = full.clone();
        changed[0]["description"] = json!("changed description");
        record_catalog(&session, "lazy", Some("client"), &changed, &exposed, |_| {
            None
        });
        record_catalog(&session, "lazy", Some("client"), &full, &exposed, |_| None);
        assert_eq!(summary()["tokenizedLoads"], 2);
        record_catalog(
            &CatalogSession::default(),
            "lazy",
            Some("client"),
            &full,
            &exposed,
            |_| None,
        );
        record_catalog(
            &session,
            "lazy",
            Some("other-client"),
            &full,
            &exposed,
            |_| None,
        );
        assert_eq!(summary()["tokenizedLoads"], 4);
        try_clear().unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn net_can_be_negative_and_mixed_carries_preserve_tokenizer_and_estimates() {
        let rows = [
            json!({"ts":1,"saved":900,"tools":50}),
            json!({"v":2,"kind":"catalog_exposure","ts":2,"estimatedTokensAvoided":800}),
            json!({"v":2,"kind":"discovery_response","ts":3,"estimatedResponseTokens":200}),
            json!({"v":3,"kind":"catalog_exposure","ts":4,"tokenizer":TOKENIZER,"fullTokens":10,"exposedTokens":20}),
            json!({"v":3,"kind":"catalog_exposure","ts":5,"tokenizer":TOKENIZER,"fullTokens":100,"exposedTokens":10}),
            json!({"v":3,"kind":"discovery_response","ts":6,"tokenizer":TOKENIZER,"discoveryTokens":90}),
        ];
        let before = aggregate(&rows);
        assert_eq!(before["tokensSaved"], -10);
        assert_eq!(before["tokenizedLoads"], 2);
        assert_eq!(before["legacyEstimatedTokensAvoided"], 900);
        assert_eq!(before["estimatedTokensAvoided"], 800);
        assert_eq!(before["estimatedDiscoveryTokens"], 200);
        assert_eq!(aggregate(&[fold(&rows[..3]), fold(&rows[3..])]), before);
        assert_eq!(aggregate(&[fold(&[fold(&rows)])]), before);
    }

    #[test]
    fn v2_store_is_read_only_and_clear_removes_all_three_eras() {
        let _guard = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!("toolport-v2-readonly-{}", std::process::id()));
        let _override = crate::registry::DataDirOverride::set(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let v2 = v2_path().unwrap();
        let old = "{\"v\":2,\"kind\":\"catalog_exposure\",\"estimatedTokensAvoided\":1000}\n";
        std::fs::write(&v2, old).unwrap();
        record_discovery("hello world", 0);
        assert_eq!(summary()["tokensSaved"], -2);
        assert_eq!(summary()["estimatedTokensAvoided"], 1000);
        assert_eq!(std::fs::read_to_string(&v2).unwrap(), old);
        try_clear().unwrap();
        assert!(!v2.exists());
        assert!(!v3_path().unwrap().exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn estimate_is_serialized_len_over_four() {
        // {"name":"x"} is 12 chars -> ceil(12/4) = 3.
        let tools = vec![json!({ "name": "x" })];
        assert_eq!(estimate_tokens(&tools), 3);
        assert_eq!(estimate_tokens(&[]), 0);
    }

    #[test]
    fn aggregate_sums_saved_and_counts_loads() {
        let entries = vec![
            json!({ "ts": 200, "saved": 100, "tools": 50 }),
            json!({ "ts": 100, "saved": 60, "tools": 80 }),
            json!({ "ts": 300, "saved": 40, "tools": 30 }),
        ];
        let s = aggregate(&entries);
        assert_eq!(s["tokensSaved"], 0); // Historical estimates are excluded.
        assert_eq!(s["legacyEstimatedTokensAvoided"], 200);
        assert_eq!(s["listLoads"], 3);
        assert_eq!(s["peakCatalog"], 80); // biggest catalog collapsed
        assert_eq!(s["sinceTs"], 100); // earliest
    }

    #[test]
    fn carry_line_preserves_totals_after_rotation() {
        // A folded carry line plus fresh detail lines aggregates the same as if
        // nothing had been trimmed.
        let detail = [
            json!({ "ts": 10, "saved": 100, "tools": 40 }),
            json!({ "ts": 20, "saved": 100, "tools": 90 }),
            json!({ "ts": 30, "saved": 100, "tools": 50 }),
        ];
        let carry = fold(&detail[..2]); // collapse the first two
        let after = vec![carry, detail[2].clone()];
        let s = aggregate(&after);
        assert_eq!(s["tokensSaved"], 0); // Historical estimates stay outside the headline.
        assert_eq!(s["listLoads"], 3); // 2 folded + 1 fresh
        assert_eq!(s["peakCatalog"], 90);
        assert_eq!(s["sinceTs"], 10);
    }

    #[test]
    fn aggregate_handles_empty() {
        let s = aggregate(&[]);
        assert_eq!(s["tokensSaved"], 0);
        assert_eq!(s["listLoads"], 0);
        assert_eq!(s["sinceTs"], 0);
        assert_eq!(s["roundTripsSaved"], 0);
    }

    #[test]
    fn orchestration_lines_total_round_trips_without_inflating_loads() {
        // Code-mode runs live in the same log; they add round-trips-saved but are NOT
        // list serves, so they must not count toward listLoads or tokensSaved.
        let entries = vec![
            json!({ "ts": 100, "saved": 60, "tools": 80 }), // one lazy list serve
            json!({ "ts": 200, "kind": "orchestration", "roundTripsSaved": 5, "loads": 0 }),
            json!({ "ts": 300, "kind": "orchestration", "roundTripsSaved": 3, "loads": 0 }),
        ];
        let s = aggregate(&entries);
        assert_eq!(s["tokensSaved"], 0);
        assert_eq!(s["listLoads"], 1); // only the list serve
        assert_eq!(s["roundTripsSaved"], 8); // 5 + 3
        assert_eq!(s["peakCatalog"], 80);
    }

    #[test]
    fn carry_line_preserves_round_trips_after_rotation() {
        // Folding a mix (list serve + orchestration) into a carry line and re-aggregating
        // yields the same totals, so rotation never loses round-trips-saved.
        let detail = [
            json!({ "ts": 10, "saved": 100, "tools": 40 }),
            json!({ "ts": 20, "kind": "orchestration", "roundTripsSaved": 7, "loads": 0 }),
        ];
        let carry = fold(&detail);
        let s = aggregate(&[carry]);
        assert_eq!(s["tokensSaved"], 0);
        assert_eq!(s["listLoads"], 1);
        assert_eq!(s["roundTripsSaved"], 7);
    }

    #[test]
    fn v2_measures_serialized_arrays_and_mixed_rotation_preserves_both_eras() {
        let full = vec![json!({"name":"a", "description":"é"}), json!({"name":"b"})];
        let exposed = vec![full[0].clone()];
        let full_bytes = surface_bytes(&full);
        let exposed_bytes = surface_bytes(&exposed);
        assert_eq!(
            full_bytes,
            serde_json::to_string(&full).unwrap().as_bytes().len() as u64
        );
        assert!(
            full_bytes
                > full
                    .iter()
                    .map(|tool| serde_json::to_string(tool).unwrap().len() as u64)
                    .sum::<u64>()
        );
        let v2 = json!({"v":2, "kind":"catalog_exposure", "ts":20,
            "fullToolCount":2, "exposedToolCount":1, "fullSurfaceBytes":full_bytes,
            "exposedSurfaceBytes":exposed_bytes,
            "avoidedSurfaceBytes":full_bytes - exposed_bytes,
            "estimatedTokensAvoided":estimated_tokens(full_bytes - exposed_bytes)});
        let rows = vec![
            json!({"ts":10, "saved":1_342_400, "tools":1725}),
            v2,
            json!({"v":2, "kind":"discovery_response", "ts":30,
                "responseContentBytes":101, "matchedSchemaBytes":77, "estimatedResponseTokens":26}),
            json!({"kind":"orchestration", "ts":40, "loads":0, "roundTripsSaved":3}),
        ];
        let before = aggregate(&rows);
        let after = aggregate(&[fold(&rows)]);
        assert_eq!(before, after);
        assert_eq!(after["legacyEstimatedTokensAvoided"], 1_342_400);
        assert_eq!(after["tokensSaved"], 0);
        assert_eq!(after["listLoads"], 2);
        assert_eq!(after["measuredLoads"], 1);
        assert_eq!(after["discoveryCount"], 1);
        assert_eq!(after["discoveryResponseBytes"], 101);
        assert_eq!(after["roundTripsSaved"], 3);
        assert_eq!(after["latestFullToolCount"], 2);
        assert_eq!(after["latestExposedToolCount"], 1);
        assert_eq!(after["latestFullSurfaceBytes"], full_bytes);
    }

    #[test]
    fn v1_carry_stays_legacy_and_never_becomes_exact_bytes() {
        let old = json!({"ts":1, "saved":3_692_944_923u64, "loads":2751, "tools":1725});
        let summary = aggregate(&[old]);
        assert_eq!(summary["tokensSaved"], 0);
        assert_eq!(summary["legacyEstimatedTokensAvoided"], 3_692_944_923u64);
        assert_eq!(summary["avoidedSurfaceBytes"], 0);
        assert_eq!(summary["listLoads"], 2751);
    }

    #[test]
    fn pure_v2_log_has_no_legacy_estimate() {
        let rows = vec![json!({"v":2, "kind":"catalog_exposure", "ts":10,
            "fullToolCount":5, "fullSurfaceBytes":1000, "exposedSurfaceBytes":200,
            "avoidedSurfaceBytes":800, "estimatedTokensAvoided":200})];
        let s = aggregate(&rows);
        assert_eq!(s["tokensSaved"], 0);
        assert_eq!(s["estimatedTokensAvoided"], 200);
        assert_eq!(s["legacyEstimatedTokensAvoided"], 0);
        assert_eq!(s["avoidedSurfaceBytes"], 800);
        assert_eq!(s["listLoads"], 1);
    }

    #[test]
    fn extra_discovery_definitions_are_never_counted_as_avoided() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir =
            std::env::temp_dir().join(format!("toolport-extra-exposure-{}", std::process::id()));
        let _override = crate::registry::DataDirOverride::set(&dir);
        record_catalog(
            &CatalogSession::default(),
            "lazy",
            None,
            &[json!({"name":"a"})],
            &[json!({"name":"long_meta_tool"})],
            |name| (name == "a").then(|| "server".to_string()),
        );
        let row = entries().pop().unwrap();
        assert_eq!(row["avoidedSurfaceBytes"], 0);
        assert!(row["extraExposedSurfaceBytes"].as_u64().unwrap() > 0);
        assert!(row["surfaceDeltaBytes"].as_i64().unwrap() < 0);
        assert_eq!(row["byServer"]["server"], 0);
        assert!(summary()["tokensSaved"].as_i64().unwrap() < 0);
        assert_eq!(summary()["listLoads"], 1);
        try_clear().unwrap();
        assert_eq!(summary()["listLoads"], 0);
        assert_eq!(summary()["extraExposedSurfaceBytes"], 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn old_writer_can_rotate_legacy_file_without_touching_v2_history() {
        let _guard = crate::registry::data_dir_test_lock();
        let dir =
            std::env::temp_dir().join(format!("toolport-savings-upgrade-{}", std::process::id()));
        let active_override = crate::registry::DataDirOverride::set(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let legacy = savings_path().unwrap();
        std::fs::write(&legacy, "{\"ts\":1,\"saved\":300,\"tools\":10}\n").unwrap();
        record_catalog(
            &CatalogSession::default(),
            "lazy",
            Some("new"),
            &[json!({"name":"alpha__x","description":"long"})],
            &[json!({"name":"toolport_status"})],
            |_| Some("alpha".into()),
        );
        record_discovery("hello world", 50);
        // The background writer owns the append now: land it before reading v2 directly.
        crate::telemetry::flush();
        let v2 = v3_path().unwrap();
        let v2_before = std::fs::read(&v2).unwrap();
        let initial = try_summary().unwrap();
        assert_eq!(initial["listLoads"], 2);
        assert_eq!(initial["legacyEstimatedTokensAvoided"], 300);
        let mut old_writer = crate::registry::open_append_private(&legacy).unwrap();
        old_writer
            .write_all(b"{\"ts\":2,\"saved\":20,\"tools\":8}\n")
            .unwrap();
        drop(old_writer);
        let before = try_summary().unwrap();
        assert_eq!(before["legacyEstimatedTokensAvoided"], 320);
        assert_eq!(before["listLoads"], 3);
        assert_eq!(before["measuredLoads"], 1);
        assert_eq!(before["discoveryCount"], 1);
        assert_eq!(before["fullSurfaceBytes"], initial["fullSurfaceBytes"]);
        assert_eq!(
            before["avoidedSurfaceBytes"],
            initial["avoidedSurfaceBytes"]
        );
        // Simulate an already-running old gateway replacing only its known file.
        crate::registry::atomic_write(
            &legacy,
            "{\"ts\":1,\"saved\":320,\"tools\":10,\"loads\":2}\n",
        )
        .unwrap();
        assert_eq!(std::fs::read(&v2).unwrap(), v2_before);
        assert_eq!(try_summary().unwrap(), before);
        // Reopen the data directory as a fresh new-process read after old rotation.
        drop(active_override);
        let _reopened = crate::registry::DataDirOverride::set(&dir);
        assert_eq!(try_summary().unwrap(), before);
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "savings::tests::upgraded_history_reopens_in_a_fresh_process",
            ])
            .env("TOOLPORT_SAVINGS_RESTART_DIR", &dir)
            .env("TOOLPORT_SAVINGS_RESTART_EXPECTED", before.to_string())
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "fresh-process read failed: {}",
            String::from_utf8_lossy(&child.stderr)
        );
        assert!(
            String::from_utf8_lossy(&child.stdout).contains("1 passed"),
            "fresh-process assertion did not run: {}",
            String::from_utf8_lossy(&child.stdout)
        );
        try_clear().unwrap();
        assert_eq!(try_summary().unwrap()["tokensSaved"], 0);
        assert!(!legacy.exists());
        assert!(!v2.exists());
        record_discovery("hello world", 12);
        assert_eq!(try_summary().unwrap()["discoveryCount"], 1);
        assert_eq!(try_summary().unwrap()["listLoads"], 0);
        assert!(v2.exists());
        try_clear().unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn upgraded_history_reopens_in_a_fresh_process() {
        let Ok(dir) = std::env::var("TOOLPORT_SAVINGS_RESTART_DIR") else {
            return;
        };
        let _guard = crate::registry::data_dir_test_lock();
        let _override = crate::registry::DataDirOverride::set(dir);
        let expected: Value =
            serde_json::from_str(&std::env::var("TOOLPORT_SAVINGS_RESTART_EXPECTED").unwrap())
                .unwrap();
        assert_eq!(try_summary().unwrap(), expected);
    }

    #[test]
    fn either_store_can_be_missing_without_erasing_the_other() {
        let _guard = crate::registry::data_dir_test_lock();
        let dir =
            std::env::temp_dir().join(format!("toolport-savings-missing-{}", std::process::id()));
        let _override = crate::registry::DataDirOverride::set(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let legacy = savings_path().unwrap();
        let v2 = v3_path().unwrap();
        assert_eq!(try_summary().unwrap()["listLoads"], 0);
        std::fs::write(&legacy, "{\"ts\":1,\"saved\":80,\"tools\":5}\n").unwrap();
        assert!(!v2.exists());
        assert_eq!(try_summary().unwrap()["legacyEstimatedTokensAvoided"], 80);
        assert_eq!(try_summary().unwrap()["measuredLoads"], 0);
        std::fs::remove_file(&legacy).unwrap();
        record_catalog(
            &CatalogSession::default(),
            "lazy",
            None,
            &[json!({"name":"alpha__long","description":"long"})],
            &[json!({"name":"toolport_status"})],
            |_| Some("alpha".into()),
        );
        // The background writer owns the append; land it before the file check.
        crate::telemetry::flush();
        assert!(!legacy.exists());
        assert!(v2.exists());
        assert_eq!(try_summary().unwrap()["legacyEstimatedTokensAvoided"], 0);
        assert_eq!(try_summary().unwrap()["measuredLoads"], 1);
        try_clear().unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn corrupt_store_is_not_reported_as_empty_history() {
        let _guard = crate::registry::data_dir_test_lock();
        let dir =
            std::env::temp_dir().join(format!("toolport-savings-corrupt-{}", std::process::id()));
        let _override = crate::registry::DataDirOverride::set(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let v2 = v3_path().unwrap();
        std::fs::write(&v2, "{broken\n").unwrap();
        assert_eq!(
            try_summary().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        append_line_at(
            &v2,
            &json!({"v":2,"kind":"discovery_response","ts":1,"responseContentBytes":4}),
            1,
            1,
            None,
        )
        .unwrap();
        assert!(std::fs::read_to_string(&v2)
            .unwrap()
            .starts_with("{broken\n"));
        assert_eq!(
            try_summary().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        try_clear().unwrap();
        let legacy = savings_path().unwrap();
        std::fs::write(&legacy, "{broken\n").unwrap();
        assert_eq!(
            try_summary().unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        try_clear().unwrap();
        assert_eq!(try_summary().unwrap()["listLoads"], 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rotation_keeps_day_and_server_buckets_without_attributing_legacy() {
        let _guard = crate::registry::data_dir_test_lock();
        let dir =
            std::env::temp_dir().join(format!("toolport-savings-days-{}", std::process::id()));
        let _override = crate::registry::DataDirOverride::set(&dir);
        let path = v3_path().unwrap();
        let day1 = 1_783_470_600_000u64;
        let day2 = day1 + 86_400_000;
        let rows = [
            json!({"v":2,"kind":"catalog_exposure","ts":day1,"fullToolCount":3,"estimatedTokensAvoided":10,"byServer":{"alpha":10}}),
            json!({"v":2,"kind":"catalog_exposure","ts":day1+1,"fullToolCount":4,"estimatedTokensAvoided":20,"byServer":{"bravo":20}}),
            json!({"v":2,"kind":"catalog_exposure","ts":day2,"fullToolCount":5,"estimatedTokensAvoided":30,"byServer":{"alpha":30}}),
            json!({"v":2,"kind":"discovery_response","ts":day2+1,"responseContentBytes":40,"estimatedResponseTokens":10}),
        ];
        for row in &rows {
            append_line_at(&path, row, 1, 2, None).unwrap();
        }
        let legacy = savings_path().unwrap();
        std::fs::write(
            &legacy,
            format!("{}\n", json!({"ts":day1,"saved":99,"tools":2})),
        )
        .unwrap();
        let entries = entries();
        let all = HashSet::from(["alpha".into(), "bravo".into()]);
        let first =
            crate::usage_report::rollup(&crate::usage_report::utc_day(day1), &[], &entries, &all);
        let second =
            crate::usage_report::rollup(&crate::usage_report::utc_day(day2), &[], &entries, &all);
        assert_eq!(first["alpha"].tokens_saved, 10);
        assert_eq!(first["bravo"].tokens_saved, 20);
        assert_eq!(second["alpha"].tokens_saved, 30);
        assert!(!second.contains_key("bravo"));
        assert_eq!(summary()["estimatedTokensAvoided"], 60);
        assert_eq!(summary()["legacyEstimatedTokensAvoided"], 99);
        assert_eq!(summary()["discoveryCount"], 1);
        try_clear().unwrap();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn append_after_rotation_snapshot_is_not_lost() {
        let _guard = crate::registry::data_dir_test_lock();
        let dir =
            std::env::temp_dir().join(format!("toolport-savings-lock-{}", std::process::id()));
        let path = dir.join("savings-v2.jsonl");
        let row = |ts| json!({"v":2,"kind":"catalog_exposure","ts":ts,"estimatedTokensAvoided":1});
        append_line_at(&path, &row(1), 1, 1, None).unwrap();
        let (at_snapshot_tx, at_snapshot_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let first_path = path.clone();
        let first = std::thread::spawn(move || {
            let mut hook = || {
                at_snapshot_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            };
            append_line_at(&first_path, &row(2), 1, 1, Some(&mut hook)).unwrap();
        });
        at_snapshot_rx.recv().unwrap();
        let lock_path = PathBuf::from(format!("{}.lock", path.display()));
        let probe = std::fs::OpenOptions::new()
            .write(true)
            .open(lock_path)
            .unwrap();
        assert!(
            probe.try_lock_exclusive().is_err(),
            "rotation must hold the append lock across its snapshot and replacement"
        );
        let second_path = path.clone();
        let second =
            std::thread::spawn(move || append_line_at(&second_path, &row(3), 1, 1, None).unwrap());
        release_tx.send(()).unwrap();
        first.join().unwrap();
        second.join().unwrap();
        let rows = read_lines(Some(path.clone())).unwrap();
        assert_eq!(aggregate(&rows)["estimatedTokensAvoided"], 3);
        let _ = std::fs::remove_dir_all(dir);
    }
}
