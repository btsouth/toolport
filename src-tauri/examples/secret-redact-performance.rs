//! Secret redaction cost at 20, 500 and 5,000 registered secrets: matcher build
//! time and memory, per-line cost on the log writers' paths, the latency of one
//! more registration, and the longest a reader waits while the main matcher is
//! rebuilt in the background.
//!
//! `cargo run --release --example secret-redact-performance --features test-support`

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use conduit_lib::secret_redact::{self, Matcher};

fn secrets(n: usize, seed: u64) -> Vec<(String, String)> {
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut state = seed;
    (0..n)
        .map(|i| {
            let value: String = (0..32)
                .map(|_| {
                    state = state
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(1442695040888963407);
                    alphabet[(state >> 33) as usize % alphabet.len()] as char
                })
                .collect();
            (format!("KEY_{i}"), format!("sk-{value}"))
        })
        .collect()
}

fn per_iter(iterations: u32, mut f: impl FnMut()) -> Duration {
    let started = Instant::now();
    for _ in 0..iterations {
        f();
    }
    started.elapsed() / iterations
}

fn main() {
    let dir = std::env::temp_dir().join(format!("toolport-redact-bench-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let line = r#"{"ts":1794700000000,"server":"github","tool":"search_issues","client":"claude-code","ok":true,"durationMs":182,"args":{"query":"is:open label:bug repo:example/project","perPage":30},"bytesOut":48211}"#;
    let error = "upstream rejected the request: 401 Unauthorized (invalid credentials for https://api.example.invalid/v1/issues?page=2)";
    println!(
        "secrets | build ms | memory MiB | line ns | error ns | register us | max reader stall us"
    );
    for n in [20usize, 500, 5000] {
        let set = secrets(n, n as u64);
        let started = Instant::now();
        let matcher = Matcher::new(&set);
        let build = started.elapsed();
        let memory = matcher.memory_usage() as f64 / (1024.0 * 1024.0);
        let line_ns = per_iter(200_000, || {
            std::hint::black_box(matcher.redact_line(std::hint::black_box(line)));
        });
        let error_ns = per_iter(50_000, || {
            std::hint::black_box(matcher.redact_fragment(std::hint::black_box(error)));
        });

        // The process registry: register `n` values, let the main build settle,
        // then time one more registration while a reader thread measures its
        // longest single call.
        let scope = dir.join(format!("n{n}"));
        std::fs::create_dir_all(&scope).unwrap();
        let _data = conduit_lib::registry::DataDirOverride::set(&scope);
        for (k, v) in &set {
            secret_redact::register("bench", k, v);
        }
        assert!(
            secret_redact::wait_for_main_build(Duration::from_secs(120)),
            "the warm-up build did not finish"
        );
        let stop = Arc::new(AtomicBool::new(false));
        let worst = Arc::new(AtomicU64::new(0));
        let reader = {
            let (stop, worst, scope) = (Arc::clone(&stop), Arc::clone(&worst), scope.clone());
            std::thread::spawn(move || {
                let _data = conduit_lib::registry::DataDirOverride::set(&scope);
                while !stop.load(Ordering::Relaxed) {
                    let started = Instant::now();
                    std::hint::black_box(secret_redact::redact_text(error));
                    worst.fetch_max(started.elapsed().as_micros() as u64, Ordering::Relaxed);
                }
            })
        };
        std::thread::sleep(Duration::from_millis(20));
        let started = Instant::now();
        secret_redact::register("bench", "ONE_MORE", "sk-one-more-registered-value-9QzX");
        let register = started.elapsed();
        assert!(
            secret_redact::redact_text("got sk-one-more-registered-value-9QzX")
                .contains("[secret:ONE_MORE]")
        );
        // Span the background rebuild this registration started: until it has
        // finished, not for a guessed time.
        assert!(
            secret_redact::wait_for_main_build(Duration::from_secs(120)),
            "the rebuild did not finish"
        );
        stop.store(true, Ordering::Relaxed);
        reader.join().unwrap();
        println!(
            "{n:>7} | {:>8.1} | {memory:>10.1} | {:>7} | {:>8} | {:>11} | {:>19}",
            build.as_secs_f64() * 1000.0,
            line_ns.as_nanos(),
            error_ns.as_nanos(),
            register.as_micros(),
            worst.load(Ordering::Relaxed),
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
