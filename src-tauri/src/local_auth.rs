//! Installation-local authentication sharing for an explicitly adopted Team definition.
//! No token is copied. Both identities use one vault namespace and refresh lock.
use crate::registry::{Registry, ServerEntry};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const FIELD: &str = "localTeamAuthentication";

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Binding {
    pub personal_id: String,
    pub team_id: String,
    pub origin: String,
    pub device_id: String,
    pub fingerprint: String,
}

fn bindings(reg: &Registry) -> Result<BTreeMap<String, Binding>, String> {
    reg.unknown_fields.get(FIELD).map_or_else(
        || Ok(BTreeMap::new()),
        |value| {
            serde_json::from_value(value.clone())
                .map_err(|_| "Local authentication bindings are unreadable".into())
        },
    )
}

pub(crate) fn supplies_authentication(reg: &Registry, personal: &str) -> Result<bool, String> {
    Ok(bindings(reg)?.iter().any(|(id, binding)| {
        binding.personal_id == personal && reg.servers.iter().any(|server| server.id == *id)
    }))
}

pub(crate) fn bind(
    reg: &mut Registry,
    managed: &ServerEntry,
    personal: &ServerEntry,
) -> Result<(), String> {
    let team = reg.team.as_ref().ok_or("Not connected to a team")?;
    let fingerprint = crate::teams::consent_fingerprint(personal);
    if fingerprint != crate::teams::consent_fingerprint(managed) {
        return Err("The shared definition changed. Review its local setup separately.".into());
    }
    let mut entries = bindings(reg)?;
    if let Some(original) = reg.servers.iter_mut().find(|s| s.id == personal.id) {
        original.unknown_fields.remove("teamRouteRemoved");
    }
    entries.insert(
        managed.id.clone(),
        Binding {
            personal_id: personal.id.clone(),
            team_id: team.team_id.clone(),
            origin: team.server_url.clone(),
            device_id: team.reporting_device_id.clone(),
            fingerprint,
        },
    );
    reg.unknown_fields.insert(
        FIELD.into(),
        serde_json::to_value(entries).map_err(|e| e.to_string())?,
    );
    Ok(())
}

pub(crate) fn owner_in(reg: &Registry, id: &str) -> Result<String, String> {
    let entries = bindings(reg)?;
    let Some(binding) = entries.get(id) else {
        return Ok(personal_http_owner(reg, id));
    };
    let valid = reg.team.as_ref().is_some_and(|team| {
        team.team_id == binding.team_id
            && team.server_url == binding.origin
            && team.reporting_device_id == binding.device_id
            && team.managed_server_ids.get(id) == Some(&binding.personal_id)
    });
    let personal = reg.servers.iter().find(|s| {
        s.id == binding.personal_id && !s.source.as_deref().unwrap_or("").starts_with("team:")
    });
    let managed = reg
        .servers
        .iter()
        .find(|s| s.id == id && s.source.as_deref() == Some(&format!("team:{}", binding.team_id)));
    if !valid
        || !personal.zip(managed).is_some_and(|(personal, managed)| {
            crate::teams::consent_fingerprint(personal) == binding.fingerprint
                && crate::teams::consent_fingerprint(managed) == binding.fingerprint
        })
    {
        return Err("This shared server's local authentication binding changed. Review its setup before signing in again.".into());
    }
    Ok(binding.personal_id.clone())
}

pub(crate) fn personal_credential_destination(server: &ServerEntry) -> String {
    use sha2::{Digest, Sha256};
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&server.url).expect("URL serialization"))
    )
}

fn personal_http_owner(reg: &Registry, id: &str) -> String {
    let Some(server) = reg.servers.iter().find(|s| s.id == id && s.url.is_some()) else {
        return id.into();
    };
    let Some(base) = reg
        .unknown_fields
        .get("personalSyncCredentialDestinations")
        .and_then(|destinations| destinations.get(id))
        .or_else(|| {
            server
                .unknown_fields
                .get("personalSyncCredentialDestination")
        })
        .and_then(serde_json::Value::as_str)
    else {
        return id.into();
    };
    let destination = personal_credential_destination(server);
    if base == destination {
        id.into()
    } else {
        format!("{id}-sync-{destination}")
    }
}

