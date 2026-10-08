//! One cross-process Toolport lock and optimistic revisions for client writers.
//! Native clients do not take our lock. Re-render on unrelated edits, refuse
//! overlapping edits, and check again immediately before each rename.
use super::*;
use serde_json::Value;
use std::cell::RefCell;
use std::collections::BTreeMap;

#[derive(Default)]
struct Pending {
    path: PathBuf,
    original: Option<String>,
    output: Option<String>,
    auxiliary: BTreeMap<PathBuf, String>,
    strict_json: bool,
    remove: bool,
    disconnecting: bool,
}

thread_local! {
    static PENDING: RefCell<Option<Pending>> = const { RefCell::new(None) };
}

struct Clear;
impl Drop for Clear {
    fn drop(&mut self) {
        PENDING.with(|slot| *slot.borrow_mut() = None);
    }
}

pub(super) fn read(path: &Path) -> Option<Option<String>> {
    PENDING.with(|slot| {
        let slot = slot.borrow();
        let pending = slot.as_ref()?;
        if path == pending.path {
            Some(if pending.remove {
                None
            } else {
                pending.output.clone().or_else(|| pending.original.clone())
            })
        } else {
            pending.auxiliary.get(path).cloned().map(Some)
        }
    })
}

pub(super) fn exists(path: &Path) -> bool {
    read(path).map_or_else(|| path.exists(), |text| text.is_some())
}

pub(super) fn strict_json(path: &Path) -> bool {
    PENDING.with(|slot| {
        slot.borrow()
            .as_ref()
            .is_some_and(|p| p.path == path && p.strict_json)
    }) || path.file_name().is_some_and(|name| name == ".claude.json")
}

pub(super) fn write(path: &Path, contents: &str) -> Result<(), String> {
    let staged = PENDING.with(|slot| -> Result<bool, String> {
        let mut slot = slot.borrow_mut();
        let Some(pending) = slot.as_mut() else {
            return Ok(false);
        };
        if path == pending.path {
            pending.output = Some(contents.into());
            pending.remove = false;
        } else {
            let dir = crate::registry::conduit_dir().ok_or("Could not resolve data dir")?;
            let parent = path.parent().ok_or("Auxiliary path has no parent")?;
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let dir = std::fs::canonicalize(dir).map_err(|e| e.to_string())?;
            let ancestor = parent
                .ancestors()
                .find(|ancestor| ancestor.exists())
                .ok_or("Auxiliary path has no existing ancestor")?;
            let ancestor = std::fs::canonicalize(ancestor).map_err(|e| e.to_string())?;
            if !ancestor.starts_with(dir)
                || path
                    .components()
                    .any(|part| part == std::path::Component::ParentDir)
                || path.is_symlink()
            {
                return Err(
                    "Auxiliary config writes must stay inside the Toolport data dir".into(),
                );
            }
            pending.auxiliary.insert(path.into(), contents.into());
        }
        Ok(true)
    })?;
    if staged {
        Ok(())
    } else {
        crate::registry::client_file::commit(
            path,
            &crate::registry::client_file::read(path)?,
            Some(contents),
        )
    }
}

pub(super) fn disconnecting() {
    PENDING.with(|slot| {
        if let Some(pending) = slot.borrow_mut().as_mut() {
            pending.disconnecting = true;
        }
    });
}

pub(super) fn remove(path: &Path) -> Result<(), String> {
    PENDING.with(|slot| {
        let mut slot = slot.borrow_mut();
        let pending = slot
            .as_mut()
            .ok_or("Config removal must hold mutation lock")?;
        if pending.path != path {
            return Err("Unexpected config removal path".into());
        }
        pending.remove = true;
        pending.output = None;
        Ok(())
    })
}

