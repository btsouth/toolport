//! Machine-local secret reads. Configurations carry locations, never commands or values.
use crate::registry::{EnvVar, ServerEntry};
use serde::Serialize;
use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(120);
const OUTPUT_LIMIT: u64 = 64 * 1024;

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Provider {
    pub scheme: &'static str,
    pub name: &'static str,
    pub binary: &'static str,
    pub example: &'static str,
    pub docs: &'static str,
    pub sign_in: &'static str,
}

// Commands are constructed below from this closed table, never from configuration.
pub const PROVIDERS: &[Provider] = &[
    Provider { scheme: "op://", name: "1Password", binary: "op", example: "op://Engineering/Docs/key", docs: "https://developer.1password.com/docs/cli/reference/commands/read/", sign_in: "Sign in to 1Password CLI or unlock the desktop app with CLI integration enabled." },
    Provider { scheme: "doppler://", name: "Doppler", binary: "doppler", example: "doppler://docs/prod/TOKEN", docs: "https://docs.doppler.com/docs/cli", sign_in: "Run doppler login on this machine." },
    Provider { scheme: "infisical://", name: "Infisical", binary: "infisical", example: "infisical://docs/prod/services/TOKEN", docs: "https://infisical.com/docs/cli/reference#secrets-get", sign_in: "Run infisical login on this machine. The project component is the project ID." },
    Provider { scheme: "vault://", name: "HashiCorp Vault", binary: "vault", example: "vault://secret/docs#token", docs: "https://developer.hashicorp.com/vault/docs/commands/kv/get", sign_in: "Run vault login and configure VAULT_ADDR locally." },
    Provider { scheme: "bws://", name: "Bitwarden Secrets Manager", binary: "bws", example: "bws://be8e0ad8-d545-4017-a55a-b02f014d4158", docs: "https://bitwarden.com/help/secrets-manager-cli/", sign_in: "Configure BWS_ACCESS_TOKEN in the local Toolport process environment." },
    Provider { scheme: "bw://", name: "Bitwarden Password Manager", binary: "bw", example: "bw://be8e0ad8-d545-4017-a55a-b02f014d4158/password", docs: "https://bitwarden.com/help/cli/", sign_in: "Run bw login and bw unlock, then start Toolport with BW_SESSION in its environment." },
    Provider { scheme: "keeper://", name: "Keeper Secrets Manager", binary: "ksm", example: "keeper://8f8I-OqPV58o2r91wVgZ_A/field/password", docs: "https://docs.keeper.io/keeperpam/secrets-manager/secrets-manager-command-line-interface/secret-command", sign_in: "Initialize a local Keeper Secrets Manager CLI profile with ksm profile init." },
    Provider { scheme: "dl://", name: "Dashlane", binary: "dcli", example: "dl://QD145B53-B987-4CFE-9408-F25803DC47A4/password", docs: "https://cli.dashlane.com/personal/secrets/read", sign_in: "Sign in locally with dcli and unlock your Dashlane vault." },
    Provider { scheme: "lpass://", name: "LastPass", binary: "lpass", example: "lpass://123456789/password", docs: "https://lastpass.github.io/lastpass-cli/lpass.1.html", sign_in: "Run lpass login on this machine and unlock its agent." },
    Provider { scheme: "env:", name: "Environment variable", binary: "", example: "env:API_TOKEN", docs: "https://doc.rust-lang.org/std/env/fn.var.html", sign_in: "Set the named environment variable before starting Toolport." },
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorState {
    InvalidReference,
    PolicyDenied,
    NotInstalled,
    Locked,
    NotFound,
    Timeout,
    InvalidOutput,
    Failed,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveError {
    pub state: ErrorState,
    pub message: String,
}
impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for ResolveError {}
fn error(provider: Option<&Provider>, state: ErrorState) -> ResolveError {
    let name = provider.map_or("Secret reference", |p| p.name);
    let detail = match state {
        ErrorState::InvalidReference => {
            "Use a supported reference with no whitespace, controls or option-like path components."
                .into()
        }
        ErrorState::PolicyDenied => {
            "This reference is not allowed by your team's secret source policy.".into()
        }
        ErrorState::NotInstalled => format!(
            "Install the official {} CLI on this machine.",
            provider.map_or("provider", |p| p.binary)
        ),
        ErrorState::Locked => provider
            .map_or("Sign in or unlock the provider locally.", |p| p.sign_in)
            .into(),
        ErrorState::NotFound => {
            "Reference not found. Check the entry, field and your access to it.".into()
        }
        ErrorState::Timeout => {
            "The read timed out. Finish unlocking locally, then test or restart the server.".into()
        }
        ErrorState::InvalidOutput => {
            "The CLI did not return one nonempty API key. Check the field and CLI version.".into()
        }
        ErrorState::Failed => {
            "The CLI read failed. Check the provider locally, then test again.".into()
        }
    };
    ResolveError {
        state,
        message: format!("{name}: {detail}"),
    }
}
fn safe(s: &str) -> bool {
    !s.is_empty()
        && s.chars().count() <= 512
        && !s.chars().any(|c| c.is_whitespace() || c.is_control())
}
fn name(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}
fn path(s: &str) -> bool {
    !s.contains(['?', '#', '@', '\\', '%', ':'])
        && s.split('/').all(|p| {
            !p.is_empty()
                && !p.starts_with('-')
                && p != "."
                && p != ".."
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_-.{}[]".contains(c))
        })
}
fn uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == b'-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}
pub fn parse(reference: &str) -> Result<&'static Provider, ResolveError> {
    let p = PROVIDERS
        .iter()
        .find(|p| reference.starts_with(p.scheme))
        .ok_or_else(|| error(None, ErrorState::InvalidReference))?;
    let r = &reference[p.scheme.len()..];
    let parts: Vec<_> = r.split('/').collect();
    let valid = safe(reference)
        && match p.scheme {
            "env:" => name(r),
            "vault://" => r.split_once('#').is_some_and(|(loc, field)| {
                path(loc) && loc.split('/').count() >= 2 && path(field) && !field.contains('/')
            }),
            "op://" => path(r) && (3..=4).contains(&parts.len()),
            "doppler://" => path(r) && parts.len() == 3,
            "infisical://" => path(r) && parts.len() >= 4,
            "bws://" => uuid(r),
            "bw://" => parts.len() == 2 && uuid(parts[0]) && parts[1] == "password",
            "keeper://" => {
                path(r)
                    && parts.len() == 3
                    && ["field", "custom_field"].contains(&parts[1])
                    && parts[0].len() == 22
            }
            "dl://" => path(r) && parts.len() == 2,
            "lpass://" => {
                parts.len() == 2
                    && parts[0].bytes().all(|c| c.is_ascii_digit())
                    && !parts[0].is_empty()
                    && parts[1] == "password"
            }
            _ => false,
        };
    if valid {
        Ok(p)
    } else {
        Err(error(Some(p), ErrorState::InvalidReference))
    }
}

/// Read only the closed {ref} model, rejecting executable or inline credential fields.
pub fn source(fields: &serde_json::Map<String, Value>) -> Result<Option<&str>, ResolveError> {
    let Some(value) = fields.get("source") else {
        return Ok(None);
    };
    let obj = value
        .as_object()
        .filter(|v| v.len() == 1)
        .ok_or_else(|| error(None, ErrorState::InvalidReference))?;
    let reference = obj
        .get("ref")
        .and_then(Value::as_str)
        .ok_or_else(|| error(None, ErrorState::InvalidReference))?;
    parse(reference)?;
    Ok(Some(reference))
}
#[derive(serde::Deserialize, serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HeaderKey {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Reference>,
}
#[derive(serde::Deserialize, serde::Serialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct Reference {
    pub r#ref: String,
}
pub fn headers(server: &ServerEntry) -> Result<Vec<HeaderKey>, ResolveError> {
    let result: Vec<HeaderKey> = match server.unknown_fields.get("headerKeys") {
        Some(v) => serde_json::from_value(v.clone())
            .map_err(|_| error(None, ErrorState::InvalidReference))?,
        None => vec![],
    };
    for h in &result {
        if !name(&h.key.replace('-', "_")) || h.env.as_deref().is_some_and(|e| !name(e)) {
            return Err(error(None, ErrorState::InvalidReference));
        }
        if let Some(r) = &h.source {
            parse(&r.r#ref)?;
        }
    }
    Ok(result)
}
pub fn check_policy(server: &ServerEntry, reference: &str) -> Result<(), ResolveError> {
    parse(reference)?;
    if let Some(policy) = server.unknown_fields.get("secretSources") {
        let obj = policy
            .as_object()
            .ok_or_else(|| error(None, ErrorState::PolicyDenied))?;
        if let Some(prefixes) = obj.get("allowedPrefixes") {
            let prefixes = prefixes
                .as_array()
                .ok_or_else(|| error(None, ErrorState::PolicyDenied))?;
            if !prefixes.iter().all(|v| {
                v.as_str()
                    .is_some_and(|s| safe(s) && PROVIDERS.iter().any(|p| s.starts_with(p.scheme)))
            }) || !prefixes
                .iter()
                .any(|v| v.as_str().is_some_and(|p| reference.starts_with(p)))
            {
                return Err(error(None, ErrorState::PolicyDenied));
            }
        }
    }
    Ok(())
}
pub fn validate_server(server: &ServerEntry) -> Result<(), ResolveError> {
    for e in &server.env {
        if let Some(r) = source(&e.unknown_fields)? {
            if !e.secret || e.value.is_some() || !name(&e.key) {
                return Err(error(None, ErrorState::InvalidReference));
            }
            check_policy(server, r)?;
        }
    }
    for i in server.launch.iter().flat_map(|l| &l.inputs) {
        if let Some(r) = source(&i.unknown_fields)? {
            if !i.secret || i.value.is_some() {
                return Err(error(None, ErrorState::InvalidReference));
            }
            check_policy(server, r)?;
        }
    }
    for h in headers(server)? {
        if let Some(r) = h.source {
            check_policy(server, &r.r#ref)?;
        }
    }
    Ok(())
}

fn arguments(p: &Provider, reference: &str) -> Vec<String> {
    let r = &reference[p.scheme.len()..];
    let parts: Vec<_> = r.split('/').collect();
    let args: Vec<String> = match p.scheme {
        "op://" => vec!["read".into(), "--no-newline".into(), reference.into()],
        "doppler://" => vec![
            "secrets".into(),
            "get".into(),
            parts[2].into(),
            "--plain".into(),
            "--project".into(),
            parts[0].into(),
            "--config".into(),
            parts[1].into(),
        ],
        "infisical://" => vec![
            "secrets".into(),
            "get".into(),
            parts.last().unwrap().to_string(),
            "--plain".into(),
            "--silent".into(),
            "--telemetry=false".into(),
            "--projectId".into(),
            parts[0].into(),
            "--env".into(),
            parts[1].into(),
            "--path".into(),
            format!("/{}", parts[2..parts.len() - 1].join("/")),
        ],
        "vault://" => {
            let (location, field) = r.split_once('#').unwrap();
            vec![
                "kv".into(),
                "get".into(),
                format!("-field={field}"),
                location.into(),
            ]
        }
        "bws://" => vec![
            "secret".into(),
            "get".into(),
            r.into(),
            "--output".into(),
            "json".into(),
        ],
        "bw://" => vec!["get".into(), "password".into(), parts[0].into()],
        "keeper://" => vec!["secret".into(), "notation".into(), reference.into()],
        "dl://" => vec!["read".into(), reference.into()],
        "lpass://" => vec![
            "show".into(),
            "--password".into(),
            "--color=never".into(),
            parts[0].into(),
        ],
        _ => vec![],
    };
    args
}

fn cli_in(p: &Provider, dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter().filter(|d| d.is_absolute()).find_map(|dir| {
        let filename = if cfg!(windows) {
            format!("{}.exe", p.binary)
        } else {
            p.binary.into()
        };
        let candidate = dir.join(filename);
        if !candidate.is_file() {
            return None;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if candidate.metadata().ok()?.permissions().mode() & 0o111 == 0 {
                return None;
            }
        }
        Some(candidate)
    })
}
fn install_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<_> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if cfg!(windows) {
        if let Some(home) = std::env::var_os("USERPROFILE") {
            let home = PathBuf::from(home);
            for binary in PROVIDERS.iter().filter(|p| !p.binary.is_empty()) {
                dirs.push(home.join("scoop/apps").join(binary.binary).join("current"));
            }
        }
        for key in ["LOCALAPPDATA", "ProgramFiles"] {
            if let Some(base) = std::env::var_os(key) {
                let base = PathBuf::from(base);
                dirs.push(base.join("Microsoft/WinGet/Links"));
                for folder in [
                    "1Password CLI",
                    "Bitwarden CLI",
                    "Doppler",
                    "Infisical",
                    "Vault",
                    "Dashlane CLI",
                ] {
                    dirs.push(base.join(folder));
                }
            }
        }
    } else {
        dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"].map(PathBuf::from));
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            dirs.extend([
                home.join(".local/bin"),
                home.join("bin"),
                home.join(".cargo/bin"),
            ]);
        }
    }
    dirs
}
fn capture(mut pipe: impl Read + Send + 'static) -> std::sync::mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.by_ref().take(OUTPUT_LIMIT + 1).read_to_end(&mut bytes);
        let _ = tx.send(bytes);
    });
    rx
}
fn stop(child: &mut std::process::Child) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}
fn read_cli(
    p: &Provider,
    reference: &str,
    binary: &Path,
    timeout: Duration,
) -> Result<String, ResolveError> {
    let mut cmd = Command::new(binary);
    cmd.args(arguments(p, reference))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    let mut child = cmd
        .spawn()
        .map_err(|_| error(Some(p), ErrorState::NotInstalled))?;
    let out = capture(child.stdout.take().unwrap());
    let err = capture(child.stderr.take().unwrap());
    let deadline = Instant::now() + timeout;
    let status = loop {
        if Instant::now() >= deadline {
            stop(&mut child);
            return Err(error(Some(p), ErrorState::Timeout));
        }
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => {
                stop(&mut child);
                return Err(error(Some(p), ErrorState::Failed));
            }
        }
    };
    let receive = |rx: std::sync::mpsc::Receiver<Vec<u8>>| {
        rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
    };
    let (Ok(out), Ok(err)) = (receive(out), receive(err)) else {
        stop(&mut child);
        return Err(error(Some(p), ErrorState::Timeout));
    };
    if out.len() as u64 > OUTPUT_LIMIT || err.len() as u64 > OUTPUT_LIMIT {
        return Err(error(Some(p), ErrorState::InvalidOutput));
    }
    if !status.success() {
        // Inspect in memory only. Never interpolate stdout, stderr, refs or OS errors.
        let lower = String::from_utf8_lossy(&err).to_ascii_lowercase();
        let state = if [
            "not found",
            "no secret",
            "does not exist",
            "could not find",
            "no record",
        ]
        .iter()
        .any(|s| lower.contains(s))
        {
            ErrorState::NotFound
        } else if [
            "sign in",
            "signin",
            "log in",
            "login",
            "locked",
            "session",
            "unauthorized",
            "authentication",
            "permission denied",
            "access token",
            "403",
            "401",
        ]
        .iter()
        .any(|s| lower.contains(s))
        {
            ErrorState::Locked
        } else {
            ErrorState::Failed
        };
        return Err(error(Some(p), state));
    }
    let output = String::from_utf8(out).map_err(|_| error(Some(p), ErrorState::InvalidOutput))?;
    let value = if p.scheme == "bws://" {
        let data: Value =
            serde_json::from_str(&output).map_err(|_| error(Some(p), ErrorState::InvalidOutput))?;
        if data.get("id").and_then(Value::as_str) != Some(&reference[p.scheme.len()..]) {
            return Err(error(Some(p), ErrorState::InvalidOutput));
        }
        data.get("value")
            .and_then(Value::as_str)
            .ok_or_else(|| error(Some(p), ErrorState::InvalidOutput))?
            .to_string()
    } else {
        output
            .strip_suffix("\r\n")
            .or_else(|| output.strip_suffix('\n'))
            .unwrap_or(&output)
            .into()
    };
    if value == "*not found*" {
        return Err(error(Some(p), ErrorState::NotFound));
    }
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(error(Some(p), ErrorState::InvalidOutput));
    }
    Ok(value)
}
pub fn resolve(reference: &str) -> Result<String, ResolveError> {
    let p = parse(reference)?;
    if p.scheme == "env:" {
        return std::env::var(&reference[4..])
            .ok()
            .filter(|v| !v.is_empty() && !v.chars().any(char::is_control))
            .ok_or_else(|| error(Some(p), ErrorState::NotFound));
    }
    if p.scheme == "bw://"
        && std::env::var("BW_SESSION")
            .ok()
            .is_none_or(|v| v.is_empty())
    {
        return Err(error(Some(p), ErrorState::Locked));
    }
    let binary =
        cli_in(p, &install_dirs()).ok_or_else(|| error(Some(p), ErrorState::NotInstalled))?;
    read_cli(p, reference, &binary, TIMEOUT)
}