/// Explicitly reviewing a changed definition drops its old local binding.
/// Authentication then belongs to the managed identity; the original stays untouched.
pub(crate) fn detach_changed(reg: &mut Registry, id: &str) -> Result<(), String> {
    let mut entries = bindings(reg)?;
    if entries.contains_key(id) && owner_in(reg, id).is_err() {
        entries.remove(id);
        reg.unknown_fields.insert(
            FIELD.into(),
            serde_json::to_value(entries).map_err(|e| e.to_string())?,
        );
    }
    Ok(())
}

/// Whether an earlier handoff bound a Team copy of this connection to `personal`,
/// and that copy is enabled in `profile`. The original may have changed since:
/// that change is what re-sharing publishes, and the handoff checks it again.
pub(crate) fn bound_copy_enabled(reg: &Registry, profile: &str, personal: &str) -> bool {
    let Some(team) = reg.team.as_ref() else {
        return false;
    };
    bindings(reg).is_ok_and(|entries| {
        entries.iter().any(|(managed, binding)| {
            binding.personal_id == personal
                && binding.team_id == team.team_id
                && binding.origin == team.server_url
                && binding.device_id == team.reporting_device_id
                && team.managed_server_ids.get(managed) == Some(&binding.personal_id)
                && reg.is_enabled(profile, managed)
        })
    })
}

pub(crate) fn ensure_unconfigured(reg: &Registry, managed: &ServerEntry) -> Result<(), String> {
    if bindings(reg)?.contains_key(&managed.id) {
        owner_in(reg, &managed.id)?;
        return Ok(());
    }
    if managed
        .env
        .iter()
        .any(|env| env.value.as_ref().is_some_and(|v| !v.is_empty()))
        || managed.launch.as_ref().is_some_and(|launch| {
            launch
                .inputs
                .iter()
                .any(|i| i.value.as_ref().is_some_and(|v| !v.is_empty()))
        })
        || crate::secrets::has_own_credentials(managed)?
    {
        return Err("This team copy already has its own local credentials. Keep its existing setup and enable it separately.".into());
    }
    Ok(())
}

/// Read one atomically saved snapshot without acquiring the registry writer lock.
/// Vault operations also occur inside registry updates, so calling registry::load
/// here would deadlock. Never recover or rewrite files from this read path.
pub(crate) fn owner(id: &str) -> Result<String, String> {
    let Some(path) = crate::registry::resolved_path() else {
        return Ok(id.into());
    };
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(id.into()),
        Err(_) => return Err("Cannot verify local authentication ownership".into()),
    };
    let reg: Registry = serde_json::from_slice(&bytes)
        .map_err(|_| "Cannot verify local authentication ownership")?;
    owner_in(&reg, id)
}

/// Keep non-secret local inputs and profile restrictions across a normal re-sync.
/// Neither is taken from another member or inferred merely from a matching name.
pub(crate) fn reconcile(reg: &mut Registry, previous: &[ServerEntry]) {
    let Ok(entries) = bindings(reg) else {
        return;
    };
    for (id, binding) in entries {
        if owner_in(reg, &id).ok().as_deref() != Some(&binding.personal_id) {
            let _ = detach_changed(reg, &id);
            continue;
        }
        let Some(previous_local) = previous.iter().find(|s| s.id == id).cloned() else {
            continue;
        };
        if let Some(managed) = reg.servers.iter_mut().find(|s| s.id == id) {
            managed.env = previous_local.env.clone();
            managed.launch = previous_local.launch.clone();
        }
        for profile in &mut reg.profiles {
            if let Some(scope) = profile.tool_scope.get(&binding.personal_id).cloned() {
                let restricted = match profile.tool_scope.get(&id) {
                    Some(team_scope) => scope
                        .into_iter()
                        .filter(|tool| team_scope.contains(tool))
                        .collect(),
                    None => scope,
                };
                profile.tool_scope.insert(id.clone(), restricted);
            }
        }
    }
}

/// Restore originals whose managed route was enabled, including a temporary review hold.
pub(crate) fn restore_personal_routes(reg: &mut Registry, team_id: &str) {
    restore_routes(reg, team_id, None);
}

