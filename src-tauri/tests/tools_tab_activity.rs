//! A Run on a server's Tools tab, as Activity lists it: Toolport's Tools tab as
//! the client, in plain words, and the time the Run took including the start of
//! a server that was not running.

#![cfg(all(unix, feature = "test-support"))]

mod chaos_support;

use std::time::Duration;

use chaos_support::{mock_entry, write_registry, Scratch, GATEWAY};
use serde_json::json;

#[test]
fn a_tools_tab_run_names_toolport_and_counts_the_start() {
    let _lock = conduit_lib::registry::data_dir_test_lock();
    let scratch = Scratch::new("tools-tab-activity");
    write_registry(
        scratch.path(),
        &[mock_entry("slow", &[("MOCK_MCP_START_DELAY_MS", "2000")])],
        &["slow"],
    );
    let _data = conduit_lib::registry::DataDirOverride::set(scratch.path());
    let result = conduit_lib::playground::call_tool_with_gateway(
        GATEWAY.as_ref(),
        "slow",
        "echo",
        json!({"text": "hello"}),
        &conduit_lib::playground::ToolRun::new(),
        |_| {},
    )
    .expect("the Run succeeds");
    assert!(result.to_string().contains("hello"), "{result}");

    // The gateway writes Activity in the background; give it a moment.
    let mut row = None;
    chaos_support::wait_for("the Run in Activity", Duration::from_secs(10), || {
        row = conduit_lib::audit::read_activity(20)
            .unwrap()
            .into_iter()
            .find(|row| row["tool"] == "echo");
        row.is_some()
    });
    let row = row.unwrap();
    assert_eq!(row["clientName"], "Toolport Tools tab", "{row}");
    assert!(
        row.get("clientLabel").is_none(),
        "no reported name beside Toolport's own: {row}"
    );
    let ms = row["durationMs"].as_u64().expect("a duration");
    assert!(ms >= 2000, "the duration counts the server start: {ms} ms");
}
