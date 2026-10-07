//! REL-01: one agent's slow call must not block other agents' calls to the same
//! server.
//!
//! Drives a Router over the real `mock-mcp-server` binary in its concurrent mode,
//! which handles each request on its own thread and answers in completion order.
//! While one call sleeps for 20 s, a second call to the same server must come back
//! at once, and 100 parallel 200 ms calls must overlap rather than queue.
//!
//! Also drives discovery and refresh, which run with no client waiting, against
//! a server that holds its `tools/list` answer until its `roots/list` is answered.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use conduit_lib::downstream::{
    set_request_context_provider, CancelRegistry, DownstreamServer, RequestContext,
    ServerRequestAction, ServerRequestHandler, StdioTransport, Transport,
};
use conduit_lib::router::Router;
use serde_json::{json, Value};

thread_local! {
    /// What the gateway's provider would report for this thread. Threads that do
    /// not set it share one client context, as with no provider at all.
    static REQUEST_CONTEXT: RefCell<RequestContext> =
        const { RefCell::new(RequestContext::Client(String::new())) };
}

fn install_request_context_provider() {
    set_request_context_provider(Arc::new(|| {
        REQUEST_CONTEXT.with(|context| context.borrow().clone())
    }));
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "toolport-multiplexed-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn concurrent_mock_router() -> Arc<Router> {
    concurrent_mock_router_with(&[])
}

fn concurrent_mock_router_with(extra_env: &[(String, String)]) -> Arc<Router> {
    let mock = env!("CARGO_BIN_EXE_mock-mcp-server");
    let mut env = vec![("MOCK_MCP_CONCURRENT".to_string(), "1".to_string())];
    env.extend_from_slice(extra_env);
    let transport = StdioTransport::spawn_watched(
        mock,
        &[],
        &env,
        None,
        false,
        Arc::new(AtomicU8::new(0)),
        None,
    )
    .expect("spawn mock");
    let server =
        DownstreamServer::connect("mock".to_string(), Box::new(transport)).expect("connect mock");
    let mut router = Router::new();
    router.add(server);
    Arc::new(router)
}

/// Wait until the mock's transcript shows a request matching `seen`.
fn wait_for_transcript(path: &std::path::Path, seen: impl Fn(&serde_json::Value) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let reached = std::fs::read_to_string(path).is_ok_and(|transcript| {
            transcript
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .any(|request| seen(&request))
        });
        if reached {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the request never reached the server"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn a_fast_call_is_not_blocked_behind_a_slow_call_to_the_same_server() {
    let dir = temp_dir("rel01");
    let transcript = dir.join("transcript.jsonl");
    let router = concurrent_mock_router_with(&[(
        "MOCK_MCP_TRANSCRIPT".to_string(),
        transcript.to_string_lossy().into_owned(),
    )]);
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
    wait_for_transcript(&transcript, |request| {
        request["params"]["name"] == "sleep" && request["params"]["arguments"]["ms"] == 20_000
    });

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
    let _ = std::fs::remove_dir_all(&dir);
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

/// Connect to and refresh a server that holds every `tools/list` answer until
/// its `roots/list` is answered, on a thread no client is waiting on. Returns
/// how the server's `roots/list` requests were answered and how many the
/// handler took.
fn discover_and_refresh(sole_client: bool) -> (Vec<Value>, usize) {
    install_request_context_provider();
    REQUEST_CONTEXT.with(|context| {
        *context.borrow_mut() = RequestContext::Background { sole_client };
    });
    let dir = temp_dir("roots-before-list");
    let transcript = dir.join("transcript.jsonl");
    let env = [
        ("MOCK_MCP_ROOTS_BEFORE_LIST".to_string(), "1".to_string()),
        (
            "MOCK_MCP_TRANSCRIPT".to_string(),
            transcript.to_string_lossy().into_owned(),
        ),
    ];
    let handled = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&handled);
    let handler: ServerRequestHandler = Arc::new(move |request| {
        if request["method"] != "roots/list" {
            return None;
        }
        counted.fetch_add(1, Ordering::SeqCst);
        Some(ServerRequestAction::Respond(json!({
            "jsonrpc": "2.0",
            "id": request["id"].clone(),
            "result": { "roots": [{ "uri": "file:///work", "name": "work" }] }
        })))
    });
    let mock = env!("CARGO_BIN_EXE_mock-mcp-server");
    let mut transport = StdioTransport::spawn_watched(
        mock,
        &[],
        &env,
        None,
        false,
        Arc::new(AtomicU8::new(0)),
        None,
    )
    .expect("spawn mock");
    transport.set_server_request_handler(handler);

    let started = Instant::now();
    let mut server =
        DownstreamServer::connect("mock".to_string(), Box::new(transport)).expect("discovery");
    assert!(!server.tools.is_empty(), "discovery listed no tools");
    let discovered = started.elapsed();
    server.tools.clear();
    let started = Instant::now();
    server.refresh_tools();
    let refreshed = started.elapsed();
    assert!(!server.tools.is_empty(), "the refresh listed no tools");
    println!("discovery took {discovered:?}, refresh {refreshed:?}");
    // Left unanswered, each would wait out the server's read timeout.
    assert!(
        discovered < Duration::from_secs(5),
        "discovery took {discovered:?}"
    );
    assert!(
        refreshed < Duration::from_secs(5),
        "refresh took {refreshed:?}"
    );
    drop(server);

    let answers = std::fs::read_to_string(&transcript)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|message| message["id"] == "mock-roots" && message.get("method").is_none())
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    (answers, handled.load(Ordering::SeqCst))
}

#[test]
fn discovery_and_refresh_answer_server_requests_for_the_sole_client() {
    let (answers, handled) = discover_and_refresh(true);
    assert_eq!(handled, 2, "{answers:?}");
    assert_eq!(answers.len(), 2, "{answers:?}");
    assert!(
        answers
            .iter()
            .all(|answer| answer["result"]["roots"][0]["uri"] == "file:///work"),
        "{answers:?}"
    );
}

#[test]
fn discovery_and_refresh_refuse_server_requests_no_client_can_answer() {
    let (answers, handled) = discover_and_refresh(false);
    assert_eq!(handled, 0, "no client context, so the handler must not run");
    assert_eq!(answers.len(), 2, "{answers:?}");
    assert!(
        answers.iter().all(|answer| answer.get("error").is_some()),
        "{answers:?}"
    );
}
