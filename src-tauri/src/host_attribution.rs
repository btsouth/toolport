//! Which app hosts the MCP client that started this gateway, when it can be told reliably.
//!
//! T3 Code runs agents as providers. Their calls reach Toolport under the provider's own
//! client id (`claude-code@claude2`, `codex`), which stays the access principal and the key
//! existing filters and stats use. This adds a display-only fact beside it: "via T3 Code",
//! and the T3 Code thread when the provider passes it on.
//!
//! Signals, strongest first, and never a guess:
//!
//! * Claude Code started by T3 Code sets `CLAUDE_CODE_ENTRYPOINT=t3-code` and passes
//!   `T3CODE_THREAD_ID` down to its MCP servers. Both reach this process's environment,
//!   and count only with T3 Code installed and among the gateway's ancestors.
//! * Pi and the ACP providers (Kiro, Grok, fx, ACP Registry agents) get `T3CODE_THREAD_ID`
//!   in their process environment from T3 Code (its Pi adapter and `acpMcpContext`), so a
//!   gateway they start inherits it. With T3 Code's own process among the gateway's
//!   ancestors, that names the thread too. The same holds for anything else running in a
//!   T3 Code thread's process tree with that variable, which is where it runs.
//! * Codex strips the environment it gives MCP servers (it keeps an allow-list such as
//!   `HOME` and `PATH`), so a Codex gateway has neither. T3 Code starts `codex app-server`
//!   itself, so the gateway's parent is `codex` and its grandparent is T3 Code's own
//!   process (or npm's Node launcher, then T3 Code). That says "via T3 Code" but not which thread: one app-server serves many
//!   threads, T3 Code puts the thread id only in Codex's shell environment, and the
//!   `threadId` Codex sends in tool call `_meta` is Codex's own id, not T3 Code's.
//! * OpenCode is the same: T3 Code starts one `opencode serve` per instance, which serves
//!   every thread of it and starts MCP servers as its own children. Antigravity's `agy acp`
//!   (installed as `agy` or `antigravity`) starts them as its children too; when T3 Code
//!   passes it the thread, the first rule names it.
//! * Nothing else is attributed: a thread id without T3 Code among the ancestors is not.
//!
//! T3 Code's process is its Electron executable run as Node (`process.execPath`):
//! `t3code` on Linux (`executableName`), and the product name on macOS and Windows, where
//! electron-builder names the executable after it: `T3 Code (Beta)` and
//! `T3 Code (Nightly)` today (T3 Code's `apps/desktop/package.json` and
//! `scripts/build-desktop-artifact.ts`), `T3 Code` if it drops the suffix.
use serde::{Deserialize, Serialize};

/// The host id recorded for T3 Code.
pub const T3_CODE: &str = "t3-code";

/// The display-only host of a session.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Host {
    /// Always [`T3_CODE`] today.
    pub host: String,
    /// T3 Code's thread id, when the provider passes it on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
}

/// Detect this process's host. Cheap when T3 Code is not installed: one environment read
/// and one `stat` of its data folder, and no process is inspected.
pub fn detect() -> Option<Host> {
    detect_with(
        |key| std::env::var(key).ok(),
        t3_installed,
        || crate::client_process::ancestor_names(std::process::id(), ANCESTOR_LIMIT),
    )
}

/// How far up the process tree to look for T3 Code: the provider, a launcher shell and
/// a helper or two sit between it and a gateway.
const ANCESTOR_LIMIT: usize = 8;

/// The operating systems T3 Code ships for, for [`is_t3_code_process_on`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Linux,
    MacOs,
    Windows,
}

/// This build's operating system.
pub const fn this_os() -> Os {
    if cfg!(target_os = "macos") {
        Os::MacOs
    } else if cfg!(windows) {
        Os::Windows
    } else {
        Os::Linux
    }
}

/// T3 Code's product names, which are its executable names on macOS and Windows.
const T3_CODE_PRODUCT_NAMES: [&str; 3] = ["T3 Code", "T3 Code (Beta)", "T3 Code (Nightly)"];

/// Whether an executable name (no folder) is T3 Code's process on `os`.
pub fn is_t3_code_process_on(name: &str, os: Os) -> bool {
    let name = name.rsplit(['/', '\\']).next().unwrap_or(name).trim();
    match os {
        Os::Linux => name == "t3code",
        Os::MacOs => T3_CODE_PRODUCT_NAMES.contains(&name),
        Os::Windows => {
            let lower = name.to_ascii_lowercase();
            let stem = lower.strip_suffix(".exe").unwrap_or(&lower);
            T3_CODE_PRODUCT_NAMES
                .iter()
                .any(|product| product.to_ascii_lowercase() == stem)
        }
    }
}

