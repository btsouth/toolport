//! P1.7 chaos: a downstream server that hangs, crashes, starts late, logs to
//! stdout, or floods stderr must never take the gateway, another server or
//! another client down with it.
//!
//! Every case drives the real gateway as a stdio adapter in front of the shared
//! host daemon, against the real `mock-mcp-server` with the failure knob set.
//! Unix only: the harness kills the detached daemon and reads `kill -0`, which
//! the Windows path does differently.

#![cfg(unix)]

mod chaos_support;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chaos_support::{
    http_entry, mock_entry, start_daemon, wait_for, write_registry, Client, Scratch,
};
use serde_json::json;

/// Every case gives the catalog this long to build before it is a finding.
const CATALOG: Duration = Duration::from_secs(60);

#[test]
fn a_hung_server_does_not_stall_another_server() {
    let scratch = Scratch::new("hang");
    write_registry(
        scratch.path(),
        &[
            mock_entry("good", &[]),
            // Every call to the bad server waits far past any caller's deadline.
            // Bounded so a mock orphaned by a killed daemon exits on its own.
            mock_entry("bad", &[("MOCK_MCP_CALL_DELAY_MS", "60000")]),
        ],
        &["good", "bad"],
    );
    let _daemon = start_daemon(scratch.path());
    let mut victim = Client::start(scratch.path(), "victim");
    let mut good = Client::start(scratch.path(), "good");
    assert!(
        good.wait_for_tool("good__echo", CATALOG),
        "{}",
        good.diagnostics()
    );

    // The bad server's call is still in flight: its response never comes.
    let hanging = victim.call_async("bad__echo", json!({ "text": "x" }));
    std::thread::sleep(Duration::from_millis(500));

    let started = Instant::now();
    let reply = good.call("good__echo", json!({ "text": "alive" }));
    assert!(
        chaos_support::reply_ok(&reply, "alive"),
        "the healthy server's call came back wrong: {reply}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "a call to a healthy server waited on a hung peer: {:?}\n{}",
        started.elapsed(),
        good.diagnostics()
    );
    // The catalog is still answered while the hung call sits in the slot.
    assert!(good.wait_for_tool("good__echo", Duration::from_secs(5)));
    // Do not await the hung call: the harness tears it down on drop.
    let _ = hanging;
}

#[test]
fn a_server_that_crashes_mid_call_is_brought_back() {
    let scratch = Scratch::new("crash");
    write_registry(
        scratch.path(),
        &[mock_entry("good", &[]), mock_entry("bad", &[])],
        &["good", "bad"],
    );
    let _daemon = start_daemon(scratch.path());
    let mut client = Client::start(scratch.path(), "a");
    assert!(
        client.wait_for_tool("bad__die", CATALOG),
        "{}",
        client.diagnostics()
    );

    // `die` exits without replying: the call must fail rather than hang forever.
    let started = Instant::now();
    let (is_error, text) = client.call_text("bad__die", json!({}));
    assert!(is_error, "a crash with no reply must error: {text}");
    assert!(
        started.elapsed() < Duration::from_secs(45),
        "the crashed call waited out a read timeout: {:?}",
        started.elapsed()
    );

    // A healthy peer is untouched by the crash.
    let peer = client.call("good__echo", json!({ "text": "ok" }));
    assert!(
        chaos_support::reply_ok(&peer, "ok"),
        "the crashed peer disturbed the healthy server: {peer}"
    );

    // The crashed server is retried and comes back without a restart.
    match client.wait_for_call("bad__echo", "chaos", Duration::from_secs(60)) {
        Ok(text) => assert!(text.starts_with("chaos")),
        Err(error) => panic!(
            "the crashed server never came back: {error}\n{}",
            client.diagnostics()
        ),
    }
}

