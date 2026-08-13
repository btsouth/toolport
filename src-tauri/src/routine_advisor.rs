//! Session-local observation of direct tool calls and deterministic fan-out synthesis.
//!
//! Capable clients rationally skip Code Mode at small fan-outs: they parallelize direct
//! calls natively and the per-call cost lands on the user's context window, not on the
//! model making the choice. The gateway, however, sees the repetition. This module keeps
//! a short-lived per-caller window of direct downstream calls, detects the one pattern
//! that is mechanically liftable — k calls to the same tool whose arguments differ in a
//! few fields — and synthesizes a parameterized orchestration draft (source, input
//! schema, and an input example) without any model involvement.
//!
//! The draft is material, not authority. The gateway statically validates it, mints a
//! regular routine candidate whose provenance says `synthesized_from_observed_calls`,
//! and surfaces one bounded hint to the caller. Persisting still requires the standard
//! promotion approval; nothing here writes to disk or grants permissions.
//!
//! Everything in this ledger is process-local, bounded, and expiring: recorded argument
//! values never outlive [`WINDOW_TTL`], never enter audit events, and are dropped
//! entirely for oversized payloads.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

/// Trailing same-tool calls needed before a pattern is worth mentioning. Below this,
/// direct calls are legitimately competitive (industry measurements agree: programmatic
/// calling at 1-2 calls per turn saves nothing), so hinting would be noise.
pub const MIN_FAN_OUT: usize = 3;
/// Most recent direct calls remembered per caller.
pub const MAX_WINDOW: usize = 24;
/// How long a recorded call stays usable as pattern evidence.
pub const WINDOW_TTL: Duration = Duration::from_secs(15 * 60);
/// Callers tracked at once; the oldest window is dropped beyond this.
pub const MAX_CALLERS: usize = 64;
/// Advisor hints one caller can receive per gateway process. A model that ignores the
/// first few will ignore the rest; repeating would be approval-nagging's little sibling.
pub const MAX_HINTS_PER_CALLER: usize = 4;
/// Arguments larger than this are recorded without their value and break pattern runs;
/// hoarding megabyte payloads in a hint ledger would be memory abuse for marginal gain.
pub const MAX_TRACKED_ARG_BYTES: usize = 16 * 1024;
/// Same-tool calls further apart than this belong to different bursts. Burst separation
/// otherwise requires a ledger-visible interruption (another direct call, a failure) -
/// but two user tasks separated by reading, typing, or routine/meta-tool work leave no
/// such trace, and repetition evidence would silently never accumulate.
pub const BURST_GAP: Duration = Duration::from_secs(90);
/// A varying-field set wider than this stops looking like one parameterized operation.
const MAX_VARYING_FIELDS: usize = 3;

/// One direct, model-visible downstream call as the ledger sees it.
#[derive(Debug, Clone)]
pub struct ObservedCall {
    pub tool: String,
    pub arguments: Value,
    pub ok: bool,
    pub result_bytes: usize,
}

/// The synthesized orchestration for a detected fan-out.
#[derive(Debug, Clone)]
pub struct SynthesizedDraft {
    pub source: String,
    pub input_schema: Value,
    /// The observed varying values, shaped as the schema's `input` — a ready-to-run
    /// example. Short-lived by construction: it is handed out through an expiring
    /// cursor and never persisted or audited.
    pub input_example: Value,
}

/// Which message, if any, this occurrence may carry. Evidence accumulates on every
/// occurrence regardless; the slot only rations the caller-visible text so one pattern
/// speaks exactly once - when first seen. Repetition-driven promotion no longer talks
/// to the model at all: measured conversion of result-embedded directives was zero,
/// so strong candidates surface in the desktop app instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintSlot {
    /// First sighting of the pattern: the informational hint (draft + numbers).
    Informational,
    /// Mint evidence, say nothing.
    Silent,
}