/// Providers T3 Code starts itself whose MCP servers may not get its thread: one process
/// serves many threads (Codex's `app-server`, OpenCode's `serve`), or T3 Code gives it a
/// filtered environment (Antigravity's `agy acp`).
const PROVIDER_STEMS: [&str; 4] = ["codex", "opencode", "agy", "antigravity"];

fn stem(name: &str) -> String {
    let name = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let lower = name.to_ascii_lowercase();
    lower.strip_suffix(".exe").unwrap_or(&lower).to_string()
}

/// [`detect`] with its inputs passed in, for tests.
pub fn detect_with(
    env: impl Fn(&str) -> Option<String>,
    installed: impl FnOnce() -> bool,
    ancestors: impl FnOnce() -> Vec<String>,
) -> Option<Host> {
    // Every signal needs T3 Code installed and among the ancestors: the environment alone
    // is something any process can set.
    if !installed() {
        return None;
    }
    let names = ancestors();
    let os = this_os();
    let thread = env("T3CODE_THREAD_ID").and_then(|id| valid_thread(&id));
    let claude_from_t3 = env("CLAUDE_CODE_ENTRYPOINT").as_deref().map(str::trim) == Some(T3_CODE);
    // Claude Code started by T3 Code, or anything in a T3 Code thread's process tree with
    // the thread T3 Code set.
    if (claude_from_t3 || thread.is_some())
        && names.iter().any(|name| is_t3_code_process_on(name, os))
    {
        return Some(Host {
            host: T3_CODE.into(),
            thread,
        });
    }
    // T3 Code starts Codex, OpenCode and Antigravity directly, or through npm's Node
    // launcher, which starts the native binary.
    let provider_under_t3 = match names.as_slice() {
        [parent, grandparent, rest @ ..] if PROVIDER_STEMS.contains(&stem(parent).as_str()) => {
            let provider = stem(parent);
            is_t3_code_process_on(grandparent, os)
                || ((stem(grandparent) == provider
                    || stem(grandparent) == format!("{provider}.js")
                    || stem(grandparent) == format!("{provider}.cjs")
                    || stem(grandparent) == "node")
                    && rest
                        .first()
                        .is_some_and(|name| is_t3_code_process_on(name, os)))
        }
        _ => false,
    };
    provider_under_t3.then(|| Host {
        host: T3_CODE.into(),
        thread: None,
    })
}

/// Whether T3 Code has a data folder on this machine: `T3CODE_HOME/userdata`, or
/// `<home>/.t3/userdata` without `T3CODE_HOME`, exists.
fn t3_installed() -> bool {
    // Tests never look at the machine's real T3 Code folder.
    #[cfg(any(test, feature = "test-support"))]
    {
        std::env::var_os("TOOLPORT_TEST_T3_INSTALLED").is_some()
    }
    #[cfg(not(any(test, feature = "test-support")))]
    {
        t3_folder_exists()
    }
}

#[cfg(not(any(test, feature = "test-support")))]
fn t3_folder_exists() -> bool {
    t3_folder_exists_in(std::env::var_os("T3CODE_HOME"), dirs::home_dir())
}

/// T3 Code's data folder (`T3CODE_HOME`, else `<home>/.t3`) exists with its `userdata`.
/// Only the folder counts, not the variable naming it.
#[cfg_attr(any(test, feature = "test-support"), allow(dead_code))]
fn t3_folder_exists_in(
    t3_home: Option<std::ffi::OsString>,
    home: Option<std::path::PathBuf>,
) -> bool {
    let folder = match t3_home.filter(|value| !value.is_empty()) {
        Some(t3_home) => std::path::PathBuf::from(t3_home),
        None => match home {
            Some(home) => home.join(".t3"),
            None => return false,
        },
    };
    folder.join("userdata").is_dir()
}

/// A T3 Code thread id as T3 Code makes them (a UUID), or any similar short opaque token.
/// Anything else is dropped rather than shown.
pub fn valid_thread(raw: &str) -> Option<String> {
    let id = raw.trim();
    (!id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b':' | b'.')))
    .then(|| id.to_string())
}

/// Whether a host id is one Toolport records.
pub fn valid_host(raw: &str) -> bool {
    raw == T3_CODE
}

/// The host and thread from adapter headers, validated. A thread without a host is dropped.
pub fn from_parts(host: Option<&str>, thread: Option<&str>) -> Option<Host> {
    let host = host.map(str::trim).filter(|host| valid_host(host))?;
    Some(Host {
        host: host.into(),
        thread: thread.and_then(valid_thread),
    })
}

/// The product name for a host id, not translated.
pub fn host_name(host: &str) -> Option<&'static str> {
    (host == T3_CODE).then_some("T3 Code")
}

