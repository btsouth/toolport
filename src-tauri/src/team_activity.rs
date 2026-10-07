//! Required Teams operational evidence, independent of retained local audit logs.
//! Cumulative counters are acknowledged by revision, never by client wall-clock time.
//! No tool names, inputs, outputs, secrets, or user/client names are stored here.
use crate::registry::{self, Registry};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Counter {
    pub server_id: String,
    pub successes: u64,
    pub failures: u64,
    pub first_success_version: Option<u64>,
    pub latest_success_version: Option<u64>,
    pub config_version: u64,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Journal {
    pub revision: u64,
    pub acknowledged: u64,
    pub counters: BTreeMap<String, Counter>,
}

pub fn new_device_id() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn path(device: &str) -> Result<PathBuf, String> {
    if device.len() != 32 || !device.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Teams reporting identity is not initialized".into());
    }
    Ok(registry::conduit_dir()
        .ok_or("data directory unavailable")?
        .join(format!("team-activity-{device}.json")))
}
fn read(path: &Path) -> Result<Journal, String> {
    match std::fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s).map_err(|e| format!("Teams activity is unreadable: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Journal::default()),
        Err(e) => Err(e.to_string()),
    }
}
fn mutate(path: &Path, f: impl FnOnce(&mut Journal)) -> Result<(), String> {
    let _lock = registry::lock_at(path)?;
    let mut journal = read(path)?;
    f(&mut journal);
    registry::atomic_write(
        path,
        &serde_json::to_string(&journal).map_err(|e| e.to_string())?,
    )
}
pub fn snapshot(device: &str) -> Result<Journal, String> {
    read(&path(device)?)
}
pub fn acknowledge(device: &str, revision: u64) -> Result<(), String> {
    mutate(&path(device)?, |j| {
        j.acknowledged = j.acknowledged.max(revision.min(j.revision))
    })
}
fn increment(j: &mut Journal, server_id: &str, version: u64, ok: bool) {
    let row = j
        .counters
        .entry(server_id.into())
        .or_insert_with(|| Counter {
            server_id: server_id.into(),
            ..Counter::default()
        });
    row.config_version = version;
    if ok {
        row.successes = row.successes.saturating_add(1);
        row.first_success_version.get_or_insert(version);
        row.latest_success_version = Some(version);
    } else {
        row.failures = row.failures.saturating_add(1);
    }
    j.revision = j.revision.saturating_add(1);
}

#[derive(Serialize, Deserialize)]
struct Event {
    server_id: String,
    version: u64,
    ok: bool,
}

/// Keep the existing locked atomic journal, with IO confined to the telemetry writer.
pub(crate) fn append_records_at(path: &Path, lines: &[String]) -> Result<(), String> {
    let events = lines
        .iter()
        .map(|line| serde_json::from_str::<Event>(line).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    mutate(path, |journal| {
        for event in events {
            increment(journal, &event.server_id, event.version, event.ok);
        }
    })
}

/// Admission is nonblocking; dropped or unconfirmed updates are exposed by telemetry health.
pub fn record(reg: &Registry, local_id: &str, ok: bool) -> Result<(), String> {
    let Some(team) = &reg.team else { return Ok(()) };
    let tag = format!("team:{}", team.team_id);
    if !reg
        .servers
        .iter()
        .any(|s| s.id == local_id && s.source.as_deref() == Some(tag.as_str()))
    {
        return Ok(());
    }
    // Missing legacy mapping is unknown, never guessed from a display prefix.
    let Some(server_id) = team.managed_server_ids.get(local_id) else {
        return Ok(());
    };
    let event = Event {
        server_id: server_id.clone(),
        version: team.last_version.max(0) as u64,
        ok,
    };
    crate::telemetry::record(
        &path(&team.reporting_device_id)?,
        &serde_json::to_string(&event).map_err(|error| error.to_string())?,
        crate::telemetry::Rotation::TeamActivity,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recording_never_waits_for_the_journal_lock_and_failed_batches_keep_saved_counts() {
        use crate::registry::tests::{with_atomic_failure, FailingAtomicWriteStep::*};
        use std::sync::mpsc;
        use std::time::Duration;

        let _lock = registry::data_dir_test_lock();
        let device = new_device_id().unwrap();
        let dir = std::env::temp_dir().join(format!("teams-queued-{device}"));
        std::fs::create_dir_all(&dir).unwrap();
        let _data = registry::DataDirOverride::set(&dir);
        let path = path(&device).unwrap();
        mutate(&path, |j| increment(j, "Audit-Echo", 1, true)).unwrap();
        let guard = registry::lock_at(&path).unwrap();
        let mut reg = Registry::default();
        reg.team = Some(
            serde_json::from_value(serde_json::json!({
                "serverUrl":"https://teams.example.com", "teamId":"test", "role":"member",
                "lastVersion":2, "reportingDeviceId":device,
                "managedServerIds":{"local":"Audit-Echo"}
            }))
            .unwrap(),
        );
        reg.servers.push(
            serde_json::from_value(serde_json::json!({
                "id":"local", "name":"fixture", "transport":"stdio", "source":"team:test"
            }))
            .unwrap(),
        );
        let (tx, rx) = mpsc::sync_channel(1);
        let caller = std::thread::spawn(move || {
            tx.send(record(&reg, "local", false)).unwrap();
        });
        let result = rx.recv_timeout(Duration::from_secs(1));
        drop(guard);
        caller.join().unwrap();
        result.expect("record waited on the journal lock").unwrap();
        assert!(crate::telemetry::flush_for_test(Duration::from_secs(5)));
        let saved = std::fs::read(&path).unwrap();
        let row = snapshot(&device)
            .unwrap()
            .counters
            .remove("Audit-Echo")
            .unwrap();
        assert_eq!((row.successes, row.failures), (1, 1));
        for step in [Permissions, Write, Rename] {
            let lines = vec![serde_json::to_string(&Event {
                server_id: "Audit-Echo".into(),
                version: 3,
                ok: true,
            })
            .unwrap()];
            assert!(with_atomic_failure(step, || append_records_at(&path, &lines)).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), saved);
            assert!(with_atomic_failure(step, || acknowledge(&device, 2)).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), saved);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn durable_counts_survive_rotation_offline_and_ack_races() {
        let dir = std::env::temp_dir().join(format!("teams-journal-{}", new_device_id().unwrap()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("activity.json");
        mutate(&p, |j| increment(j, "Audit-Echo", 2, true)).unwrap();
        let sent = read(&p).unwrap();
        mutate(&p, |j| increment(j, "Audit-Echo", 3, false)).unwrap();
        mutate(&p, |j| j.acknowledged = sent.revision).unwrap();
        let after = read(&p).unwrap();
        assert_eq!(after.revision, 2);
        assert_eq!(after.acknowledged, 1);
        let row = &after.counters["Audit-Echo"];
        assert_eq!((row.successes, row.failures), (1, 1));
        assert_eq!(row.first_success_version, Some(2));
        assert_eq!(row.latest_success_version, Some(2));
        assert_eq!(row.config_version, 3);
        let wire = serde_json::to_value(row).unwrap();
        assert_eq!(wire.as_object().unwrap().len(), 6);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
