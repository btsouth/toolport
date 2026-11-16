//! Devin Desktop and Devin Local share one Toolport entry in one file.
use super::*;

const ORIGINAL: &str = "{\n  \"keep\": 1,\n  \"mcpServers\": {\n    \"github\": {\n      \"command\": \"gh-mcp\",\n      \"env\": {\"GITHUB_TOKEN\": \"ghp_syntheticSharedConfig000000000001\"}\n    }\n  }\n}\n";

struct Fixture {
    path: PathBuf,
    _vars: Vec<clients::EnvRestore>,
    _env: std::sync::MutexGuard<'static, ()>,
    _registry: std::sync::MutexGuard<'static, ()>,
}

/// Call inside `with_isolated_vault`: a scratch data dir and synthetic vault.
fn fixture() -> Fixture {
    let registry_lock = registry::REGISTRY_ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let env = clients::env_test_lock();
    let xdg = registry::conduit_dir().unwrap().join("xdg");
    std::fs::create_dir_all(xdg.join("devin")).unwrap();
    let path = xdg.join("devin").join("mcp_config.json");
    std::fs::write(&path, ORIGINAL).unwrap();
    registry::save(&Registry::default()).unwrap();
    Fixture {
        path,
        _vars: vec![clients::EnvRestore::set("XDG_CONFIG_HOME", &xdg)],
        _env: env,
        _registry: registry_lock,
    }
}

fn file(id: &str, name: &str) -> PathBuf {
    clients::backup_dir(id).unwrap().join(name)
}

fn receipt(id: &str, path: &Path, prefix: &str) -> PathBuf {
    let hash = registry::sha256_hex(&path.to_string_lossy());
    file(id, &format!("{prefix}-{hash}.json"))
}

