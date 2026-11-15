//! Removal fixtures run in child processes with isolated HOME, data and bus state.
use std::process::Command;

fn isolated_command(root: &std::path::Path, data: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_toolport-gateway"));
    // Client-path overrides and other ambient config must not escape the fixture.
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default());
    #[cfg(windows)]
    for key in ["SystemRoot", "WINDIR"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    let home = root.join("home");
    // A real account has a home directory in which removal writes its report.
    std::fs::create_dir_all(&home).expect("create isolated account home");
    command
        .env("TOOLPORT_TEST_ROOT", &home)
        .env("TOOLPORT_DATA_DIR", data)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("APPDATA", home.join("AppData/Roaming"))
        .env("LOCALAPPDATA", home.join("AppData/Local"))
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("XDG_RUNTIME_DIR", root.join("absent-runtime"))
        .env_remove("DBUS_SESSION_BUS_ADDRESS")
        .env_remove("CONDUIT_SECRET_KEY")
        .env_remove("TOOLPORT_SECRET_KEY");
    command
}

#[cfg(target_os = "linux")]
#[test]
fn purge_headless_without_keyring_and_with_file_backend_removes_data() {
    let env = conduit_lib::registry::DataDirTestEnv::new("purge_cli_headless");
    for file_backend in [false, true] {
        let data = env.dir.join(if file_backend { "file" } else { "no-bus" });
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(
            data.join("registry.json"),
            serde_json::to_vec(&conduit_lib::registry::Registry::default()).unwrap(),
        )
        .unwrap();
        std::fs::write(data.join("secrets.enc"), "encrypted fixture").unwrap();
        let mut command = isolated_command(&env.dir, &data);
        command.args(["--remove-data", "--confirm"]);
        if file_backend {
            command.env("TOOLPORT_SECRET_KEY", "purge-fixture-key").env(
                "DBUS_SESSION_BUS_ADDRESS",
                "unix:path=/nonexistent/purge-fixture-bus",
            );
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(report["leftovers"].as_array().unwrap().is_empty());
        assert!(report["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|note| note.as_str().unwrap().contains("No keyring in use")));
        assert!(!data.join("secrets.enc").exists());
        assert!(!data.join("registry.json").exists());
    }
}

#[test]
fn purge_r5_unconfirmed_cli_rejects_unowned_directory() {
    let env = conduit_lib::registry::DataDirTestEnv::new("purge_r5_cli_plan");
    let data = env.dir.join("Toolport");
    std::fs::create_dir(&data).unwrap();
    for all_installs in [false, true] {
        let mut command = isolated_command(&env.dir, &data);
        command.arg("--remove-data");
        if all_installs {
            command.arg("--all-installs");
        }
        let output = command.output().unwrap();
        assert!(
            !output.status.success(),
            "planning must refuse before confirmation"
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("ownership"));
    }
    let bytes = br#"{"servers":[]}"#;
    std::fs::write(data.join("registry.json"), bytes).unwrap();
    let output = isolated_command(&env.dir, &data)
        .arg("--remove-data")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("ownership"));
    assert_eq!(std::fs::read(data.join("registry.json")).unwrap(), bytes);
}

#[test]
fn purge_dry_run_and_confirm_report_the_same_ownership_error() {
    let env = conduit_lib::registry::DataDirTestEnv::new("purge_cli_error_order");
    let data = env.dir.join("Toolport");
    std::fs::create_dir(&data).unwrap();
    // A corrupt sibling install must not mask the selected folder's ownership error.
    let other = env.dir.join("Toolport-dev");
    std::fs::create_dir(&other).unwrap();
    std::fs::write(other.join("registry.json"), "{corrupt").unwrap();
    for confirm in [false, true] {
        let mut command = isolated_command(&env.dir, &data);
        command.arg("--remove-data");
        if confirm {
            command.arg("--confirm");
        }
        let output = command.output().unwrap();
        assert!(!output.status.success());
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            text.contains("Could not verify Toolport ownership"),
            "confirm={confirm}: {text}"
        );
        assert!(
            !text.contains("Credential ownership inventory"),
            "confirm={confirm}: {text}"
        );
    }
    assert_eq!(
        std::fs::read(other.join("registry.json")).unwrap(),
        b"{corrupt"
    );
}

#[test]
fn purge_named_override_without_registry_never_deletes_user_files() {
    let env = conduit_lib::registry::DataDirTestEnv::new("purge_cli_named_override");
    let data = env.dir.join("Toolport");
    std::fs::create_dir(&data).unwrap();
    std::fs::write(data.join("personal.txt"), "keep").unwrap();
    let output = isolated_command(&env.dir, &data)
        .args(["--remove-data", "--confirm"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("ownership"));
    assert_eq!(
        std::fs::read_to_string(data.join("personal.txt")).unwrap(),
        "keep"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn purge_override_with_registry_preserves_user_files_for_both_brand_overrides() {
    let env = conduit_lib::registry::DataDirTestEnv::new("purge_cli_user_files");
    for legacy in [false, true] {
        let root = env.dir.join(if legacy { "legacy" } else { "current" });
        let data = root.join(if legacy { "Conduit" } else { "Toolport" });
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(
            data.join("registry.json"),
            serde_json::to_vec(&conduit_lib::registry::Registry::default()).unwrap(),
        )
        .unwrap();
        for name in [
            "backups/tax.zip",
            "backups/personal/tax.zip",
            "backups/notes/original-report.json",
            "bin/personal-script",
            "logs/other-app.log",
            "daemon-notes.json",
        ] {
            let path = data.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "user bytes").unwrap();
        }
        std::fs::write(data.join("personal.txt"), "keep").unwrap();
        std::fs::write(data.join("secrets.enc"), "fixture").unwrap();
        let mut command = isolated_command(&root, &data);
        if legacy {
            command
                .env_remove("TOOLPORT_DATA_DIR")
                .env("CONDUIT_DATA_DIR", &data);
        }
        let output = command
            .args(["--remove-data", "--confirm"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(report["preserved"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry.as_str().unwrap().contains("personal.txt")));
        assert_eq!(
            std::fs::read_to_string(data.join("personal.txt")).unwrap(),
            "keep"
        );
        for name in [
            "backups/tax.zip",
            "backups/personal/tax.zip",
            "backups/notes/original-report.json",
            "bin/personal-script",
            "logs/other-app.log",
            "daemon-notes.json",
        ] {
            assert_eq!(
                std::fs::read_to_string(data.join(name)).unwrap(),
                "user bytes",
                "{name}"
            );
            // Unknown folders are preserved and reported as a unit.
            let reported = match name {
                "backups/personal/tax.zip" => "backups/personal",
                "backups/notes/original-report.json" => "backups/notes",
                other => other,
            };
            assert!(
                report["preserved"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|entry| entry.as_str().unwrap().contains(reported)),
                "{name}"
            );
        }
        assert!(!data.join("registry.json").exists() && !data.join("secrets.enc").exists());
    }
}

#[cfg(target_os = "linux")]
#[test]
fn purge_global_option_proceeds_with_corrupt_registry() {
    let env = conduit_lib::registry::DataDirTestEnv::new("purge_cli_corrupt");
    let data = env.dir.join("Toolport");
    std::fs::create_dir(&data).unwrap();
    let path = data.join("registry.json");
    std::fs::write(&path, "broken").unwrap();
    std::fs::write(data.join("personal.txt"), "keep").unwrap();
    let output = isolated_command(&env.dir, &data)
        .args(["--remove-data", "--confirm"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(text.contains(path.to_str().unwrap()), "{text}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "broken");
    let output = isolated_command(&env.dir, &data)
        .args(["--remove-data", "--all-installs", "--confirm"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(!path.exists());
    assert_eq!(
        std::fs::read_to_string(data.join("personal.txt")).unwrap(),
        "keep"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn purge_missing_desktop_bus_keeps_only_the_registry_for_credential_retry() {
    let env = conduit_lib::registry::DataDirTestEnv::new("purge_cli_bus_retry");
    let data = env.dir.join("Toolport");
    let runtime = env.dir.join("runtime");
    std::fs::create_dir(&data).unwrap();
    std::fs::create_dir(&runtime).unwrap();
    let registry = serde_json::to_vec(&conduit_lib::registry::Registry::default()).unwrap();
    std::fs::write(data.join("registry.json"), &registry).unwrap();
    for key in ["", "selected-file-backend"] {
        std::fs::write(data.join("secrets.enc"), "fixture").unwrap();
        let output = isolated_command(&env.dir, &data)
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("TOOLPORT_SECRET_KEY", key)
            .args(["--remove-data", "--confirm"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(report["leftovers"][0]["path"]
            .as_str()
            .unwrap()
            .contains("credential cleanup not done"));
        // The keyring could not be checked, so the registry stays for a retry.
        // Everything else that is Toolport's goes, the file secrets included.
        assert_eq!(std::fs::read(data.join("registry.json")).unwrap(), registry);
        assert!(!data.join("secrets.enc").exists());
    }
    std::fs::remove_dir(runtime).unwrap();
    let output = isolated_command(&env.dir, &data)
        .args(["--remove-data", "--confirm"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(!data.join("registry.json").exists());
}

#[cfg(target_os = "linux")]
#[test]
fn purge_recovers_runtime_bus_but_keeps_registry_when_secret_service_is_closed() {
    use std::io::BufRead;
    let env = conduit_lib::registry::DataDirTestEnv::new("purge_cli_runtime_bus");
    let data = env.dir.join("Toolport");
    let runtime = env.dir.join("runtime");
    std::fs::create_dir(&data).unwrap();
    std::fs::create_dir(&runtime).unwrap();
    let config = env.dir.join("bus.conf");
    // A private bus with no activation directories cannot touch a desktop keyring.
    std::fs::write(&config, format!(r#"<busconfig><type>session</type><listen>unix:path={}</listen><auth>EXTERNAL</auth><policy context="default"><allow send_destination="*"/><allow receive_sender="*"/><allow own="*"/></policy></busconfig>"#, runtime.join("bus").display())).unwrap();
    struct Bus(std::process::Child);
    impl Drop for Bus {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut bus = Bus(Command::new("dbus-daemon")
        .arg(format!("--config-file={}", config.display()))
        .args(["--nofork", "--nopidfile", "--print-address=1"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap());
    let mut address = String::new();
    std::io::BufReader::new(bus.0.stdout.take().unwrap())
        .read_line(&mut address)
        .unwrap();
    assert!(address.starts_with("unix:path="));
    let registry = serde_json::to_vec(&conduit_lib::registry::Registry::default()).unwrap();
    std::fs::write(data.join("registry.json"), &registry).unwrap();
    for key in ["", "selected-file-backend"] {
        let output = isolated_command(&env.dir, &data)
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("TOOLPORT_SECRET_KEY", key)
            .args(["--remove-data", "--confirm"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(
            report["leftovers"][0]["error"]
                .as_str()
                .unwrap()
                .contains("Secret Service is unavailable"),
            "{report}"
        );
        assert_eq!(std::fs::read(data.join("registry.json")).unwrap(), registry);
        assert!(report["removed"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| !p.as_str().unwrap().contains("registry.json")));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn purge_other_registry_failure_exits_nonzero_and_retry_converges() {
    let env = conduit_lib::registry::DataDirTestEnv::new("purge_cli_other_retry");
    let data = env.dir.join("Toolport");
    let other = env.dir.join("Toolport-dev");
    for dir in [&data, &other] {
        std::fs::create_dir(dir).unwrap();
    }
    let registry = serde_json::to_vec(&conduit_lib::registry::Registry::default()).unwrap();
    std::fs::write(data.join("registry.json"), &registry).unwrap();
    std::fs::write(data.join("secrets.enc"), "selected encrypted tokens").unwrap();
    let other_registry = other.join("registry.json");
    std::fs::write(&other_registry, "broken").unwrap();
    let output = isolated_command(&env.dir, &data)
        .args(["--remove-data", "--confirm"])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains(other_registry.to_str().unwrap())
            && text.contains("Credentials were not removed"),
        "{text}"
    );
    assert_eq!(std::fs::read(data.join("registry.json")).unwrap(), registry);
    assert!(data.join("secrets.enc").exists());
    std::fs::write(&other_registry, &registry).unwrap();
    let output = isolated_command(&env.dir, &data)
        .args(["--remove-data", "--confirm"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(!data.join("registry.json").exists() && !data.join("secrets.enc").exists());
    assert_eq!(std::fs::read(other_registry).unwrap(), registry);
}

#[cfg(target_os = "linux")]
#[test]
fn purge_r5_recovery_only_other_install_converges_after_rename() {
    let env = conduit_lib::registry::DataDirTestEnv::new("purge_r5_cli_recovery");
    let data = env.dir.join("Toolport");
    let other = env.dir.join("Toolport-dev");
    for dir in [&data, &other] {
        std::fs::create_dir(dir).unwrap();
    }
    let registry = serde_json::to_vec(&conduit_lib::registry::Registry::default()).unwrap();
    std::fs::write(data.join("registry.json"), &registry).unwrap();
    std::fs::write(data.join("secrets.enc"), "encrypted bytes").unwrap();
    let leftover = other.join("registry.json.unreadable-123");
    std::fs::write(&leftover, "broken").unwrap();
    let output = isolated_command(&env.dir, &data)
        .args(["--remove-data", "--confirm"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(
        text.contains(&leftover.display().to_string()) && text.contains("rename"),
        "{text}"
    );
    assert_eq!(std::fs::read(data.join("registry.json")).unwrap(), registry);
    assert_eq!(
        std::fs::read_to_string(data.join("secrets.enc")).unwrap(),
        "encrypted bytes"
    );
    std::fs::rename(leftover, other.join("saved-recovery.json")).unwrap();
    std::fs::write(other.join("registry.json.bak"), &registry).unwrap();
    for _ in 0..2 {
        let output = isolated_command(&env.dir, &data)
            .args(["--remove-data", "--dry-run"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let output = isolated_command(&env.dir, &data)
        .args(["--remove-data", "--confirm"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(!data.join("registry.json").exists() && !data.join("secrets.enc").exists());
    assert_eq!(
        std::fs::read(other.join("registry.json.bak")).unwrap(),
        registry
    );
    assert_eq!(
        std::fs::read_to_string(other.join("saved-recovery.json")).unwrap(),
        "broken"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn purge_r4_dev_leftovers_do_not_block_later_main_install_removal() {
    let env = conduit_lib::registry::DataDirTestEnv::new("purge_cli_r4_leftovers");
    let dev = env.dir.join("Toolport-dev");
    let main = env.dir.join("Toolport");
    let registry = serde_json::to_vec(&conduit_lib::registry::Registry::default()).unwrap();
    for dir in [&dev, &main] {
        std::fs::create_dir(dir).unwrap();
        std::fs::write(dir.join("registry.json"), &registry).unwrap();
    }
    std::fs::create_dir(dev.join("logs")).unwrap();
    std::fs::write(dev.join("logs/other-app.log"), "user bytes").unwrap();
    let output = isolated_command(&env.dir, &dev)
        .args(["--remove-data", "--confirm"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(!dev.join("registry.json").exists());
    let output = isolated_command(&env.dir, &main)
        .args(["--remove-data", "--confirm"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(!main.join("registry.json").exists());
    assert_eq!(
        std::fs::read_to_string(dev.join("logs/other-app.log")).unwrap(),
        "user bytes"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn purge_r4_custom_plan_guidance_matches_post_removal_ownership_check() {
    let env = conduit_lib::registry::DataDirTestEnv::new("purge_cli_r4_guidance");
    let data = env.dir.join("custom-data");
    std::fs::create_dir(&data).unwrap();
    std::fs::write(
        data.join("registry.json"),
        serde_json::to_vec(&conduit_lib::registry::Registry::default()).unwrap(),
    )
    .unwrap();
    let output = isolated_command(&env.dir, &data)
        .args(["--remove-data", "--dry-run"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let plan: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        plan["resources"].as_array().unwrap().iter().any(|entry| {
            let text = entry.as_str().unwrap();
            text.contains("custom data directory")
                && text.contains("before removing its registry")
                && text.contains("OS credential manager")
        }),
        "{plan}"
    );
    let output = isolated_command(&env.dir, &data)
        .args(["--remove-data", "--confirm"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(!data.join("registry.json").exists());
    let output = isolated_command(&env.dir, &data)
        .args(["--remove-data", "--all-installs", "--confirm"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Could not verify Toolport ownership"));
}

#[cfg(target_os = "linux")]
#[test]
fn purge_r7_cli_removes_location_store_and_exact_backup_names() {
    let env = conduit_lib::registry::DataDirTestEnv::new("purge_r7_cli_locations");
    let data = env.dir.join("data");
    std::fs::create_dir(&data).unwrap();
    std::fs::write(
        data.join("registry.json"),
        serde_json::to_vec(&conduit_lib::registry::Registry::default()).unwrap(),
    )
    .unwrap();
    let _selected = conduit_lib::registry::DataDirOverride::set(data.clone());
    // These writers run in this process, so child-only roots are insufficient.
    struct TestRoot(Option<std::ffi::OsString>);
    impl Drop for TestRoot {
        fn drop(&mut self) {
            match self.0.take() {
                Some(value) => std::env::set_var("TOOLPORT_TEST_ROOT", value),
                None => std::env::remove_var("TOOLPORT_TEST_ROOT"),
            }
        }
    }
    let _root = TestRoot(std::env::var_os("TOOLPORT_TEST_ROOT"));
    std::env::set_var("TOOLPORT_TEST_ROOT", env.dir.join("home"));
    let long_id = format!("claude-code@claude-{}", "x".repeat(210));
    let prefix: String = long_id.chars().take(120).collect();
    let bounded = format!("{prefix}-{}", conduit_lib::registry::sha256_hex(&long_id));
    let mut rows = Vec::new();
    let mut generated = Vec::new();
    for (id, component, kind, basename) in [
        (
            "claude-code@claude2",
            "claude-code@claude2",
            "claude-code",
            ".claude.json",
        ),
        (
            long_id.as_str(),
            bounded.as_str(),
            "claude-code",
            ".claude.json",
        ),
        (
            "codex@0123456789abcdef",
            "codex@0123456789abcdef",
            "codex",
            "config.toml",
        ),
    ] {
        let config = env
            .dir
            .join(format!("client-{}", rows.len()))
            .join(basename);
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, if kind == "codex" { "" } else { "{}" }).unwrap();
        rows.push(serde_json::json!({"id":id,"clientType":kind,"name":"fixture","configPath":config,"manual":true}));
        let dir = data.join("backups").join(component);
        std::fs::create_dir_all(&dir).unwrap();
        for name in [
            format!("123-{basename}"),
            format!("123-{basename}.123.1.conduit-tmp"),
        ] {
            let path = dir.join(name);
            std::fs::write(&path, "generated").unwrap();
            generated.push(path);
        }
    }
    // The earlier unbounded long-ID writer is still supported alongside the
    // current bounded component when both recovery folders exist.
    let legacy = data.join("backups").join(&long_id);
    std::fs::create_dir(&legacy).unwrap();
    let legacy_file = legacy.join("123-.claude.json");
    std::fs::write(&legacy_file, "generated").unwrap();
    generated.push(legacy_file);
    let orphan_id = format!("claude-code@claude-{}", "y".repeat(210));
    let orphan_folder = env
        .dir
        .join("home")
        .join(format!(".claude-{}", "y".repeat(210)));
    std::fs::create_dir_all(&orphan_folder).unwrap();
    let orphan_config = orphan_folder.join(".claude.json");
    std::fs::write(&orphan_config, "{}").unwrap();
    rows.push(serde_json::json!({"id":orphan_id,"clientType":"claude-code","name":"forgotten","configPath":orphan_config,"manual":true}));
    std::fs::write(
        data.join("client-locations.json"),
        serde_json::to_vec(&rows).unwrap(),
    )
    .unwrap();
    // Real writers supply valid provenance, which the CLI must restore before
    // purge. Fake recovery JSON would correctly block the entire removal.
    for row in &rows {
        let id = row["id"].as_str().unwrap();
        let write = conduit_lib::clients::write_servers(id, &[]).unwrap();
        generated.push(std::path::PathBuf::from(write.backup.unwrap()));
        let record = write.recovery_path.unwrap();
        generated.push(record.clone());
        let moved = record.parent().unwrap().join("moved-servers.json");
        std::fs::write(
            &moved,
            serde_json::to_vec(&serde_json::json!({"configPath":row["configPath"],"entries":[]}))
                .unwrap(),
        )
        .unwrap();
        generated.push(moved);
    }

    let orphan_backup = generated.last().unwrap().parent().unwrap().to_owned();
    conduit_lib::clients::remove_client_location(&orphan_id).unwrap();
    std::fs::remove_dir_all(&orphan_folder).unwrap();
    rows.pop();
    let orphan_legacy = data.join("backups").join(&orphan_id);
    std::fs::create_dir(&orphan_legacy).unwrap();
    let orphan_legacy_file = orphan_legacy.join("123-.claude.json");
    std::fs::write(&orphan_legacy_file, "generated").unwrap();
    generated.push(orphan_legacy_file);
    for name in [
        "client-locations.json.lock",
        "client-locations.json.123.1.conduit-tmp",
    ] {
        let path = data.join(name);
        std::fs::write(&path, "generated").unwrap();
        generated.push(path);
    }
    let wrong_hash = format!("backups/{prefix}-{}", "0".repeat(64));
    let collisions = [
        "client-locations.json.notes".to_owned(),
        "client-locations.json.123.bad.conduit-tmp".into(),
        "backups/claude-code@claude2/2024-taxes.pdf".into(),
        "backups/claude-code@claude2/123-config.toml".into(),
        "backups/codex@personal/123-config.toml".into(),
        format!("{wrong_hash}/123-.claude.json"),
    ];
    for name in &collisions {
        let path = data.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "user bytes").unwrap();
    }
    // A directory colliding with an owned file is retained with its contents.
    std::fs::create_dir(data.join("client-locations.json.123.2.conduit-tmp")).unwrap();
    std::fs::write(
        data.join("client-locations.json.123.2.conduit-tmp/notes"),
        "user bytes",
    )
    .unwrap();
    let output = isolated_command(&env.dir, &data)
        .args(["--remove-data", "--confirm"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(report["leftovers"].as_array().unwrap().is_empty());
    assert!(!data.join("client-locations.json").exists());
    for path in generated {
        assert!(!path.exists(), "{}", path.display());
    }
    for name in collisions {
        assert_eq!(std::fs::read(data.join(name)).unwrap(), b"user bytes");
    }
    assert_eq!(
        std::fs::read(data.join("client-locations.json.123.2.conduit-tmp/notes")).unwrap(),
        b"user bytes"
    );
    assert!(
        !data.join("backups").join(bounded).exists(),
        "empty generated folder removed"
    );
    assert!(!legacy.exists(), "empty legacy long-ID folder removed");
    assert!(!orphan_backup.exists(), "forgotten bounded folder removed");
    assert!(!orphan_legacy.exists(), "forgotten legacy folder removed");
    assert!(!data.join("backups/codex@0123456789abcdef").exists());
    assert!(
        data.join("backups/claude-code@claude2").is_dir(),
        "foreign files keep their folder"
    );
    for row in rows {
        assert!(std::path::Path::new(row["configPath"].as_str().unwrap()).exists());
    }
}

#[test]
fn purge_r7_cli_test_root_redirects_os_known_folders() {
    let env = conduit_lib::registry::DataDirTestEnv::new("purge_r7_cli_roots");
    let data = env.dir.join("data");
    std::fs::create_dir(&data).unwrap();
    std::fs::write(
        data.join("registry.json"),
        serde_json::to_vec(&conduit_lib::registry::Registry::default()).unwrap(),
    )
    .unwrap();
    let output = isolated_command(&env.dir, &data)
        .arg("--remove-data")
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "unconfirmed removal only prints its plan"
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("repeat with --confirm"));
    let plan: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(std::path::Path::new(plan["reportPath"].as_str().unwrap())
        .starts_with(env.dir.join("home")));
    assert!(
        data.join("registry.json").exists(),
        "planning stays read-only"
    );
}

#[test]
#[should_panic(expected = "in-process integration tests require TOOLPORT_TEST_ROOT")]
fn in_process_client_discovery_requires_test_root() {
    let _env = conduit_lib::registry::DataDirTestEnv::new("purge_cli_root_guard");
    struct RestoreRoot(Option<std::ffi::OsString>);
    impl Drop for RestoreRoot {
        fn drop(&mut self) {
            if let Some(value) = self.0.take() {
                std::env::set_var("TOOLPORT_TEST_ROOT", value);
            }
        }
    }
    let _restore = RestoreRoot(std::env::var_os("TOOLPORT_TEST_ROOT"));
    std::env::remove_var("TOOLPORT_TEST_ROOT");
    conduit_lib::clients::detect_clients();
}

// A confirmed removal inventories the OS credential store. On Linux the
// fixture has no session bus, so it never reaches a keyring; on macOS and
// Windows it would query the runner's real Keychain or Credential Manager.
// Opening the report file is covered on every platform by the purge unit tests.
#[cfg(target_os = "linux")]
#[test]
fn confirmed_removal_persists_the_console_report_in_the_users_home() {
    let env = conduit_lib::registry::DataDirTestEnv::new("purge_cli_report");
    let data = env.dir.join("Toolport");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(
        data.join("registry.json"),
        serde_json::to_vec(&conduit_lib::registry::Registry::default()).unwrap(),
    )
    .unwrap();
    let output = isolated_command(&env.dir, &data)
        .args(["--remove-data", "--confirm"])
        .env("TOOLPORT_SECRET_KEY", "purge-fixture-key")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let console: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let reports: Vec<_> = std::fs::read_dir(env.dir.join("home"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("Toolport-removal-report-")
        })
        .collect();
    assert_eq!(reports.len(), 1);
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(reports[0].path()).unwrap()).unwrap();
    assert_eq!(saved, console);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(reports[0].path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    assert!(!data.join("registry.json").exists());
}
