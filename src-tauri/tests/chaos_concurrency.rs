//! P1.7 chaos: many calls at once. The engine must not drop, misroute or corrupt
//! any of them.
//!
//! One hundred calls spread across two servers already overlap as far as each
//! server allows, and must all succeed. Head-of-line blocking within one server,
//! and one hundred parallel calls to one server, are the REL-01 cases that the
//! multiplexed stdio transport (#1019) fixed.
//!
//! Unix only for the shared daemon harness.

#![cfg(unix)]

mod chaos_support;

use std::thread;
use std::time::{Duration, Instant};

use chaos_support::{mock_entry, start_daemon, write_registry, Client, Scratch};
use serde_json::json;

const CATALOG: Duration = Duration::from_secs(60);

/// One client issues `calls` requests without waiting, then collects them all.
fn burst(
    client: &mut Client,
    server: &str,
    calls: i64,
    delay_ms: u64,
) -> Vec<Result<String, String>> {
    let ids: Vec<i64> = (0..calls)
        .map(|_| {
            let tool = format!("{server}__sleep");
            client.call_async(&tool, json!({ "ms": delay_ms }))
        })
        .collect();
    ids.into_iter()
        .map(|id| {
            let reply = client.wait_for_id(id, Duration::from_secs(120));
            if let Some(error) = reply.get("error") {
                return Err(error.to_string());
            }
            if reply["result"]["isError"] == true {
                return Err(reply["result"]["content"][0]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string());
            }
            Ok(reply["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_string())
        })
        .collect()
}

#[test]
fn one_hundred_calls_spread_across_servers_all_succeed() {
    let scratch = Scratch::new("load-spread");
    // Every echo waits 100 ms, so the two servers are each a real bottleneck.
    write_registry(
        scratch.path(),
        &[
            mock_entry("x", &[("MOCK_MCP_CALL_DELAY_MS", "100")]),
            mock_entry("y", &[("MOCK_MCP_CALL_DELAY_MS", "100")]),
        ],
        &["x", "y"],
    );
    let _daemon = start_daemon(scratch.path());

    let mut clients: Vec<Client> = (0..4)
        .map(|index| Client::start(scratch.path(), &format!("c{index}")))
        .collect();
    for client in &mut clients {
        assert!(
            client.wait_for_tool("x__echo", CATALOG) && client.wait_for_tool("y__echo", CATALOG),
            "{}",
            client.diagnostics()
        );
    }

    let started = Instant::now();
    let workers: Vec<_> = clients
        .into_iter()
        .enumerate()
        .map(|(index, mut client)| {
            thread::spawn(move || {
                // 25 calls per client, alternating servers: 100 in total.
                let mut results = Vec::new();
                let mut ids = Vec::new();
                for call in 0..25 {
                    let server = if (index + call) % 2 == 0 { "x" } else { "y" };
                    let tool = format!("{server}__echo");
                    let text = format!("c{index}-{call}");
                    // Echo returns its argument, so a misroute is visible.
                    let id = client.send_request(
                        "tools/call",
                        json!({ "name": tool, "arguments": { "text": text } }),
                    );
                    ids.push((id, text));
                }
                for (id, text) in ids {
                    let reply = client.wait_for_id(id, Duration::from_secs(120));
                    if reply["result"]["isError"] == true {
                        results.push(Err(reply["result"]["content"][0]["text"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string()));
                    } else {
                        results.push(Ok((
                            text,
                            reply["result"]["content"][0]["text"]
                                .as_str()
                                .unwrap_or_default()
                                .to_string(),
                        )));
                    }
                }
                results
            })
        })
        .collect();

    let mut total = 0;
    for worker in workers {
        for result in worker.join().expect("worker") {
            match result {
                Ok((expected, got)) => {
                    assert!(
                        got.starts_with(&expected),
                        "a call came back misrouted or corrupt: expected {expected}, got {got}"
                    );
                    total += 1;
                }
                Err(error) => panic!("a call under load failed: {error}"),
            }
        }
    }
    assert_eq!(total, 100, "all 100 calls must return");
    // Today each server serializes its share, so this is a liveness bound, not a
    // latency one: the point is that nothing is lost or corrupted.
    assert!(
        started.elapsed() < Duration::from_secs(60),
        "100 calls took {:?}",
        started.elapsed()
    );
}

/// REL-01: while one call to a server is in flight, a second call to the SAME
/// server must come back at once (REL-01).
#[test]
fn a_slow_call_does_not_block_a_fast_call_to_the_same_server() {
    let scratch = Scratch::new("hol");
    write_registry(
        scratch.path(),
        &[mock_entry("x", &[("MOCK_MCP_CONCURRENT", "1")])],
        &["x"],
    );
    let _daemon = start_daemon(scratch.path());
    let mut client = Client::start(scratch.path(), "a");
    assert!(
        client.wait_for_tool("x__sleep", CATALOG),
        "{}",
        client.diagnostics()
    );

    // A 20 s call is in flight.
    let slow = client.call_async("x__sleep", json!({ "ms": 20000 }));
    std::thread::sleep(Duration::from_millis(1000));

    let started = Instant::now();
    let fast = client.call("x__echo", json!({ "text": "fast" }));
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(1),
        "a fast call queued behind a slow one: {elapsed:?}"
    );
    assert!(
        chaos_support::reply_ok(&fast, "fast"),
        "the fast call came back wrong: {fast}"
    );

    // The slow call is left in flight; the harness tears it down on drop.
    let _ = slow;
}

/// REL-01: all one hundred calls must arrive before the mock releases any reply.
#[test]
fn one_hundred_parallel_calls_to_one_server_overlap() {
    let scratch = Scratch::new("load-one");
    write_registry(
        scratch.path(),
        &[mock_entry(
            "x",
            &[
                ("MOCK_MCP_CONCURRENT", "1"),
                ("MOCK_MCP_SLEEP_BARRIER", "100"),
            ],
        )],
        &["x"],
    );
    let _daemon = start_daemon(scratch.path());

    let mut clients: Vec<Client> = (0..4)
        .map(|index| Client::start(scratch.path(), &format!("c{index}")))
        .collect();
    for client in &mut clients {
        assert!(
            client.wait_for_tool("x__sleep", CATALOG),
            "{}",
            client.diagnostics()
        );
    }

    let workers: Vec<_> = clients
        .into_iter()
        .map(|mut client| {
            thread::spawn(move || {
                // 25 per client: 100 together, the auditor's s10 shape.
                burst(&mut client, "x", 25, 200)
            })
        })
        .collect();

    let mut total = 0;
    for worker in workers {
        for result in worker.join().expect("worker") {
            match result {
                Ok(_) => total += 1,
                Err(error) => panic!("a parallel call failed: {error}"),
            }
        }
    }
    assert_eq!(total, 100);
}
