//! `/usr/bin/toolport` on Linux desktop installs is the GTK shell. A CLI command line runs
//! the CLI there before GTK or the display is touched; these runs have no display at all.
#![cfg(all(target_os = "linux", feature = "gtk-desktop"))]

mod cli_support;

use cli_support::{stderr, stdout, Fixture};

const GTK: &str = env!("CARGO_BIN_EXE_toolport-gtk");

fn run(f: &Fixture, exe: &str, args: &[&str]) -> std::process::Output {
    f.command_for(exe)
        .args(args)
        .output()
        .expect("run toolport-gtk")
}

#[test]
fn desktop_shell_runs_cli_commands_without_a_display() {
    let f = Fixture::new("desktop-dispatch");
    f.clean_machine();

    let out = run(&f, GTK, &["--version", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let version: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(version["name"], "toolport");
    assert_eq!(version["binary"], "toolport-gtk");
    assert_eq!(version["version"], env!("CARGO_PKG_VERSION"));
    assert!(version["detail"]
        .as_str()
        .unwrap()
        .starts_with("TOOLPORT_BUILD_STAMP:"));

    let out = run(&f, GTK, &["--help"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let help = stdout(&out);
    assert!(help.contains("toolport usage"), "{help}");
    assert!(help.contains("Desktop app:"), "{help}");
    assert!(help.contains("toolport-gtk --hidden"), "{help}");

    let scan = run(&f, GTK, &["scan", "--json"]);
    assert!(scan.status.success(), "{}", stderr(&scan));
    let scan: serde_json::Value = serde_json::from_slice(&scan.stdout).unwrap();
    assert!(scan.is_object());

    let out = run(&f, GTK, &["scna"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("did you mean 'toolport scan'"));
}

#[test]
fn hook_installed_through_the_desktop_toolport_runs_the_cli() {
    let f = Fixture::new("desktop-hook");
    f.clean_machine();
    let settings = f.write(".claude/settings.json", "{}\n");
    // The packages' `/usr/bin/toolport -> toolport-gtk`.
    let bin = f.root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let toolport = bin.join("toolport");
    std::os::unix::fs::symlink(GTK, &toolport).unwrap();
    let path = std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap();
    let toolport = toolport.to_str().unwrap();

    let out = f
        .command_for(toolport)
        .env("PATH", &path)
        .args(["hook", "install", "--agent", "claude-code", "--yes"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}{}", stdout(&out), stderr(&out));
    let text = f.read(&settings);
    assert!(
        text.contains(&format!("\\\"{toolport}\\\" hook check")),
        "{text}"
    );

    // What the agent runs at session start: the CLI's check, silent on a clean machine.
    let out = f
        .command_for(toolport)
        .env("PATH", &path)
        .args(["hook", "check"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(stdout(&out), "");
}

#[test]
fn desktop_shell_never_carries_the_npm_launchers_marker() {
    // Spelled in two parts so this test binary doesn't match itself by accident.
    let marker = format!("{}-binary-v1", "toolport-cli");
    let gtk = std::fs::read(GTK).unwrap();
    assert!(
        !gtk.windows(marker.len()).any(|w| w == marker.as_bytes()),
        "toolport-gtk contains the standalone CLI marker"
    );
    let cli = std::fs::read(env!("CARGO_BIN_EXE_toolport")).unwrap();
    assert!(cli.windows(marker.len()).any(|w| w == marker.as_bytes()));
}