/// A newly detected fan-out burst.
#[derive(Debug, Clone)]
pub struct FanOutPattern {
    pub tool: String,
    pub calls: usize,
    pub varying_fields: Vec<String>,
    pub intermediate_bytes: usize,
    pub pattern_key: String,
    /// 1 for the first burst of this pattern in the session, 2 for the next, ...
    /// Distinct bursts are separated by a broken run; a growing burst re-fires nothing.
    pub occurrence: usize,
    pub hint: HintSlot,
    pub draft: SynthesizedDraft,
}

#[derive(Debug, Clone)]
struct Entry {
    tool: String,
    /// `None` when the arguments were not a small JSON object; such calls still occupy
    /// the window (they break runs) but can never contribute to synthesis.
    arguments: Option<Value>,
    ok: bool,
    result_bytes: usize,
    at_ms: u128,
    /// Ledger-wide monotonic sequence, the unambiguous burst identity (timestamps
    /// collide within a millisecond under load and in tests).
    seq: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct PatternState {
    /// Distinct bursts seen for this (caller, pattern) pair.
    bursts: usize,
    /// `seq` of the first entry of the burst that last produced a detection, so a
    /// still-growing burst (call 4, 5, ... of the same fan-out) is one occurrence,
    /// not inflated repetition evidence.
    last_burst_start_seq: u64,
}

#[derive(Debug, Default)]
struct State {
    seq: u64,
    windows: HashMap<String, VecDeque<Entry>>,
    window_order: VecDeque<String>,
    patterns: HashMap<(String, String), PatternState>,
    pattern_order: VecDeque<(String, String)>,
    hint_counts: HashMap<String, usize>,
    hint_count_order: VecDeque<String>,
}

#[derive(Debug, Clone, Default)]
pub struct AdvisorLedger {
    state: Arc<Mutex<State>>,
}

impl AdvisorLedger {
    /// Record one direct call and return a pattern once per distinct burst: the first
    /// time a fresh fan-out crosses [`MIN_FAN_OUT`], and again for each later burst of
    /// the same shape after the run was broken in between. The [`HintSlot`] rations
    /// what may be said; the returned evidence is minted every time regardless, so
    /// repetition accumulates toward a strong recommendation without re-prompting.
    pub fn record(&self, caller: &str, call: ObservedCall) -> Option<FanOutPattern> {
        self.record_at(caller, call, now_ms())
    }

