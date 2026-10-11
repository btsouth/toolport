//! Last observed Teams sync, separate from registry configuration and backups.
use crate::registry::{self, TeamConnection};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    pub state: String,
    pub last_success_ms: Option<u64>,
    pub checked_at_ms: u64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Stored {
    team_id: String,
    server_url: String,
    device_id: String,
    status: SyncStatus,
}

// Scope the observation to both the data directory and the connected team.
static LAST_OBSERVATION: std::sync::Mutex<Option<(std::path::PathBuf, Stored)>> =
    std::sync::Mutex::new(None);

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn path() -> Result<std::path::PathBuf, String> {
    registry::conduit_dir()
        .map(|dir| dir.join("team-sync-status.json"))
        .ok_or_else(|| "Could not resolve team status directory".into())
}

fn for_connection(conn: &TeamConnection) -> SyncStatus {
    let receipt_path = path().unwrap_or_default();
    if let Some((observed_path, stored)) = LAST_OBSERVATION
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
    {
        if *observed_path == receipt_path
            && stored.team_id == conn.team_id
            && stored.server_url == conn.server_url
            && stored.device_id == conn.reporting_device_id
        {
            return stored.status.clone();
        }
    }
    std::fs::read_to_string(receipt_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Stored>(&raw).ok())
        .filter(|stored| {
            stored.team_id == conn.team_id
                && stored.server_url == conn.server_url
                && stored.device_id == conn.reporting_device_id
        })
        .map(|stored| stored.status)
        .unwrap_or_else(|| SyncStatus {
            state: "not_checked".into(),
            ..Default::default()
        })
}

pub fn current() -> SyncStatus {
    let mut status = registry::load()
        .ok()
        .and_then(|r| r.team)
        .map(|conn| for_connection(&conn))
        .unwrap_or_default();
    // A retained receipt describes the last check, never an indefinitely live connection.
    if now_ms().saturating_sub(status.checked_at_ms) > 90_000 {
        status.state = "not_checked".into();
    }
    status
}

pub fn record(conn: &TeamConnection, result: Result<(), &str>) -> Result<(), String> {
    let Some(current) = registry::load()?.team else {
        return Ok(());
    };
    if current.team_id != conn.team_id
        || current.server_url != conn.server_url
        || current.reporting_device_id != conn.reporting_device_id
    {
        return Ok(());
    }
    let mut status = for_connection(conn);
    status.checked_at_ms = now_ms();
    status.state = match result {
        Ok(()) => {
            status.last_success_ms = Some(status.checked_at_ms);
            "synced"
        }
        Err(error) if error.starts_with("could not reach the team server:") => "offline",
        Err(_) => "error",
    }
    .into();
    let stored = Stored {
        team_id: conn.team_id.clone(),
        server_url: conn.server_url.clone(),
        device_id: conn.reporting_device_id.clone(),
        status,
    };
    let receipt_path = path()?;
    *LAST_OBSERVATION
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) =
        Some((receipt_path.clone(), stored.clone()));
    registry::atomic_write(
        &receipt_path,
        &serde_json::to_string(&stored).map_err(|e| e.to_string())?,
    )
}

pub fn summary(status: &SyncStatus) -> String {
    let state = match status.state.as_str() {
        "synced" => "Last sync succeeded",
        "offline" => "Offline: cannot reach the team server",
        "error" => "Team sync failed. Try Sync now",
        _ => "Team connection has not been checked recently",
    };
    state.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failure_preserves_last_success_and_does_not_follow_a_different_team() {
        let _data = registry::DataDirTestEnv::new("team-sync-status-history");
        let mut reg = registry::Registry::default();
        let conn: TeamConnection = serde_json::from_value(serde_json::json!({
            "serverUrl":"https://example.invalid", "teamId":"one", "role":"member"
        }))
        .unwrap();
        reg.team = Some(conn.clone());
        registry::save(&reg).unwrap();
        record(&conn, Ok(())).unwrap();
        let success = current().last_success_ms;
        assert!(success.is_some());
        record(
            &conn,
            Err("could not reach the team server: connection refused"),
        )
        .unwrap();
        assert_eq!(current().state, "offline");
        assert_eq!(current().last_success_ms, success);
        assert!(!summary(&current()).contains("up to date"));
        reg.team.as_mut().unwrap().team_id = "two".into();
        registry::save(&reg).unwrap();
        record(&conn, Ok(())).unwrap();
        assert_eq!(current().last_success_ms, None);
    }
    #[test]
    fn failed_receipt_write_keeps_latest_observation_and_success_history() {
        let _data = registry::DataDirTestEnv::new("team-sync-status-write-failure");
        let mut reg = registry::Registry::default();
        let conn: TeamConnection = serde_json::from_value(serde_json::json!({
            "serverUrl":"https://example.invalid", "teamId":"one", "role":"member"
        }))
        .unwrap();
        reg.team = Some(conn.clone());
        registry::save(&reg).unwrap();
        record(&conn, Ok(())).unwrap();
        let success = current().last_success_ms;
        let receipt = path().unwrap();
        let old_receipt = std::fs::read_to_string(&receipt).unwrap();
        std::fs::remove_file(&receipt).unwrap();
        std::fs::create_dir(&receipt).unwrap();
        assert!(record(
            &conn,
            Err("could not reach the team server: connection refused")
        )
        .is_err());
        assert_eq!(current().state, "offline");
        assert_eq!(current().last_success_ms, success);
        // Even when the previous successful receipt remains, both shells must
        // receive the latest failed observation rather than rereading success.
        std::fs::remove_dir(&receipt).unwrap();
        std::fs::write(&receipt, old_receipt).unwrap();
        assert_eq!(current().state, "offline");
        assert_eq!(current().last_success_ms, success);
        reg.team.as_mut().unwrap().reporting_device_id = "different-device".into();
        registry::save(&reg).unwrap();
        assert_eq!(current().state, "not_checked");
        assert_eq!(current().last_success_ms, None);
    }

    #[test]
    fn old_success_is_history_not_a_live_connection() {
        let _data = registry::DataDirTestEnv::new("team-sync-status-age");
        let mut reg = registry::Registry::default();
        let conn: TeamConnection = serde_json::from_value(serde_json::json!({
            "serverUrl":"https://example.invalid", "teamId":"one", "role":"member"
        }))
        .unwrap();
        reg.team = Some(conn.clone());
        registry::save(&reg).unwrap();
        record(&conn, Ok(())).unwrap();
        let success = current().last_success_ms;
        let mut stored: Stored =
            serde_json::from_str(&std::fs::read_to_string(path().unwrap()).unwrap()).unwrap();
        stored.status.checked_at_ms = 0;
        registry::atomic_write(&path().unwrap(), &serde_json::to_string(&stored).unwrap()).unwrap();
        *LAST_OBSERVATION.lock().unwrap() = None;
        assert_eq!(current().state, "not_checked");
        assert_eq!(current().last_success_ms, success);
        record(&conn, Err("server returned 500")).unwrap();
        assert_eq!(current().state, "error");
        assert_eq!(current().last_success_ms, success);
    }
}
