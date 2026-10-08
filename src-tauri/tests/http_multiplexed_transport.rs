//! REL-01 / PERF-06 for independent downstream HTTP POSTs.
mod http_support;
use conduit_lib::downstream::{CancelRegistry, HttpTransport, RefreshFn, Transport};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

#[test]
fn fast_http_call_and_cancellation_do_not_wait_for_a_twenty_second_call() {
    let mock = http_support::HttpMock::new();
    let router = mock.router();
    let cancellations = CancelRegistry::new();
    assert!(cancellations.begin_client_request("slow".into()));
    let cancel = cancellations.context("slow".into());
    let slow = {
        let router = Arc::clone(&router);
        std::thread::spawn(move || {
            router.route_call_with_cancel("http__sleep", json!({"ms":20_000}), Some(cancel), None)
        })
    };
    let body = mock.wait_for(|body| body["params"]["arguments"]["ms"] == 20_000);
    let started = Instant::now();
    assert_eq!(
        router
            .route_call("http__echo", json!({"text":"fast"}))
            .unwrap()["content"][0]["text"],
        "fast"
    );
    println!("HTTP REL-01 fast call: {:?}", started.elapsed());
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(!slow.is_finished());
    let started = Instant::now();
    assert!(cancellations.cancel("slow", Some("test done")));
    assert!(slow.join().unwrap().is_err());
    assert!(started.elapsed() < Duration::from_secs(1));
    let cancellation = mock.wait_for(|body| body["method"] == "notifications/cancelled");
    assert_eq!(cancellation["params"]["requestId"], body["id"]);
    assert_eq!(
        router
            .route_call("http__echo", json!({"text":"after"}))
            .unwrap()["content"][0]["text"],
        "after"
    );
}

