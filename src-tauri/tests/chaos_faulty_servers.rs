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
use std::sync::atomic::{AtomicUsize, Ordering};
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

/// A server that answers every request 401, like an OAuth server nobody signed
/// into yet.  The gateway must say so and wait for new credentials instead of
/// retrying in a loop that hides the real cause.
fn unauthorized_server() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&hits);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            counted.fetch_add(1, Ordering::SeqCst);
            let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
            let mut buf = [0u8; 8192];
            let _ = stream.read(&mut buf);
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
    (url, hits)
}

#[test]
fn an_auth_required_server_waits_for_sign_in_without_taking_the_gateway_down() {
    let scratch = Scratch::new("auth");
    let (url, hits) = unauthorized_server();
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
    assert_eq!(
        hits.load(Ordering::SeqCst),
        after_connect,
        "an auth-required server must wait for new credentials"
    );
}
