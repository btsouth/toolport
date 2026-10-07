#![allow(dead_code)]
use conduit_lib::downstream::{DownstreamServer, HttpTransport};
use conduit_lib::router::Router;
use serde_json::{json, Value};
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
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}/mcp", server.server_addr());
        let stop = Arc::new(AtomicBool::new(false));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let (tx, seen) = mpsc::channel();
        let ended = Arc::clone(&stop);
        let released = Arc::clone(&release);
        let worker = std::thread::spawn(move || {
            let mut workers = Vec::new();
            while !ended.load(Ordering::SeqCst) {
                let Some(mut request) = server.recv_timeout(Duration::from_millis(20)).unwrap()
                else {
                    continue;
                };
                let mut text = String::new();
                request.as_reader().read_to_string(&mut text).unwrap();
                let body: Value = serde_json::from_str(&text).unwrap();
                tx.send(body.clone()).unwrap();
                let release = Arc::clone(&released);
                let rendezvous = Arc::clone(&rendezvous);
                workers.push(std::thread::spawn(move || {
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
                                    let _ = request.respond(tiny_http::Response::from_string(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32603,"message":"HTTP calls did not overlap at rendezvous"}}).to_string()));
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
                            let _ = request.respond(tiny_http::Response::empty(202));
                            return;
                        },
                        method if method.starts_with("notifications/") => {
                            let _ = request.respond(tiny_http::Response::empty(202)); return;
                        },
                        _ => {
                            let _ = request.respond(tiny_http::Response::from_string(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"unsupported"}}).to_string())); return;
                        }
                    };
                    let _ = request.respond(tiny_http::Response::from_string(json!({"jsonrpc":"2.0", "id":id, "result":result}).to_string()).with_header(tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap()));
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
impl Drop for HttpMock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        *self.release.0.lock().unwrap() = true;
        self.release.1.notify_all();
        self.worker.take().unwrap().join().unwrap();
    }
}
