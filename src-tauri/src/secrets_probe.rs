//! Throwaway cross-process stress probe for the Windows chunked credential store.
//! Not for merge.
use std::collections::BTreeMap;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const KEY: &str = "OAUTH_TOKEN";

fn values() -> (String, String) {
    (
        format!("{}{}", "a".repeat(3_500), "\u{1F512}".repeat(300)),
        format!("{}{}", "b".repeat(6_100), "\u{1F680}".repeat(100)),
    )
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
}

/// One process of the probe. Role and target come from the environment.
#[test]
#[ignore = "probe child, spawned by probe::parent"]
fn child() {
    let _data = crate::registry::DataDirTestEnv::new("keyring-probe-child");
    let role = env("TP_PROBE_ROLE");
    let sid = env("TP_PROBE_SID");
    let secs: u64 = env("TP_PROBE_SECS").parse().unwrap_or(30);
    let (a, b) = values();
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    let mut samples: Vec<String> = Vec::new();
    let mut note = |kind: &str, detail: String, counts: &mut BTreeMap<String, u64>| {
        *counts.entry(kind.to_string()).or_default() += 1;
        if kind != "ok" && samples.len() < 5 {
            samples.push(format!("{kind}: {detail}"));
        }
    };
    match role.as_str() {
        "writer" => {
            let mut index = 0u64;
            while Instant::now() < deadline {
                // Mostly chunked generation swaps, with an occasional direct value so
                // chunk cleanup after a shrink is exercised too.
                let value = match index % 5 {
                    4 => "short replacement",
                    i if i % 2 == 0 => a.as_str(),
                    _ => b.as_str(),
                };
                match super::set_secret(&sid, KEY, value) {
                    Ok(()) => note("ok", String::new(), &mut counts),
                    Err(error) => note("write_error", error, &mut counts),
                }
                index += 1;
            }
        }
        "reader" => {
            let valid: Vec<&str> = env("TP_PROBE_VALID")
                .split(',')
                .map(|v| match v {
                    "a" => a.as_str(),
                    "b" => b.as_str(),
                    "s" => "short replacement",
                    _ => "",
                })
                .collect();
            while Instant::now() < deadline {
                match super::get_secret_result(&sid, KEY) {
                    Ok(Some(value)) if valid.contains(&value.as_str()) => {
                        note("ok", String::new(), &mut counts)
                    }
                    Ok(Some(value)) => note(
                        "mixed",
                        format!("len {} head {:?}", value.len(), &value[..value.len().min(24)]),
                        &mut counts,
                    ),
                    Ok(None) => note("absent", String::new(), &mut counts),
                    Err(error) => note("error", error, &mut counts),
                }
            }
        }
        other => panic!("unknown probe role {other:?}"),
    }
    println!(
        "PROBE_RESULT {} {}",
        role,
        serde_json::json!({ "counts": counts, "samples": samples })
    );
}

fn spawn(role: &str, sid: &str, secs: u64, valid: &str) -> std::process::Child {
    Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "secrets::probe::child", "--ignored", "--nocapture"])
        .env("TP_PROBE_ROLE", role)
        .env("TP_PROBE_SID", sid)
        .env("TP_PROBE_SECS", secs.to_string())
        .env("TP_PROBE_VALID", valid)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn run_scenario(name: &str, readers: usize, secs: u64) {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let (a, _) = values();
    let (read_sid, write_sid, valid) = match name {
        // Readers and the writer share one credential.
        "same" => {
            let sid = format!("toolport-probe-same-{unique}");
            (sid.clone(), sid, "a,b,s")
        }
        // Readers read a credential nobody rewrites while another one churns.
        "cross" => (
            format!("toolport-probe-stable-{unique}"),
            format!("toolport-probe-churn-{unique}"),
            "a",
        ),
        other => panic!("unknown scenario {other}"),
    };
    super::set_secret(&read_sid, KEY, &a).unwrap();
    let mut children = vec![spawn("writer", &write_sid, secs, valid)];
    children.extend((0..readers).map(|_| spawn("reader", &read_sid, secs, valid)));
    let mut totals: BTreeMap<String, u64> = BTreeMap::new();
    for child in children {
        let output = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let line = stdout
            .lines()
            .find(|line| line.starts_with("PROBE_RESULT "))
            .unwrap_or_else(|| {
                panic!(
                    "child produced no result: {}\n{stdout}\n{}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr)
                )
            })
            .to_string();
        println!("{name}: {line}");
        let mut parts = line.splitn(3, ' ');
        let role = parts.nth(1).unwrap().to_string();
        let result: serde_json::Value = serde_json::from_str(parts.next().unwrap()).unwrap();
        for (kind, count) in result["counts"].as_object().unwrap() {
            *totals.entry(format!("{role}.{kind}")).or_default() += count.as_u64().unwrap();
        }
    }
    println!("PROBE_SUMMARY {name} readers={readers} secs={secs} {totals:?}");
    let _ = super::delete_secret(&read_sid, KEY);
    let _ = super::delete_secret(&write_sid, KEY);
}

#[test]
#[ignore = "manual Windows probe"]
fn parent() {
    let _data = crate::registry::DataDirTestEnv::new("keyring-probe-parent");
    let secs: u64 = env("TP_PROBE_SECS").parse().unwrap_or(60);
    let readers: usize = env("TP_PROBE_READERS").parse().unwrap_or(3);
    for scenario in env("TP_PROBE_SCENARIOS").split(',').filter(|s| !s.is_empty()) {
        run_scenario(scenario, readers, secs);
    }
}
