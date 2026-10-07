//! Opt-in daemon throughput measurement using the real chaos harness.
//! Run with --test throughput_benchmark -- --ignored --nocapture.
#![cfg(unix)]

mod chaos_support;

use chaos_support::{mock_entry, start_daemon, write_registry, Client, Scratch};
use serde_json::json;
use std::thread;
use std::time::{Duration, Instant};

#[test]
#[ignore = "prints throughput timings; no runner-dependent speed assertion"]
fn hundred_call_throughput() {
    let scratch = Scratch::new("throughput");
    write_registry(
        scratch.path(),
        &[mock_entry("x", &[("MOCK_MCP_CONCURRENT", "1")])],
        &["x"],
    );
    let setup = Instant::now();
    let _daemon = start_daemon(scratch.path());
    let mut clients: Vec<_> = (0..4)
        .map(|i| Client::start(scratch.path(), &format!("c{i}")))
        .collect();
    for client in &mut clients {
        assert!(
            client.wait_for_tool("x__sleep", Duration::from_secs(60)),
            "{}",
            client.diagnostics()
        );
    }
    println!(
        "BENCH os={} arch={} profile={} setup_ms={:.3}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        setup.elapsed().as_secs_f64() * 1000.0
    );
    // Report the first burst separately from two repeats on the same daemon.
    for trial in 0..3 {
        let started = Instant::now();
        thread::scope(|scope| {
            let workers: Vec<_> = clients
                .iter_mut()
                .map(|client| {
                    scope.spawn(move || {
                        let ids: Vec<_> = (0..25)
                            .map(|_| client.call_async("x__sleep", json!({ "ms": 200 })))
                            .collect();
                        for id in ids {
                            let reply = client.wait_for_id(id, Duration::from_secs(30));
                            assert!(
                                reply.get("error").is_none() && reply["result"]["isError"] != true,
                                "{reply}"
                            );
                            assert!(
                                reply["result"]["content"][0]["text"]
                                    .as_str()
                                    .is_some_and(|text| text.starts_with("slept 200")),
                                "{reply}"
                            );
                        }
                    })
                })
                .collect();
            for worker in workers {
                worker.join().unwrap();
            }
        });
        println!(
            "BENCH daemon trial={trial} calls=100 delay_ms=200 wall_ms={:.3}",
            started.elapsed().as_secs_f64() * 1000.0
        );
    }
}
