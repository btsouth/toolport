//! Shared fixtures for the `toolport` CLI tests. Every run is a child process with a cleared
//! environment, its own home, data folder, cache folder and encrypted-file keychain, so a test
//! can never read or write the developer's real agent configs, Toolport data or OS keyring.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

pub struct Fixture {
    pub root: PathBuf,
    pub home: PathBuf,
    pub data: PathBuf,
    pub project: PathBuf,
}

impl Fixture {
    pub fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "toolport-cli-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        let data = root.join("data");
        let project = home.join("work").join("demo");
        for dir in [&home, &data, &project] {
            std::fs::create_dir_all(dir).unwrap();
        }
        Fixture {
            root,
            home,
            data,
            project,
        }
    }

    pub fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_toolport"));
        command
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default());
        #[cfg(windows)]
        for key in ["SystemRoot", "WINDIR"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        command
            .env("TOOLPORT_TEST_ROOT", &self.home)
            .env("TOOLPORT_DATA_DIR", &self.data)
            .env("TOOLPORT_CACHE_DIR", self.root.join("cache"))
            .env("TOOLPORT_SECRET_KEY", "cli-fixture-key")
            .env("HOME", &self.home)
            .env("USERPROFILE", &self.home)
            .env("APPDATA", self.home.join("AppData/Roaming"))
            .env("LOCALAPPDATA", self.home.join("AppData/Local"))
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("XDG_DATA_HOME", self.home.join(".local/share"))
            .env("XDG_CACHE_HOME", self.home.join(".cache"))
            .env("XDG_RUNTIME_DIR", self.root.join("absent-runtime"))
            .env("NO_COLOR", "1")
            .env("LANG", "C.UTF-8")
            .current_dir(&self.project);
        command
    }

    pub fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().expect("run toolport")
    }

    pub fn run_env(&self, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut c = self.command();
        for (k, v) in env {
            c.env(k, v);
        }
        c.args(args).output().expect("run toolport")
    }

    pub fn json(&self, args: &[&str]) -> serde_json::Value {
        let out = self.run(args);
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "{args:?} did not print JSON ({e}):\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        })
    }

    pub fn write(&self, rel: &str, content: &str) -> PathBuf {
        let path = self.home.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        path
    }

    pub fn write_project(&self, rel: &str, content: &str) -> PathBuf {
        let path = self.project.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        path
    }

    pub fn read(&self, path: &Path) -> String {
        std::fs::read_to_string(path).unwrap_or_default()
    }

    /// Claude Code and Cursor configs that hold the same plaintext token, two servers that
    /// drift, a broken skill link and a shared skills.sh folder.
    pub fn messy_machine(&self) {
        self.write(
            ".claude.json",
            &format!(
                r#"{{"mcpServers":{{
                    "github":{{"command":"npx","args":["-y","@modelcontextprotocol/server-github"],"env":{{"GITHUB_TOKEN":"{SECRET}"}}}},
                    "fs":{{"command":"npx","args":["-y","server-fs","/a"]}}
                }}}}"#
            ),
        );
        self.write(".claude/settings.json", "{}\n");
        self.write(
            ".claude/CLAUDE.md",
            "# Global rules\n\nBe brief. Run tests before you finish.\n",
        );
        self.write(
            ".cursor/mcp.json",
            &format!(
                r#"{{"mcpServers":{{
                    "github":{{"command":"npx","args":["-y","@modelcontextprotocol/server-github"],"env":{{"GITHUB_TOKEN":"{SECRET}"}}}},
                    "fs":{{"command":"npx","args":["-y","server-fs","/b"]}}
                }}}}"#
            ),
        );
        self.write(
            ".agents/skills/shared/SKILL.md",
            "---\nname: shared\ndescription: A skill every agent can read from the shared folder.\n---\nBody.\n",
        );
        let skills = self.home.join(".claude/skills");
        std::fs::create_dir_all(&skills).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(self.home.join("gone/skill"), skills.join("broken")).unwrap();
    }

    /// A machine with one agent and nothing wrong.
    pub fn clean_machine(&self) {
        self.write(
            ".claude.json",
            r#"{"mcpServers":{"fs":{"command":"npx","args":["-y","server-fs","/a"]}}}"#,
        );
        self.write(".claude/settings.json", "{}\n");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A token-shaped value that must never appear in any output.
pub const SECRET: &str = "ghp_FixtureValue0123456789abcdefABCDEF";

pub fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

pub fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Epoch milliseconds to `2026-11-24T05:16:35.011Z`.
pub fn iso(ms: u64) -> String {
    let days = (ms / 86_400_000) as i64;
    let rem = ms % 86_400_000;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3_600_000,
        rem / 60_000 % 60,
        rem / 1000 % 60,
        rem % 1000
    )
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}
