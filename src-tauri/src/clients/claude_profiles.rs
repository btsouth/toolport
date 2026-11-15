//! Labels for machines with several Claude Code profiles (`~/.claude.json`, `~/.claude2`, ...)
//! or Codex homes (`~/.codex`, `~/Codex-Work/.codex`, ...). Folder names alone don't say
//! which account a profile is or how it is started, so the Clients page shows the command
//! that starts each one and, for Claude Code, the account signed in to it.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// What tells one Claude Code profile, or one Codex home, apart from the others on this
/// machine.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeProfile {
    /// The command that starts the client with this profile: `claude` or `codex` for the
    /// default config, or a launcher script or shell alias that sets `CLAUDE_CONFIG_DIR`
    /// or `CODEX_HOME` to it.
    pub command: Option<String>,
    /// The claude.ai account signed in to this profile, if any. Never set for Codex: its
    /// account is only in `auth.json`, next to its tokens.
    pub account: Option<String>,
    /// Where the profile lives, in `~` form: the config folder, or `~/.claude.json` for
    /// the default Claude Code profile.
    pub folder: String,
    /// When the client last saved this profile's settings (Claude Code) or its history and
    /// state (Codex), in milliseconds since the epoch.
    pub last_used_ms: Option<u64>,
    /// The client id of the profile this one copies: same account, in `~/.claude`, read
    /// only when something sets `CLAUDE_CONFIG_DIR=~/.claude`. Background tools such as
    /// usage meters do that, so it is listed under the real profile instead of beside it.
    pub same_as: Option<String>,
}

/// Launcher scripts are a few lines; anything bigger is some other program.
const LAUNCHER_READ_LIMIT: u64 = 16 * 1024;
/// Shell startup files bigger than this are not read for aliases.
const RC_READ_LIMIT: u64 = 1024 * 1024;

fn is_claude_code(id: &str) -> bool {
    id == "claude-code" || id.starts_with("claude-code@")
}

/// A Claude Code entry on this machine. One whose config can't be read still belongs
/// with its siblings; it just has no account to show.
fn is_profile(client: &DetectedClient) -> bool {
    is_claude_code(&client.id)
        && (client.config_exists || client.app_present || client.error.is_some())
}

fn is_codex(id: &str) -> bool {
    id == "codex" || id.starts_with("codex@")
}

/// A Codex home on this machine, the default `~/.codex` or another `CODEX_HOME`.
fn is_codex_profile(client: &DetectedClient) -> bool {
    is_codex(&client.id) && (client.config_exists || client.app_present || client.error.is_some())
}

/// Fill in `claude_profile` when more than one Claude Code profile, or more than one
/// Codex home, is listed.
pub(super) fn annotate(clients: &mut [DetectedClient]) {
    // Most machines have one profile; skip the launcher scan for them.
    let claude = clients.iter().filter(|client| is_profile(client)).count() >= 2;
    let codex = clients
        .iter()
        .filter(|client| is_codex_profile(client))
        .count()
        >= 2;
    if !claude && !codex {
        return;
    }
    let Some(home) = home() else {
        return;
    };
    let path_dirs = user_path_dirs();
    if claude {
        annotate_in(clients, &home, &launchers(&home, &path_dirs));
    }
    if codex {
        annotate_codex_in(clients, &home, &launchers_of(CODEX, &home, &path_dirs));
    }
}

/// Codex homes get the command that starts them and when they were last used. There is
/// no account: Codex keeps it only in `auth.json`, which is never read.
pub(super) fn annotate_codex_in(
    clients: &mut [DetectedClient],
    home: &Path,
    launchers: &[(String, PathBuf)],
) {
    if clients
        .iter()
        .filter(|client| is_codex_profile(client))
        .count()
        < 2
    {
        return;
    }
    for client in clients.iter_mut().filter(|client| is_codex_profile(client)) {
        let config = PathBuf::from(&client.config_path);
        let Some(folder) = config.parent() else {
            continue;
        };
        let command = if client.id == "codex" {
            Some("codex".to_string())
        } else {
            let folder = canonical(folder);
            launchers
                .iter()
                .filter(|(_, dir)| *dir == folder)
                .map(|(name, _)| name.clone())
                .min_by_key(|name| (name.len(), name.clone()))
        };
        client.claude_profile = Some(ClaudeProfile {
            command,
            account: None,
            folder: tilde(home, folder),
            last_used_ms: codex_last_active(folder),
            same_as: None,
        });
    }
}