pub(super) fn value(format: Format, text: Option<&str>) -> Result<Value, String> {
    let Some(text) = text.filter(|text| !text.trim().is_empty()) else {
        return Ok(serde_json::json!({}));
    };
    match format {
        Format::TomlMcpServers => {
            serde_json::to_value(read_existing_toml(text)?).map_err(|e| e.to_string())
        }
        Format::YamlExtensions | Format::YamlMcpServers | Format::YamlMcpServersList => {
            serde_json::to_value(parse_existing_yaml_content(text)?).map_err(|e| e.to_string())
        }
        _ => parse_json_value(text),
    }
}

pub(super) fn changes(
    before: Option<&Value>,
    after: Option<&Value>,
    path: &mut Vec<String>,
    out: &mut Vec<Vec<String>>,
) {
    if before == after {
        return;
    }
    if let (Some(Value::Object(before)), Some(Value::Object(after))) = (before, after) {
        let keys: std::collections::BTreeSet<_> = before.keys().chain(after.keys()).collect();
        for key in keys {
            path.push(key.clone());
            changes(before.get(key), after.get(key), path, out);
            path.pop();
        }
    } else {
        out.push(path.clone());
    }
}

fn unrelated(
    format: Format,
    original: Option<&str>,
    output: Option<&str>,
    current: Option<&str>,
) -> Result<bool, String> {
    // File deletion/creation is consequential even when its parsed value is {}.
    if original.is_some() != current.is_some() {
        return Ok(false);
    }
    let before = value(format, original)?;
    let after = value(format, output)?;
    let native = value(format, current)?;
    let mut ours = Vec::new();
    let mut theirs = Vec::new();
    changes(Some(&before), Some(&after), &mut Vec::new(), &mut ours);
    changes(Some(&before), Some(&native), &mut Vec::new(), &mut theirs);
    Ok(!ours
        .iter()
        .any(|a| theirs.iter().any(|b| a.starts_with(b) || b.starts_with(a))))
}

pub(super) fn run<T>(
    client_id: &str,
    path: &Path,
    format: Format,
    edit: impl FnMut() -> Result<T, String>,
) -> Result<T, String> {
    run_inner(client_id, path, format, false, edit)
}

pub(super) fn settings<T>(
    client_id: &str,
    path: &Path,
    mut edit: impl FnMut() -> Result<T, String>,
) -> Result<T, String> {
    run_inner(client_id, path, Format::JsonMcpServers, true, &mut edit)
}