fn backups(id: &str) -> Vec<String> {
    let mut names: Vec<_> = std::fs::read_dir(clients::backup_dir(id).unwrap())
        .map(|dir| {
            dir.filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn carrier(path: &Path) -> Option<String> {
    let root: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    root["mcpServers"]["toolport"]["env"][crate::brand::CLIENT_ID]
        .as_str()
        .map(str::to_string)
}

fn connect(id: &str) -> Result<MigrateOutcome, String> {
    let review = preview_client_setup(id)?;
    let names: Vec<_> = review
        .items
        .iter()
        .filter(|item| item.unsupported.is_none())
        .map(|item| item.name.clone())
        .collect();
    migrate_client_reviewed_with(id, None, false, &names, &review.revision, |_, _, _, _| {
        Ok(Vec::new().into())
    })
}

fn shared(id: &str) -> Option<clients::SharedEntry> {
    let mut detected = clients::detect_clients();
    clients::apply_entry_states(
        &mut detected,
        &read_registry_exact().unwrap().client_managed_entries,
    );
    let client = detected.into_iter().find(|client| client.id == id).unwrap();
    client.shared
}

/// S0 -> S1 -> S2, then either the carrier (handover, S3) or the joined
/// client leaves first, and the last one out restores the original file.
#[test]
fn shared_entry_transitions_in_both_orders_restore_everything() {
    for (a, b) in [("windsurf", "devin-cli"), ("devin-cli", "windsurf")] {
        for carrier_first in [true, false] {
            crate::secrets::tests::with_isolated_vault(|| {
                let fx = fixture();
                let p = fx.path.as_path();
                connect(a).unwrap();
                let s1 = std::fs::read_to_string(p).unwrap();
                assert!(!s1.contains("github"), "{a}: the server moved");
                assert_eq!(carrier(p).as_deref(), Some(a));
                assert!(receipt(a, p, "original").exists());
                assert!(file(a, "moved-servers.json").exists());
                assert!(
                    crate::secrets::file_vault_backups() > 0,
                    "the token is vaulted"
                );
                let current = read_registry_exact().unwrap();
                let path_text = p.to_string_lossy().into_owned();
                assert_eq!(
                    current.client_managed_entries[a].config_path.as_deref(),
                    Some(path_text.as_str())
                );
                update_registry_on_disk(&|r| {
                    r.set_client_discovery(a, Some("lazy"));
                    r.set_client_discovery(b, Some("full"));
                })
                .unwrap();

                // Join: no file write, no receipt or move record for b.
                let review = preview_client_setup(b).unwrap();
                let notice = review.shared_notice.unwrap();
                assert!(notice.contains(&clients::shared::name(a)), "{notice}");
                assert!(notice.contains("will be replaced"), "{notice}");
                assert_eq!(
                    read_registry_exact().unwrap().client_discovery_mode(b),
                    Some("full"),
                    "nothing is replaced before the confirmation"
                );
                if carrier_first {
                    connect(b).unwrap();
                } else {
                    connect_client_stdio(b, None, false).unwrap();
                }
                assert_eq!(std::fs::read_to_string(p).unwrap(), s1);
                assert!(!receipt(b, p, "original").exists());
                assert!(!file(b, "moved-servers.json").exists());
                let current = read_registry_exact().unwrap();
                assert_eq!(
                    current.client_managed_entries[b].config_path.as_deref(),
                    Some(path_text.as_str())
                );
                assert_eq!(current.client_discovery_mode(b), Some("lazy"));
                assert_eq!(
                    shared(b),
                    Some(clients::SharedEntry {
                        joined: true,
                        with: clients::shared::name(a)
                    })
                );
                assert_eq!(
                    shared(a),
                    Some(clients::SharedEntry {
                        joined: false,
                        with: clients::shared::name(b)
                    })
                );

                if carrier_first {
                    // Handover: the entry names b; a keeps its receipt and move record.
                    disconnect_client(a).unwrap();
                    assert_eq!(carrier(p).as_deref(), Some(b));
                    assert!(receipt(a, p, "original").exists());
                    assert!(file(a, "moved-servers.json").exists());
                    assert!(!receipt(b, p, "original").exists());
                    let current = read_registry_exact().unwrap();
                    assert!(!current.client_managed_entries.contains_key(a));
                    assert_eq!(current.client_discovery_mode(a), None);
                    assert_eq!(current.client_discovery_mode(b), Some("lazy"));
                    assert_eq!(shared(a), None);
                    disconnect_client(b).unwrap();
                } else {
                    disconnect_client(b).unwrap();
                    assert_eq!(std::fs::read_to_string(p).unwrap(), s1);
                    assert!(!read_registry_exact()
                        .unwrap()
                        .client_managed_entries
                        .contains_key(b));
                    disconnect_client(a).unwrap();
                }

                assert_eq!(std::fs::read_to_string(p).unwrap(), ORIGINAL, "{a} {b}");
                assert!(!receipt(a, p, "original").exists());
                assert!(receipt(a, p, "released").exists());
                assert!(!file(a, "moved-servers.json").exists());
                assert!(read_registry_exact()
                    .unwrap()
                    .client_managed_entries
                    .is_empty());
                assert_eq!(
                    crate::secrets::file_vault_backups(),
                    0,
                    "{a} {b} {carrier_first}"
                );
                assert_eq!(
                    backups(a),
                    vec![receipt(a, p, "released")
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()]
                );
                assert!(backups(b).is_empty(), "{:?}", backups(b));
            });
        }
    }
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

fn records() -> Vec<String> {
    let mut ids: Vec<_> = read_registry_exact()
        .unwrap()
        .client_managed_entries
        .into_keys()
        .collect();
    ids.sort();
    ids
}

/// S2: `a` moved its server and carries the entry, `b` joined it.
fn joined(a: &str, b: &str) -> String {
    connect(a).unwrap();
    update_registry_on_disk(&|r| r.set_client_discovery(a, Some("lazy"))).unwrap();
    connect_client_stdio(b, None, false).unwrap();
    std::fs::read_to_string(
        clients::shared::derive(a, &HashMap::new())
            .unwrap()
            .unwrap()
            .path,
    )
    .unwrap()
}

/// The last member out restores the original and every backup credential goes.
fn assert_restored(path: &Path) {
    assert_eq!(read(path), ORIGINAL);
    assert!(records().is_empty());
    assert_eq!(crate::secrets::file_vault_backups(), 0);
}

/// An update that fails on call `fail` (1-based) and writes the others.
type Change<'a> = &'a dyn Fn(&mut Registry);

fn failing(fail: usize) -> impl Fn(Change<'_>) -> Result<Registry, String> {
    let calls = std::cell::Cell::new(0);
    move |change| {
        calls.set(calls.get() + 1);
        if calls.get() == fail {
            return Err("registry full".into());
        }
        update_registry_on_disk(change)
    }
}

fn group(id: &str) -> clients::shared::Group {
    let current = read_registry_exact().unwrap();
    clients::shared::derive(id, &current.client_managed_entries)
        .unwrap()
        .unwrap()
}

/// Turn `id`'s receipt into the tombstone a recovery Discard leaves.
fn discard(id: &str, path: &Path) {
    std::fs::remove_file(receipt(id, path, "original")).unwrap();
    let tombstone = serde_json::json!({"version":1,"format":"JsonMcpServers","configPath":path,
        "original":null,"originalHash":null,"baseline":null,"capturedAt":0,"toolportVersion":"0",
        "lastWritten":null,"lastWrittenHash":null,"createdParents":[],"exactEligible":false,
        "preexistingGateways":[],"disconnected":true});
    std::fs::write(receipt(id, path, "released"), tombstone.to_string()).unwrap();
}

#[test]
fn a_failed_handover_rolls_back_and_crashes_leave_consistent_states() {
    let (a, b) = ("windsurf", "devin-cli");
    // Registry write 2 fails: the entry goes back to the carrier.
    crate::secrets::tests::with_isolated_vault(|| {
        let fx = fixture();
        let s2 = joined(a, b);
        assert!(disconnect_shared(a, &failing(2)).is_err());
        assert_eq!(read(&fx.path), s2);
        assert_eq!(records(), [b, a]);
        assert_eq!(
            read_registry_exact().unwrap().client_discovery_mode(b),
            Some("lazy")
        );
        disconnect_client(a).unwrap();
        assert_eq!(carrier(&fx.path).as_deref(), Some(b));
        disconnect_client(b).unwrap();
        assert_restored(&fx.path);
    });
    // Crash after write 1: a still carries the entry with b's access equal.
    crate::secrets::tests::with_isolated_vault(|| {
        let fx = fixture();
        let s2 = joined(a, b);
        update_registry_on_disk(&|r| clients::shared::copy_access(r, a, b)).unwrap();
        assert_eq!(group(b).carrier.as_deref(), Some(a));
        disconnect_client(b).unwrap();
        assert_eq!(read(&fx.path), s2);
        disconnect_client(a).unwrap();
        assert_restored(&fx.path);
    });
    // Crash after write 2: the entry names b and a derives as joined.
    crate::secrets::tests::with_isolated_vault(|| {
        let fx = fixture();
        joined(a, b);
        update_registry_on_disk(&|r| clients::shared::copy_access(r, a, b)).unwrap();
        let g = group(a);
        g.rewrite(b, g.profile.as_deref()).unwrap();
        let s3 = read(&fx.path);
        assert_eq!(shared(a).map(|s| s.joined), Some(true));
        disconnect_client(a).unwrap();
        assert_eq!(read(&fx.path), s3);
        assert_eq!(records(), [b]);
        disconnect_client(b).unwrap();
        assert_restored(&fx.path);
    });
}

#[test]
fn a_failed_registry_write_after_a_non_owner_last_out_puts_the_file_back() {
    crate::secrets::tests::with_isolated_vault(|| {
        let fx = fixture();
        let (a, b) = ("devin-cli", "windsurf");
        joined(a, b);
        disconnect_client(a).unwrap();
        let s3 = read(&fx.path);
        assert!(disconnect_shared(b, &failing(1)).is_err());
        assert_eq!(read(&fx.path), s3);
        assert_eq!(records(), [b]);
        assert!(receipt(a, &fx.path, "original").exists());
        assert!(!receipt(a, &fx.path, "released").exists());
        assert!(file(a, "moved-servers.json").exists());
        assert!(crate::secrets::file_vault_backups() > 0);
        disconnect_client(b).unwrap();
        assert_restored(&fx.path);
    });
}

#[test]
fn connect_and_disconnect_on_one_file_serialize_on_the_path_lock() {
    crate::secrets::tests::with_isolated_vault(|| {
        let fx = fixture();
        connect("windsurf").unwrap();
        let held = clients::shared::lock(&fx.path).unwrap();
        let xdg = fx.path.parent().unwrap().parent().unwrap().to_path_buf();
        let join = std::thread::spawn(move || {
            let _xdg = clients::EnvRestore::set("XDG_CONFIG_HOME", &xdg);
            connect_client_stdio("devin-cli", None, false).map(|_| ())
        });
        std::thread::sleep(std::time::Duration::from_millis(700));
        assert_eq!(records(), ["windsurf"], "Connect waits for the path lock");
        drop(held);
        join.join().unwrap().unwrap();
        assert_eq!(records(), ["devin-cli", "windsurf"]);
        let xdg = fx.path.parent().unwrap().parent().unwrap().to_path_buf();
        let leave = std::thread::spawn(move || {
            let _xdg = clients::EnvRestore::set("XDG_CONFIG_HOME", &xdg);
            disconnect_client("windsurf").map(|_| ())
        });
        let again = std::thread::spawn({
            let xdg = fx.path.parent().unwrap().parent().unwrap().to_path_buf();
            move || {
                let _xdg = clients::EnvRestore::set("XDG_CONFIG_HOME", &xdg);
                connect_client_stdio("devin-cli", None, false).map(|_| ())
            }
        });
        leave.join().unwrap().unwrap();
        again.join().unwrap().unwrap();
        // Either order ends on one live receipt with the entry naming devin-cli.
        assert_eq!(carrier(&fx.path).as_deref(), Some("devin-cli"));
        assert_eq!(records(), ["devin-cli"]);
        assert!(receipt("windsurf", &fx.path, "original").exists());
        assert!(!receipt("devin-cli", &fx.path, "original").exists());
    });
}

#[test]
fn a_shared_http_entry_is_not_joined() {
    crate::secrets::tests::with_isolated_vault(|| {
        let fx = fixture();
        connect("windsurf").unwrap();
        update_registry_on_disk(&|r| {
            let mut record = r.client_managed_entries["windsurf"].clone();
            record.transport = "sharedHttp".into();
            r.set_client_managed_entry("windsurf", record);
        })
        .unwrap();
        let before = read(&fx.path);
        let error = connect_client_stdio("devin-cli", None, false).unwrap_err();
        assert!(error.contains("through Shared HTTP"), "{error}");
        assert!(preview_client_setup("devin-cli").is_err());
        assert_eq!(read(&fx.path), before);
        assert_eq!(records(), ["windsurf"]);
    });
}

/// Main's code path for every operation on an unsupported pair's file.
fn assert_main_path(ids: &[&str]) {
    let current = read_registry_exact().unwrap();
    for id in ids {
        assert!(clients::shared::derive(id, &current.client_managed_entries)
            .unwrap()
            .is_none());
        assert!(matches!(
            plan_shared_connect(id, &current).unwrap(),
            SharedConnect::Main
        ));
        assert!(disconnect_shared(id, &update_registry_on_disk)
            .unwrap()
            .is_none());
    }
}

#[test]
fn unsupported_pairs_refuse_a_new_connect_and_keep_existing_ones_on_main() {
    for (first, second, var, file_name) in [
        ("pi", "omp", Some("PI_CODING_AGENT_DIR"), "mcp.json"),
        ("codex", "grok", None, "config.toml"),
    ] {
        crate::secrets::tests::with_isolated_vault(|| {
            let _fx = fixture();
            let dir = registry::conduit_dir().unwrap().join("pair");
            std::fs::create_dir_all(&dir).unwrap();
            let _vars: Vec<_> = match var {
                Some(var) => vec![clients::EnvRestore::set(var, &dir)],
                None => ["CODEX_HOME", "GROK_HOME"]
                    .map(|var| clients::EnvRestore::set(var, &dir))
                    .into(),
            };
            let path = dir.join(file_name);
            connect_client_stdio(first, None, false).unwrap();
            let before = read(&path);
            let error = connect_client_stdio(second, None, false).unwrap_err();
            assert!(error.contains("already uses"), "{error}");
            assert_eq!(read(&path), before);
            assert_eq!(records(), [first]);
            // Today's double connection (main wrote a second receipt) stays main's.
            let outcome = clients::install_gateway(second, None).unwrap();
            update_registry_on_disk(&|r| {
                r.set_client_managed_entry(second, outcome.managed.clone().unwrap())
            })
            .unwrap();
            assert_main_path(&[first, second]);
            connect_client_stdio(second, Some("default"), false).unwrap();
            disconnect_client(first).unwrap();
            disconnect_client(second).unwrap();
        });
    }
    // A saved Codex location on Grok's built-in file is refused too.
    crate::secrets::tests::with_isolated_vault(|| {
        let _fx = fixture();
        let dir = registry::conduit_dir().unwrap().join("grok");
        std::fs::create_dir_all(&dir).unwrap();
        let _grok = clients::EnvRestore::set("GROK_HOME", &dir);
        connect_client_stdio("grok", None, false).unwrap();
        let id = clients::add_client_location("codex", &dir.to_string_lossy()).unwrap();
        let error = connect_client_stdio(&id, None, false).unwrap_err();
        assert!(error.contains("already uses"), "{error}");
        assert_eq!(records(), ["grok"]);
    });
    // So is a T3 Code Codex home on Grok's file, in either order.
    for grok_first in [true, false] {
        crate::secrets::tests::with_isolated_vault(|| {
            let _fx = fixture();
            let home = registry::conduit_dir().unwrap().join("home");
            std::fs::create_dir_all(home.join(".t3/userdata")).unwrap();
            std::fs::create_dir_all(home.join(".grok")).unwrap();
            std::fs::write(
                home.join(".t3/userdata/settings.json"),
                r#"{"providerInstances": {"cx": {"driver": "codex", "config": {"homePath": "~/.grok"}}}}"#,
            )
            .unwrap();
            let _home = clients::TestHome::set(&home);
            let _grok = clients::EnvRestore::set("GROK_HOME", &home.join(".grok"));
            let t3 = clients::detect_clients_for_skills()
                .into_iter()
                .find(|client| client.id.starts_with("codex@t3-"))
                .expect("T3 Code lists the Codex home")
                .id;
            let (first, second) = if grok_first {
                ("grok", t3.as_str())
            } else {
                (t3.as_str(), "grok")
            };
            connect_client_stdio(first, None, false).unwrap();
            let path = home.join(".grok/config.toml");
            let before = read(&path);
            let error = connect_client_stdio(second, None, false).unwrap_err();
            assert!(error.contains("already uses"), "{error}");
            assert_eq!(read(&path), before);
            assert_eq!(records(), [first]);
            disconnect_client(first).unwrap();
        });
    }
}

#[test]
fn a_discarded_recovery_still_removes_the_entry_without_touching_others() {
    let (a, b) = ("windsurf", "devin-cli");
    let discarded = |warnings: &[String]| {
        warnings
            .iter()
            .any(|w| w.contains("recovery file was discarded"))
    };
    // One client: main kept Toolport's entry forever after a Discard.
    crate::secrets::tests::with_isolated_vault(|| {
        let fx = fixture();
        connect_client_stdio(a, None, false).unwrap();
        discard(a, &fx.path);
        let result = disconnect_client(a).unwrap();
        assert_eq!(carrier(&fx.path), None);
        assert!(discarded(&result.outcome.warnings));
    });
    // The owner discarded while b is joined: b leaves alone, then a removes it.
    crate::secrets::tests::with_isolated_vault(|| {
        let fx = fixture();
        let s2 = joined(a, b);
        discard(a, &fx.path);
        disconnect_client(b).unwrap();
        assert_eq!(read(&fx.path), s2);
        let result = disconnect_client(a).unwrap();
        assert_eq!(carrier(&fx.path), None);
        assert!(discarded(&result.outcome.warnings));
    });
    // The carrier discarded: handover writes no receipt; b removes the entry.
    crate::secrets::tests::with_isolated_vault(|| {
        let fx = fixture();
        joined(a, b);
        discard(a, &fx.path);
        disconnect_client(a).unwrap();
        assert_eq!(carrier(&fx.path).as_deref(), Some(b));
        assert!(!receipt(a, &fx.path, "original").exists());
        assert!(!receipt(b, &fx.path, "original").exists());
        let result = disconnect_client(b).unwrap();
        assert_eq!(carrier(&fx.path), None);
        assert!(discarded(&result.outcome.warnings));
        assert!(records().is_empty());
    });
}

#[test]
fn the_group_comes_from_facts_after_the_owner_moves_off_the_file() {
    crate::secrets::tests::with_isolated_vault(|| {
        let fx = fixture();
        joined("windsurf", "devin-cli");
        // Both Devin apps now resolve elsewhere; the receipt and records still name P.
        let elsewhere = registry::conduit_dir().unwrap().join("moved");
        std::fs::create_dir_all(elsewhere.join("devin")).unwrap();
        let _moved = clients::EnvRestore::set("XDG_CONFIG_HOME", &elsewhere);
        assert_eq!(group("windsurf").path, fx.path);
        disconnect_client("windsurf").unwrap();
        assert_eq!(carrier(&fx.path).as_deref(), Some("devin-cli"));
        disconnect_client("devin-cli").unwrap();
        assert_restored(&fx.path);
    });
}