    /// Timestamp-injectable body of [`Self::record`], so tests can exercise the burst
    /// gap without sleeping through it.
    fn record_at(&self, caller: &str, call: ObservedCall, now: u128) -> Option<FanOutPattern> {
        let arguments = match &call.arguments {
            Value::Object(_)
                if serde_json::to_vec(&call.arguments)
                    .map(|bytes| bytes.len() <= MAX_TRACKED_ARG_BYTES)
                    .unwrap_or(false) =>
            {
                Some(call.arguments.clone())
            }
            _ => None,
        };

        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let state = &mut *state;
        reserve_tracked_key(
            &mut state.windows,
            &mut state.window_order,
            MAX_CALLERS,
            &caller.to_string(),
        );
        let seq = state.seq;
        state.seq += 1;
        let window = state.windows.entry(caller.to_string()).or_default();
        window.retain(|entry| now.saturating_sub(entry.at_ms) < WINDOW_TTL.as_millis());
        window.push_back(Entry {
            tool: call.tool.clone(),
            arguments,
            ok: call.ok,
            result_bytes: call.result_bytes,
            at_ms: now,
            seq,
        });
        while window.len() > MAX_WINDOW {
            window.pop_front();
        }

        // The trailing run: consecutive successful, argument-bearing calls to one tool,
        // with no [`BURST_GAP`]-sized silence inside - a long pause separates two task
        // instances even when nothing else touched the ledger in between.
        let mut run: Vec<&Entry> = Vec::new();
        let mut newer_at: Option<u128> = None;
        for entry in window.iter().rev() {
            if entry.tool != call.tool || !entry.ok || entry.arguments.is_none() {
                break;
            }
            if let Some(newer) = newer_at {
                if newer.saturating_sub(entry.at_ms) > BURST_GAP.as_millis() {
                    break;
                }
            }
            newer_at = Some(entry.at_ms);
            run.push(entry);
        }
        if run.len() < MIN_FAN_OUT {
            return None;
        }
        let run: Vec<&Entry> = run.into_iter().rev().collect();

        let arguments: Vec<&Map<String, Value>> = run
            .iter()
            .filter_map(|entry| entry.arguments.as_ref().and_then(Value::as_object))
            .collect();
        let split = split_arguments(&arguments)?;
        // No credential screen here anymore: synthesis lifts EVERY field into `input`,
        // so the persistent source and schema carry field names only, never observed
        // values. Values exist solely in the ephemeral input example.
        let pattern_key = pattern_key(&call.tool, &split);
        let hint_key = (caller.to_string(), pattern_key.clone());
        let burst_start_seq = run.first().map(|entry| entry.seq).unwrap_or(seq);
        let previous = state.patterns.get(&hint_key).copied();
        if let Some(previous) = previous {
            // The same burst growing past the threshold is still one occurrence; only
            // a burst that started after the last detection counts as repetition.
            if previous.last_burst_start_seq == burst_start_seq {
                return None;
            }
        }

        let occurrence = previous.map(|state| state.bursts).unwrap_or(0) + 1;
        let hint = if occurrence == 1 {
            // The informational hint shares the per-caller budget; an exhausted budget
            // still mints evidence silently.
            let count = state.hint_counts.get(caller).copied().unwrap_or(0);
            if count < MAX_HINTS_PER_CALLER {
                reserve_tracked_key(
                    &mut state.hint_counts,
                    &mut state.hint_count_order,
                    MAX_CALLERS,
                    &caller.to_string(),
                );
                *state.hint_counts.entry(caller.to_string()).or_default() = count + 1;
                HintSlot::Informational
            } else {
                HintSlot::Silent
            }
        } else {
            // Repeat bursts accumulate evidence silently; conversion happens through
            // the desktop app's suggestion area, not through the model.
            HintSlot::Silent
        };

        let draft = synthesize(&call.tool, &arguments, &split);
        reserve_tracked_key(
            &mut state.patterns,
            &mut state.pattern_order,
            MAX_CALLERS * 8,
            &hint_key,
        );
        let entry = state.patterns.entry(hint_key).or_default();
        entry.bursts = occurrence;
        entry.last_burst_start_seq = burst_start_seq;

        Some(FanOutPattern {
            tool: call.tool,
            calls: run.len(),
            varying_fields: split.varying.clone(),
            intermediate_bytes: run.iter().map(|entry| entry.result_bytes).sum(),
            pattern_key,
            occurrence,
            hint,
            draft,
        })
    }
}

/// Keeps a tracking map bounded, evicting oldest entries first. Same contract as the
/// candidate registry's helper: every removal keeps `order` in step with `map`.
fn reserve_tracked_key<K, V>(map: &mut HashMap<K, V>, order: &mut VecDeque<K>, cap: usize, key: &K)
where
    K: Eq + std::hash::Hash + Clone,
{
    if map.contains_key(key) {
        return;
    }
    while map.len() >= cap {
        let Some(oldest) = order.pop_front() else {
            break;
        };
        map.remove(&oldest);
    }
    order.push_back(key.clone());
}

struct ArgSplit {
    /// Keys whose values differ across the run, in sorted order.
    varying: Vec<String>,
    /// Keys whose values are identical across the run, with that shared value.
    fixed: Vec<(String, Value)>,
}

/// Partition argument keys into varying and fixed, or `None` when the calls do not
/// form one parameterizable operation (mismatched key sets, too many degrees of
/// freedom, heterogeneous types, or nothing actually varying).
fn split_arguments(arguments: &[&Map<String, Value>]) -> Option<ArgSplit> {
    let first = arguments.first()?;
    let mut keys: Vec<&String> = first.keys().collect();
    keys.sort();
    for other in arguments.iter().skip(1) {
        let mut other_keys: Vec<&String> = other.keys().collect();
        other_keys.sort();
        if other_keys != keys {
            return None;
        }
    }

    let mut varying = Vec::new();
    let mut fixed = Vec::new();
    for key in keys {
        let reference = first.get(key)?;
        if arguments
            .iter()
            .all(|args| args.get(key) == Some(reference))
        {
            fixed.push((key.clone(), reference.clone()));
            continue;
        }
        // A varying field must keep one JSON type, or the derived schema would be a
        // union the validator (and the model) cannot use confidently.
        if arguments.iter().any(|args| {
            args.get(key)
                .is_none_or(|value| json_kind(value) != json_kind(reference))
        }) {
            return None;
        }
        varying.push(key.clone());
    }
    if varying.is_empty() || varying.len() > MAX_VARYING_FIELDS {
        return None;
    }
    Some(ArgSplit { varying, fixed })
}

fn json_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Stable identity for one (tool, key set) shape. Deliberately independent of both the
/// observed values AND the varying/fixed partition: synthesis lifts every field into
/// `input`, so two bursts over the same key set produce byte-identical source no matter
/// which fields happened to vary - the property repetition counting depends on.
fn pattern_key(tool: &str, split: &ArgSplit) -> String {
    let mut keys: Vec<&String> = split
        .varying
        .iter()
        .chain(split.fixed.iter().map(|(key, _)| key))
        .collect();
    keys.sort();
    let mut hasher = Sha256::new();
    hasher.update(tool.as_bytes());
    hasher.update([0x1e]);
    for key in keys {
        hasher.update(key.as_bytes());
        hasher.update([0x1f]);
    }
    format!("pattern_{:x}", hasher.finalize())
}

/// Deterministically render the parameterized orchestration. `toolport.callAsync` is
/// used over the typed `servers.*` surface so the source never depends on identifier
/// sanitization, and `item[...]` indexing keeps arbitrary field names valid JavaScript.
///
/// EVERY observed field is lifted into `input`, including ones that never varied in
/// this burst. Two invariants depend on that:
/// - fingerprint stability: the source carries field names only, so a later burst with
///   a different "fixed" value (a new question, a new limit) still hashes to the same
///   definition and repetition evidence accumulates;
/// - value hygiene: no observed value can ever reach the persistent source or schema;
///   values live solely in the ephemeral input example.
fn synthesize(tool: &str, arguments: &[&Map<String, Value>], split: &ArgSplit) -> SynthesizedDraft {
    let mut keys: Vec<String> = split
        .varying
        .iter()
        .cloned()
        .chain(split.fixed.iter().map(|(key, _)| key.clone()))
        .collect();
    keys.sort();
    let single = keys.len() == 1;

    let lines: Vec<String> = keys
        .iter()
        .map(|key| {
            let expr = if single {
                "item".to_string()
            } else {
                format!("item[{}]", js_string(key))
            };
            format!("    {}: {}", js_string(key), expr)
        })
        .collect();
    // No occurrence-specific material (like the observed call count) may enter the
    // source: the definition fingerprint hashes it, and repetition detection needs a
    // burst of 3 and a burst of 5 to resolve to the SAME definition.
    //
    // The tool name appears ONLY as the escaped callAsync argument, never in the
    // comment: it comes from a downstream server, and a name containing a newline
    // would otherwise terminate the comment and inject the rest as executable
    // source into a definition a human is asked to approve.
    let source = format!(
        "// Synthesized by Toolport from observed calls to the tool named below.\n\
         const results = await Promise.all(input.items.map((item) => toolport.callAsync({tool_name}, {{\n\
         {args}\n\
         }})));\n\
         return results;",
        tool_name = js_string(tool),
        args = lines.join(",\n"),
    );

    let kind_of = |key: &str| {
        arguments
            .first()
            .and_then(|args| args.get(key))
            .map(json_kind)
            .unwrap_or("string")
    };
    let items_schema = if single {
        let key = &keys[0];
        json!({
            "type": kind_of(key),
            "description": format!("Each entry becomes the {key} argument of one {tool} call."),
        })
    } else {
        let mut properties = Map::new();
        for key in &keys {
            properties.insert(key.clone(), json!({ "type": kind_of(key) }));
        }
        json!({
            "type": "object",
            "properties": properties,
            "required": keys,
            "additionalProperties": false,
        })
    };
    let input_schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "type": "object",
        "properties": {
            "items": { "type": "array", "minItems": 1, "items": items_schema }
        },
        "required": ["items"],
        "additionalProperties": false,
    });

    let items: Vec<Value> = arguments
        .iter()
        .map(|args| {
            if single {
                args.get(&keys[0]).cloned().unwrap_or(Value::Null)
            } else {
                let mut item = Map::new();
                for key in &keys {
                    item.insert(key.clone(), args.get(key).cloned().unwrap_or(Value::Null));
                }
                Value::Object(item)
            }
        })
        .collect();

    SynthesizedDraft {
        source,
        input_schema,
        input_example: json!({ "items": items }),
    }
}

