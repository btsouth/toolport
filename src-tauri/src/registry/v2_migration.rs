//! Registry schema v1 -> v2, the Toolport 2.0 upgrade.
//!
//! 2.0 cut agent rules, agent activity hooks, agent permissions and the guard hook,
//! routines, agent control and the legacy gateway topology, and folded the separate
//! safety toggles into one `safetyLevel`. This step drops the cut fields. Anything the
//! user wrote that only lived there is exported to `<data dir>/exports/` first, so
//! nothing they authored is lost with the field. Client files are never edited.
//!
//! The 1.x safety toggles stay as a mirror of the level, because released 1.x builds
//! have no schema check and a 1.x process can still be running across the upgrade.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::{atomic_write, MigrationContext};

/// Frozen 1.x personal-rules markers (#1018). 1.x wrote the rule text into client
/// files either as a block between these markers or as an owned `toolport-rules.md`
/// whose first line starts with the owned header.
const RULES_START_PREFIX: &str = "<!-- toolport:rules:start";
const RULES_END: &str = "<!-- toolport:rules:end -->";
const RULES_OWNED_HEADER_PREFIX: &str = "<!-- Toolport personal rules";

/// A recorded rules file larger than this is noted in the export, not copied.
const MAX_RULES_FILE_BYTES: u64 = 1024 * 1024;

/// Top-level v1 keys that do not exist in v2.
const DROPPED_KEYS: &[&str] = &[
    // Agent rules (exported first).
    "ruleSets",
    "activeRuleSetId",
    "rulesClients",
    "rulesTargets",
    "rulesProjects",
    // Agent permissions and the guard hook (rules exported first).
    "guardCursorMode",
    "guardCursorAskViaToolport",
    "guardClaudeMode",
    "guardTargets",
    "agentPermissionsEnabled",
    "agentPermissionRules",
    "agentPermissionTargets",
    // Agent activity hooks.
    "hooksEnabled",
    "hookTargets",
    // Routines (routines.json exported first) and agent control.
    "allowRoutineWrites",
    "allowAgentControl",
    // Legacy per-client gateway topology.
    "gatewayTopology",
    // Always on in 2.0.
    "contentDefense",
    "integrityCheck",
];

pub(super) fn migrate_v1_to_v2(
    value: &mut Value,
    context: &MigrationContext,
) -> Result<(), String> {
    let registry = value
        .as_object_mut()
        .ok_or_else(|| "the registry is not a JSON object".to_string())?;
    // Exports first: a failure here leaves the v1 file as it was.
    export_rules(registry, context)?;
    export_agent_permissions(registry, context)?;
    export_routines(context)?;

    let level = safety_level(registry);
    registry.insert("safetyLevel".to_string(), Value::from(level));
    // The 1.x mirror of the level; see `Registry::sync_legacy_safety_mirror`.
    let strict = level == "strict";
    for key in ["denyDestructive", "quarantineOnDrift", "blockOnInjection"] {
        registry.insert(key.to_string(), Value::Bool(strict));
    }
    registry.insert("humanApproval".to_string(), Value::Bool(level != "off"));
    registry.insert("confirmDestructive".to_string(), Value::Bool(false));
    // Code Mode is opt-in in 2.0, including for existing users.
    // TOOLPORT_CODE_MODE=1 still forces it on.
    registry.insert("codeMode".to_string(), Value::Bool(false));
    for key in DROPPED_KEYS {
        registry.remove(*key);
    }
    Ok(())
}

/// A level already chosen in a 2.0 preview is kept. Otherwise any v1 blocking flag
/// means Strict, and everything else gets the 2.0 default, Ask. Team-forced flags
/// are not read here: team policy arrives from the service.
fn safety_level(registry: &Map<String, Value>) -> &'static str {
    match registry.get("safetyLevel").and_then(Value::as_str) {
        Some("off") => return "off",
        Some("ask") => return "ask",
        Some("strict") => return "strict",
        _ => {}
    }
    let on = |key: &str| registry.get(key).and_then(Value::as_bool).unwrap_or(false);
    if on("denyDestructive") || on("quarantineOnDrift") || on("blockOnInjection") {
        "strict"
    } else {
        "ask"
    }
}

fn array<'a>(registry: &'a Map<String, Value>, key: &str) -> &'a [Value] {
    registry
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn export_rules(registry: &Map<String, Value>, context: &MigrationContext) -> Result<(), String> {
    let sets = array(registry, "ruleSets");
    let projects = array(registry, "rulesProjects");
    let mut targets: Vec<String> = array(registry, "rulesTargets")
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    for project in projects {
        let project_targets = project.get("targets").and_then(Value::as_array);
        for target in project_targets
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !targets.iter().any(|known| known == target) {
                targets.push(target.to_string());
            }
        }
    }
    if sets.is_empty() && projects.is_empty() && targets.is_empty() {
        return Ok(());
    }
    let active = registry
        .get("activeRuleSetId")
        .and_then(Value::as_str)
        .unwrap_or("");
    let set_name = |id: &str| {
        sets.iter()
            .find(|set| text(set, "id") == id)
            .map(|set| text(set, "name").to_string())
            .unwrap_or_else(|| id.to_string())
    };

    let mut out = String::from(
        "# Toolport personal agent rules\n\n\
         Toolport 2.0 removed personal agent rules. This is the rule text your Toolport 1.x \
         settings held, saved when Toolport upgraded them. Toolport did not change or remove \
         any of the files listed below. Edit them yourself if you want the rules changed or gone.\n",
    );
    if !sets.is_empty() {
        out.push_str("\n## Rule sets\n");
        for set in sets {
            let mut heading = text(set, "name").to_string();
            if heading.is_empty() {
                heading = text(set, "id").to_string();
            }
            if !active.is_empty() && text(set, "id") == active {
                heading.push_str(" (active)");
            }
            out.push_str(&format!("\n### {heading}\n\n"));
            push_fenced(&mut out, text(set, "content"));
        }
    }
    if !projects.is_empty() {
        out.push_str("\n## Projects\n\n");
        for project in projects {
            let mut line = format!("- `{}`", text(project, "path"));
            if let Some(set_id) = project.get("setId").and_then(Value::as_str) {
                line.push_str(&format!(", rule set \"{}\"", set_name(set_id)));
            }
            let mut files: Vec<&str> = project
                .get("files")
                .and_then(Value::as_object)
                .map(|files| {
                    files
                        .iter()
                        .filter(|(_, on)| on.as_bool() == Some(true))
                        .map(|(key, _)| key.as_str())
                        .collect()
                })
                .unwrap_or_default();
            files.sort_unstable();
            if !files.is_empty() {
                line.push_str(&format!(", files: {}", files.join(", ")));
            }
            out.push_str(&line);
            out.push('\n');
        }
    }
    if !targets.is_empty() {
        out.push_str("\n## Files Toolport wrote rules into\n");
        for target in &targets {
            out.push_str(&format!("\n### `{target}`\n\n"));
            match rules_in_file(Path::new(target)) {
                Ok(body) => push_fenced(&mut out, &body),
                Err(note) => {
                    out.push_str(&note);
                    out.push('\n');
                }
            }
        }
    }
    write_export(context, "rules", "md", &out).map(|_| ())
}

