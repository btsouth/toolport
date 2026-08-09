//! Repro from SBS-631: code mode silently corrupted Date/BigInt return values to `{}` or
//! `null` and still reported success. Kept as the ticket filed it so the acceptance
//! criteria stay checkable from outside the crate.
use conduit_lib::codemode::{run_script, Limits};
use serde_json::json;
use std::sync::Arc;

fn run(script: &str) -> conduit_lib::codemode::ScriptOutcome {
    run_script(
        script,
        json!({}),
        Arc::new(|_n, _a| json!({})),
        None,
        Limits::default(),
        &[],
    )
}

/// Code mode must not report success when the script's return value was
/// silently replaced with null or {}.
#[test]
fn date_return_must_preserve_instant_or_error() {
    let outcome = run("return new Date(0);");
    // A successful Date return must carry the instant (ISO string or epoch),
    // not an empty object that looks like success with no data.
    assert!(
        outcome.error.is_some()
            || outcome.value.as_str().is_some()
            || outcome.value.as_i64().is_some()
            || outcome.value.as_f64().is_some()
            || outcome.value.get("$date").is_some()
            || (outcome.value.is_object()
                && outcome
                    .value
                    .as_object()
                    .map(|o| !o.is_empty())
                    .unwrap_or(false)),
        "Date(0) return was silently corrupted: value={:?} error={:?}",
        outcome.value,
        outcome.error
    );
}

#[test]
fn bigint_return_must_not_become_null_success() {
    let outcome = run("return 9007199254740993n;");
    assert!(
        !(outcome.error.is_none() && outcome.value == json!(null)),
        "BigInt became null success: value={:?} error={:?}",
        outcome.value,
        outcome.error
    );
}

#[test]
fn nested_date_in_array_must_not_become_empty_object() {
    let outcome = run("return [new Date(0), 1];");
    assert!(outcome.error.is_none() || outcome.value != json!(null));
    if outcome.error.is_some() {
        return; // fail-closed is acceptable
    }
    let arr = outcome
        .value
        .as_array()
        .expect("successful return should be an array");
    assert_eq!(arr.len(), 2);
    assert!(
        arr[0].as_str().is_some()
            || arr[0].as_i64().is_some()
            || arr[0].as_f64().is_some()
            || (arr[0].is_object() && !arr[0].as_object().unwrap().is_empty()),
        "Date in array corrupted to {:?}",
        arr[0]
    );
}