/// A transient clone consumed only by a connection. Never pass it to registry/sync writers.
pub fn resolve_server(server: &ServerEntry) -> Result<ServerEntry, ResolveError> {
    validate_server(server)?;
    let mut resolved = server.clone();
    for e in &mut resolved.env {
        if let Some(r) = source(&e.unknown_fields)? {
            e.value = Some(resolve(r)?);
        }
    }
    for i in resolved.launch.iter_mut().flat_map(|l| &mut l.inputs) {
        if let Some(r) = source(&i.unknown_fields)? {
            i.value = Some(resolve(r)?);
        }
    }
    Ok(resolved)
}
pub fn has_references(server: &ServerEntry) -> bool {
    server
        .env
        .iter()
        .any(|e| e.unknown_fields.contains_key("source"))
        || server
            .launch
            .iter()
            .flat_map(|l| &l.inputs)
            .any(|i| i.unknown_fields.contains_key("source"))
        || headers(server).is_ok_and(|h| h.iter().any(|h| h.source.is_some()))
}

pub fn reference_for(entry: &EnvVar) -> Option<&str> {
    source(&entry.unknown_fields).ok().flatten()
}

/// Header values live only in the transport and its redactor.
pub fn resolve_headers(server: &ServerEntry) -> Result<Vec<(String, String)>, ResolveError> {
    headers(server)?
        .into_iter()
        .map(|h| {
            let value = if let Some(r) = h.source {
                check_policy(server, &r.r#ref)?;
                resolve(&r.r#ref)?
            } else {
                crate::secrets::get_secret_result(&server.id, h.env.as_deref().unwrap_or(&h.key))
                    .map_err(|_| error(None, ErrorState::Locked))?
                    .ok_or_else(|| error(None, ErrorState::NotFound))?
            };
            Ok((h.key, value))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "toolport-ref-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        #[cfg(unix)]
        fn fake(&self, name: &str, body: &str) -> PathBuf {
            use std::os::unix::fs::PermissionsExt;
            let path = self.0.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            path
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn closed_provider_table_and_frontend_match() {
        let ts = include_str!("../../src/lib/secretRefs.ts");
        let mut seen = std::collections::HashSet::new();
        for p in PROVIDERS {
            assert!(seen.insert(p.scheme));
            assert!(p.docs.starts_with("https://"));
            assert!(ts.contains(p.scheme) && ts.contains(p.example));
            assert_eq!(parse(p.example).unwrap().name, p.name);
            assert!(!p.sign_in.is_empty());
        }
        assert_eq!(PROVIDERS.len(), 10);
    }
    #[test]
    fn rejects_executable_flags_traversal_controls_and_credential_payloads() {
        for r in [
            "exec:op read",
            "op://-out/file/key",
            "op://v/../key",
            "op://v/i/key?out-file=/tmp/leak",
            "op://v/i/key\n",
            "env:-TOKEN",
            "env:TO KEN",
            "vault://secret/--file#token",
            "bw://name/password",
            "lpass://.*//password",
            "dl://id/password?json=$.password",
            "keeper://8f8I-OqPV58o2r91wVgZ_A/file/secret",
            "env:TOKEN\0",
        ] {
            assert_eq!(
                parse(r).unwrap_err().state,
                ErrorState::InvalidReference,
                "{r}"
            );
        }
        for v in [
            serde_json::json!({"ref":"env:TOKEN", "command":"evil"}),
            serde_json::json!({"ref":"env:TOKEN", "env":{"PATH":"evil"}}),
            serde_json::json!("op://v/i/key"),
        ] {
            assert!(source(&[("source".into(), v)].into_iter().collect()).is_err());
        }
    }
    #[test]
    fn fixed_commands_use_arguments_without_shells() {
        let cases = [
            (
                "op://Engineering/Docs/key",
                vec!["read", "--no-newline", "op://Engineering/Docs/key"],
            ),
            (
                "doppler://docs/prod/TOKEN",
                vec![
                    "secrets",
                    "get",
                    "TOKEN",
                    "--plain",
                    "--project",
                    "docs",
                    "--config",
                    "prod",
                ],
            ),
            (
                "vault://secret/docs#token",
                vec!["kv", "get", "-field=token", "secret/docs"],
            ),
            (
                "keeper://8f8I-OqPV58o2r91wVgZ_A/field/password",
                vec![
                    "secret",
                    "notation",
                    "keeper://8f8I-OqPV58o2r91wVgZ_A/field/password",
                ],
            ),
            (
                "dl://QD145B53-B987-4CFE-9408-F25803DC47A4/password",
                vec!["read", "dl://QD145B53-B987-4CFE-9408-F25803DC47A4/password"],
            ),
            (
                "lpass://123456789/password",
                vec!["show", "--password", "--color=never", "123456789"],
            ),
        ];
        for (r, args) in cases {
            assert_eq!(arguments(parse(r).unwrap(), r), args);
        }
        assert_eq!(
            arguments(parse(PROVIDERS[2].example).unwrap(), PROVIDERS[2].example),
            [
                "secrets",
                "get",
                "TOKEN",
                "--plain",
                "--silent",
                "--telemetry=false",
                "--projectId",
                "docs",
                "--env",
                "prod",
                "--path",
                "/services"
            ]
        );
    }
    fn server() -> ServerEntry {
        serde_json::from_value(serde_json::json!({"id":"refs", "name":"Refs", "transport":"stdio", "command":"mock", "env":[{"key":"TOKEN", "secret":true, "source":{"ref":"op://Engineering/Docs/key"}}]})).unwrap()
    }
    #[test]
    fn policy_is_checked_before_resolution_and_empty_allowlist_denies() {
        let mut s = server();
        s.unknown_fields.insert(
            "secretSources".into(),
            serde_json::json!({"allowedPrefixes":["op://Engineering/"]}),
        );
        validate_server(&s).unwrap();
        assert_eq!(
            check_policy(&s, "op://Personal/Docs/key")
                .unwrap_err()
                .state,
            ErrorState::PolicyDenied
        );
        s.unknown_fields.insert(
            "secretSources".into(),
            serde_json::json!({"allowedPrefixes":[]}),
        );
        assert_eq!(
            resolve_server(&s).unwrap_err().state,
            ErrorState::PolicyDenied
        );
        s.unknown_fields.insert(
            "secretSources".into(),
            serde_json::json!({"allowedPrefixes":"op://"}),
        );
        assert!(validate_server(&s).is_err());
    }
    #[test]
    fn refs_are_exclusive_with_inline_values_and_header_commands() {
        let mut s = server();
        s.env[0].value = Some("do-not-persist".into());
        assert!(validate_server(&s).is_err());
        s.env[0].value = None;
        s.env[0].secret = false;
        assert!(validate_server(&s).is_err());
        s.env[0].secret = true;
        s.unknown_fields.insert("headerKeys".into(),serde_json::json!([{"key":"X-Api-Key", "source":{"ref":"op://Engineering/Docs/key","command":"evil"}}]));
        assert!(validate_server(&s).is_err());
    }
    #[test]
    fn registry_and_setup_export_roundtrip_locations_only() {
        let mut reg = crate::registry::Registry::default();
        let mut s = server();
        s.unknown_fields.insert(
            "headerKeys".into(),
            serde_json::json!([{"key":"X-Api-Key","source":{"ref":"op://Engineering/Docs/key"}}]),
        );
        reg.servers.push(s);
        let encoded = serde_json::to_string(&reg).unwrap();
        let roundtrip: crate::registry::Registry = serde_json::from_str(&encoded).unwrap();
        assert_eq!(
            reference_for(&roundtrip.servers[0].env[0]),
            Some("op://Engineering/Docs/key")
        );
        let export = crate::sharing_controller::build_export(&roundtrip, None, None, None);
        assert_eq!(
            export["servers"][0]["env"][0]["source"]["ref"],
            "op://Engineering/Docs/key"
        );
        assert_eq!(
            export["servers"][0]["headerKeys"][0]["source"]["ref"],
            "op://Engineering/Docs/key"
        );
        assert!(export["servers"][0]["env"][0].get("value").is_none());
    }
    #[test]
    fn missing_environment_is_distinct() {
        assert_eq!(
            resolve("env:TOOLPORT_FAKE_MISSING_REF_428976")
                .unwrap_err()
                .state,
            ErrorState::NotFound
        );
    }
    #[cfg(unix)]
    #[test]
    fn fake_path_success_preserves_spaces_and_never_runs_shell_interpolation() {
        let tmp = Scratch::new();
        let p = parse(PROVIDERS[0].example).unwrap();
        let binary = tmp.fake(p.binary, "printf '  synthetic-ref-value  '");
        assert_eq!(
            cli_in(p, std::slice::from_ref(&tmp.0)),
            Some(binary.clone())
        );
        assert_eq!(
            read_cli(p, p.example, &binary, Duration::from_secs(1)).unwrap(),
            "  synthetic-ref-value  "
        );
    }
    #[cfg(unix)]
    #[test]
    fn fake_path_not_installed_and_relative_path_are_rejected() {
        let tmp = Scratch::new();
        let p = parse(PROVIDERS[0].example).unwrap();
        assert!(cli_in(p, std::slice::from_ref(&tmp.0)).is_none());
        assert!(cli_in(p, &[PathBuf::from(".")]).is_none());
        let e = read_cli(p, p.example, &tmp.0.join("missing"), Duration::from_secs(1)).unwrap_err();
        assert_eq!(e.state, ErrorState::NotInstalled);
    }
    #[cfg(unix)]
    #[test]
    fn fake_errors_never_echo_stdout_stderr_or_reference() {
        let tmp = Scratch::new();
        let p = parse(PROVIDERS[0].example).unwrap();
        for (body, state) in [
            (
                "printf 'synthetic-ref-value'; printf 'locked synthetic-ref-value' >&2; exit 1",
                ErrorState::Locked,
            ),
            (
                "printf 'not found synthetic-ref-value' >&2; exit 1",
                ErrorState::NotFound,
            ),
            (
                "printf 'synthetic-ref-value' >&2; exit 1",
                ErrorState::Failed,
            ),
        ] {
            let binary = tmp.fake(p.binary, body);
            let e = read_cli(p, p.example, &binary, Duration::from_secs(1)).unwrap_err();
            assert_eq!(e.state, state);
            let emitted = serde_json::to_string(&e).unwrap();
            assert!(!emitted.contains("synthetic-ref-value") && !emitted.contains(p.example));
        }
    }
    #[cfg(unix)]
    #[test]
    fn fake_timeout_kills_cli_and_its_pipe_holding_child() {
        let tmp = Scratch::new();
        let p = parse(PROVIDERS[0].example).unwrap();
        let binary = tmp.fake(p.binary, "/bin/sleep 10");
        let start = Instant::now();
        let e = read_cli(p, p.example, &binary, Duration::from_millis(50)).unwrap_err();
        assert_eq!(e.state, ErrorState::Timeout);
        assert!(start.elapsed() < Duration::from_secs(1));
    }
    #[cfg(unix)]
    #[test]
    fn fake_output_parsing_for_every_vendor() {
        let tmp = Scratch::new();
        for p in PROVIDERS.iter().filter(|p| p.scheme != "env:") {
            let body = if p.scheme == "bws://" {
                format!(
                    "printf '%s' '{{\"id\":\"{}\",\"value\":\"synthetic-ref-value\"}}'",
                    &p.example[p.scheme.len()..]
                )
            } else {
                "printf 'synthetic-ref-value\\n'".into()
            };
            let binary = tmp.fake(p.binary, &body);
            assert_eq!(
                read_cli(p, p.example, &binary, Duration::from_secs(1)).unwrap(),
                "synthetic-ref-value",
                "{}",
                p.name
            );
        }
    }
    #[cfg(unix)]
    #[test]
    fn fake_output_rejects_multiple_values_invalid_json_missing_and_oversize() {
        let tmp = Scratch::new();
        let p = parse(PROVIDERS[0].example).unwrap();
        for body in [
            "printf 'one\\ntwo\\n'",
            "printf ''",
            "/usr/bin/head -c 70000 /dev/zero",
        ] {
            let binary = tmp.fake(p.binary, body);
            assert_eq!(
                read_cli(p, p.example, &binary, Duration::from_secs(1))
                    .unwrap_err()
                    .state,
                ErrorState::InvalidOutput
            );
        }
        let p = &PROVIDERS[2];
        let binary = tmp.fake(p.binary, "printf '*not found*\\n'");
        assert_eq!(
            read_cli(p, p.example, &binary, Duration::from_secs(1))
                .unwrap_err()
                .state,
            ErrorState::NotFound
        );
        let p = &PROVIDERS[4];
        for body in [
            "printf '{broken'",
            "printf '{}'",
            "printf '{\"value\":\"bad\",\"id\":\"wrong\"}'",
        ] {
            let binary = tmp.fake(p.binary, body);
            assert_eq!(
                read_cli(p, p.example, &binary, Duration::from_secs(1))
                    .unwrap_err()
                    .state,
                ErrorState::InvalidOutput
            );
        }
    }
}
