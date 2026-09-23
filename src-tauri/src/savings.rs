//! Local MCP catalog exposure measurements and legacy estimates.
//!
//! Every lazy/grouped `tools/list` writes the exact serialized full and exposed
//! tool-array byte sizes for that client. Discovery search response bytes are
//! recorded separately. `tokensSaved` remains a compatibility estimate and
//! preserves old v1 rows without manufacturing exact bytes for them.
//!
//! v1 gateways retain ownership of `savings.jsonl`. New events go to
//! `savings-v2.jsonl`, which old gateways cannot rotate. Its append and rotation
//! share a cross-process lock.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// Rotate v2 detail once it passes this size. Lifetime and daily aggregates
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

/// Delete both savings logs, including carry-forward aggregates (called when the
/// user clears retained activity). Returns `Err` only on a real removal failure; a
/// missing file (nothing to clear) is success. Local and irreversible; the running
/// total resets to zero and the next serve starts a fresh file.
pub fn try_clear() -> std::io::Result<()> {
    let mut first_error = None;
    for path in [savings_path(), v2_path()].into_iter().flatten() {
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
pub fn surface_bytes(tools: &[Value]) -> u64 {
    serialize_surface(tools, |_, _| {})
}

/// Serialize the array once while exposing each element's exact byte length to
/// attribution. This produces the same bytes as serde_json::to_vec(tools).
fn serialize_surface(tools: &[Value], mut on_tool: impl FnMut(&Value, u64)) -> u64 {
    let mut bytes = Vec::new();
    bytes.push(b'[');
    for (index, tool) in tools.iter().enumerate() {
        if index > 0 {
            bytes.push(b',');
        }
        let start = bytes.len();
        serde_json::to_writer(&mut bytes, tool).expect("serde_json::Value serializes");
        on_tool(tool, (bytes.len() - start) as u64);
    }
    bytes.push(b']');
    bytes.len() as u64
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

/// Record the exact surfaces returned by this gateway and by full mode for the
/// same client. `by_server_bytes` is exact omitted downstream definition bytes;
/// its sum need not equal the global difference, which includes meta tools and
/// JSON array punctuation. The legacy team token estimate is apportioned from
/// the global positive avoided estimate, so it never credits extra exposure.
pub fn record_catalog(
    mode: &str,
    client: Option<&str>,
    full: &[Value],
    exposed: &[Value],
    route: impl Fn(&str) -> Option<String>,
) {
    let exposed_names: std::collections::HashSet<&str> = exposed
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .collect();
    let mut by_server_bytes = BTreeMap::<String, u64>::new();
    let full_bytes = serialize_surface(full, |tool, bytes| {
        let Some(name) = tool.get("name").and_then(Value::as_str) else {
            return;
        };
        if exposed_names.contains(name) {
            return;
        }
        let Some(server) = route(name) else {
            return;
        };
        *by_server_bytes.entry(server).or_default() += bytes;
    });
    let exposed_bytes = surface_bytes(exposed);
    let avoided = full_bytes.saturating_sub(exposed_bytes);
    let extra = exposed_bytes.saturating_sub(full_bytes);
    let attributable: u128 = by_server_bytes.values().map(|bytes| *bytes as u128).sum();
    let estimated = estimated_tokens(avoided);
    let mut remaining = estimated;
    let server_count = by_server_bytes.len();
    let by_server: BTreeMap<String, u64> = by_server_bytes
        .iter()
        .enumerate()
        .map(|(index, (server, bytes))| {
            let share = if attributable == 0 {
                0
            } else if index + 1 == server_count {
                remaining
            } else {
                ((estimated as u128 * *bytes as u128) / attributable) as u64
            };
            remaining = remaining.saturating_sub(share);
            (server.clone(), share)
        })
        .collect();
    let mut row = json!({
        "v": 2, "kind": "catalog_exposure", "ts": epoch_millis() as u64,
        "mode": mode, "fullToolCount": full.len(), "exposedToolCount": exposed.len(),
        "fullSurfaceBytes": full_bytes, "exposedSurfaceBytes": exposed_bytes,
        "avoidedSurfaceBytes": avoided,
        "extraExposedSurfaceBytes": extra,
        "surfaceDeltaBytes": full_bytes as i64 - exposed_bytes as i64,
        "estimatedTokensAvoided": estimated,
        "estimateMethod": ESTIMATE_METHOD,
        "byServerBytes": by_server_bytes, "byServer": by_server,
    });
    if let Some(client) = client.filter(|client| !client.is_empty()) {
        row["client"] = json!(client);
    }
    append_line(&row);
}

/// Search content is measured after composing the text returned by tools/call.
pub fn record_discovery(content_bytes: u64, matched_schema_bytes: u64) {
    append_line(&json!({
        "v": 2, "kind": "discovery_response", "ts": epoch_millis() as u64,
        "responseContentBytes": content_bytes,
        "matchedSchemaBytes": matched_schema_bytes,
        "estimatedResponseTokens": estimated_tokens(content_bytes),
        "estimateMethod": ESTIMATE_METHOD,
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
    if let Some(path) = v2_path() {
        let _ = append_line_at(&path, entry, MAX_SAVINGS_BYTES, KEEP_LINES, None);
    }
}

fn append_line_at(
    path: &Path,
    entry: &Value,
    max_bytes: u64,
    keep_lines: usize,
    after_snapshot: Option<&mut dyn FnMut()>,
) -> Result<(), String> {
    let _lock = crate::registry::lock_at(path)?;
    let mut file = crate::registry::open_append_private(path).map_err(|e| e.to_string())?;
    file.write_all(format!("{entry}\n").as_bytes())
        .map_err(|e| e.to_string())?;
    let size = file.metadata().map_err(|e| e.to_string())?.len();
    drop(file);
    if size > max_bytes {
        rotate_if_large(path, keep_lines, after_snapshot);
    }
    Ok(())
}

/// Collapse old lines into a single carry line once the log exceeds the cap, so
/// the running total is preserved while the file stays bounded. Best-effort.
fn rotate_if_large(path: &Path, keep_lines: usize, mut after_snapshot: Option<&mut dyn FnMut()>) {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return,
    };
    let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() <= keep_lines {
        return;
    }
    let Ok(parsed): Result<Vec<Value>, _> = lines
        .iter()
        .map(|line| serde_json::from_str(line))
        .collect()
    else {
        // Preserve damaged history for explicit recovery instead of folding it
        // away during a later append.
        return;
    };
    if parsed.iter().any(|row| !row.is_object()) {
        return;
    }
    let mut details: Vec<Value> = parsed
        .iter()
        .filter(|row| row.get("kind").and_then(Value::as_str) != Some("team_daily"))
        .cloned()
        .collect();
    if details.len() <= keep_lines {
        return;
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
    let _ = crate::registry::atomic_write(path, &out);
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
    json!({ "v": 2, "kind": "carry", "ts": since, "saved": saved, "legacyTokensSaved": saved, "tools": peak, "loads": loads, "roundTripsSaved": round_trips,
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
    let mut old = read_lines(savings_path())?;
    if old.iter().any(|row| row["v"] == 2) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected v2 telemetry in legacy savings.jsonl",
        ));
    }
    if let Some(path) = v2_path() {
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
    json!({
        "tokensSaved": folded["saved"].as_u64().unwrap_or(0).saturating_add(folded["estimatedTokensAvoided"].as_u64().unwrap_or(0)),
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
    use super::*;
    use fs2::FileExt;
    use std::collections::HashSet;

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
        assert_eq!(s["tokensSaved"], 200); // 100 + 60 + 40
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
        assert_eq!(s["tokensSaved"], 300); // total survives the fold
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
        assert_eq!(s["tokensSaved"], 60);
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
        assert_eq!(s["tokensSaved"], 100);
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
        assert_eq!(
            after["tokensSaved"],
            1_342_400 + estimated_tokens(full_bytes - exposed_bytes)
        );
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
        assert_eq!(summary["tokensSaved"], 3_692_944_923u64);
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
        assert_eq!(s["tokensSaved"], 200);
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
        assert_eq!(summary()["tokensSaved"], 0);
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
            "lazy",
            Some("new"),
            &[json!({"name":"alpha__x","description":"long"})],
            &[json!({"name":"toolport_status"})],
            |_| Some("alpha".into()),
        );
        record_discovery(101, 50);
        let v2 = v2_path().unwrap();
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
        record_discovery(44, 12);
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
        let v2 = v2_path().unwrap();
        assert_eq!(try_summary().unwrap()["listLoads"], 0);
        std::fs::write(&legacy, "{\"ts\":1,\"saved\":80,\"tools\":5}\n").unwrap();
        assert!(!v2.exists());
        assert_eq!(try_summary().unwrap()["legacyEstimatedTokensAvoided"], 80);
        assert_eq!(try_summary().unwrap()["measuredLoads"], 0);
        std::fs::remove_file(&legacy).unwrap();
        record_catalog(
            "lazy",
            None,
            &[json!({"name":"alpha__long","description":"long"})],
            &[json!({"name":"toolport_status"})],
            |_| Some("alpha".into()),
        );
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
        let v2 = v2_path().unwrap();
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
        let path = v2_path().unwrap();
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