/// The newest of a Codex home's settings, history and state files. Codex rewrites its
/// `state_*.sqlite` and `logs_*.sqlite` databases while it runs; `config.toml` alone can
/// be months old. Only file times are read, never contents.
fn codex_last_active(folder: &Path) -> Option<u64> {
    std::fs::read_dir(folder)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_name().to_str().is_some_and(|name| {
                name == "config.toml"
                    || name == "history.jsonl"
                    || ((name.starts_with("state_") || name.starts_with("logs_"))
                        && name.contains(".sqlite"))
            })
        })
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| modified_ms(&entry.path()))
        .max()
}

pub(super) fn annotate_in(
    clients: &mut [DetectedClient],
    home: &Path,
    launchers: &[(String, PathBuf)],
) {
    if clients.iter().filter(|client| is_profile(client)).count() < 2 {
        return;
    }
    let default_config = home.join(".claude.json");
    for client in clients.iter_mut().filter(|client| is_profile(client)) {
        let config = PathBuf::from(&client.config_path);
        let is_default = same_path(&config, &default_config);
        let Some(folder) = config.parent() else {
            continue;
        };
        let command = if is_default {
            Some("claude".to_string())
        } else {
            let folder = canonical(folder);
            launchers
                .iter()
                .filter(|(_, dir)| *dir == folder)
                .map(|(name, _)| name.clone())
                .min_by_key(|name| (name.len(), name.clone()))
        };
        client.claude_profile = Some(ClaudeProfile {
            command,
            account: client.error.is_none().then(|| account(&config)).flatten(),
            folder: tilde(home, if is_default { &config } else { folder }),
            last_used_ms: modified_ms(&config),
            same_as: None,
        });
    }
    // `~/.claude/.claude.json` beside a default `~/.claude.json` for the same account is the
    // default profile read through CLAUDE_CONFIG_DIR, not an account of its own.
    let default = clients
        .iter()
        .find(|client| {
            is_claude_code(&client.id) && same_path(Path::new(&client.config_path), &default_config)
        })
        .and_then(|client| {
            let account = client.claude_profile.as_ref()?.account.clone()?;
            Some((client.id.clone(), account))
        });
    let Some((default_id, default_account)) = default else {
        return;
    };
    let copy_config = home.join(".claude").join(".claude.json");
    for client in clients.iter_mut().filter(|client| {
        is_claude_code(&client.id)
            && !client.gateway_installed
            && same_path(Path::new(&client.config_path), &copy_config)
    }) {
        if let Some(profile) = client.claude_profile.as_mut() {
            if profile.command.is_none() && profile.account.as_deref() == Some(&default_account) {
                profile.same_as = Some(default_id.clone());
            }
        }
    }
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn same_path(a: &Path, b: &Path) -> bool {
    a == b || canonical(a) == canonical(b)
}

fn tilde(home: &Path, path: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display()),
        Err(_) => path.display().to_string(),
    }
}

fn modified_ms(path: &Path) -> Option<u64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let since = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    u64::try_from(since.as_millis()).ok()
}

#[derive(Deserialize)]
struct SavedProfile {
    #[serde(rename = "oauthAccount")]
    oauth_account: Option<SavedAccount>,
}

#[derive(Deserialize)]
struct SavedAccount {
    #[serde(rename = "emailAddress")]
    email_address: Option<String>,
}

/// The claude.ai account email saved in a profile's `.claude.json`. Read the way the
/// client scan reads it; only the account is parsed, the rest is skipped.
fn account(config: &Path) -> Option<String> {
    let text = read_config_file(config).ok()?;
    let saved: SavedProfile = serde_json::from_str(&text).ok()?;
    let email = saved.oauth_account?.email_address?;
    let email = email.trim();
    (!email.is_empty() && email.len() <= 320 && !email.chars().any(char::is_control))
        .then(|| email.to_string())
}

