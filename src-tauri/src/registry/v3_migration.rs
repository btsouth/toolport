//! Preserve every existing client's server and tool access while separating global enablement.
use super::MigrationContext;
use serde_json::Value;
use std::collections::BTreeSet;

pub(super) fn migrate_v2_to_v3(value: &mut Value, _: &MigrationContext) -> Result<(), String> {
    if value.get("version").and_then(Value::as_u64).unwrap_or(1) >= 3 {
        return Ok(());
    }
    let document = value.as_object_mut().ok_or("registry must be an object")?;
    // Malformed typed fields are left to the framework's deserialization/recovery path.
    let Some(profiles) = document.get("profiles").and_then(Value::as_array) else {
        return Ok(());
    };
    let active = document
        .get("activeProfileId")
        .and_then(Value::as_str)
        .or_else(|| {
            profiles
                .first()
                .and_then(|p| p.get("id"))
                .and_then(Value::as_str)
        })
        .unwrap_or("default")
        .to_string();
    if profiles.iter().any(|p| {
        p.get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| id.starts_with('@'))
    }) {
        return Err("Access-set ids beginning with @ are reserved; the original registry was left untouched".into());
    }
    let ids = |profile: &Value| -> Result<BTreeSet<String>, String> {
        profile
            .get("enabledServerIds")
            .and_then(Value::as_array)
            .unwrap_or(&Vec::new())
            .iter()
            .map(|id| {
                id.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| "server id must be a string".to_string())
            })
            .collect()
    };
    let mut union = BTreeSet::new();
    for profile in profiles {
        union.extend(ids(profile)?);
    }
    let old_active = profiles
        .iter()
        .find(|p| p.get("id").and_then(Value::as_str) == Some(&active));
    let active_set = old_active.map(ids).transpose()?.unwrap_or_default();
    let active_missing = old_active.is_none();
    // Store the stable historical context even when the server sets match. Tool scopes,
    // instructions and integrity baselines are independent of server membership.
    document.insert(
        "defaultAccessContextId".into(),
        Value::String(active.clone()),
    );
    document.insert("defaultAccessLegacyPolicy".into(), Value::Bool(true));
    if active_set != union || active_missing {
        document.insert("defaultAccessProfileId".into(), Value::String(active));
    } else {
        document.remove("defaultAccessProfileId");
    }
    let servers = document
        .get_mut("servers")
        .and_then(Value::as_array_mut)
        .ok_or("servers must be an array")?;
    for server in servers {
        let id = server
            .get("id")
            .and_then(Value::as_str)
            .ok_or("server id must be a string")?;
        let enabled = union.contains(id);
        server
            .as_object_mut()
            .ok_or("server must be an object")?
            .insert("enabled".into(), Value::Bool(enabled));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{
        load_from, load_from_with_migrations_for_test, migration_backup_files, Registry, MIGRATIONS,
    };
    use serde_json::json;

    fn fixture(multiple: bool) -> Value {
        let mut value = json!({"version":2,"servers":[
            {"id":"github","name":"GitHub","transport":"http","url":"https://example.com/github","future":"keep"},
            {"id":"files","name":"Files","transport":"stdio","command":"files"},
            {"id":"off","name":"Off","transport":"http","url":"https://example.com/off"}],
            "profiles":[{"id":"default","name":"Default","enabledServerIds":["github"],"toolScope":{"github":["read"]},"instructions":"Keep this"}],
            "activeProfileId":"default","clientScopes":{"unscoped":"","scoped":"default"},
            "folderProfiles":[{"path":"/work","profile":"default"}],"future":{"keep":true}});
        if multiple {
            value["profiles"].as_array_mut().unwrap().push(json!({"id":"work","name":"Work","enabledServerIds":["files"],"toolScope":{"files":["list"]}}));
            value["clientScopes"]["worker"] = json!("work");
            value["folderProfiles"]
                .as_array_mut()
                .unwrap()
                .push(json!({"path":"/work/project","profile":"work"}));
        }
        value
    }
    fn view(reg: &Registry, reference: &str) -> (Vec<String>, Value) {
        let id = reg.resolve_profile_id(reference);
        let servers = reg
            .enabled_servers_for(reference)
            .iter()
            .map(|s| s.id.clone())
            .collect();
        let scope = reg
            .access_profile(&id)
            .map(|p| json!(p.tool_scope))
            .unwrap_or(json!({}));
        (servers, scope)
    }
    #[test]
    fn v3_migration_preserves_unscoped_scoped_and_folder_access() {
        for multiple in [false, true] {
            let original = fixture(multiple);
            let before: Registry = serde_json::from_value(original.clone()).unwrap();
            let mut migrated = original.clone();
            migrate_v2_to_v3(
                &mut migrated,
                &MigrationContext {
                    date: "2026-10-07".into(),
                    data_dir: std::env::temp_dir(),
                },
            )
            .unwrap();
            migrated["version"] = json!(3);
            let after: Registry = serde_json::from_value(migrated.clone()).unwrap();
            assert_eq!(
                after.default_access_profile_id.as_deref(),
                multiple.then_some("default")
            );
            for reference in std::iter::once("")
                .chain(before.client_scopes.values().map(String::as_str))
                .chain(before.folder_profiles.iter().map(|f| f.profile.as_str()))
            {
                assert_eq!(
                    view(&before, reference),
                    view(&after, reference),
                    "{multiple} {reference}"
                );
            }
            for field in ["profiles", "clientScopes", "folderProfiles", "future"] {
                assert_eq!(original[field], migrated[field]);
            }
            assert_eq!(migrated["servers"][0]["future"], "keep");
            assert!(!after.server_enabled("off"));
            assert_eq!(
                crate::registry::profile_store_key(&after.default_access_id()),
                crate::registry::profile_store_key("default")
            );
            assert_eq!(
                crate::registry::profile_store_key(&after.all_access_id()),
                crate::registry::profile_store_key("default")
            );
            let once = migrated.clone();
            migrate_v2_to_v3(
                &mut migrated,
                &MigrationContext {
                    date: "2026-10-07".into(),
                    data_dir: std::env::temp_dir(),
                },
            )
            .unwrap();
            assert_eq!(once, migrated);
        }
    }
    #[test]
    fn v3_loader_backs_up_raw_document_is_idempotent_and_v2_refuses_it() {
        let dir = std::env::temp_dir().join(format!(
            "toolport-v3-access-{}-{}",
            std::process::id(),
            crate::registry::now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("registry.json");
        let raw = serde_json::to_string_pretty(&fixture(true)).unwrap();
        std::fs::write(&path, &raw).unwrap();
        let migrated = load_from(&path).unwrap();
        assert_eq!(migrated.version, 3);
        let backups = migration_backup_files(&path);
        assert_eq!(backups.len(), 1);
        assert_eq!(std::fs::read_to_string(&backups[0]).unwrap(), raw);
        let bytes = std::fs::read(&path).unwrap();
        load_from(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(migration_backup_files(&path).len(), 1);
        let error = load_from_with_migrations_for_test(&path, &MIGRATIONS[..1], 2).unwrap_err();
        assert!(error.contains("newer"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn global_off_overrides_every_access_set_and_all_access_includes_unprofiled_servers() {
        let mut value = fixture(true);
        migrate_v2_to_v3(
            &mut value,
            &MigrationContext {
                date: "2026-10-07".into(),
                data_dir: std::env::temp_dir(),
            },
        )
        .unwrap();
        value["version"] = json!(3);
        let mut reg: Registry = serde_json::from_value(value).unwrap();
        reg.set_global_server_enabled("github", false).unwrap();
        assert!(!reg.is_enabled("default", "github"));
        assert!(!reg.is_enabled(&reg.default_access_id(), "github"));
        assert!(!reg.is_enabled(&reg.all_access_id(), "github"));
        reg.set_global_server_enabled("off", true).unwrap();
        assert!(reg.is_enabled(&reg.all_access_id(), "off"));
        assert!(!reg.is_enabled("default", "off"));
        assert!(!reg.is_enabled("missing", "off"));
    }
    #[test]
    fn migrated_default_keeps_quarantine_without_leaking_to_other_access_sets() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!(
            "toolport-v3-quarantine-{}-{}",
            std::process::id(),
            crate::registry::now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _override = crate::registry::DataDirOverride::new(&dir);
        for (id, tool) in [("default", "github__read"), ("work", "files__list")] {
            std::fs::write(
                dir.join(format!(
                    "quarantine-v2-{}.json",
                    crate::registry::profile_store_key(id)
                )),
                json!({tool: {"reason":"changed", "at":1}}).to_string(),
            )
            .unwrap();
        }
        for multiple in [false, true] {
            let mut value = fixture(multiple);
            migrate_v2_to_v3(
                &mut value,
                &MigrationContext {
                    data_dir: dir.clone(),
                    date: "2026-10-07".into(),
                },
            )
            .unwrap();
            value["version"] = json!(3);
            let mut reg: Registry = serde_json::from_value(value).unwrap();
            let blocked =
                crate::integrity::quarantined_checked(Some(&reg.default_access_id())).unwrap();
            assert!(blocked.contains("github__read"));
            assert!(!blocked.contains("files__list"));
            assert!(crate::integrity::quarantined_checked(Some("work"))
                .unwrap()
                .contains("files__list"));
            assert!(!crate::integrity::quarantined_checked(Some("work"))
                .unwrap()
                .contains("github__read"));
            reg.set_default_access(None).unwrap();
            assert!(
                reg.access_profile(&reg.default_access_id()).is_none(),
                "explicit All drops legacy tool narrowing"
            );
            assert!(
                crate::integrity::quarantined_checked(Some(&reg.default_access_id()))
                    .unwrap()
                    .contains("github__read")
            );
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