fn run_inner<T>(
    client_id: &str,
    path: &Path,
    format: Format,
    jsonc_settings: bool,
    mut edit: impl FnMut() -> Result<T, String>,
) -> Result<T, String> {
    // Nested calls (move -> write_servers, disconnect -> install_or_remove) share
    // the outer operation. No writer can commit half of a migration.
    if PENDING.with(|slot| slot.borrow().is_some()) {
        return edit();
    }
    let dir = crate::registry::conduit_dir().ok_or("Could not resolve data dir")?;
    let _lock = crate::registry::lock_at(&dir.join("client-config-mutation"))?;
    let mut revision = crate::registry::client_file::read(path)?;
    let mut original = revision.text.clone();
    let strict_json = !jsonc_settings
        && !matches!(
            format,
            Format::TomlMcpServers
                | Format::YamlExtensions
                | Format::YamlMcpServers
                | Format::YamlMcpServersList
        )
        && !matches!(client_id, "vscode" | "zed" | "opencode" | "kilo-code");
    for _ in 0..3 {
        if !value(format, original.as_deref())?.is_object() {
            return Err("Client config root must be an object; leaving it untouched".into());
        }
        if strict_json {
            if let Some(text) = original.as_deref().filter(|text| !text.trim().is_empty()) {
                serde_json::from_str::<Value>(text.strip_prefix('\u{feff}').unwrap_or(text)).map_err(|_| format!("{} requires strict JSON; remove comments or trailing commas before connecting. Config unchanged.", path.display()))?;
            }
        }
        PENDING.with(|slot| {
            *slot.borrow_mut() = Some(Pending {
                path: path.into(),
                original: original.clone(),
                strict_json,
                ..Pending::default()
            })
        });
        let clear = Clear;
        let result = edit()?;
        let pending = PENDING.with(|slot| slot.borrow_mut().take().unwrap());
        drop(clear);
        if pending.output.is_none() && !pending.remove {
            super::restore::remember(
                client_id,
                path,
                format,
                original.as_deref(),
                original.as_deref(),
                pending.disconnecting,
                jsonc_settings,
            )?;
            return Ok(result);
        }
        let output = pending.output;
        #[cfg(test)]
        before_commit(path);
        let current_revision = crate::registry::client_file::read(path)?;
        let current = current_revision.text.clone();
        if current != original {
            if !unrelated(
                format,
                original.as_deref(),
                output.as_deref(),
                current.as_deref(),
            )? {
                return Err(format!("Client config conflict at {}: native edits overlap this operation. Config unchanged.", path.display()));
            }
            revision = current_revision;
            original = revision.text.clone();
            continue;
        }
        // Recovery records must land before the config they protect. Roll them
        // back too if the final revision check refuses the client write.
        let mut auxiliary_recovery = Vec::new();
        let commit = (|| {
            for (auxiliary, text) in pending.auxiliary {
                auxiliary_recovery.push(super::restore::checkpoint(&auxiliary)?);
                crate::registry::atomic_write(&auxiliary, &text)?;
            }
            let recovery = super::restore::remember(
                client_id,
                path,
                format,
                original.as_deref(),
                output.as_deref(),
                pending.disconnecting,
                jsonc_settings,
            )?;
            let commit = crate::registry::client_file::commit(path, &revision, output.as_deref());
            if commit.is_err() {
                recovery.rollback()?;
            }
            commit
        })();
        if commit.is_err() {
            for receipt in auxiliary_recovery.into_iter().rev() {
                receipt.rollback()?;
            }
        }
        match commit {
            Ok(()) => return Ok(result),
            Err(e) if e == "Client config revision changed before rename" => {
                let current_revision = crate::registry::client_file::read(path)?;
                let current = current_revision.text.clone();
                if !unrelated(
                    format,
                    original.as_deref(),
                    output.as_deref(),
                    current.as_deref(),
                )? {
                    return Err(format!(
                        "Client config conflict at {}. Config unchanged.",
                        path.display()
                    ));
                }
                revision = current_revision;
                original = revision.text.clone();
            }
            Err(e) => return Err(e),
        }
    }
    Err(format!("Client config {} keeps changing; retry after the client finishes saving. Config unchanged.", path.display()))
}