/// Remote removal, disablement or access revocation stops the saved original too.
/// Forget the binding so a later disconnect cannot undo this tightening.
pub(crate) fn revoke_personal_route(reg: &mut Registry, team_id: &str, managed: &str) {
    let Ok(mut entries) = bindings(reg) else {
        return;
    };
    let Some(binding) = entries.get(managed).filter(|b| b.team_id == team_id) else {
        return;
    };
    if let Some(personal) = reg.servers.iter_mut().find(|s| s.id == binding.personal_id) {
        personal.enabled = false;
        personal
            .unknown_fields
            .insert("teamRouteRemoved".into(), serde_json::Value::Bool(true));
    }
    for profile in &mut reg.profiles {
        profile
            .enabled_server_ids
            .retain(|id| id != &binding.personal_id);
    }
    entries.remove(managed);
    if let Ok(value) = serde_json::to_value(entries) {
        reg.unknown_fields.insert(FIELD.into(), value);
    }
}

fn restore_routes(reg: &mut Registry, team_id: &str, only: Option<&str>) {
    let Ok(mut entries) = bindings(reg) else {
        return;
    };
    for (managed, binding) in &entries {
        if binding.team_id != team_id
            || only.is_some_and(|id| id != managed)
            || !reg.servers.iter().any(|s| s.id == binding.personal_id)
        {
            continue;
        }
        let held = crate::teams::held_server_access(reg, managed);
        let managed_on = held
            .as_ref()
            .map_or_else(|| reg.server_enabled(managed), |(enabled, _)| *enabled);
        for profile in &mut reg.profiles {
            if managed_on
                && (profile.enabled_server_ids.contains(managed)
                    || held
                        .as_ref()
                        .is_some_and(|(_, profiles)| profiles.contains(&profile.id)))
                && !profile.enabled_server_ids.contains(&binding.personal_id)
            {
                profile.enabled_server_ids.push(binding.personal_id.clone());
            }
        }
        if managed_on {
            if let Some(personal) = reg.servers.iter_mut().find(|s| s.id == binding.personal_id) {
                personal.enabled = true;
            }
        }
    }
    entries.retain(|managed, binding| {
        binding.team_id != team_id || only.is_some_and(|id| id != managed)
    });
    if let Ok(value) = serde_json::to_value(entries) {
        reg.unknown_fields.insert(FIELD.into(), value);
    }
}

/// Consolidate a previously adopted copy onto its existing credential identity.
/// Only exact, still-valid bindings qualify; no credentials are read or copied.
pub(crate) fn adopt_personal_sync_routes(reg: &mut Registry) -> Result<(), String> {
    let mut entries = bindings(reg)?;
    for (managed_id, binding) in entries.clone() {
        if owner_in(reg, &managed_id).ok().as_deref() != Some(&binding.personal_id) {
            continue;
        }
        let Some(mut managed) = reg.servers.iter().find(|s| s.id == managed_id).cloned() else {
            continue;
        };
        managed.id = binding.personal_id.clone();
        managed.unknown_fields.insert(
            "teamOriginalId".into(),
            serde_json::json!(binding.personal_id),
        );
        reg.servers
            .retain(|s| s.id != managed_id && s.id != binding.personal_id);
        for profile in &mut reg.profiles {
            for id in &mut profile.enabled_server_ids {
                if *id == managed_id {
                    *id = binding.personal_id.clone();
                }
            }
            profile.enabled_server_ids.sort();
            profile.enabled_server_ids.dedup();
            if let Some(scope) = profile.tool_scope.remove(&managed_id) {
                let scope = match profile.tool_scope.get(&binding.personal_id) {
                    Some(personal_scope) => scope
                        .into_iter()
                        .filter(|tool| personal_scope.contains(tool))
                        .collect(),
                    None => scope,
                };
                profile
                    .tool_scope
                    .insert(binding.personal_id.clone(), scope);
            }
        }
        if let Some(team) = &mut reg.team {
            team.managed_server_ids.remove(&managed_id);
            team.managed_server_ids
                .insert(binding.personal_id.clone(), binding.personal_id.clone());
        }
        reg.servers.push(managed);
        entries.remove(&managed_id);
    }
    reg.unknown_fields.insert(
        FIELD.into(),
        serde_json::to_value(entries).map_err(|e| e.to_string())?,
    );
    Ok(())
}