fn js_string(text: &str) -> String {
    serde_json::to_string(&Value::String(text.to_string())).unwrap_or_else(|_| "\"\"".to_string())
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(tool: &str, args: Value) -> ObservedCall {
        ObservedCall {
            tool: tool.to_string(),
            arguments: args,
            ok: true,
            result_bytes: 4096,
        }
    }

    #[test]
    fn three_same_shape_calls_yield_one_pattern_then_suppress() {
        let ledger = AdvisorLedger::default();
        assert!(ledger
            .record("caller", call("d__read", json!({ "repoName": "a/x" })))
            .is_none());
        assert!(ledger
            .record("caller", call("d__read", json!({ "repoName": "b/y" })))
            .is_none());
        let pattern = ledger
            .record("caller", call("d__read", json!({ "repoName": "c/z" })))
            .expect("third same-shape call crosses the threshold");
        assert_eq!(pattern.calls, 3);
        assert_eq!(pattern.occurrence, 1);
        assert_eq!(pattern.hint, HintSlot::Informational);
        assert_eq!(pattern.varying_fields, vec!["repoName"]);
        assert_eq!(pattern.intermediate_bytes, 3 * 4096);
        assert_eq!(
            pattern.draft.input_example,
            json!({ "items": ["a/x", "b/y", "c/z"] })
        );
        assert!(pattern
            .draft
            .source
            .contains("toolport.callAsync(\"d__read\""));
        assert!(pattern.draft.source.contains("\"repoName\": item"));

        // The same burst growing to a fourth call is still one occurrence.
        assert!(ledger
            .record("caller", call("d__read", json!({ "repoName": "d/w" })))
            .is_none());
    }

    #[test]
    fn a_quiet_gap_separates_bursts_without_any_ledger_interruption() {
        // Two user tasks with only reading/typing (or routine calls, which never touch
        // the ledger) in between must still count as repetition.
        let ledger = AdvisorLedger::default();
        let base = 1_000_000_u128;
        let mut at = base;
        let mut send = |value: &str, at_ms: u128| {
            ledger.record_at(
                "caller",
                call("d__read", json!({ "repoName": value })),
                at_ms,
            )
        };
        send("a", at);
        at += 1_000;
        send("b", at);
        at += 1_000;
        let first = send("c", at).expect("first burst");
        assert_eq!((first.occurrence, first.hint), (1, HintSlot::Informational));

        // Well past the gap, still inside the window TTL: a new burst, not growth.
        // Repetition accumulates silently; conversion lives in the desktop app now.
        at += BURST_GAP.as_millis() + 5_000;
        send("d", at);
        at += 1_000;
        send("e", at);
        at += 1_000;
        let second = send("f", at).expect("second burst after quiet gap");
        assert_eq!((second.occurrence, second.hint), (2, HintSlot::Silent));
        assert_eq!(
            first.draft.source, second.draft.source,
            "same shape must keep the same source, or repetition never shares a fingerprint"
        );

        // Small gaps stay one burst: growing it further re-fires nothing.
        at += 1_000;
        assert!(send("g", at).is_none());
    }

    #[test]
    fn changing_a_fixed_field_value_across_bursts_keeps_the_same_definition() {
        // The 2026-08-13 field bug: a fixed field's VALUE was inlined into the source,
        // so a new question next burst meant a new fingerprint and repetition evidence
        // never accumulated. Full lifting makes the definition value-independent.
        let ledger = AdvisorLedger::default();
        let burst = |question: &str, repos: [&str; 3]| {
            let mut last = None;
            for repo in repos {
                last = ledger.record(
                    "caller",
                    call("d__ask", json!({ "repoName": repo, "question": question })),
                );
            }
            last
        };
        let break_run = || {
            let mut failed = call("d__ask", json!({ "repoName": "broken", "question": "x" }));
            failed.ok = false;
            assert!(ledger.record("caller", failed).is_none());
        };

        let first = burst("How is state stored?", ["a", "b", "c"]).expect("first burst");
        assert_eq!((first.occurrence, first.hint), (1, HintSlot::Informational));
        assert!(
            !first.draft.source.contains("How is state stored?"),
            "observed values must never enter the source: {}",
            first.draft.source
        );

        // Second burst asks a DIFFERENT question (the previously-fixed field changed
        // value): the definition must be byte-identical anyway, and stay silent -
        // conversion now lives in the desktop app, not in model-facing escalations.
        break_run();
        let second = burst("How is the build organized?", ["d", "e", "f"]).expect("second");
        assert_eq!((second.occurrence, second.hint), (2, HintSlot::Silent));
        assert_eq!(first.draft.source, second.draft.source);
        assert_eq!(first.draft.input_schema, second.draft.input_schema);
        assert_eq!(first.pattern_key, second.pattern_key);

        // The examples DO differ - values live only there.
        assert_ne!(first.draft.input_example, second.draft.input_example);
    }

    #[test]
    fn all_fields_lift_into_input_including_ones_that_never_varied() {
        let ledger = AdvisorLedger::default();
        for (library, topic) in [
            ("react", "hooks"),
            ("vue", "reactivity"),
            ("svelte", "runes"),
        ] {
            let result = ledger.record(
                "caller",
                call(
                    "c__query",
                    json!({ "library": library, "topic": topic, "limit": 5 }),
                ),
            );
            if library == "svelte" {
                let pattern = result.expect("threshold");
                let mut varying = pattern.varying_fields.clone();
                varying.sort();
                assert_eq!(varying, vec!["library", "topic"]);
                // The fixed `limit` is a parameter too: its VALUE stays out of the
                // source and schema, and rides only in the input example.
                assert!(pattern.draft.source.contains("item[\"limit\"]"));
                assert!(!pattern.draft.source.contains(": 5"));
                let schema = &pattern.draft.input_schema["properties"]["items"]["items"];
                assert_eq!(schema["type"], "object");
                assert_eq!(schema["properties"]["topic"]["type"], "string");
                assert_eq!(schema["properties"]["limit"]["type"], "number");
                assert!(schema["required"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("limit")));
                assert_eq!(
                    pattern.draft.input_example["items"][0],
                    json!({ "library": "react", "topic": "hooks", "limit": 5 })
                );
            } else {
                assert!(result.is_none());
            }
        }
    }

    #[test]
    fn runs_break_on_failures_other_tools_and_shape_changes() {
        let ledger = AdvisorLedger::default();
        ledger.record("caller", call("d__read", json!({ "repoName": "a" })));
        ledger.record("caller", call("d__read", json!({ "repoName": "b" })));
        // A failed call resets the trailing run.
        let mut failed = call("d__read", json!({ "repoName": "c" }));
        failed.ok = false;
        assert!(ledger.record("caller", failed).is_none());
        assert!(ledger
            .record("caller", call("d__read", json!({ "repoName": "d" })))
            .is_none());

        // Mismatched key sets never form a pattern.
        let other = AdvisorLedger::default();
        other.record("caller", call("t__x", json!({ "a": 1 })));
        other.record("caller", call("t__x", json!({ "a": 2 })));
        assert!(other
            .record("caller", call("t__x", json!({ "a": 3, "b": 1 })))
            .is_none());

        // Identical calls have no varying field to lift.
        let same = AdvisorLedger::default();
        same.record("caller", call("t__y", json!({ "a": 1 })));
        same.record("caller", call("t__y", json!({ "a": 1 })));
        assert!(same
            .record("caller", call("t__y", json!({ "a": 1 })))
            .is_none());
    }

    #[test]
    fn secret_values_never_reach_source_or_schema() {
        let ledger = AdvisorLedger::default();
        for repo in ["a", "b", "c"] {
            let result = ledger.record(
                "caller",
                call(
                    "d__read",
                    json!({ "repoName": repo, "token": "ghp_0123456789abcdef0123456789abcdef0123" }),
                ),
            );
            if repo == "c" {
                let pattern = result.expect("full lifting makes token-bearing patterns safe");
                let source = &pattern.draft.source;
                let schema = serde_json::to_string(&pattern.draft.input_schema).unwrap();
                assert!(
                    !source.contains("ghp_"),
                    "value leaked into source: {source}"
                );
                assert!(
                    !schema.contains("ghp_"),
                    "value leaked into schema: {schema}"
                );
                assert!(source.contains("item[\"token\"]"));
                // The ephemeral example is the only place the value lives.
                assert!(serde_json::to_string(&pattern.draft.input_example)
                    .unwrap()
                    .contains("ghp_"));
            } else {
                assert!(result.is_none());
            }
        }
    }

    #[test]
    fn hints_are_caller_isolated_and_budgeted() {
        let ledger = AdvisorLedger::default();
        for index in 0..MAX_HINTS_PER_CALLER + 1 {
            let tool = format!("t__tool{index}");
            for value in ["a", "b"] {
                ledger.record("caller-a", call(&tool, json!({ "field": value })));
            }
            let third = ledger
                .record("caller-a", call(&tool, json!({ "field": "c" })))
                .expect("evidence is minted with or without hint budget");
            if index < MAX_HINTS_PER_CALLER {
                assert_eq!(third.hint, HintSlot::Informational, "hint {index}");
            } else {
                assert_eq!(
                    third.hint,
                    HintSlot::Silent,
                    "budget exhausted stays silent"
                );
            }
        }
        // A different caller has its own window and budget.
        for value in ["a", "b"] {
            ledger.record("caller-b", call("t__tool0", json!({ "field": value })));
        }
        assert_eq!(
            ledger
                .record("caller-b", call("t__tool0", json!({ "field": "c" })))
                .unwrap()
                .hint,
            HintSlot::Informational
        );
    }

    #[test]
    fn oversized_arguments_are_not_retained_and_break_patterns() {
        let ledger = AdvisorLedger::default();
        let huge = "x".repeat(MAX_TRACKED_ARG_BYTES + 1);
        ledger.record("caller", call("t__big", json!({ "field": "a" })));
        ledger.record("caller", call("t__big", json!({ "field": "b" })));
        assert!(ledger
            .record("caller", call("t__big", json!({ "field": huge })))
            .is_none());
    }
}