#[test]
fn one_hundred_parallel_http_calls_overlap_and_keep_their_ids() {
    let mock = http_support::HttpMock::new();
    let router = mock.router();
    let started = Instant::now();
    let calls: Vec<_> = (0..100)
        .map(|_| {
            let router = Arc::clone(&router);
            std::thread::spawn(move || router.route_call("http__sleep", json!({"ms":200})))
        })
        .collect();
    for call in calls {
        assert_eq!(
            call.join().unwrap().unwrap()["content"][0]["text"],
            "slept 200 ms"
        );
    }
    println!(
        "HTTP REL-01 100 parallel 200 ms calls: {:?}",
        started.elapsed()
    );
    // Set only for the release baseline measurement on the serialized base.
    if std::env::var_os("TOOLPORT_HTTP_BASELINE").is_none() {
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}

#[test]
fn simultaneous_unauthorized_posts_share_one_forced_refresh() {
    const CALLS: usize = 8;
    use std::io::{BufRead, BufReader, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let rejected = Arc::new(Gate::new(CALLS));
    let wire = std::thread::spawn(move || {
        let mut workers = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while workers.len() < CALLS * 2 {
            let (mut stream, _) = match listener.accept() {
                Ok(connection) => connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "auth fixture did not receive all POSTs"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                    continue;
                }
                Err(error) => panic!("{error}"),
            };
            // Windows accepted sockets inherit the listener's non-blocking mode.
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let rejected = Arc::clone(&rejected);
            workers.push(std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut length = 0;
                let mut stale = false;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse::<usize>().unwrap();
                    }
                    stale |= line.trim().eq_ignore_ascii_case("authorization: Bearer stale");
                }
                let mut bytes = vec![0; length];
                reader.read_exact(&mut bytes).unwrap();
                let body: Value = serde_json::from_slice(&bytes).unwrap();
                if stale {
                    rejected.wait();
                    write!(stream,"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                } else {
                    let result=json!({"jsonrpc":"2.0","id":body["id"],"result":body["params"]}).to_string();
                    write!(stream,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",result.len(),result).unwrap();
                }
                stream.flush().unwrap();
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }
    });
    let refreshed = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&refreshed);
    let refresh: RefreshFn = Box::new(move |force, _| {
        if force {
            counted.fetch_add(1, Ordering::SeqCst);
            Ok(Some("fresh".into()))
        } else {
            Ok(None)
        }
    });
    let transport = HttpTransport::with_auth_refresh(&url, Some("stale".into()), Some(refresh));
    let handle = transport.concurrent().unwrap();
    let (tx, rx) = mpsc::channel();
    for id in 0..CALLS {
        let handle = Arc::clone(&handle);
        let tx = tx.clone();
        std::thread::spawn(move || {
            tx.send((
                id,
                handle.request_with_cancel("echo", json!({"call":id}), None),
            ))
            .unwrap();
        });
    }
    for _ in 0..CALLS {
        let (id, result) = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(result.unwrap()["call"], id);
    }
    assert_eq!(refreshed.load(Ordering::SeqCst), 1);
    wire.join().unwrap();
}

#[test]
fn shutdown_wakes_every_http_waiter() {
    let mock = http_support::HttpMock::new();
    let transport = HttpTransport::new(&mock.url);
    let handle = transport.concurrent().unwrap();
    let call = std::thread::spawn(move || {
        handle.request_with_cancel(
            "tools/call",
            json!({"name":"sleep","arguments":{"ms":20_000}}),
            None,
        )
    });
    mock.wait_for(|body| body["params"]["arguments"]["ms"] == 20_000);
    let started = Instant::now();
    drop(transport);
    assert!(call.join().unwrap().is_err());
    assert!(started.elapsed() < Duration::from_secs(1));
}

thread_local! {
    static OWNER: std::cell::RefCell<conduit_lib::downstream::RequestContext> =
        const { std::cell::RefCell::new(conduit_lib::downstream::RequestContext::Client(String::new())) };
}

// Each response stream asks for roots and waits for the inline POST before
// yielding its final response. Request receipt and replies use channels.
fn sse_fixture(calls: usize) -> (String, std::thread::JoinHandle<Vec<Value>>) {
    use std::io::{BufRead, BufReader, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let worker = std::thread::spawn(move || {
        let replies = Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        let (collected, answers) = mpsc::channel();
        let mut workers = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while workers.len() < calls * 2 {
            let (mut stream, _) = match listener.accept() {
                Ok(connection) => connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "SSE fixture did not receive all POSTs"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                    continue;
                }
                Err(error) => panic!("{error}"),
            };
            // Windows accepted sockets inherit the listener's non-blocking mode.
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse::<usize>().unwrap();
                }
            }
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            if body["method"]
                .as_str()
                .is_some_and(|method| method.starts_with("notifications/"))
            {
                write!(
                    stream,
                    "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
                .unwrap();
                continue;
            }
            let replies = Arc::clone(&replies);
            let collected = collected.clone();
            workers.push(std::thread::spawn(move || {
                if body.get("method").is_some() {
                    let root_id = format!("roots-{}", body["id"]);
                    let (tx, rx) = mpsc::channel();
                    replies.lock().unwrap().insert(root_id.clone(), tx);
                    write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {}\n\n", json!({"jsonrpc":"2.0","id":root_id,"method":"roots/list","params":{"owner":body["params"]["owner"]}})).unwrap();
                    stream.flush().unwrap();
                    let response: Value = rx.recv_timeout(Duration::from_secs(10)).unwrap();
                    collected.send(response.clone()).unwrap();
                    // Cancellation and shutdown retire the original SSE reader after
                    // sending the refusal, so the final response may hit a closed socket.
                    let final_response = write!(stream,"data: {}\n\n",json!({"jsonrpc":"2.0","id":body["id"],"result":response}))
                        .and_then(|()| stream.flush());
                    if let Err(error) = final_response {
                        assert!(matches!(error.kind(), std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted), "{error}");
                    }
                } else {
                    let id = body["id"].as_str().unwrap();
                    replies.lock().unwrap().remove(id).unwrap().send(body).unwrap();
                    write!(stream,"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                }
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }
        answers.try_iter().collect()
    });
    (url, worker)
}

fn set_owner(owner: &str) {
    OWNER.with(|context| {
        *context.borrow_mut() = conduit_lib::downstream::RequestContext::Client(owner.into())
    });
}
fn install_owner() {
    conduit_lib::downstream::set_request_context_provider(Arc::new(|| {
        OWNER.with(|context| context.borrow().clone())
    }));
}

#[test]
fn sse_server_requests_run_on_their_own_client_threads() {
    use conduit_lib::downstream::ServerRequestAction;
    install_owner();
    let (url, wire) = sse_fixture(2);
    let mut transport = HttpTransport::new(&url);
    let gate = Arc::new(Gate::new(2));
    let handler_gate = Arc::clone(&gate);
    transport.set_server_request_handler(Arc::new(move |request| {
        let expected = request["params"]["owner"].as_str().unwrap();
        OWNER.with(|context| {
            assert_eq!(
                *context.borrow(),
                conduit_lib::downstream::RequestContext::Client(expected.into())
            )
        });
        handler_gate.wait();
        Some(ServerRequestAction::Respond(
            json!({"jsonrpc":"2.0","id":request["id"],"result":{"owner":expected}}),
        ))
    }));
    let handle = transport.concurrent().unwrap();
    let calls: Vec<_> = ["client-a", "client-b"]
        .into_iter()
        .map(|owner| {
            let handle = Arc::clone(&handle);
            std::thread::spawn(move || {
                set_owner(owner);
                let result = handle
                    .request_with_cancel("echo", json!({"owner":owner}), None)
                    .unwrap();
                assert_eq!(result["result"]["owner"], owner);
            })
        })
        .collect();
    for call in calls {
        call.join().unwrap();
    }
    assert_eq!(wire.join().unwrap().len(), 2);
}

#[test]
fn background_http_sse_requests_are_refused_without_discovery_deadlock() {
    install_owner();
    OWNER.with(|context| {
        *context.borrow_mut() =
            conduit_lib::downstream::RequestContext::Background { sole_client: false }
    });
    let (url, wire) = sse_fixture(1);
    let mut transport = HttpTransport::new(&url);
    transport.set_server_request_handler(Arc::new(|_| {
        panic!("background must not invoke client handler")
    }));
    let handle = transport.concurrent().unwrap();
    let result = handle
        .request_with_cancel("tools/list", json!({}), None)
        .unwrap();
    assert!(result.get("error").is_some());
    assert!(wire.join().unwrap()[0].get("error").is_some());
    set_owner("");
}

#[test]
fn http_sse_continuations_validate_the_call_and_allow_a_new_request_nonce() {
    use conduit_lib::downstream::ServerRequestAction;
    install_owner();
    set_owner("client-a|#1");
    let (url, wire) = sse_fixture(1);
    let mut transport = HttpTransport::new(&url);
    transport.set_server_request_handler(Arc::new(|_| Some(ServerRequestAction::InputRequired)));
    let handle = transport.concurrent().unwrap();
    let incomplete = handle
        .request_with_cancel("echo", json!({"owner":"client-a"}), None)
        .unwrap();
    assert_eq!(handle.suspended_calls(), 1);
    let key = incomplete["inputRequests"]
        .as_object()
        .unwrap()
        .keys()
        .next()
        .unwrap();
    let retry = json!({"owner":"client-a","requestState":incomplete["requestState"],"inputResponses":{key:{"roots":[]}}});
    set_owner("client-b");
    let mut different_call = retry.clone();
    different_call["owner"] = json!("client-b");
    assert!(handle
        .request_with_cancel("echo", different_call, None)
        .is_err());
    assert_eq!(handle.suspended_calls(), 1);
    set_owner("client-a|#2");
    assert!(handle.request_with_cancel("echo", retry, None).is_ok());
    assert_eq!(handle.suspended_calls(), 0);
    assert_eq!(wire.join().unwrap().len(), 1);
    set_owner("");
}

// A barrier with an explicit deadline, so a missing sibling fails the fixture.
struct Gate {
    target: usize,
    count: std::sync::Mutex<usize>,
    ready: std::sync::Condvar,
}
impl Gate {
    fn new(target: usize) -> Self {
        Self {
            target,
            count: std::sync::Mutex::new(0),
            ready: std::sync::Condvar::new(),
        }
    }
    fn wait(&self) {
        let mut count = self.count.lock().unwrap();
        *count += 1;
        self.ready.notify_all();
        let (count, _) = self
            .ready
            .wait_timeout_while(count, Duration::from_secs(10), |count| *count < self.target)
            .unwrap();
        assert!(*count >= self.target, "fixture gate timed out");
    }
}

#[test]
fn cancelling_a_suspended_http_call_refuses_the_owned_server_request() {
    use conduit_lib::downstream::ServerRequestAction;
    install_owner();
    set_owner("cancel-owner");
    let (url, wire) = sse_fixture(1);
    let mut transport = HttpTransport::new(&url);
    transport.set_server_request_handler(Arc::new(|_| Some(ServerRequestAction::InputRequired)));
    let handle = transport.concurrent().unwrap();
    let incomplete = handle
        .request_with_cancel("echo", json!({"owner":"cancel-owner"}), None)
        .unwrap();
    let cancellations = CancelRegistry::new();
    assert!(cancellations.begin_client_request("cancel".into()));
    let cancel = cancellations.context("cancel".into());
    assert!(cancellations.cancel("cancel", None));
    assert!(handle
        .request_with_cancel(
            "echo",
            json!({"owner":"cancel-owner","requestState":incomplete["requestState"]}),
            Some(cancel)
        )
        .is_err());
    assert_eq!(handle.suspended_calls(), 0);
    assert!(wire.join().unwrap()[0].get("error").is_some());
    set_owner("");
}

#[test]
fn shutdown_refuses_suspended_http_server_requests() {
    use conduit_lib::downstream::ServerRequestAction;
    install_owner();
    set_owner("shutdown-owner");
    let (url, wire) = sse_fixture(1);
    let mut transport = HttpTransport::new(&url);
    transport.set_server_request_handler(Arc::new(|_| Some(ServerRequestAction::InputRequired)));
    let handle = transport.concurrent().unwrap();
    handle
        .request_with_cancel("echo", json!({"owner":"shutdown-owner"}), None)
        .unwrap();
    assert_eq!(handle.suspended_calls(), 1);
    drop(transport);
    assert!(handle.is_closed());
    assert_eq!(handle.suspended_calls(), 0);
    assert!(wire.join().unwrap()[0].get("error").is_some());
    set_owner("");
}

#[test]
fn http_responses_with_another_requests_id_are_rejected() {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", server.server_addr());
    let wire = std::thread::spawn(move || {
        let request = server
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap();
        request
            .respond(tiny_http::Response::from_string(
                json!({"jsonrpc":"2.0","id":999,"result":{"wrong":true}}).to_string(),
            ))
            .unwrap();
    });
    let transport = HttpTransport::new(&url);
    let result = transport
        .concurrent()
        .unwrap()
        .request_with_cancel("echo", json!({}), None);
    assert!(result.unwrap_err().to_string().contains("response id"));
    wire.join().unwrap();
}

fn failed_forced_refresh_probe(concurrent: bool) {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", server.server_addr());
    let wire = std::thread::spawn(move || {
        for _ in 0..3 {
            let Some(mut request) = server.recv_timeout(Duration::from_secs(3)).unwrap() else {
                break;
            };
            let fresh = request
                .headers()
                .iter()
                .any(|h| h.field.equiv("Authorization") && h.value.as_str() == "Bearer fresh");
            let mut body = String::new();
            request.as_reader().read_to_string(&mut body).unwrap();
            let body: Value = serde_json::from_str(&body).unwrap();
            let response = if fresh {
                tiny_http::Response::from_string(
                    json!({"jsonrpc":"2.0","id":body["id"],"result":{"ok":true}}).to_string(),
                )
            } else {
                tiny_http::Response::from_string("revoked").with_status_code(401)
            };
            request.respond(response).unwrap();
        }
    });
    const BUSY: &str = "OAuth refresh is busy or its cross-process lock is unavailable; try again.";
    let attempts = Arc::new(AtomicUsize::new(0));
    let forced = Arc::clone(&attempts);
    let refresh: RefreshFn = Box::new(move |force, _| {
        if !force {
            return Ok(None);
        }
        if forced.fetch_add(1, Ordering::SeqCst) == 0 {
            Err(BUSY.into())
        } else {
            Ok(Some("fresh".into()))
        }
    });
    let mut transport = HttpTransport::with_auth_refresh(&url, Some("stale".into()), Some(refresh));
    let handle = transport.concurrent().unwrap();
    let mut call = || {
        if concurrent {
            handle.request_with_cancel("echo", json!({}), None)
        } else {
            transport.request("echo", json!({}))
        }
    };
    let first = call().unwrap_err().to_string();
    assert_eq!(first, BUSY);
    assert!(!conduit_lib::remote::is_auth_error(&first));
    let second = call();
    wire.join().unwrap();
    assert_eq!(
        second.unwrap(),
        json!({"ok":true}),
        "concurrent={concurrent}"
    );
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[test]
fn failed_forced_refresh_recovers_on_the_next_serial_call() {
    failed_forced_refresh_probe(false);
}

#[test]
fn failed_forced_refresh_recovers_on_the_next_concurrent_call() {
    failed_forced_refresh_probe(true);
}

#[test]
fn http_null_id_errors_preserve_the_server_message() {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", server.server_addr());
    let wire = std::thread::spawn(move || {
        for status in [200, 400] {
            let request = server
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
                .unwrap();
            request.respond(tiny_http::Response::from_string(json!({"jsonrpc":"2.0","id":null,"error":{"code":-32600,"message":"invalid request from server"}}).to_string()).with_status_code(status)).unwrap();
        }
    });
    let mut transport = HttpTransport::new(&url);
    transport.set_protocol_meta(Some(
        json!({"io.modelcontextprotocol/protocolVersion":"2026-07-28"}),
    ));
    for _ in 0..2 {
        let error = transport.request("echo", json!({})).unwrap_err();
        assert!(
            matches!(error, conduit_lib::downstream::TransportError::Rpc(_)),
            "{error}"
        );
        assert!(error.to_string().contains("invalid request from server"));
    }
    wire.join().unwrap();
}