/// The Toolport rules text currently in a recorded client file, read only. `Err`
/// carries a one-line note for the export when there is nothing to copy.
fn rules_in_file(path: &Path) -> Result<String, String> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err("The file no longer exists.".to_string());
        }
        Err(error) => return Err(format!("Could not read the file: {error}.")),
    };
    // Never open a FIFO or device: a read could block the migration.
    if !metadata.is_file() {
        return Err("Not a regular file; skipped.".to_string());
    }
    if metadata.len() > MAX_RULES_FILE_BYTES {
        return Err("The file is larger than 1 MiB; skipped.".to_string());
    }
    let content = std::fs::read_to_string(path)
        .map_err(|error| format!("Could not read the file: {error}."))?;
    if let Some(start) = content.find(RULES_START_PREFIX) {
        let after_marker = content[start..]
            .find('\n')
            .map(|newline| start + newline + 1)
            .unwrap_or(content.len());
        let end = content[after_marker..]
            .find(RULES_END)
            .map(|end| after_marker + end)
            .unwrap_or(content.len());
        return Ok(content[after_marker..end].trim_matches('\n').to_string());
    }
    if let Some(rest) = content.strip_prefix(RULES_OWNED_HEADER_PREFIX) {
        let body = rest.split_once('\n').map(|(_, body)| body).unwrap_or("");
        return Ok(body.trim_matches('\n').to_string());
    }
    Err("No Toolport rules block found in the file.".to_string())
}

/// Append `body` in a fence longer than any backtick run inside it, so rule text that
/// holds its own code blocks survives intact.
fn push_fenced(out: &mut String, body: &str) {
    let mut longest = 0;
    let mut run = 0;
    for c in body.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let fence = "`".repeat((longest + 1).max(3));
    out.push_str(&format!("{fence}markdown\n{body}\n{fence}\n"));
}

fn export_agent_permissions(
    registry: &Map<String, Value>,
    context: &MigrationContext,
) -> Result<(), String> {
    let rules = registry.get("agentPermissionRules").cloned();
    let added = registry.get("agentPermissionTargets").cloned();
    let has = |value: &Option<Value>| match value {
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(items)) => !items.is_empty(),
        _ => false,
    };
    if !has(&rules) && !has(&added) {
        return Ok(());
    }
    let export = serde_json::json!({
        "note": "Toolport 2.0 removed agent permissions. These are the rules you set in Toolport 1.x and the entries it added to each settings file. Toolport left those entries in place.",
        "enabled": registry.get("agentPermissionsEnabled").cloned().unwrap_or(Value::Bool(false)),
        "rules": rules.unwrap_or_else(|| Value::Array(Vec::new())),
        "addedToFiles": added.unwrap_or_else(|| Value::Object(Map::new())),
    });
    let json = serde_json::to_string_pretty(&export).map_err(|error| error.to_string())?;
    write_export(context, "agent-permissions", "json", &format!("{json}\n")).map(|_| ())
}

/// Copy `routines.json` as is. The original stays where it was.
fn export_routines(context: &MigrationContext) -> Result<(), String> {
    let source = context.data_dir.join("routines.json");
    match std::fs::metadata(&source) {
        Ok(metadata) if metadata.is_file() => {}
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("could not read {}: {error}", source.display())),
    }
    let content = std::fs::read_to_string(&source)
        .map_err(|error| format!("could not read {}: {error}", source.display()))?;
    write_export(context, "routines", "json", &content).map(|_| ())
}

/// Write `<data dir>/exports/<stem>-<date>.<ext>`, never overwriting: a taken name
/// gets a `-2`, `-3`... suffix. An earlier export with the same bytes counts as done,
/// so running the step again adds nothing. Returns the file now holding `content`.
fn write_export(
    context: &MigrationContext,
    stem: &str,
    extension: &str,
    content: &str,
) -> Result<PathBuf, String> {
    let dir = context.data_dir.join("exports");
    if let Some(existing) = matching_export(&dir, stem, extension, content) {
        return Ok(existing);
    }
    let mut attempt = 1u32;
    loop {
        let name = if attempt == 1 {
            format!("{stem}-{}.{extension}", context.date)
        } else {
            format!("{stem}-{}-{attempt}.{extension}", context.date)
        };
        let path = dir.join(name);
        // `symlink_metadata` so a dangling link is a taken name, never written through.
        if std::fs::symlink_metadata(&path).is_err() {
            atomic_write(&path, content)
                .map_err(|error| format!("could not write {}: {error}", path.display()))?;
            return Ok(path);
        }
        attempt += 1;
    }
}