/// The first eight characters of a thread id, for a short label. The full id is shown on
/// hover and kept in the record.
pub fn short_thread(id: &str) -> String {
    let id = id.strip_prefix("thread:").unwrap_or(id);
    id.chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            pairs
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| value.to_string())
        }
    }

    /// T3 Code's process name on the platform the test runs on.
    fn t3_code_process() -> &'static str {
        match this_os() {
            Os::Linux => "t3code",
            Os::MacOs => "T3 Code",
            Os::Windows => "T3 Code.exe",
        }
    }

    #[test]
    fn claude_code_under_t3_code_carries_its_thread() {
        let host = detect_with(
            env(&[
                ("CLAUDE_CODE_ENTRYPOINT", "t3-code"),
                ("T3CODE_THREAD_ID", "465774fd-c52e-4b7e-9d5c-3f0f3a6f0a11"),
            ]),
            || true,
            || vec!["claude".into(), t3_code_process().into()],
        )
        .unwrap();
        assert_eq!(host.host, "t3-code");
        assert_eq!(
            host.thread.as_deref(),
            Some("465774fd-c52e-4b7e-9d5c-3f0f3a6f0a11")
        );
        assert_eq!(short_thread(host.thread.as_deref().unwrap()), "465774fd");
        // Started by T3 Code without a thread id: still via T3 Code.
        let host = detect_with(
            env(&[("CLAUDE_CODE_ENTRYPOINT", "t3-code")]),
            || true,
            || vec!["claude".into(), t3_code_process().into()],
        )
        .unwrap();
        assert_eq!(host.thread, None);
    }

    #[test]
    fn t3_code_home_counts_only_when_its_folder_exists() {
        let root = std::env::temp_dir().join(format!("toolport-t3-gate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let missing = root.join("missing");
        assert!(!t3_folder_exists_in(
            Some(missing.clone().into_os_string()),
            Some(home.clone())
        ));
        assert!(!t3_folder_exists_in(None, Some(home.clone())));
        std::fs::create_dir_all(missing.join("userdata")).unwrap();
        assert!(t3_folder_exists_in(
            Some(missing.into_os_string()),
            Some(home.clone())
        ));
        std::fs::create_dir_all(home.join(".t3/userdata")).unwrap();
        assert!(t3_folder_exists_in(None, Some(home)));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn claude_code_environment_alone_is_not_t3_code() {
        // Anyone can set these variables: T3 Code must be installed and an ancestor.
        let claimed = env(&[
            ("CLAUDE_CODE_ENTRYPOINT", "t3-code"),
            ("T3CODE_THREAD_ID", "chosen-thread"),
        ]);
        assert_eq!(
            detect_with(&claimed, || false, || vec!["bash".into()]),
            None
        );
        assert_eq!(detect_with(&claimed, || true, || vec!["bash".into()]), None);
        assert_eq!(
            detect_with(
                &claimed,
                || false,
                || panic!("no process walk without T3 Code installed")
            ),
            None
        );
    }

    #[test]
    fn a_thread_id_alone_is_not_t3_code() {
        // A thread id with no T3 Code process among the ancestors (a copied or exported
        // variable) is not attributed.
        assert_eq!(
            detect_with(
                env(&[
                    ("CLAUDE_CODE_ENTRYPOINT", "cli"),
                    ("T3CODE_THREAD_ID", "465774fd"),
                ]),
                || true,
                || vec!["claude".into(), "bash".into()],
            ),
            None
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn codex_installed_with_npm_is_attributed_through_its_launcher() {
        // npm's `codex` is a Node launcher that starts the native binary.
        for launcher in ["codex.js", "node", "codex"] {
            assert!(
                detect_with(
                    env(&[]),
                    || true,
                    || vec!["codex".into(), launcher.into(), "t3code".into()]
                )
                .is_some(),
                "{launcher}"
            );
        }
        // Any other process between Codex and T3 Code says nothing.
        assert!(detect_with(
            env(&[]),
            || true,
            || vec!["codex".into(), "bash".into(), "t3code".into()]
        )
        .is_none());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn codex_is_attributed_by_its_parents_without_a_thread() {
        let host =
            detect_with(env(&[]), || true, || vec!["codex".into(), "t3code".into()]).unwrap();
        assert_eq!(
            host,
            Host {
                host: "t3-code".into(),
                thread: None
            }
        );
        // The Windows and macOS names are checked in their own cfg-gated tests.
        // A Codex started from a shell, or a Codex whose parent is gone, says nothing.
        for chain in [
            vec!["codex".to_string(), "bash".to_string()],
            vec!["codex".to_string()],
            vec!["claude".to_string(), "t3code".to_string()],
            vec![],
        ] {
            assert_eq!(
                detect_with(env(&[]), || true, || chain.clone()),
                None,
                "{chain:?}"
            );
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn providers_that_pass_the_thread_are_attributed_with_it() {
        // Pi or an ACP agent: T3 Code's launcher shell and the agent sit between.
        let host = detect_with(
            env(&[("T3CODE_THREAD_ID", "thread:0f8e2c1a-9b7d")]),
            || true,
            || {
                vec![
                    "kiro-cli".into(),
                    "sh".into(),
                    "t3code".into(),
                    "t3code".into(),
                ]
            },
        )
        .unwrap();
        assert_eq!(host.thread.as_deref(), Some("thread:0f8e2c1a-9b7d"));
        // An invalid thread id is not a signal at all.
        assert_eq!(
            detect_with(
                env(&[("T3CODE_THREAD_ID", "bad id")]),
                || true,
                || vec!["pi".into(), "t3code".into()],
            ),
            None
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn opencode_and_antigravity_are_attributed_by_their_parents_without_a_thread() {
        for chain in [
            vec!["opencode", "t3code"],
            vec!["opencode", "node", "t3code"],
            vec!["opencode", "opencode", "t3code"],
            vec!["agy", "t3code"],
            vec!["antigravity", "t3code"],
        ] {
            let host = detect_with(
                env(&[]),
                || true,
                || chain.iter().map(|name| name.to_string()).collect(),
            )
            .unwrap();
            assert_eq!(host.host, T3_CODE);
            assert_eq!(host.thread, None);
        }
        // OpenCode started from a shell, or T3 Code further up, is not attributed.
        for chain in [
            vec!["opencode", "bash", "t3code"],
            vec!["opencode", "bash"],
            vec!["bash", "opencode", "t3code"],
        ] {
            assert_eq!(
                detect_with(
                    env(&[]),
                    || true,
                    || { chain.iter().map(|name| name.to_string()).collect() }
                ),
                None,
                "{chain:?}"
            );
        }
    }

    #[test]
    fn t3_code_process_names_per_platform() {
        assert!(is_t3_code_process_on("t3code", Os::Linux));
        assert!(is_t3_code_process_on("/opt/T3 Code/t3code", Os::Linux));
        assert!(!is_t3_code_process_on("T3 Code (Beta)", Os::Linux));
        for name in ["T3 Code (Beta)", "T3 Code (Nightly)", "T3 Code"] {
            assert!(is_t3_code_process_on(name, Os::MacOs), "{name}");
            assert!(
                is_t3_code_process_on(&format!("{name}.exe"), Os::Windows),
                "{name}"
            );
        }
        assert!(is_t3_code_process_on("t3 code (beta).EXE", Os::Windows));
        // Development runs use Electron's own name; that is not attributed.
        for os in [Os::Linux, Os::MacOs, Os::Windows] {
            assert!(!is_t3_code_process_on("Electron", os));
            assert!(!is_t3_code_process_on("t3code-helper", os));
        }
        assert!(!is_t3_code_process_on("T3 Code Helper", Os::MacOs));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn codex_under_t3_code_on_macos() {
        assert!(detect_with(
            env(&[]),
            || true,
            || vec!["codex".into(), "T3 Code (Beta)".into()]
        )
        .is_some());
        assert!(detect_with(env(&[]), || true, || vec!["codex".into(), "t3code".into()]).is_none());
    }

    #[test]
    #[cfg(windows)]
    fn codex_under_t3_code_on_windows() {
        assert!(detect_with(
            env(&[]),
            || true,
            || vec!["codex.exe".into(), "T3 Code (Beta).exe".into()]
        )
        .is_some());
        assert!(detect_with(
            env(&[]),
            || true,
            || vec!["codex.exe".into(), "t3code.exe".into()]
        )
        .is_none());
    }

    #[test]
    fn nothing_is_inspected_without_t3_code() {
        assert_eq!(
            detect_with(
                env(&[]),
                || false,
                || panic!("no process walk without T3 Code")
            ),
            None
        );
    }

    #[test]
    fn odd_thread_ids_and_hosts_are_dropped() {
        assert_eq!(valid_thread("  "), None);
        assert_eq!(valid_thread("a b"), None);
        assert_eq!(valid_thread("x\ny"), None);
        assert_eq!(valid_thread(&"a".repeat(129)), None);
        assert_eq!(
            valid_thread("thread:abc-1_2.3").as_deref(),
            Some("thread:abc-1_2.3")
        );
        assert_eq!(short_thread("thread:465774fd-c52e"), "465774fd");
        assert_eq!(from_parts(Some("other"), Some("x")), None);
        assert_eq!(from_parts(None, Some("x")), None);
        assert_eq!(
            from_parts(Some("t3-code"), Some("bad id")),
            Some(Host {
                host: "t3-code".into(),
                thread: None
            })
        );
    }
}