#[test]
fn a_slow_starting_server_does_not_hold_back_the_catalog() {
    let scratch = Scratch::new("slow-start");
    write_registry(
        scratch.path(),
        &[
            mock_entry("good", &[]),
            mock_entry("slow", &[("MOCK_MCP_START_DELAY_MS", "3000")]),
        ],
        &["good", "slow"],
    );
    let _daemon = start_daemon(scratch.path());
    let mut client = Client::start(scratch.path(), "a");

    // The good server is browsable without waiting out the slow server's delay.
    let started = Instant::now();
    assert!(
        client.wait_for_tool("good__echo", Duration::from_secs(20)),
        "{}",
        client.diagnostics()
    );
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "the slow server held back the whole catalog: {:?}",
        started.elapsed()
    );

    // The slow server itself joins once its own delay has passed.
    assert!(
        client.wait_for_tool("slow__echo", CATALOG),
        "the slow server never joined\n{}",
        client.diagnostics()
    );
    let late = client.call("slow__echo", json!({ "text": "late" }));
    assert!(
        chaos_support::reply_ok(&late, "late"),
        "the slow server's answer came back wrong: {late}"
    );
}

#[test]
fn garbage_on_a_servers_stdout_does_not_break_it() {
    let scratch = Scratch::new("garbage");
    write_registry(
        scratch.path(),
        &[
            mock_entry("good", &[]),
            // A server that logs to stdout, which MCP forbids.
            mock_entry("noisy", &[("MOCK_MCP_GARBAGE_STDOUT_MS", "20")]),
        ],
        &["good", "noisy"],
    );
    let _daemon = start_daemon(scratch.path());
    let mut client = Client::start(scratch.path(), "a");
    assert!(
        client.wait_for_tool("noisy__echo", CATALOG),
        "the noisy server never joined\n{}",
        client.diagnostics()
    );

    for round in 0..3 {
        let text = format!("survives-{round}");
        let reply = client.call("noisy__echo", json!({ "text": text }));
        assert!(
            reply.get("error").is_none() && reply["result"]["isError"] != true,
            "a call through the noisy server failed: {reply}"
        );
        // The gateway may append its repeated-call advisor note; the server's
        // own answer must still lead.
        assert!(
            reply["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .starts_with(&text),
            "the noisy server's answer came back wrong: {reply}"
        );
    }
    let peer = client.call("good__echo", json!({ "text": "peer" }));
    assert!(
        chaos_support::reply_ok(&peer, "peer"),
        "the noisy server disturbed the healthy peer: {peer}"
    );
}

#[test]
fn a_stderr_flood_does_not_wedge_the_server_or_the_gateway() {
    let scratch = Scratch::new("stderr-flood");
    write_registry(
        scratch.path(),
        &[
            mock_entry("good", &[]),
            mock_entry("chatty", &[("MOCK_MCP_STDERR_FLOOD", "1")]),
        ],
        &["good", "chatty"],
    );
    let _daemon = start_daemon(scratch.path());
    let mut client = Client::start(scratch.path(), "a");
    assert!(
        client.wait_for_tool("chatty__echo", CATALOG),
        "the chatty server never joined\n{}",
        client.diagnostics()
    );

    // Give the flood time to build up, then prove the server still answers.
    std::thread::sleep(Duration::from_millis(1000));
    for round in 0..3 {
        let text = format!("chatty-{round}");
        let reply = client.call("chatty__echo", json!({ "text": text }));
        assert!(
            reply.get("error").is_none() && reply["result"]["isError"] != true,
            "the stderr flood wedged the server: {reply}"
        );
        assert!(
            reply["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .starts_with(&text),
            "the flood server's answer came back wrong: {reply}"
        );
    }
    let peer = client.call("good__echo", json!({ "text": "peer" }));
    assert!(
        chaos_support::reply_ok(&peer, "peer"),
        "the flood disturbed the healthy server: {peer}"
    );
}

/// An endpoint that initially rejects auth, then recovers without a registry
/// edit. Retries must respect backoff and leave healthy peers available.
fn unauthorized_server() -> (String, Arc<AtomicUsize>, Arc<AtomicBool>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&hits);
    let recovered = Arc::new(AtomicBool::new(false));
    let ready = Arc::clone(&recovered);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            counted.fetch_add(1, Ordering::SeqCst);
            let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
            let mut request = Vec::new();
            let mut buf = [0u8; 8192];
            let (header_end, length) = loop {
                let Ok(n) = stream.read(&mut buf) else {
                    break (0, 0);
                };
                if n == 0 {
                    break (0, 0);
                }
                request.extend_from_slice(&buf[..n]);
                if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..end]);
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().ok())
                                .flatten()
                        })
                        .unwrap_or(0);
                    break (end + 4, length);
                }
            };
            while request.len() < header_end + length {
                let Ok(n) = stream.read(&mut buf) else { break };
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
            }
            if ready.load(Ordering::SeqCst) && request.starts_with(b"POST ") {
                let rpc: serde_json::Value =
                    serde_json::from_slice(&request[header_end..]).unwrap_or_default();
                let Some(id) = rpc.get("id") else {
                    let _ = write!(
                        stream,
                        "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    continue;
                };
                let result = match rpc["method"].as_str().unwrap_or("") {
                    "initialize" => {
                        json!({"protocolVersion":"2025-03-26", "capabilities":{"tools":{}}, "serverInfo":{"name":"recovered", "version":"1"}})
                    }
                    "tools/list" => {
                        json!({"tools":[{"name":"echo", "description":"Echo", "inputSchema":{"type":"object"}}]})
                    }
                    "tools/call" => {
                        json!({"content":[{"type":"text", "text":rpc["params"]["arguments"]["text"].as_str().unwrap_or("")}]})
                    }
                    "prompts/list" => json!({"prompts":[]}),
                    "resources/list" => json!({"resources":[]}),
                    "resources/templates/list" => json!({"resourceTemplates":[]}),
                    _ => json!({}),
                };
                let body = json!({"jsonrpc":"2.0", "id":id, "result":result}).to_string();
                let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                continue;
            }
            let body = r#"{"error":"unauthorized"}"#;
            let _ = write!(
                stream,
                "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Bearer\r\n\
                 Content-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (url, hits, recovered)
}

#[test]
fn an_auth_required_endpoint_recovers_after_backoff_without_taking_the_gateway_down() {
    let scratch = Scratch::new("auth");
    let (url, hits, recovered) = unauthorized_server();
    write_registry(
        scratch.path(),
        &[mock_entry("good", &[]), http_entry("locked", &url)],
        &["good", "locked"],
    );
    let _daemon = start_daemon(scratch.path());
    let mut client = Client::start(scratch.path(), "a");
    assert!(
        client.wait_for_tool("good__echo", CATALOG),
        "{}",
        client.diagnostics()
    );

    wait_for("the sign-in status", Duration::from_secs(30), || {
        let status = client.status();
        status.contains("Needs sign-in") && status.contains("locked")
    });
    let after_connect = hits.load(Ordering::SeqCst);
    assert!(after_connect > 0, "the first connect reached the server");

    // The healthy peer keeps working, and calls to the locked server explain
    // themselves without causing a retry storm.
    let peer = client.call("good__echo", json!({ "text": "ok" }));
    assert!(
        chaos_support::reply_ok(&peer, "ok"),
        "the auth-required server disturbed the healthy peer: {peer}"
    );
    for _ in 0..3 {
        let (is_error, text) = client.call_text("locked__anything", json!({}));
        assert!(is_error);
        assert!(text.contains("needs sign-in"), "{text}");
        std::thread::sleep(Duration::from_millis(500));
    }
    assert!(
        hits.load(Ordering::SeqCst) <= after_connect + 1,
        "endpoint auth retries ignored exponential backoff"
    );
    recovered.store(true, Ordering::SeqCst);
    assert!(
        client.wait_for_tool("locked__echo", CATALOG),
        "{}",
        client.diagnostics()
    );
    let reply = client.call("locked__echo", json!({"text":"recovered"}));
    assert!(chaos_support::reply_ok(&reply, "recovered"), "{reply}");
    let peer = client.call("good__echo", json!({"text":"still available"}));
    assert!(chaos_support::reply_ok(&peer, "still available"), "{peer}");
}