#[cfg(test)]
thread_local! { pub(super) static BEFORE_COMMIT: RefCell<Option<Box<dyn FnOnce(&Path)>>> = const { RefCell::new(None) }; }
#[cfg(test)]
pub(super) fn before_commit(path: &Path) {
    let hook = BEFORE_COMMIT.with(|slot| slot.borrow_mut().take());
    if let Some(hook) = hook {
        hook(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (PathBuf, crate::registry::DataDirOverride) {
        let dir = std::env::temp_dir().join(format!(
            "toolport-mutation-{}-{}",
            std::process::id(),
            epoch_millis()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let data = crate::registry::DataDirOverride::set(dir.join("data"));
        (dir, data)
    }
    fn connect(path: &Path) -> Result<(), String> {
        run("cursor", path, Format::JsonMcpServers, || {
            let source = read_config_file(path)?;
            let mut root = parse_json_value(&source)?;
            root["mcpServers"]["toolport"] = serde_json::json!({"command":"toolport-gateway"});
            atomic_write_json_config(path, Some(&source), &root, "mcpServers")
        })
    }
    #[test]
    fn published_cleanup_failure_keeps_original_recovery_record() {
        let _lock = crate::registry::data_dir_test_lock();
        let (dir, _data) = fixture();
        let path = dir.join("config.json");
        let original = r#"{ "session": 1, "mcpServers": {} }"#;
        std::fs::write(&path, original).unwrap();
        crate::registry::client_file::FORCE_CLEANUP_ERROR.with(|slot| slot.set(true));
        let result = connect(&path);
        crate::registry::client_file::FORCE_CLEANUP_ERROR.with(|slot| slot.set(false));
        result.unwrap();
        assert!(
            parse_json_value(&std::fs::read_to_string(&path).unwrap()).unwrap()["mcpServers"]
                ["toolport"]
                .is_object()
        );
        run("cursor", &path, Format::JsonMcpServers, || {
            disconnecting();
            super::super::restore::apply("cursor", Format::JsonMcpServers, &path)
        })
        .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn native_unrelated_connect_edit_is_reapplied_without_sleeps() {
        let _lock = crate::registry::data_dir_test_lock();
        let (dir, _data) = fixture();
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"session":1,"mcpServers":{}}"#).unwrap();
        BEFORE_COMMIT.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(|path| {
                std::fs::write(path, r#"{"session":2,"mcpServers":{}}"#).unwrap();
            }))
        });
        connect(&path).unwrap();
        let root = parse_json_value(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(root["session"], 2);
        assert!(root["mcpServers"]["toolport"].is_object());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn native_overlapping_connect_edit_refuses_every_staged_write() {
        let _lock = crate::registry::data_dir_test_lock();
        let (dir, _data) = fixture();
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"mcpServers":{}}"#).unwrap();
        let native = r#"{"mcpServers":{"toolport":{"command":"custom"}}}"#;
        BEFORE_COMMIT.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move |path| {
                std::fs::write(path, native).unwrap();
            }))
        });
        assert!(connect(&path).unwrap_err().contains("conflict"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), native);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn strict_json_bom_is_accepted_and_preserved() {
        let _lock = crate::registry::data_dir_test_lock();
        let (dir, _data) = fixture();
        let path = dir.join(".claude.json");
        std::fs::write(&path, "\u{feff}{\"mcpServers\":{}}").unwrap();
        connect(&path).unwrap();
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .starts_with('\u{feff}'));
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn exchange_conflict_replays_unrelated_native_save() {
        let _lock = crate::registry::data_dir_test_lock();
        let (dir, _data) = fixture();
        let path = dir.join("config.json");
        std::fs::write(&path, r#"{"session":1,"mcpServers":{}}"#).unwrap();
        crate::registry::client_file::HOOK.with(|slot| {
            *slot.borrow_mut() = Some((
                "commit",
                Box::new(|path| {
                    let tmp = path.with_extension("native");
                    std::fs::write(&tmp, r#"{"session":2,"mcpServers":{}}"#).unwrap();
                    #[cfg(windows)]
                    std::fs::remove_file(path).unwrap();
                    std::fs::rename(tmp, path).unwrap();
                }),
            ))
        });
        connect(&path).unwrap();
        let root = parse_json_value(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(root["session"], 2);
        assert!(root["mcpServers"]["toolport"].is_object());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn auxiliary_write_outside_data_dir_is_refused() {
        let _lock = crate::registry::data_dir_test_lock();
        let (dir, _data) = fixture();
        let path = dir.join("config.json");
        std::fs::write(&path, "{}").unwrap();
        let outside = dir.join("outside.json");
        let error = run("cursor", &path, Format::JsonMcpServers, || {
            write(&outside, "secret")
        })
        .unwrap_err();
        assert!(error.contains("inside the Toolport data dir"));
        assert!(!outside.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn strict_json_client_refuses_jsonc_without_writing() {
        let _lock = crate::registry::data_dir_test_lock();
        let (dir, _data) = fixture();
        let path = dir.join(".claude.json");
        let original = "{ // comment\n \"mcpServers\": {},\n}";
        std::fs::write(&path, original).unwrap();
        assert!(run("claude-code", &path, Format::JsonMcpServers, || {
            atomic_write(&path, "{}")
        })
        .unwrap_err()
        .contains("strict JSON"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