/// A program that takes its config folder from one environment variable.
#[derive(Clone, Copy)]
pub(super) struct Launched {
    /// The program's command name, e.g. `claude`.
    pub program: &'static str,
    /// The variable that sets its config folder, e.g. `CLAUDE_CONFIG_DIR`.
    pub variable: &'static str,
}

pub(super) const CLAUDE: Launched = Launched {
    program: "claude",
    variable: "CLAUDE_CONFIG_DIR",
};

pub(super) const CODEX: Launched = Launched {
    program: "codex",
    variable: "CODEX_HOME",
};

/// Commands that start Claude Code with a fixed `CLAUDE_CONFIG_DIR`: small scripts in the
/// user's own bin folders and bash or zsh aliases. Returns (command, canonical folder).
pub(super) fn launchers(home: &Path, path_dirs: &[PathBuf]) -> Vec<(String, PathBuf)> {
    launchers_of(CLAUDE, home, path_dirs)
}

/// The user's PATH folders below home, for [`launchers_of`].
pub(super) fn user_path_dirs() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default()
}

/// Commands that start `tool` with its config folder fixed: small scripts in the user's
/// own bin folders and bash or zsh aliases. Returns (command, canonical folder).
pub(super) fn launchers_of(
    tool: Launched,
    home: &Path,
    path_dirs: &[PathBuf],
) -> Vec<(String, PathBuf)> {
    let mut dirs = vec![home.join(".local").join("bin"), home.join("bin")];
    // Only the user's own PATH folders, below home: system folders hold thousands of
    // binaries, and home itself holds dotfiles.
    let real_home = canonical(home);
    for dir in path_dirs {
        let real = canonical(dir);
        if real.starts_with(&real_home)
            && real != real_home
            && !dirs.iter().any(|known| canonical(known) == real)
        {
            dirs.push(real);
        }
    }
    let mut found = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_string)
            else {
                continue;
            };
            let Some(text) = launcher_text(&path, LAUNCHER_READ_LIMIT) else {
                continue;
            };
            if let Some(folder) = script_config_dir(tool, &text, home) {
                found.push((name, canonical(&folder)));
            }
        }
    }
    for rc in [".bashrc", ".zshrc", ".bash_aliases"] {
        let Some(text) = launcher_text(&home.join(rc), RC_READ_LIMIT) else {
            continue;
        };
        for line in text.lines() {
            if let Some((name, folder)) = alias_config_dir(tool, line, home) {
                found.push((name, canonical(&folder)));
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

/// Launcher candidates read so far, by path, with the size and modification time they
/// had. A Clients refresh lists launchers several times, for both Claude Code and Codex;
/// only files that changed since are read again. Files that set neither variable keep no
/// text.
type LauncherTexts = HashMap<PathBuf, (u64, std::time::SystemTime, Option<Arc<str>>)>;
static LAUNCHER_TEXTS: std::sync::Mutex<Option<LauncherTexts>> = std::sync::Mutex::new(None);
/// More cached files than this means a bin folder full of small programs; start over.
const LAUNCHER_CACHE_LIMIT: usize = 4096;

/// The text of an ordinary file of at most `limit` bytes, if it mentions a variable a
/// launcher sets.
fn launcher_text(path: &Path, limit: u64) -> Option<Arc<str>> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > limit {
        return None;
    }
    let modified = meta.modified().ok()?;
    let mut cache = LAUNCHER_TEXTS
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let cache = cache.get_or_insert_with(HashMap::new);
    if let Some((len, time, text)) = cache.get(path) {
        if *len == meta.len() && *time == modified {
            return text.clone();
        }
    }
    let text = std::fs::read_to_string(path)
        .ok()
        .filter(|text| {
            [CLAUDE, CODEX]
                .iter()
                .any(|tool| text.contains(tool.variable))
        })
        .map(Arc::from);
    if cache.len() >= LAUNCHER_CACHE_LIMIT {
        cache.clear();
    }
    cache.insert(path.to_path_buf(), (meta.len(), modified, text.clone()));
    text
}

/// A launcher script: it assigns the config variable on a line of its own (or right
/// before a command) and runs the program.
fn script_config_dir(tool: Launched, text: &str, home: &Path) -> Option<PathBuf> {
    if !runs(tool, text) {
        return None;
    }
    let assignment = format!("{}=", tool.variable);
    text.lines().find_map(|line| {
        let line = line.trim();
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let line = line.strip_prefix("exec ").unwrap_or(line).trim_start();
        let line = line.strip_prefix("env ").unwrap_or(line).trim_start();
        let value = line.strip_prefix(assignment.as_str())?;
        config_dir_value(value, home)
    })
}

/// `alias name='CLAUDE_CONFIG_DIR=~/.claude2 claude'` and the double-quoted form.
fn alias_config_dir(tool: Launched, line: &str, home: &Path) -> Option<(String, PathBuf)> {
    let rest = line.trim().strip_prefix("alias ")?.trim_start();
    let (name, body) = rest.split_once('=')?;
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return None;
    }
    let body = body.trim();
    let quote = body.chars().next().filter(|c| matches!(c, '\'' | '"'))?;
    let body = body[1..].strip_suffix(quote)?;
    if !runs(tool, body) {
        return None;
    }
    let body = body.trim_start();
    let body = body.strip_prefix("env ").unwrap_or(body).trim_start();
    let value = body.strip_prefix(format!("{}=", tool.variable).as_str())?;
    Some((name.to_string(), config_dir_value(value, home)?))
}

/// Whether the text runs the program, by name or by path.
fn runs(tool: Launched, text: &str) -> bool {
    let by_path = format!("/{}", tool.program);
    text.split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ';' | '&' | '|'))
        .any(|word| word == tool.program || word.ends_with(by_path.as_str()))
}

