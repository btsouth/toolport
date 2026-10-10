//! Acceptance test for the HTTP chunk-header follow-up.
//! This deliberately fails with upstream ureq 2.12.1: framing is decoded before
//! Toolport's body cap. A fix must reject within the allocation budget and recover
//! for JSON and SSE, including chunk extensions. Do not ignore this test.
use conduit_lib::downstream::{HttpTransport, Transport};
use serde_json::json;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

fn read_request(stream: &mut std::net::TcpStream) -> serde_json::Value {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut len = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                len = value.trim().parse().unwrap();
            }
        }
    }
    let mut body = vec![0; len];
    reader.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}

static MAX_ALLOC: AtomicUsize = AtomicUsize::new(0);
struct CountingAllocator;
unsafe impl std::alloc::GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        MAX_ALLOC.fetch_max(layout.size(), Ordering::Relaxed);
        std::alloc::System.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        std::alloc::System.dealloc(ptr, layout)
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: std::alloc::Layout, size: usize) -> *mut u8 {
        MAX_ALLOC.fetch_max(size, Ordering::Relaxed);
        std::alloc::System.realloc(ptr, layout, size)
    }
}
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

#[test]
fn http_chunk_headers_stay_within_allocation_budget_and_next_call_works() {
    let mut failures = Vec::new();
    for kind in ["application/json", "text/event-stream"] {
        for metadata in [
            "size",
            "extension",
            "unterminated",
            "short",
            "header",
            "fields",
        ] {
            let extension = metadata == "extension";
            let terminated = metadata != "unterminated";
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/", listener.local_addr().unwrap());
            let worker = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                socket
                    .set_write_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                read_request(&mut socket);
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n").unwrap();
                if metadata == "header" {
                    socket.write_all(b"X-Large: ").unwrap();
                } else if metadata == "fields" {
                    for _ in 0..130 {
                        let _ = socket.write_all(b"X-Field: value\r\n");
                    }
                } else {
                    socket.write_all(b"\r\n").unwrap();
                }
                if metadata == "short" {
                    let _ = socket.write_all(b"000000000000000000000\r\n\r\n");
                }
                if extension {
                    socket.write_all(b"1;").unwrap();
                }
                let zeros = [b'0'; 8192];
                // Offer the reviewer's 20 MiB line without allocating it in the fixture.
                let mut sent = 0;
                for _ in 0..if matches!(metadata, "short" | "fields") {
                    0
                } else {
                    2560
                } {
                    if socket.write_all(&zeros).is_err() {
                        break;
                    }
                    sent += zeros.len();
                }
                if terminated {
                    let _ = socket.write_all(b"\r\n\r\n");
                }
                drop(socket);
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                socket
                    .set_write_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let request = read_request(&mut socket);
                // Valid chunk framing is still accepted after the poisoned connection.
                let body =
                    json!({"jsonrpc":"2.0","id":request["id"],"result":{"ok":true}}).to_string();
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{body}\r\n0\r\n\r\n", body.len()).unwrap();
                sent
            });
            let mut transport = HttpTransport::new(&url);
            MAX_ALLOC.store(0, Ordering::SeqCst);
            let error = transport.request("tools/call", json!({})).unwrap_err();
            let rejected = matches!(
                error,
                conduit_lib::downstream::TransportError::FrameRejected(_)
            );
            let largest = MAX_ALLOC.load(Ordering::SeqCst);
            // Send the recovery call before checking assertions so the fixture can exit.
            let recovered = transport.request("tools/call", json!({}));
            let sent = worker.join().unwrap();
            eprintln!("{kind} metadata={metadata}: largest allocation={largest}, sent={sent}, error={error}");
            if !rejected
                || largest > 16 * 1024 * 1024
                || !error.to_string().contains(if metadata == "fields" {
                    "too many header fields"
                } else {
                    "response headers exceeded the 65536-byte limit"
                })
            {
                failures.push(format!(
                    "{kind} metadata={metadata}: largest allocation={largest}, error={error}"
                ));
            }
            assert_eq!(recovered.unwrap()["ok"], true);
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn response_headers_accept_40_kib_and_reject_over_64_kib() {
    for kind in ["application/json", "text/event-stream"] {
        for header_bytes in [40 * 1024, 65 * 1024] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}/", listener.local_addr().unwrap());
            let worker = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                socket
                    .set_write_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let request = read_request(&mut socket);
                let json =
                    json!({"jsonrpc":"2.0","id":request["id"],"result":{"ok":true}}).to_string();
                let body = if kind == "text/event-stream" {
                    format!("event: message\ndata: {json}\n\n")
                } else {
                    json
                };
                let response = format!("HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nX-Large: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", "x".repeat(header_bytes), body.len());
                let _ = socket.write_all(response.as_bytes());
            });
            let result = HttpTransport::new(&url).request("tools/call", json!({}));
            worker.join().unwrap();
            if header_bytes < 64 * 1024 {
                assert_eq!(result.unwrap()["ok"], true, "{kind}");
            } else {
                let error = result.unwrap_err();
                assert!(matches!(
                    error,
                    conduit_lib::downstream::TransportError::FrameRejected(_)
                ));
                assert!(
                    error
                        .to_string()
                        .contains("response headers exceeded the 65536-byte limit"),
                    "{error}"
                );
            }
        }
    }
}
