//! REL-01: one agent's slow call must not block other agents' calls to the same
//! server.
//!
//! Drives a Router over the real `mock-mcp-server` binary in its concurrent mode,
//! which handles each request on its own thread and answers in completion order.
//! While one call sleeps for 20 s, a second call to the same server must come back
//! at once, and 100 parallel 200 ms calls must overlap rather than queue.

use std::sync::atomic::AtomicU8;
use std::sync::Arc;
use std::time::{Duration, Instant};

use conduit_lib::downstream::{CancelRegistry, DownstreamServer, StdioTransport};
use conduit_lib::router::Router;
use serde_json::json;

fn concurrent_mock_router() -> Arc<Router> {
    let mock = env!("CARGO_BIN_EXE_mock-mcp-server");
    let env = [("MOCK_MCP_CONCURRENT".to_string(), "1".to_string())];
    let transport =
        StdioTransport::spawn_watched(mock, &[], &env, None, Arc::new(AtomicU8::new(0)), None)
            .expect("spawn mock");
    let server =
        DownstreamServer::connect("mock".to_string(), Box::new(transport)).expect("connect mock");
    let mut router = Router::new();
    router.add(server);
    Arc::new(router)
}

#[test]
fn a_fast_call_is_not_blocked_behind_a_slow_call_to_the_same_server() {
    let router = concurrent_mock_router();
    let cancellations = CancelRegistry::new();
    assert!(cancellations.begin_client_request("slow".to_string()));
    let slow = {
        let router = Arc::clone(&router);
        let cancel = cancellations.context("slow".to_string());
        std::thread::spawn(move || {
            router.route_call_with_cancel(
                "mock__sleep",
                json!({ "ms": 20_000 }),
                Some(cancel),
                None,
            )
        })
    };
    // Give the slow call time to reach the server.
    std::thread::sleep(Duration::from_millis(200));

    let started = Instant::now();
    let fast = router
        .route_call("mock__echo", json!({ "text": "hi" }))
        .expect("fast call");
    let elapsed = started.elapsed();
    println!("REL-01: fast call took {elapsed:?} while a 20 s call was in flight");
    assert_eq!(fast["content"][0]["text"], "hi");
    assert!(
        !slow.is_finished(),
        "the slow call should still be in flight"
    );
    // The target is under 50 ms; the bound leaves room for a loaded CI runner while
    // still failing if the call queued behind the 20 s one.
    assert!(
        elapsed < Duration::from_secs(1),
        "fast call took {elapsed:?}"
    );

    // Cancelling frees the slow call's thread at once instead of after 20 s.
    let started = Instant::now();
    assert!(cancellations.cancel("slow", Some("test done")));
    let result = slow.join().unwrap();
    assert!(result.is_err(), "the cancelled call must not succeed");
    assert!(started.elapsed() < Duration::from_secs(5));
    cancellations.finish_client_request("slow");
}

#[test]
fn parallel_calls_to_one_server_overlap() {
    let router = concurrent_mock_router();
    let started = Instant::now();
    let calls: Vec<_> = (0..100)
        .map(|_| {
            let router = Arc::clone(&router);
            std::thread::spawn(move || router.route_call("mock__sleep", json!({ "ms": 200 })))
        })
        .collect();
    for call in calls {
        let result = call.join().unwrap().expect("sleep call");
        assert_eq!(result["content"][0]["text"], "slept 200 ms");
    }
    let elapsed = started.elapsed();
    println!("REL-01: 100 parallel 200 ms calls took {elapsed:?}");
    // Serialized, these take 20 s. The target is 200 to 400 ms; the bound
    // tolerates a loaded runner.
    assert!(elapsed < Duration::from_secs(2), "took {elapsed:?}");
}