fn matching_export(dir: &Path, stem: &str, extension: &str, content: &str) -> Option<PathBuf> {
    let prefix = format!("{stem}-");
    let suffix = format!(".{extension}");
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(&suffix))
        })
        .find(|path| std::fs::read_to_string(path).is_ok_and(|existing| existing == content))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{
        backup_path, data_dir_test_lock, load_from_with_migrations_for_test,
        migration_backup_files, DataDirOverride, Migration, Registry, SafetyLevel,
        REGISTRY_ENV_LOCK,
    };
    use serde_json::json;

    const REGISTRY_VERSION: u32 = 2;

    fn load_from(path: &std::path::Path) -> Result<Registry, String> {
        load_from_with_migrations_for_test(path, &[migrate_v1_to_v2], 2)
    }

    fn scratch_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "toolport-v2-migration-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn context(dir: &Path) -> MigrationContext {
        MigrationContext {
            data_dir: dir.to_path_buf(),
            date: "2026-10-07".to_string(),
        }
    }

    const CLAUDE_MD: &str = "# My notes\n\nKeep this.\n\n<!-- toolport:rules:start set=personal v=3 -->\nAlways run the tests.\nNever push to main.\n<!-- toolport:rules:end -->\n\nAfter the block.\n";
    const OWNED_RULES: &str = "<!-- Toolport personal rules: set personal, v3. Edits are overwritten on the next apply; change them in Toolport. -->\n\nAlways run the tests.\nNever push to main.\n";
    const PROJECT_AGENTS_MD: &str = "<!-- toolport:rules:start set=work v=1 -->\nUse `cargo test`.\n<!-- toolport:rules:end -->\n";
    const ROUTINES: &str =
        "{\n  \"routines\": [{\"name\": \"triage\", \"script\": \"return 1\"}]\n}\n";

    /// `rel` under the scratch dir, spelled the way the fixture spells it.
    fn under(dir: &Path, rel: &str) -> String {
        format!("{}/{rel}", dir.display())
    }

    /// Client files 1.x wrote rules into, plus a saved routines file, the way a
    /// daily 1.24 install leaves them.
    fn seed_user_files(dir: &Path) -> Vec<PathBuf> {
        let claude_md = PathBuf::from(under(dir, "home/.claude/CLAUDE.md"));
        let owned = PathBuf::from(under(dir, "home/.cursor/rules/toolport-rules.md"));
        let project = PathBuf::from(under(dir, "proj/AGENTS.md"));
        for (path, content) in [
            (&claude_md, CLAUDE_MD),
            (&owned, OWNED_RULES),
            (&project, PROJECT_AGENTS_MD),
        ] {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        std::fs::write(dir.join("routines.json"), ROUTINES).unwrap();
        vec![claude_md, owned, project]
    }

    /// A Brandon-like 1.24 registry: every safety flag off, Code Mode on, two rule
    /// sets applied to clients and a project, routines, hooks and the guard on, a
    /// shared-HTTP client entry and a joined team. `@DIR@` is the scratch dir.
    const BRANDON_V1: &str = r#"{
    "version": 1,
    "servers": [
        {
            "id": "github",
            "name": "GitHub",
            "transport": "stdio",
            "command": "npx",
            "args": ["-y", "@modelcontextprotocol/server-github"],
            "env": [{"key": "GITHUB_TOKEN", "secret": true, "futureEnvField": "keep-env"}],
            "source": "manual",
            "disabledTools": ["delete_repository"]
        },
        {
            "id": "linear",
            "name": "Linear",
            "transport": "http",
            "url": "https://mcp.linear.app/mcp",
            "args": [],
            "env": [],
            "source": "team:t1"
        }
    ],
    "profiles": [
        {
            "id": "default",
            "name": "Default",
            "enabledServerIds": ["github", "linear"],
            "toolScope": {"github": ["search_code", "get_issue"]}
        },
        {"id": "work", "name": "Work", "enabledServerIds": ["linear"]}
    ],
    "activeProfileId": "default",
    "gatewayTopology": "legacy",
    "denyDestructive": false,
    "confirmDestructive": false,
    "humanApproval": false,
    "humanApprovalAllow": ["github/search_code"],
    "teamForcedHumanApproval": true,
    "teamForcedDenyDestructive": false,
    "teamForcedContentDefense": true,
    "teamForcedQuarantineOnDrift": false,
    "teamForcedBlockOnInjection": false,
    "teamForcedPiiRedaction": false,
    "toolOverrides": {"github": {"create_issue": {"description": "Open an issue"}}},
    "pinnedTools": {"github": ["get_me"]},
    "quarantineOnDrift": false,
    "lazyDiscovery": true,
    "discoveryMode": "grouped",
    "codeMode": true,
    "allowRoutineWrites": true,
    "allowAgentControl": true,
    "integrityCheck": true,
    "contentDefense": true,
    "piiRedaction": false,
    "blockOnInjection": false,
    "liveInspect": true,
    "team": {
        "serverUrl": "https://teams.toolport.dev",
        "teamId": "t1",
        "role": "member",
        "lastVersion": 12,
        "managedServerIds": {"linear": "srv_1"},
        "reportingDeviceId": "dev-1",
        "teamName": "Acme",
        "futureTeamField": {"keep": ["team"]},
        "teamInstructionsVersion": 2,
        "teamInstructionsTargets": ["@DIR@/home/.claude/rules/toolport-team-rules.md"]
    },
    "clientScopes": {"cursor": "work"},
    "folderProfiles": [{"path": "@DIR@/proj", "profile": "work"}],
    "clientDiscovery": {"cursor": "lazy"},
    "clientManagedEntries": {
        "claude-desktop": {
            "command": "toolport-gateway",
            "args": [],
            "env": {"TOOLPORT_CLIENT_ID": "claude-desktop"},
            "transport": "sharedHttp",
            "url": "http://127.0.0.1:8765/mcp",
            "updatedAt": 1790000000000,
            "futureEntryField": "keep-entry"
        }
    },
    "ruleSets": [
        {"id": "personal", "name": "Personal", "content": "Always run the tests.\nNever push to main.\n", "revision": 3},
        {"id": "work", "name": "Work", "content": "Use `cargo test`.\n```bash\ncargo test\n```\n", "revision": 1}
    ],
    "activeRuleSetId": "personal",
    "rulesClients": {"claude-code": true, "cursor": true},
    "rulesTargets": [
        "@DIR@/home/.claude/CLAUDE.md",
        "@DIR@/home/.cursor/rules/toolport-rules.md",
        "@DIR@/home/.gemini/GEMINI.md"
    ],
    "rulesProjects": [{
        "id": "proj",
        "path": "@DIR@/proj",
        "name": "proj",
        "setId": "work",
        "files": {"agents-md": true, "gemini-md": false},
        "targets": ["@DIR@/proj/AGENTS.md"]
    }],
    "guardCursorMode": "enforce",
    "guardClaudeMode": "observe",
    "guardTargets": ["@DIR@/home/.cursor/hooks.json"],
    "agentPermissionsEnabled": true,
    "agentPermissionRules": [{"pattern": "Bash(rm -rf *)", "action": "deny"}],
    "agentPermissionTargets": {
        "@DIR@/home/.claude/settings.json": [{"pattern": "Bash(rm -rf *)", "action": "deny"}]
    },
    "hooksEnabled": true,
    "hookTargets": ["@DIR@/home/.claude/settings.json"],
    "secretsGeneration": 4
}"#;

    fn brandon_v1(dir: &Path) -> Value {
        let escaped = serde_json::to_string(&dir.display().to_string()).unwrap();
        let dir_json = &escaped[1..escaped.len() - 1];
        serde_json::from_str(&BRANDON_V1.replace("@DIR@", dir_json)).unwrap()
    }

    fn write_json(path: &Path, value: &Value) -> String {
        let text = serde_json::to_string_pretty(value).unwrap();
        std::fs::write(path, &text).unwrap();
        text
    }

    fn read_json(path: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn exports(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir.join("exports"))
            .map(|entries| {
                entries
                    .filter_map(|entry| entry.ok())
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    #[test]
    fn brandon_like_v1_registry_migrates_to_v2() {
        let _data = data_dir_test_lock();
        let _env = REGISTRY_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = scratch_dir("brandon");
        let _override = DataDirOverride::set(&dir);
        let client_files = seed_user_files(&dir);
        let client_bytes: Vec<String> = client_files
            .iter()
            .map(|path| std::fs::read_to_string(path).unwrap())
            .collect();
        let path = dir.join("registry.json");
        let v1 = brandon_v1(&dir);
        let original = write_json(&path, &v1);

        let registry = load_from(&path).unwrap();

        // The pre-migration snapshot holds the exact v1 bytes.
        let backups = migration_backup_files(&path);
        assert_eq!(backups.len(), 1);
        let name = backups[0]
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(name.starts_with("registry.json.v1-"), "{name}");
        assert_eq!(std::fs::read_to_string(&backups[0]).unwrap(), original);

        let v2 = read_json(&path);
        assert_eq!(v2["version"], 2);
        assert_eq!(registry.version, REGISTRY_VERSION);
        // No blocking flag was on, so the 2.0 default applies.
        assert_eq!(v2["safetyLevel"], "ask");
        assert_eq!(registry.safety_level, Some(SafetyLevel::Ask));
        assert_eq!(v2["codeMode"], false);
        assert!(!registry.code_mode);
        for key in DROPPED_KEYS {
            assert!(v2.get(*key).is_none(), "{key} survived the migration");
            assert!(!registry.unknown_fields.contains_key(*key), "{key}");
        }
        // The 1.x mirror of Ask: approval on, nothing blocked.
        assert_eq!(v2["humanApproval"], true);
        for key in [
            "denyDestructive",
            "quarantineOnDrift",
            "blockOnInjection",
            "confirmDestructive",
        ] {
            assert_eq!(v2[key], false, "{key}");
        }
        // Fields this build does not model survive at every depth.
        assert_eq!(v2["servers"][0]["env"][0]["futureEnvField"], "keep-env");
        assert_eq!(v2["team"]["futureTeamField"], json!({"keep": ["team"]}));
        assert_eq!(
            v2["clientManagedEntries"]["claude-desktop"]["futureEntryField"],
            "keep-entry"
        );

        // Kept exactly as they were.
        for key in [
            "servers",
            "activeProfileId",
            "humanApprovalAllow",
            "teamForcedHumanApproval",
            "teamForcedDenyDestructive",
            "teamForcedContentDefense",
            "teamForcedQuarantineOnDrift",
            "teamForcedBlockOnInjection",
            "teamForcedPiiRedaction",
            "toolOverrides",
            "pinnedTools",
            "lazyDiscovery",
            "discoveryMode",
            "piiRedaction",
            "liveInspect",
            "clientScopes",
            "folderProfiles",
            "clientDiscovery",
            "clientManagedEntries",
            "secretsGeneration",
        ] {
            assert_eq!(v2[key], v1[key], "{key} changed");
        }
        assert_eq!(
            v2["profiles"][0]["toolScope"],
            v1["profiles"][0]["toolScope"]
        );
        assert_eq!(v2["profiles"][1], v1["profiles"][1]);
        for (key, value) in v1["team"].as_object().unwrap() {
            assert_eq!(&v2["team"][key], value, "team.{key} changed");
        }
        assert_eq!(
            v2["clientManagedEntries"]["claude-desktop"]["transport"],
            "sharedHttp"
        );
        assert_eq!(
            registry.servers[0].disabled_tools,
            vec!["delete_repository"]
        );
        // The team lock still applies on top of the member's own level.
        assert_eq!(registry.safety_level_effective(), SafetyLevel::Ask);

        // Exports: rules text, agent permission rules and routines, under dated names.
        let date = MigrationContext::for_registry(&path).date;
        assert_eq!(
            exports(&dir),
            vec![
                format!("agent-permissions-{date}.json"),
                format!("routines-{date}.json"),
                format!("rules-{date}.md"),
            ]
        );
        let rules = std::fs::read_to_string(dir.join(format!("exports/rules-{date}.md"))).unwrap();
        assert!(rules.contains("### Personal (active)"), "{rules}");
        assert!(rules.contains("### Work\n"), "{rules}");
        assert!(
            rules.contains("Always run the tests.\nNever push to main."),
            "{rules}"
        );
        assert!(
            rules.contains("````markdown\nUse `cargo test`.\n```bash"),
            "{rules}"
        );
        let project_line = format!(
            "- `{}`, rule set \"Work\", files: agents-md",
            under(&dir, "proj")
        );
        assert!(rules.contains(&project_line), "{rules}");
        assert!(
            rules.contains(&format!("### `{}`", client_files[0].display())),
            "{rules}"
        );
        assert!(
            rules.contains(&format!("### `{}`", client_files[2].display())),
            "{rules}"
        );
        assert!(rules.contains("The file no longer exists."), "{rules}");
        assert!(
            !rules.contains("toolport:rules:"),
            "markers must not be exported: {rules}"
        );
        assert!(!rules.contains("Toolport personal rules:"), "{rules}");
        assert!(
            !rules.contains("After the block."),
            "only the Toolport block is copied: {rules}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join(format!("exports/routines-{date}.json"))).unwrap(),
            ROUTINES
        );
        let permissions = read_json(&dir.join(format!("exports/agent-permissions-{date}.json")));
        assert_eq!(permissions["rules"], v1["agentPermissionRules"]);
        assert_eq!(permissions["addedToFiles"], v1["agentPermissionTargets"]);

        // Nothing outside the data dir's exports was touched.
        assert_eq!(
            std::fs::read_to_string(dir.join("routines.json")).unwrap(),
            ROUTINES
        );
        for (path, before) in client_files.iter().zip(&client_bytes) {
            assert_eq!(
                &std::fs::read_to_string(path).unwrap(),
                before,
                "{}",
                path.display()
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn brandon_like_v1_to_v3_preserves_every_clients_existing_access() {
        let _data = data_dir_test_lock();
        let _env = REGISTRY_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for active in ["default", "work", " Work ", "deleted", ""] {
            let dir = scratch_dir("brandon-v3");
            let _override = DataDirOverride::set(&dir);
            let client_files = seed_user_files(&dir);
            let client_bytes: Vec<_> = client_files
                .iter()
                .map(|p| std::fs::read(p).unwrap())
                .collect();
            let path = dir.join("registry.json");
            let mut v1 = brandon_v1(&dir);
            v1["activeProfileId"] = json!(active);
            if active.is_empty() {
                v1["profiles"][1]["name"] = json!("");
            }
            v1["teamMinSafetyLevel"] = json!("strict");
            v1["team"]["minSafetyLevel"] = json!("strict");
            v1["profiles"][0]["futureProfileField"] = json!({"keep": true});
            v1["folderProfiles"][0]["futureFolderField"] = json!("keep-folder");
            v1["httpClients"] = json!([
                {"id":"unscoped-http", "label":"Unscoped", "tokenSha256":"hash", "profile":"", "futureHttpField":"keep-http"},
                {"id":"scoped-http", "label":"Scoped", "tokenSha256":"hash2", "profile":"default"}
            ]);
            v1["clientScopes"]["empty"] = json!("");
            let mut before: Registry = serde_json::from_value(v1.clone()).unwrap();
            before.normalize_profile_references();
            let original = write_json(&path, &v1);
            let after = crate::registry::load_from(&path).unwrap();
            let v3 = read_json(&path);
            assert_eq!(v3["version"], 3);
            assert_eq!(
                after.default_access_profile_id.as_deref(),
                matches!(active, "work" | " Work ").then_some("work")
            );
            let ids = |servers: Vec<&crate::registry::ServerEntry>| -> Vec<String> {
                servers.iter().map(|s| s.id.clone()).collect()
            };
            // No explicit access means EXACTLY the pre-upgrade view, including an
            // absent binding and an explicit empty binding. No server appears or disappears.
            for client in ["claude-desktop", "new-client", "empty", "cursor"] {
                let scope = before
                    .client_scopes
                    .get(client)
                    .map(String::as_str)
                    .unwrap_or("");
                assert_eq!(
                    ids(before.enabled_servers_for(scope)),
                    ids(after.enabled_servers_for(scope)),
                    "{active}: {client}"
                );
                assert_eq!(
                    before.configured_instructions(Some(scope)),
                    after.configured_instructions(Some(scope))
                );
                for server in &before.servers {
                    for tool in ["search_code", "delete_repository"] {
                        assert_eq!(
                            before.profile_allows_tool(scope, &server.id, tool),
                            after.profile_allows_tool(scope, &server.id, tool),
                            "{active}: {client}/{tool}"
                        );
                    }
                }
            }
            for scope in before.folder_profiles.iter().map(|f| f.profile.as_str()) {
                assert_eq!(
                    ids(before.enabled_servers_for(scope)),
                    ids(after.enabled_servers_for(scope))
                );
            }
            assert_eq!(
                ids(before.bridge_enabled_servers(None)),
                ids(after.bridge_enabled_servers(None)),
                "unscoped HTTP retains its connected union"
            );
            for client in before.http_clients.iter().filter(|c| !c.profile.is_empty()) {
                assert_eq!(
                    ids(before.enabled_servers_for(&client.profile)),
                    ids(after.enabled_servers_for(&client.profile))
                );
            }
            for (field, value) in
                v1.as_object().unwrap().iter().filter(|(k, _)| {
                    k.starts_with("teamForced") || k.as_str() == "teamMinSafetyLevel"
                })
            {
                assert_eq!(&v3[field], value, "{field}");
            }
            for field in [
                "profiles",
                "clientScopes",
                "folderProfiles",
                "httpClients",
                "clientManagedEntries",
            ] {
                assert_eq!(v3[field], v1[field], "{field}");
            }
            assert_eq!(v3["team"]["minSafetyLevel"], "strict");
            assert_eq!(after.safety_level_effective(), SafetyLevel::Strict);
            let backups = migration_backup_files(&path);
            assert_eq!(backups.len(), 1);
            assert_eq!(std::fs::read_to_string(&backups[0]).unwrap(), original);
            let bytes = std::fs::read(&path).unwrap();
            let export_names = exports(&dir);
            crate::registry::load_from(&path).unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            assert_eq!(migration_backup_files(&path).len(), 1);
            assert_eq!(exports(&dir), export_names);
            crate::registry::save_to(&path, &after).unwrap();
            let saved = read_json(&path);
            for pointer in [
                "/servers/0/env/0/futureEnvField",
                "/team/futureTeamField",
                "/team/minSafetyLevel",
                "/profiles/0/futureProfileField",
                "/folderProfiles/0/futureFolderField",
                "/httpClients/0/futureHttpField",
                "/clientManagedEntries/claude-desktop/futureEntryField",
            ] {
                assert_eq!(saved.pointer(pointer), v1.pointer(pointer), "{pointer}");
            }
            for (file, bytes) in client_files.iter().zip(client_bytes) {
                assert_eq!(std::fs::read(file).unwrap(), bytes);
            }
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }

    #[test]
    fn safety_level_follows_the_v1_flags() {
        let dir = scratch_dir("safety");
        let cases: [(Value, &str); 9] = [
            (json!({}), "ask"),
            (json!({"denyDestructive": true}), "strict"),
            (json!({"quarantineOnDrift": true}), "strict"),
            (json!({"blockOnInjection": true}), "strict"),
            (json!({"humanApproval": true}), "ask"),
            (json!({"confirmDestructive": true}), "ask"),
            // Team locks are not the member's choice and stay as they were.
            (json!({"teamForcedDenyDestructive": true}), "ask"),
            // A level picked in a 2.0 preview wins over the old flags.
            (
                json!({"safetyLevel": "off", "denyDestructive": true}),
                "off",
            ),
            (
                json!({"safetyLevel": "bogus", "blockOnInjection": true}),
                "strict",
            ),
        ];
        for (flags, expected) in cases {
            let mut value = json!({"version": 1, "servers": [], "profiles": []});
            for (key, flag) in flags.as_object().unwrap() {
                value[key] = flag.clone();
            }
            migrate_v1_to_v2(&mut value, &context(&dir)).unwrap();
            assert_eq!(value["safetyLevel"], expected, "{flags}");
            assert_eq!(level_name(as_1x_reader(&value)), expected, "{flags}");
            if flags.get("teamForcedDenyDestructive").is_some() {
                assert_eq!(value["teamForcedDenyDestructive"], true);
            }
        }
        assert!(
            exports(&dir).is_empty(),
            "nothing to export, nothing written"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn migration_is_idempotent() {
        let _data = data_dir_test_lock();
        let _env = REGISTRY_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = scratch_dir("idempotent");
        let _override = DataDirOverride::set(&dir);
        seed_user_files(&dir);

        // The step itself: a second run changes nothing and exports nothing new.
        let mut once = brandon_v1(&dir);
        migrate_v1_to_v2(&mut once, &context(&dir)).unwrap();
        let after_first = exports(&dir);
        let mut twice = once.clone();
        migrate_v1_to_v2(&mut twice, &context(&dir)).unwrap();
        assert_eq!(twice, once);
        assert_eq!(exports(&dir), after_first);
        std::fs::remove_dir_all(dir.join("exports")).unwrap();

        // The loader: a migrated file is current, so loading it again neither
        // rewrites it nor takes another backup.
        let path = dir.join("registry.json");
        write_json(&path, &brandon_v1(&dir));
        load_from(&path).unwrap();
        let migrated = std::fs::read(&path).unwrap();
        let exported = exports(&dir);
        load_from(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), migrated);
        assert_eq!(migration_backup_files(&path).len(), 1);
        assert_eq!(exports(&dir), exported);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Runs inside the pipeline, where the primary has not been written yet: the
    /// backup must already exist, and the exports must exist before the step returns.
    fn checked_v1_to_v2(value: &mut Value, context: &MigrationContext) -> Result<(), String> {
        let path = context.data_dir.join("registry.json");
        let backups = migration_backup_files(&path);
        assert_eq!(backups.len(), 1, "the v1 backup is written before any step");
        let original = std::fs::read(&backups[0]).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(exports(&context.data_dir).is_empty());
        migrate_v1_to_v2(value, context)?;
        assert_eq!(
            exports(&context.data_dir).len(),
            3,
            "exports land in the step"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            original,
            "the primary is still v1 while the exports are written"
        );
        Ok(())
    }

    fn failing_v2_to_v3(_value: &mut Value, _context: &MigrationContext) -> Result<(), String> {
        Err("boom".to_string())
    }

    #[test]
    fn backup_and_exports_are_written_before_the_primary() {
        let _data = data_dir_test_lock();
        let _env = REGISTRY_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = scratch_dir("ordering");
        let _override = DataDirOverride::set(&dir);
        seed_user_files(&dir);
        let path = dir.join("registry.json");
        write_json(&path, &brandon_v1(&dir));

        let pipeline: &[Migration] = &[checked_v1_to_v2];
        let registry = load_from_with_migrations_for_test(&path, pipeline, 2).unwrap();
        assert_eq!(registry.version, 2);
        assert_eq!(read_json(&path)["version"], 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_failure_mid_migration_leaves_the_v1_file_intact() {
        let _data = data_dir_test_lock();
        let _env = REGISTRY_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = scratch_dir("mid-failure");
        let _override = DataDirOverride::set(&dir);
        seed_user_files(&dir);
        let path = dir.join("registry.json");
        let original = write_json(&path, &brandon_v1(&dir));

        // A later step fails after the v2 step ran and exported.
        let pipeline: &[Migration] = &[migrate_v1_to_v2, failing_v2_to_v3];
        let error = load_from_with_migrations_for_test(&path, pipeline, 3).unwrap_err();
        assert!(
            error.contains("v2 to v3") && error.contains("boom"),
            "{error}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert!(!backup_path(&path).exists());
        assert_eq!(migration_backup_files(&path).len(), 1);

        // An export that cannot be written stops the migration before the primary.
        std::fs::remove_dir_all(dir.join("exports")).unwrap();
        std::fs::write(dir.join("exports"), "not a directory").unwrap();
        let error = load_from(&path).unwrap_err();
        assert!(error.contains("v1 to v2"), "{error}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert!(!backup_path(&path).exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// What a released 1.x build sees: no `version` field and no `safetyLevel`, so it
    /// derives the level from the toggles alone.
    fn as_1x_reader(document: &Value) -> Registry {
        let mut document = document.clone();
        let fields = document.as_object_mut().unwrap();
        fields.remove("version");
        fields.remove("safetyLevel");
        let registry: Registry = serde_json::from_value(document).unwrap();
        assert_eq!(registry.version, 1);
        registry
    }

    fn level_name(registry: Registry) -> &'static str {
        match registry.safety_level_selected() {
            SafetyLevel::Off => "off",
            SafetyLevel::Ask => "ask",
            SafetyLevel::Strict => "strict",
        }
    }

    #[test]
    fn a_1x_reader_derives_the_same_level_from_a_migrated_registry() {
        let _data = data_dir_test_lock();
        let _env = REGISTRY_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = scratch_dir("1x-reader");
        let _override = DataDirOverride::set(&dir);
        let path = dir.join("registry.json");
        let fixture: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/registry-v1-to-v2/v2.json"
        ))
        .unwrap();
        assert_eq!(level_name(as_1x_reader(&fixture)), "ask");

        for (flags, level) in [
            (json!({"denyDestructive": true}), "strict"),
            (json!({}), "ask"),
            (json!({"safetyLevel": "off"}), "off"),
        ] {
            let mut v1 = brandon_v1(&dir);
            for (key, flag) in flags.as_object().unwrap() {
                v1[key] = flag.clone();
            }
            write_json(&path, &v1);
            let migrated = load_from(&path).unwrap();
            assert_eq!(level_name(migrated), level);
            let on_disk = read_json(&path);
            let reader = as_1x_reader(&on_disk);
            assert_eq!(level_name(reader.clone()), level, "{flags}");
            // 1.x enforces the matching gates.
            assert_eq!(reader.deny_destructive, level == "strict");
            assert_eq!(reader.human_approval, level != "off");

            // 1.24 reads `version` without checking it and keeps unknown fields, so its
            // rewrite still says v2 and keeps `safetyLevel`: 2.0 reads the same level.
            let mut rewritten = serde_json::to_value(&reader).unwrap();
            rewritten["version"] = on_disk["version"].clone();
            rewritten["safetyLevel"] = on_disk["safetyLevel"].clone();
            write_json(&path, &rewritten);
            assert_eq!(level_name(load_from(&path).unwrap()), level, "{flags}");
            // Marked v1 again, it migrates a second time to the same level.
            rewritten["version"] = json!(1);
            write_json(&path, &rewritten);
            assert_eq!(level_name(load_from(&path).unwrap()), level, "{flags}");
        }

        // Saves from 2.0 keep the mirror in step with the level.
        let mut registry = load_from(&path).unwrap();
        registry.set_safety_level(SafetyLevel::Strict);
        crate::registry::save_to(&path, &registry).unwrap();
        let on_disk = read_json(&path);
        assert_eq!(on_disk["denyDestructive"], true);
        assert_eq!(level_name(as_1x_reader(&on_disk)), "strict");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_v2_registry_without_a_level_is_ask() {
        let mut registry: Registry = serde_json::from_value(json!({
            "version": 2,
            "servers": [],
            "profiles": [],
            "denyDestructive": false,
            "humanApproval": false
        }))
        .unwrap();
        assert_eq!(registry.safety_level_selected(), SafetyLevel::Ask);
        // The mirror is never read from v2 on, even when a 1.x process changed it.
        registry.deny_destructive = true;
        assert_eq!(registry.safety_level_selected(), SafetyLevel::Ask);
        // A v1 registry still derives its level from the toggles.
        registry.version = 1;
        assert_eq!(registry.safety_level_selected(), SafetyLevel::Strict);
        registry.deny_destructive = false;
        assert_eq!(registry.safety_level_selected(), SafetyLevel::Off);
    }

    /// The preview rollback test restores `v1.json` over `v2.json`. This keeps
    /// `v2.json` what the real migration writes for `v1.json`.
    #[test]
    fn rollback_fixture_is_a_real_migration() {
        let _data = data_dir_test_lock();
        let _env = REGISTRY_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = scratch_dir("rollback-fixture");
        let _override = DataDirOverride::set(&dir);
        let path = dir.join("registry.json");
        std::fs::write(
            &path,
            include_str!("../../tests/fixtures/registry-v1-to-v2/v1.json"),
        )
        .unwrap();
        load_from(&path).unwrap();
        let actual = read_json(&path);
        let expected: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/registry-v1-to-v2/v2.json"
        ))
        .unwrap();
        assert_eq!(
            actual,
            expected,
            "regenerate v2.json from this output:\n{}",
            serde_json::to_string_pretty(&actual).unwrap()
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn exports_never_overwrite() {
        let dir = scratch_dir("no-overwrite");
        let first = write_export(&context(&dir), "routines", "json", "one").unwrap();
        let second = write_export(&context(&dir), "routines", "json", "two").unwrap();
        let again = write_export(&context(&dir), "routines", "json", "one").unwrap();
        assert_eq!(first.file_name().unwrap(), "routines-2026-10-07.json");
        assert_eq!(second.file_name().unwrap(), "routines-2026-10-07-2.json");
        assert_eq!(again, first, "identical content is not exported twice");
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "one");
        assert_eq!(std::fs::read_to_string(&second).unwrap(), "two");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ordinary_save_preserves_migrated_nested_fields() {
        let _data = data_dir_test_lock();
        let _env = REGISTRY_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = scratch_dir("migrated-nested-save");
        let _override = DataDirOverride::set(&dir);
        let path = dir.join("registry.json");
        write_json(&path, &brandon_v1(&dir));
        let mut loaded = load_from(&path).unwrap();
        let first = read_json(&path);
        assert_eq!(first["team"]["futureTeamField"], json!({"keep": ["team"]}));
        assert_eq!(first["servers"][0]["env"][0]["futureEnvField"], "keep-env");
        assert_eq!(
            first["clientManagedEntries"]["claude-desktop"]["futureEntryField"],
            "keep-entry"
        );
        loaded.live_inspect = !loaded.live_inspect;
        crate::registry::save_to(&path, &loaded).unwrap();
        let saved = read_json(&path);
        let observed = json!({"team": saved["team"]["futureTeamField"],
            "env": saved["servers"][0]["env"][0]["futureEnvField"],
            "client": saved["clientManagedEntries"]["claude-desktop"]["futureEntryField"]});
        assert_eq!(
            observed,
            json!({"team": {"keep": ["team"]}, "env": "keep-env", "client": "keep-entry"})
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn catalog_migration_preserves_migrated_nested_fields() {
        let _data = data_dir_test_lock();
        let _env = REGISTRY_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = scratch_dir("migrated-nested-catalog");
        let _override = DataDirOverride::set(&dir);
        let path = dir.join("registry.json");
        let mut v1 = brandon_v1(&dir);
        v1["servers"].as_array_mut().unwrap().push(json!({
            "id": "atlassian-work", "name": "Atlassian", "transport": "http",
            "url": "https://mcp.atlassian.com/v1/mcp/authv2", "source": "catalog:curated"
        }));
        write_json(&path, &v1);
        let loaded = load_from(&path).unwrap();
        assert_eq!(
            loaded.servers[2].url.as_deref(),
            Some("https://mcp.atlassian.com/v2/mcp?tools=all")
        );
        let saved = read_json(&path);
        let observed = json!({"team": saved["team"]["futureTeamField"],
            "env": saved["servers"][0]["env"][0]["futureEnvField"],
            "client": saved["clientManagedEntries"]["claude-desktop"]["futureEntryField"]});
        assert_eq!(
            observed,
            json!({"team": {"keep": ["team"]}, "env": "keep-env", "client": "keep-entry"})
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn v2_save_preserves_unknown_fields_at_every_persisted_depth() {
        let _data = data_dir_test_lock();
        let _env = REGISTRY_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = scratch_dir("v2-nested-save");
        let _override = DataDirOverride::set(&dir);
        let path = dir.join("registry.json");
        let mut document = serde_json::to_value(crate::registry::Registry::default()).unwrap();
        document["version"] = json!(2);
        document["servers"] = json!([{
            "id": "custom", "name": "Custom", "transport": "stdio", "command": "echo",
            "args": ["<launch-input>"], "env": [{"key": "ENV", "value": null}],
            "clientCredentials": {"clientId": "client"},
            "launch": {"inputs": [{"key": "INPUT", "label": "Input", "secret": false, "value": "value"}],
                "bindings": [{"index": 0, "parts": [{"kind": "literal", "value": "/"}, {"kind": "input", "key": "INPUT"}]}]}
        }]);
        document["profiles"] =
            json!([{"id": "default", "name": "Default", "enabledServerIds": ["custom"]}]);
        document["folderProfiles"] = json!([{"path": "/project", "profile": "default"}]);
        document["httpClients"] = json!([{"id": "web", "label": "Web", "tokenSha256": "hash"}]);
        document["toolOverrides"] = json!({"custom": {"tool": {"name": "renamed"}}});
        document["team"] = json!({"serverUrl": "https://teams.example.com", "teamId": "team", "role": "member",
            "rateLimits": [{"id": "cap", "window": "day", "maxCalls": 10}]});
        document["clientManagedEntries"] =
            json!({"client": {"command": "gateway", "transport": "stdio"}});
        let paths = [
            "",
            "/servers/0",
            "/servers/0/env/0",
            "/servers/0/clientCredentials",
            "/servers/0/launch",
            "/servers/0/launch/inputs/0",
            "/servers/0/launch/bindings/0",
            "/servers/0/launch/bindings/0/parts/0",
            "/servers/0/launch/bindings/0/parts/1",
            "/profiles/0",
            "/folderProfiles/0",
            "/httpClients/0",
            "/toolOverrides/custom/tool",
            "/semanticSearch",
            "/team",
            "/team/rateLimits/0",
            "/clientManagedEntries/client",
        ];
        let sentinel = json!({"array": [null, true, 18446744073709551615u64, "\u{96ea}", {"known": "nested"}]});
        for pointer in paths {
            document
                .pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("futureField".into(), sentinel.clone());
        }
        write_json(&path, &document);
        let mut loaded = load_from(&path).unwrap();
        // Known keys are consumed by serde rather than being replayed from the flatten map.
        assert!(!loaded.servers[0].unknown_fields.contains_key("name"));
        assert!(!loaded.servers[0].env[0]
            .unknown_fields
            .contains_key("value"));
        assert!(!loaded
            .team
            .as_ref()
            .unwrap()
            .unknown_fields
            .contains_key("teamId"));
        loaded.servers[0].name = "Changed".into();
        for _ in 0..2 {
            crate::registry::save_to(&path, &loaded).unwrap();
            let saved = read_json(&path);
            assert_eq!(saved["servers"][0]["name"], "Changed");
            assert!(saved["servers"][0]["env"][0].get("value").is_none());
            for pointer in paths {
                assert_eq!(
                    saved.pointer(pointer).unwrap()["futureField"],
                    sentinel,
                    "{pointer}"
                );
            }
            loaded = load_from(&path).unwrap();
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
