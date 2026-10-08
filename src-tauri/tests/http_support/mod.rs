#![allow(dead_code)]
use conduit_lib::downstream::{DownstreamServer, HttpTransport};
use conduit_lib::router::Router;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::Duration;

pub struct HttpMock {
    pub url: String,
    pub seen: mpsc::Receiver<Value>,
    stop: Arc<AtomicBool>,
    release: Arc<(Mutex<bool>, Condvar)>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl HttpMock {
    pub fn new() -> Self {
        Self::with_rendezvous(0)
    }
    pub fn with_rendezvous(target: usize) -> Self {
        let rendezvous = Arc::new((Mutex::new(0usize), Condvar::new()));
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        server.set_nonblocking(true).unwrap();
        let url = format!("http://{}/mcp", server.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let (tx, seen) = mpsc::channel();
        let ended = Arc::clone(&stop);
        let released = Arc::clone(&release);
        let worker = std::thread::spawn(move || {
            let mut workers = Vec::new();
            while !ended.load(Ordering::SeqCst) {
                let stream = match server.accept() {
                    Ok((stream, _)) => stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::park_timeout(Duration::from_millis(20));
                        continue;
                    }
                    Err(error) => panic!("HTTP mock accept: {error}"),
                };
                let release = Arc::clone(&released);
                let rendezvous = Arc::clone(&rendezvous);
                let tx = tx.clone();
                // A queued reader pool can strand a rendezvous when its first
                // connections await replies. Start every reader independently.
                workers.push(std::thread::spawn(move || {
                    stream.set_nonblocking(false).unwrap();
                    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                    stream.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
                    let mut request = BufReader::new(stream);
                    let body = read_request(&mut request);
                    tx.send(body.clone()).unwrap();
                    let id = body["id"].clone();
                    let result = match body["method"].as_str().unwrap_or_default() {
                        "initialize" => json!({"protocolVersion":"2025-11-25", "capabilities":{"tools":{}}, "serverInfo":{"name":"http-mock", "version":"1"}}),
                        "tools/list" => json!({"tools":[{"name":"sleep", "inputSchema":{"type":"object"}}, {"name":"echo", "inputSchema":{"type":"object"}}]}),
                        "tools/call" => {
                            let args = &body["params"]["arguments"];
                            if args["rendezvous"] == true {
                                assert!(target > 0);
                                let (lock, ready) = &*rendezvous;
                                let mut arrived = lock.lock().unwrap();
                                let wave = *arrived / target;
                                *arrived += 1;
                                ready.notify_all();
                                let (arrived, _) = ready.wait_timeout_while(arrived, Duration::from_secs(5), |arrived| *arrived / target == wave).unwrap();
                                if *arrived / target == wave {
                                    respond(request.get_mut(), 200, json!({"jsonrpc":"2.0","id":id,"error":{"code":-32603,"message":"HTTP calls did not overlap at rendezvous"}}).to_string());
                                    return;
                                }
                            }
                            let text = if let Some(ms) = args["ms"].as_u64() {
                                let (lock, ready) = &*release;
                                let _ = ready.wait_timeout_while(lock.lock().unwrap(), Duration::from_millis(ms), |released| !*released).unwrap();
                                format!("slept {ms} ms")
                            } else { args["text"].as_str().unwrap_or_default().to_string() };
                            json!({"content":[{"type":"text", "text":text}]})
                        },
                        "notifications/cancelled" => {
                            *release.0.lock().unwrap() = true;
                            release.1.notify_all();
                            respond(request.get_mut(), 202, String::new());
                            return;
                        },
                        method if method.starts_with("notifications/") => {
                            respond(request.get_mut(), 202, String::new()); return;
                        },
                        _ => {
                            respond(request.get_mut(), 200, json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"unsupported"}}).to_string()); return;
                        }
                    };
                    respond(request.get_mut(), 200, json!({"jsonrpc":"2.0", "id":id, "result":result}).to_string());
                }));
            }
            for worker in workers {
                worker.join().unwrap();
            }
        });
        Self {
            url,
            seen,
            stop,
            release,
            worker: Some(worker),
        }
    }
    pub fn router(&self) -> Arc<Router> {
        let server =
            DownstreamServer::connect("http".into(), Box::new(HttpTransport::new(&self.url)))
                .unwrap();
        let mut router = Router::new();
        router.add(server);
        Arc::new(router)
    }
    pub fn wait_for(&self, predicate: impl Fn(&Value) -> bool) -> Value {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let body = self
                .seen
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .unwrap();
            if predicate(&body) {
                return body;
            }
        }
    }
}

fn read_request(stream: &mut BufReader<TcpStream>) -> Value {
    let mut line = String::new();
    stream.read_line(&mut line).unwrap();
    assert!(line.starts_with("POST "));
    let mut length = None;
    let mut header_bytes = line.len();
    loop {
        line.clear();
        assert!(stream.read_line(&mut line).unwrap() > 0);
        header_bytes += line.len();
        assert!(header_bytes <= 16 * 1024);
        if line == "\r\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("Content-Length") {
                length = Some(value.trim().parse::<usize>().unwrap());
            }
        }
    }
    let length = length.expect("HTTP fixture request has Content-Length");
    assert!(length <= 1024 * 1024);
    let mut body = vec![0; length];
    stream.read_exact(&mut body).unwrap();
    serde_json::from_slice(&body).unwrap()
}

fn respond(stream: &mut TcpStream, status: u16, body: String) {
    // Closing each connection keeps the mock's reader lifetime explicit.
    let _ = write!(stream, "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
}

impl Drop for HttpMock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        *self.release.0.lock().unwrap() = true;
        self.release.1.notify_all();
        self.worker.as_ref().unwrap().thread().unpark();
        self.worker.take().unwrap().join().unwrap();
    }
}