/// The folder from a `CLAUDE_CONFIG_DIR=` value: the first word, quotes removed, with
/// `~`, `$HOME` or `${HOME}` at the start. Anything else that needs the shell is skipped.
fn config_dir_value(value: &str, home: &Path) -> Option<PathBuf> {
    let value = value.trim_start();
    let word = match value.chars().next()? {
        quote @ ('"' | '\'') => value[1..].split(quote).next()?,
        _ => value
            .split(|c: char| c.is_whitespace() || matches!(c, ';' | '&' | '|'))
            .next()?,
    };
    let rest = word
        .strip_prefix("${HOME}")
        .or_else(|| word.strip_prefix("$HOME"))
        .or_else(|| word.strip_prefix('~'));
    let path = match rest {
        Some(rest) => {
            let rest = rest.strip_prefix('/')?;
            home.join(rest)
        }
        None => PathBuf::from(word),
    };
    let text = path.to_string_lossy();
    (path.is_absolute() && !text.contains('$') && !text.contains('`')).then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway home folder, removed on drop.
    struct TempHome(PathBuf);

    impl TempHome {
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn home() -> TempHome {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "toolport-claude-profiles-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Canonical, so paths compare equal to what the scan resolves.
        TempHome(std::fs::canonicalize(&dir).unwrap())
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn client(id: &str, config: &Path) -> DetectedClient {
        DetectedClient {
            id: id.into(),
            name: id.into(),
            uses_connectors: false,
            location_removable: false,
            location_kept_by_t3_code: false,
            discovery: discovery::capabilities("claude-code"),
            config_path: config.to_string_lossy().into_owned(),
            config_exists: config.exists(),
            app_present: true,
            servers: Vec::new(),
            plugin_servers: Vec::new(),
            gateway_installed: false,
            entry_state: GatewayEntryState::Absent,
            error: None,
            claude_profile: None,
            t3_code: Vec::new(),
        }
    }

    fn signed_in(email: &str) -> String {
        format!(r#"{{"oauthAccount":{{"emailAddress":"{email}"}},"mcpServers":{{}}}}"#)
    }

    #[test]
    fn launcher_scripts_and_aliases_name_their_profiles() {
        let dir = home();
        let home = dir.path();
        std::fs::create_dir_all(home.join(".claude2")).unwrap();
        std::fs::create_dir_all(home.join(".claude3")).unwrap();
        std::fs::create_dir_all(home.join(".claude-work")).unwrap();
        write(
            &home.join(".local/bin/claude2"),
            "#!/bin/bash\n# Second Claude Code account\nexport CLAUDE_CONFIG_DIR=\"$HOME/.claude2\"\nexec \"$HOME/.local/bin/claude\" \"$@\"\n",
        );
        write(
            &home.join("bin/c3"),
            "#!/bin/sh\nCLAUDE_CONFIG_DIR=~/.claude3 exec claude \"$@\"\n",
        );
        // Mentions the variable in text and never runs claude: not a launcher.
        write(
            &home.join(".local/bin/usage-meter"),
            "#!/bin/sh\necho 'Run claude with CLAUDE_CONFIG_DIR=~/.claude2 to log in'\nnode meter.js\n",
        );
        // Reads the folder from its arguments: nothing fixed to match.
        write(
            &home.join(".local/bin/collect"),
            "#!/bin/sh\nexport CLAUDE_CONFIG_DIR=\"$1\"\nclaude --print usage\n",
        );
        write(
            &home.join(".bashrc"),
            "alias cw='CLAUDE_CONFIG_DIR=${HOME}/.claude-work claude'\nalias ll='ls -l'\n",
        );
        // An unquoted value ends at the shell's `;`, in a PATH folder of the user's own.
        std::fs::create_dir_all(home.join(".claude4")).unwrap();
        write(
            &home.join("tools/c4"),
            "#!/bin/sh\nexport CLAUDE_CONFIG_DIR=$HOME/.claude4; exec claude \"$@\"\n",
        );
        // Home itself on PATH is not scanned: it holds dotfiles, not launchers.
        write(
            &home.join(".c5"),
            "#!/bin/sh\nexport CLAUDE_CONFIG_DIR=~/.claude4\nexec claude\n",
        );
        let found = launchers(home, &[home.to_path_buf(), home.join("tools")]);
        let canon = |p: &str| std::fs::canonicalize(home.join(p)).unwrap();
        assert_eq!(
            found,
            vec![
                ("c3".to_string(), canon(".claude3")),
                ("c4".to_string(), canon(".claude4")),
                ("claude2".to_string(), canon(".claude2")),
                ("cw".to_string(), canon(".claude-work")),
            ]
        );
    }

    #[test]
    fn profiles_get_their_command_account_and_a_copy_of_the_default_is_marked() {
        let dir = home();
        let home = dir.path();
        let default = home.join(".claude.json");
        let copy = home.join(".claude/.claude.json");
        let second = home.join(".claude2/.claude.json");
        let unsigned = home.join(".claude-api/.claude.json");
        write(&default, &signed_in("me@work.example"));
        write(&copy, &signed_in("me@work.example"));
        write(&second, &signed_in("me@home.example"));
        write(&unsigned, r#"{"mcpServers":{}}"#);
        let mut clients = vec![
            client("claude-code", &default),
            client("claude-code@claude", &copy),
            client("claude-code@claude2", &second),
            client("claude-code@claude-api", &unsigned),
            client("cursor", &home.join(".cursor/mcp.json")),
        ];
        let launchers = vec![(
            "claude2".to_string(),
            std::fs::canonicalize(home.join(".claude2")).unwrap(),
        )];
        annotate_in(&mut clients, home, &launchers);

        let profile = |i: usize| clients[i].claude_profile.clone().unwrap();
        assert_eq!(profile(0).command.as_deref(), Some("claude"));
        assert_eq!(profile(0).account.as_deref(), Some("me@work.example"));
        assert_eq!(
            profile(0).folder,
            format!("~{}.claude.json", std::path::MAIN_SEPARATOR)
        );
        assert!(profile(0).last_used_ms.is_some());
        assert_eq!(profile(0).same_as, None);
        assert_eq!(profile(1).same_as.as_deref(), Some("claude-code"));
        assert_eq!(profile(1).command, None);
        assert_eq!(profile(2).command.as_deref(), Some("claude2"));
        assert_eq!(profile(2).account.as_deref(), Some("me@home.example"));
        assert_eq!(profile(2).same_as, None);
        assert_eq!(profile(3).account, None);
        assert_eq!(profile(3).command, None);
        assert!(clients[4].claude_profile.is_none());
    }

    #[test]
    fn a_connected_or_different_account_copy_stays_its_own_profile() {
        let dir = home();
        let home = dir.path();
        let default = home.join(".claude.json");
        let copy = home.join(".claude/.claude.json");
        write(&default, &signed_in("me@work.example"));
        write(&copy, &signed_in("other@work.example"));
        let mut clients = vec![
            client("claude-code", &default),
            client("claude-code@claude", &copy),
        ];
        annotate_in(&mut clients, home, &[]);
        assert_eq!(clients[1].claude_profile.as_ref().unwrap().same_as, None);

        write(&copy, &signed_in("me@work.example"));
        let mut clients = vec![
            client("claude-code", &default),
            client("claude-code@claude", &copy),
        ];
        clients[1].gateway_installed = true;
        annotate_in(&mut clients, home, &[]);
        assert_eq!(clients[1].claude_profile.as_ref().unwrap().same_as, None);
    }

    #[test]
    fn a_copy_alone_and_an_unreadable_profile_still_join_the_group() {
        let dir = home();
        let home = dir.path();
        let default = home.join(".claude.json");
        let copy = home.join(".claude/.claude.json");
        write(&default, &signed_in("me@work.example"));
        write(&copy, &signed_in("me@work.example"));
        let broken = home.join(".claude2/.claude.json");
        write(&broken, "{ not json");
        let mut unreadable = client("claude-code@claude2", &broken);
        unreadable.error = Some("invalid JSON".into());
        let mut clients = vec![
            client("claude-code", &default),
            client("claude-code@claude", &copy),
            unreadable,
        ];
        annotate_in(&mut clients, home, &[]);
        assert_eq!(
            clients[1]
                .claude_profile
                .as_ref()
                .unwrap()
                .same_as
                .as_deref(),
            Some("claude-code")
        );
        let broken = clients[2].claude_profile.as_ref().unwrap();
        assert_eq!(
            broken.folder,
            format!("~{}.claude2", std::path::MAIN_SEPARATOR)
        );
        assert_eq!(broken.account, None);

        // The default and its copy alone: one profile, with the copy behind a note.
        clients.pop();
        for client in &mut clients {
            client.claude_profile = None;
        }
        annotate_in(&mut clients, home, &[]);
        assert!(clients[0].claude_profile.is_some());
        assert!(clients[1]
            .claude_profile
            .as_ref()
            .unwrap()
            .same_as
            .is_some());
    }

    #[test]
    fn a_single_claude_profile_needs_no_labels() {
        let dir = home();
        let home = dir.path();
        let default = home.join(".claude.json");
        write(&default, &signed_in("me@work.example"));
        let mut clients = vec![client("claude-code", &default)];
        annotate_in(&mut clients, home, &[]);
        assert!(clients[0].claude_profile.is_none());

        // An added location beside a default profile that is not on this machine.
        std::fs::remove_file(&default).unwrap();
        let extra = home.join(".claude2").join(".claude.json");
        write(&extra, &signed_in("me@home.example"));
        let mut absent = client("claude-code", &default);
        absent.app_present = false;
        let mut clients = vec![absent, client("claude-code@claude2", &extra)];
        annotate_in(&mut clients, home, &[]);
        assert!(clients.iter().all(|client| client.claude_profile.is_none()));
    }

    #[test]
    fn codex_launchers_name_their_codex_homes() {
        let dir = home();
        let home = dir.path();
        std::fs::create_dir_all(home.join("Codex-Work/.codex")).unwrap();
        std::fs::create_dir_all(home.join("Codex-Second/.codex")).unwrap();
        std::fs::create_dir_all(home.join(".claude2")).unwrap();
        write(
            &home.join(".local/bin/codex-work"),
            "#!/bin/bash\nexport CODEX_HOME=\"$HOME/Codex-Work/.codex\"\nunset OPENAI_API_KEY CODEX_API_KEY\nexec codex \"$@\"\n",
        );
        // Runs Codex without choosing a home: nothing to match.
        write(
            &home.join(".local/bin/codex-scratch"),
            "#!/bin/bash\nexec /opt/codex/bin/codex \"$@\"\n",
        );
        // Reads a home's files without running Codex.
        write(
            &home.join(".local/bin/codex-tps"),
            "#!/usr/bin/env python3\nROOT = os.path.expanduser('~/.codex/sessions')\n",
        );
        // A Claude launcher is not a Codex one, and the other way around.
        write(
            &home.join(".local/bin/claude2"),
            "#!/bin/sh\nexport CLAUDE_CONFIG_DIR=~/.claude2\nexec claude\n",
        );
        write(
            &home.join(".zshrc"),
            "alias cx2=\"CODEX_HOME=~/Codex-Second/.codex codex\"\n",
        );
        let canon = |p: &str| std::fs::canonicalize(home.join(p)).unwrap();
        assert_eq!(
            launchers_of(CODEX, home, &[]),
            vec![
                ("codex-work".to_string(), canon("Codex-Work/.codex")),
                ("cx2".to_string(), canon("Codex-Second/.codex")),
            ]
        );
        assert_eq!(
            launchers(home, &[]),
            vec![("claude2".to_string(), canon(".claude2"))]
        );
    }

    #[test]
    fn codex_homes_get_their_command_and_last_activity_but_no_account() {
        let dir = home();
        let home = dir.path();
        let main = home.join(".codex/config.toml");
        let work = home.join("Codex-Work/.codex/config.toml");
        let unused = home.join(".codex2/config.toml");
        write(&main, "");
        write(&work, "");
        write(&unused, "");
        write(
            &home.join("Codex-Work/.codex/auth.json"),
            r#"{"tokens":{"id_token":"secret"}}"#,
        );
        write(&home.join("Codex-Work/.codex/state_5.sqlite"), "");
        let mut clients = vec![
            client("codex", &main),
            client("codex@Codex-Work%2F.codex", &work),
            client("codex@.codex2", &unused),
            client("claude-code", &home.join(".claude.json")),
        ];
        let launchers = vec![(
            "codex-work".to_string(),
            std::fs::canonicalize(home.join("Codex-Work/.codex")).unwrap(),
        )];
        annotate_codex_in(&mut clients, home, &launchers);
        let profile = |i: usize| clients[i].claude_profile.clone().unwrap();
        assert_eq!(profile(0).command.as_deref(), Some("codex"));
        assert_eq!(
            profile(0).folder,
            format!("~{}.codex", std::path::MAIN_SEPARATOR)
        );
        assert_eq!(profile(1).command.as_deref(), Some("codex-work"));
        assert_eq!(
            profile(1).folder,
            format!(
                "~{sep}Codex-Work{sep}.codex",
                sep = std::path::MAIN_SEPARATOR
            )
        );
        assert!(profile(1).last_used_ms.is_some());
        assert_eq!(profile(2).command, None);
        assert!((0..3).all(|i| profile(i).account.is_none() && profile(i).same_as.is_none()));
        assert!(clients[3].claude_profile.is_none());

        // One Codex home needs no labels.
        let mut clients = vec![client("codex", &main)];
        annotate_codex_in(&mut clients, home, &launchers);
        assert!(clients[0].claude_profile.is_none());
    }

    // Launchers are shell scripts with Unix paths; `/home/me` is not absolute on Windows.
    #[cfg(unix)]
    #[test]
    fn config_dir_values_only_accept_plain_paths() {
        let home = Path::new("/home/me");
        assert_eq!(
            config_dir_value("\"$HOME/.claude2\"", home),
            Some(PathBuf::from("/home/me/.claude2"))
        );
        assert_eq!(
            config_dir_value("~/.claude3 claude", home),
            Some(PathBuf::from("/home/me/.claude3"))
        );
        assert_eq!(
            config_dir_value("'/opt/claude work'", home),
            Some(PathBuf::from("/opt/claude work"))
        );
        assert_eq!(config_dir_value("\"$1\"", home), None);
        assert_eq!(config_dir_value("$XDG_CONFIG_HOME/claude", home), None);
        assert_eq!(config_dir_value("relative/dir", home), None);
        assert_eq!(config_dir_value("~other/.claude", home), None);
    }
}
