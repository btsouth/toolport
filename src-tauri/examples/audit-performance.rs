//! Offline Activity benchmark. Uses a private temporary directory, never user logs.
//! cargo run --manifest-path src-tauri/Cargo.toml --no-default-features --features test-support --example audit-performance
use conduit_lib::{audit, registry::DataDirOverride};
use serde_json::json;
use std::{hint::black_box, time::Instant};

fn measure(mut work: impl FnMut(), samples: usize) -> serde_json::Value {
    for _ in 0..5 {
        work();
    }
    let mut times = Vec::with_capacity(samples);
    for _ in 0..samples {
        let start = Instant::now();
        work();
        times.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(f64::total_cmp);
    json!({"median_ms": times[samples / 2], "p95_ms": times[samples * 95 / 100]})
}

fn main() {
    let dir = std::env::temp_dir().join(format!(
        "toolport-audit-bench-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(dir.clone());
    let _data = DataDirOverride::set(&dir);
    let mut content = String::new();
    for i in 0..10_000 {
        content.push_str(
            &json!({"ts": i, "server": format!("server-{}", i % 10),
            "tool": format!("tool-{}", i % 100), "ok": i % 11 != 0,
            "durationMs": i % 500, "error": "x".repeat(260)})
            .to_string(),
        );
        content.push('\n');
    }
    let path = dir.join("audit.jsonl");
    std::fs::write(&path, &content).unwrap();
    // Run memory cases in separate processes so the old JSON tree cannot
    // contaminate the streamed path's peak RSS. VmHWM is Linux-only.
    if let Some(mode) =
        std::env::args().find(|arg| arg == "--memory-uncached" || arg == "--memory-streamed")
    {
        let result = if mode == "--memory-uncached" {
            audit::stats_for_entries(&audit::read_all().unwrap())
        } else {
            audit::stats().unwrap()
        };
        assert_eq!(result["total"], 10_000);
        let peak_rss_kib = std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|status| {
                status.lines().find_map(|line| {
                    line.strip_prefix("VmHWM:")
                        .and_then(|value| value.split_whitespace().next()?.parse::<u64>().ok())
                })
            });
        println!(
            "{}",
            json!({"mode": mode, "rows": 10_000, "bytes": content.len(), "peak_rss_kib": peak_rss_kib})
        );
        return;
    }
    assert_eq!(audit::read_recent(200).unwrap().len(), 200);
    assert_eq!(audit::stats().unwrap()["total"], 10_000);
    let recent = measure(
        || {
            black_box(audit::read_recent(200).unwrap());
        },
        50,
    );
    let uncached = measure(
        || {
            black_box(audit::stats_for_entries(&audit::read_all().unwrap()));
        },
        50,
    );
    let unchanged = measure(
        || {
            black_box(audit::stats().unwrap());
        },
        50,
    );
    // Alternate equal-length snapshots to catch caches based only on file size.
    let changed = content.replace("server-0", "server-X");
    let mut flip = false;
    let changing_uncached = measure(
        || {
            std::fs::write(&path, if flip { &content } else { &changed }).unwrap();
            flip = !flip;
            black_box(audit::stats_for_entries(&audit::read_all().unwrap()));
        },
        20,
    );
    let changing = measure(
        || {
            std::fs::write(&path, if flip { &content } else { &changed }).unwrap();
            flip = !flip;
            black_box(audit::stats().unwrap());
        },
        20,
    );
    println!(
        "{}",
        json!({"profile": if cfg!(debug_assertions) {"debug"} else {"release"},
        "rows": 10_000, "bytes": content.len(), "recent_200": recent,
        "stats_uncached": uncached, "stats_unchanged": unchanged,
        "rewrite_and_uncached_stats": changing_uncached, "rewrite_and_stats": changing})
    );
}
