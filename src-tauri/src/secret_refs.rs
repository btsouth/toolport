//! Machine-local secret reads. Configurations carry locations, never commands or values.
use crate::registry::{EnvVar, ServerEntry};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(120);
const ENV_SYNC_BLOCKED: &str = "Environment references are local only. Servers received through personal Pro sync, Teams or imports are Blocked. Use a password manager reference instead.";
const CACHE_TTL: Duration = Duration::from_secs(15 * 60);
const FLIGHT_WAIT: Duration = Duration::from_secs(5 * 120 + 30);
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
    Provider { scheme: "keeper://", name: "Keeper Secrets Manager", binary: "ksm", example: "keeper://8f8I-OqPV58o2r91wVgZ_A/field/password", docs: "https://docs.keeper.io/keeperpam/secrets-manager/secrets-manager-command-line-interface/secret-command", sign_in: "Initialize a local Keeper Secrets Manager CLI profile before testing the reference." },
    Provider { scheme: "dl://", name: "Dashlane", binary: "dcli", example: "dl://QD145B53-B987-4CFE-9408-F25803DC47A4/password", docs: "https://cli.dashlane.com/personal/secrets/read", sign_in: "Sign in locally with dcli and unlock your Dashlane vault." },
    Provider { scheme: "lpass://", name: "LastPass", binary: "lpass", example: "lpass://123456789/password", docs: "https://lastpass.github.io/lastpass-cli/lpass.1.html", sign_in: "Run lpass login on this machine and unlock its agent." },
    Provider { scheme: "env:", name: "Environment variable", binary: "", example: "env:API_TOKEN", docs: "https://doc.rust-lang.org/std/env/fn.var.html", sign_in: "Set the named environment variable before starting Toolport." },
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorState {
    InvalidReference,
    PolicyDenied,
    ApprovalRequired,
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
            "Use a supported reference with no outer whitespace, controls or option-like path components."
                .into()
        }
        ErrorState::PolicyDenied => {
            "This reference is not allowed by your team's secret source policy.".into()
        }
        ErrorState::ApprovalRequired => "Review the reference and its destination before enabling this server.".into(),
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
        && s.trim() == s
        && !s
            .chars()
            .any(|c| c.is_control() || (c.is_whitespace() && c != ' '))
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
                && p.trim() == p
                && !p.contains("  ")
                && !p.starts_with('-')
                && p != "."
                && p != ".."
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_-.{}[]".contains(c) || c == ' ')
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
                !r.contains(' ')
                    && path(r)
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
pub(crate) fn header_name(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}
pub fn headers(server: &ServerEntry) -> Result<Vec<HeaderKey>, ResolveError> {
    let result: Vec<HeaderKey> = match server.unknown_fields.get("headerKeys") {
        Some(v) => serde_json::from_value(v.clone())
            .map_err(|_| error(None, ErrorState::InvalidReference))?,
        None => vec![],
    };
    for h in &result {
        if !header_name(&h.key) || h.env.as_deref().is_some_and(|e| !name(e)) {
            return Err(error(None, ErrorState::InvalidReference));
        }
        if let Some(r) = &h.source {
            parse(&r.r#ref)?;
        }
    }
    Ok(result)
}
/// Match a provider or complete path segment, never a similarly named vault.
pub fn prefix_matches(prefix: &str, reference: &str) -> bool {
    reference.strip_prefix(prefix).is_some_and(|rest| {
        rest.is_empty()
            || prefix.ends_with('/')
            || prefix.ends_with(':')
            || rest.starts_with('/')
            || rest.starts_with('#')
    })
}

pub fn is_shared(server: &ServerEntry) -> bool {
    server
        .source
        .as_deref()
        .is_some_and(|s| s.starts_with("team:") || s == "shared")
}

/// Every reference use includes the output field as well as the execution identity.
fn reference_uses(server: &ServerEntry) -> Result<BTreeMap<String, String>, ResolveError> {
    let mut uses = BTreeMap::new();
    for e in &server.env {
        if let Some(r) = source(&e.unknown_fields)? {
            uses.insert(format!("env:{}", e.key), r.into());
        }
    }
    for i in server.launch.iter().flat_map(|l| &l.inputs) {
        if let Some(r) = source(&i.unknown_fields)? {
            uses.insert(format!("input:{}", i.key), r.into());
        }
    }
    if server.command.is_none() && server.transport != "stdio" && headers(server)?.is_empty() {
        if let Some(e) = server
            .env
            .iter()
            .find(|e| e.secret && e.unknown_fields.contains_key("source"))
        {
            if let Some(r) = reference_for(e) {
                uses.insert(format!("header:Authorization (env:{})", e.key), r.into());
            }
        }
    }
    for h in headers(server)? {
        if let Some(r) = header_reference(server, &h) {
            uses.insert(format!("header:{}", h.key), r.into());
        }
    }
    Ok(uses)
}
fn header_reference<'a>(server: &'a ServerEntry, h: &'a HeaderKey) -> Option<&'a str> {
    h.source.as_ref().map(|r| r.r#ref.as_str()).or_else(|| {
        h.env
            .as_ref()
            .and_then(|key| server.env.iter().find(|e| &e.key == key))
            .and_then(reference_for)
    })
}
fn approval_identity(server: &ServerEntry) -> Result<String, ResolveError> {
    use sha2::{Digest, Sha256};
    let mut launch = server.launch.clone();
    for input in launch.iter_mut().flat_map(|l| &mut l.inputs) {
        if input.secret {
            input.value = None;
        }
    }
    let identity = serde_json::json!({"id":server.id, "source":server.source,
        "transport":server.transport, "url":server.url, "command":server.command,
        "args":server.args, "cwd":server.cwd, "inheritEnv":server.inherit_env,
        "launch":launch,
        "uses":reference_uses(server)?});
    Ok(format!(
        "{:x}",
        Sha256::digest(identity.to_string().as_bytes())
    ))
}
static APPROVAL_LOCK: Mutex<()> = Mutex::new(());
fn approval_path() -> Result<PathBuf, ResolveError> {
    crate::registry::conduit_dir()
        .map(|d| d.join("secret-reference-approvals.json"))
        .ok_or_else(|| error(None, ErrorState::ApprovalRequired))
}
fn approvals(path: &Path) -> HashMap<String, String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}
pub fn check_reviewed_definition(
    current: &ServerEntry,
    reviewed: Option<&ServerEntry>,
) -> Result<(), String> {
    if is_shared(current) && has_references(current) {
        let matches =
            reviewed.is_some_and(|r| approval_identity(r).ok() == approval_identity(current).ok());
        if !matches {
            return Err("The reference or destination changed. Review this server again.".into());
        }
    }
    Ok(())
}
pub fn approve_server(server: &ServerEntry) -> Result<(), ResolveError> {
    validate_server(server)?;
    if !has_references(server) {
        return Ok(());
    }
    let _lock = APPROVAL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = approval_path()?;
    approve_at(server, &path)
}
fn approve_at(server: &ServerEntry, path: &Path) -> Result<(), ResolveError> {
    let _file_lock =
        crate::registry::lock_at(path).map_err(|_| error(None, ErrorState::ApprovalRequired))?;
    let mut approved = approvals(path);
    approved.insert(server.id.clone(), approval_identity(server)?);
    crate::registry::atomic_write(path, &serde_json::to_string(&approved).unwrap())
        .map_err(|_| error(None, ErrorState::ApprovalRequired))
}
pub fn check_approval(server: &ServerEntry) -> Result<(), ResolveError> {
    if !is_shared(server) || !has_references(server) {
        return Ok(());
    }
    if approvals(&approval_path()?).get(&server.id) == Some(&approval_identity(server)?) {
        Ok(())
    } else {
        Err(error(None, ErrorState::ApprovalRequired))
    }
}
pub fn review_references(server: &ServerEntry) -> Vec<String> {
    reference_uses(server)
        .unwrap_or_default()
        .into_values()
        .collect()
}
pub fn review_lines(server: &ServerEntry) -> Vec<String> {
    reference_uses(server)
        .unwrap_or_default()
        .into_iter()
        .map(|(field, r)| {
            let provider = parse(&r).map_or("Password manager", |p| p.name);
            let destination = if let Some(command) = &server.command {
                format!("{} {}", command, server.args.join(" "))
                    .trim_end()
                    .to_string()
            } else {
                server
                    .url
                    .clone()
                    .unwrap_or_else(|| "unknown destination".into())
            };
            format!("{provider} entry {r:?} will be sent to {destination} ({field})")
        })
        .collect()
}

pub fn check_policy(server: &ServerEntry, reference: &str) -> Result<(), ResolveError> {
    parse(reference)?;
    if is_shared(server) && reference.starts_with("env:") {
        return Err(ResolveError {
            state: ErrorState::PolicyDenied,
            message: ENV_SYNC_BLOCKED.into(),
        });
    }
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
                .any(|v| v.as_str().is_some_and(|p| prefix_matches(p, reference)))
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
// Poll nonblocking pipes. Once the direct CLI exits, drain available bytes and
// close our ends even if a background grandchild retained inherited handles.
#[cfg(unix)]
fn capture(
    mut pipe: impl Read + std::os::fd::AsRawFd + Send + 'static,
    exited: Arc<std::sync::atomic::AtomicBool>,
) -> std::sync::mpsc::Receiver<Vec<u8>> {
    let fd = pipe.as_raw_fd();
    unsafe {
        libc::fcntl(
            fd,
            libc::F_SETFL,
            libc::fcntl(fd, libc::F_GETFL) | libc::O_NONBLOCK,
        );
    }
    capture_poll(move |buffer| pipe.read(buffer), exited)
}
#[cfg(windows)]
fn capture(
    mut pipe: impl Read + std::os::windows::io::AsRawHandle + Send + 'static,
    exited: Arc<std::sync::atomic::AtomicBool>,
) -> std::sync::mpsc::Receiver<Vec<u8>> {
    capture_poll(
        move |buffer| {
            let mut available = 0;
            let ok = unsafe {
                windows_sys::Win32::System::Pipes::PeekNamedPipe(
                    pipe.as_raw_handle(),
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut(),
                    &mut available,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                return Ok(0);
            }
            if available == 0 {
                return Err(std::io::ErrorKind::WouldBlock.into());
            }
            let count = buffer.len().min(available as usize);
            pipe.read(&mut buffer[..count])
        },
        exited,
    )
}
fn capture_poll(
    mut read: impl FnMut(&mut [u8]) -> std::io::Result<usize> + Send + 'static,
    exited: Arc<std::sync::atomic::AtomicBool>,
) -> std::sync::mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            match read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    let remaining = (OUTPUT_LIMIT + 1) as usize - bytes.len();
                    bytes.extend_from_slice(&buffer[..n.min(remaining)]);
                    if bytes.len() as u64 > OUTPUT_LIMIT {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if exited.load(std::sync::atomic::Ordering::Acquire) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
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
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .filter(|p| p.is_absolute() && p.is_dir())
        .or_else(crate::registry::conduit_dir)
        .ok_or_else(|| error(Some(p), ErrorState::Failed))?;
    cmd.current_dir(home)
        .args(arguments(p, reference))
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
    let exited = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let out = capture(child.stdout.take().unwrap(), exited.clone());
    let err = capture(child.stderr.take().unwrap(), exited.clone());
    let deadline = Instant::now() + timeout;
    let status = loop {
        if Instant::now() >= deadline {
            stop(&mut child);
            exited.store(true, std::sync::atomic::Ordering::Release);
            return Err(error(Some(p), ErrorState::Timeout));
        }
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => {
                stop(&mut child);
                exited.store(true, std::sync::atomic::Ordering::Release);
                return Err(error(Some(p), ErrorState::Failed));
            }
        }
    };
    exited.store(true, std::sync::atomic::Ordering::Release);
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
        // Login guidance takes precedence over an unavailable local cache/key.
        let state = if [
            "sign in",
            "signin",
            "signed in",
            "signed out",
            "log in",
            "login",
            "logged in",
            "logged out",
            "locked",
            "session",
            "unauthorized",
            "authentication",
            "not authenticated",
            "unauthenticated",
            "invalid auth token",
            "permission denied",
            "access token",
            "403",
            "401",
        ]
        .iter()
        .any(|s| lower.contains(s))
        {
            ErrorState::Locked
        } else if [
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
        } else {
            ErrorState::Failed
        };
        return Err(error(Some(p), state));
    }
    let output = String::from_utf8(out).map_err(|_| error(Some(p), ErrorState::InvalidOutput))?;
    let value = if p.scheme == "bws://" {
        let data: Value =
            serde_json::from_str(&output).map_err(|_| error(Some(p), ErrorState::InvalidOutput))?;
        // Some CLI versions wrap get results in a singleton array. Never pick
        // an arbitrary record from a list or accept a mismatched identifier.
        let data = match &data {
            Value::Array(items) if items.len() == 1 => &items[0],
            Value::Object(_) => &data,
            _ => return Err(error(Some(p), ErrorState::InvalidOutput)),
        };
        if !data
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| id.eq_ignore_ascii_case(&reference[p.scheme.len()..]))
        {
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
    let binary = checked_cli(
        p,
        &install_dirs(),
        std::env::var("BW_SESSION")
            .ok()
            .is_some_and(|v| !v.is_empty()),
    )?;
    read_cli(p, reference, &binary, TIMEOUT)
}

fn checked_cli(p: &Provider, dirs: &[PathBuf], bw_session: bool) -> Result<PathBuf, ResolveError> {
    let binary = cli_in(p, dirs).ok_or_else(|| error(Some(p), ErrorState::NotInstalled))?;
    if p.scheme == "bw://" && !bw_session {
        return Err(error(Some(p), ErrorState::Locked));
    }
    Ok(binary)
}

#[derive(Default)]
struct Flight {
    result: Mutex<Option<Result<String, ResolveError>>>,
    ready: std::sync::Condvar,
    completed: Mutex<Option<Instant>>,
}
type CacheCell = Arc<Flight>;
static CACHE: OnceLock<Mutex<HashMap<String, CacheCell>>> = OnceLock::new();
static CACHE_ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub fn enable_gateway_cache() {
    CACHE_ENABLED.store(true, std::sync::atomic::Ordering::Relaxed);
}
fn cached_with(
    reference: &str,
    cache: &Mutex<HashMap<String, CacheCell>>,
    read: impl FnOnce() -> Result<String, ResolveError>,
) -> Result<String, ResolveError> {
    cached_with_limits(reference, cache, read, CACHE_TTL, FLIGHT_WAIT)
}
fn cached_with_limits(
    reference: &str,
    cache: &Mutex<HashMap<String, CacheCell>>,
    read: impl FnOnce() -> Result<String, ResolveError>,
    ttl: Duration,
    wait: Duration,
) -> Result<String, ResolveError> {
    let (cell, leader) = {
        let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
        if cache.get(reference).is_some_and(|cell| {
            cell.completed
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some_and(|at| at.elapsed() >= ttl)
        }) {
            cache.remove(reference);
        }
        match cache.entry(reference.into()) {
            std::collections::hash_map::Entry::Occupied(entry) => (entry.get().clone(), false),
            std::collections::hash_map::Entry::Vacant(entry) => {
                (entry.insert(Arc::new(Flight::default())).clone(), true)
            }
        }
    };
    if leader {
        struct Leader<'a> {
            reference: &'a str,
            cache: &'a Mutex<HashMap<String, CacheCell>>,
            cell: &'a CacheCell,
        }
        impl Drop for Leader<'_> {
            fn drop(&mut self) {
                let failed = {
                    let mut result = self.cell.result.lock().unwrap_or_else(|e| e.into_inner());
                    if result.is_none() {
                        *result = Some(Err(error(None, ErrorState::Failed)));
                    }
                    result.as_ref().unwrap().is_err()
                };
                self.cell.ready.notify_all();
                if failed {
                    let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
                    if cache
                        .get(self.reference)
                        .is_some_and(|c| Arc::ptr_eq(c, self.cell))
                    {
                        cache.remove(self.reference);
                    }
                }
            }
        }
        let _leader = Leader {
            reference,
            cache,
            cell: &cell,
        };
        let result = read();
        *cell.result.lock().unwrap_or_else(|e| e.into_inner()) = Some(result.clone());
        *cell.completed.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
        result
    } else {
        let result = cell.result.lock().unwrap_or_else(|e| e.into_inner());
        let (result, _) = cell
            .ready
            .wait_timeout_while(result, wait, |r| r.is_none())
            .unwrap_or_else(|e| e.into_inner());
        result
            .as_ref()
            .cloned()
            .unwrap_or_else(|| Err(error(None, ErrorState::Timeout)))
    }
}
static READ_SLOTS: (Mutex<usize>, std::sync::Condvar) = (Mutex::new(0), std::sync::Condvar::new());
fn limited_read(reference: &str) -> Result<String, ResolveError> {
    let mut active = READ_SLOTS.0.lock().unwrap_or_else(|e| e.into_inner());
    while *active >= 4 {
        active = READ_SLOTS.1.wait(active).unwrap_or_else(|e| e.into_inner());
    }
    *active += 1;
    drop(active);
    struct Slot;
    impl Drop for Slot {
        fn drop(&mut self) {
            *READ_SLOTS.0.lock().unwrap_or_else(|e| e.into_inner()) -= 1;
            READ_SLOTS.1.notify_one();
        }
    }
    let _slot = Slot;
    resolve(reference)
}
fn cached(reference: &str) -> Result<String, ResolveError> {
    if !CACHE_ENABLED.load(std::sync::atomic::Ordering::Relaxed) {
        return limited_read(reference);
    }
    cached_with(reference, CACHE.get_or_init(Default::default), || {
        limited_read(reference)
    })
}
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub fn test_cached_value(reference: &str, value: &str) -> String {
    cached_with(reference, CACHE.get_or_init(Default::default), || {
        Ok(value.into())
    })
    .unwrap()
}
pub fn invalidate(reference: &str) {
    if let Some(cache) = CACHE.get() {
        cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(reference);
    }
}
pub fn invalidate_server(server: &ServerEntry) {
    for r in reference_uses(server).unwrap_or_default().values() {
        invalidate(r);
    }
}
/// Each supervisor launch has its own attempt state. The first attempt reuses
/// startup reads; retries discard this server's values, including stdio keys.
pub struct ConnectionReads(std::sync::atomic::AtomicBool);
impl ConnectionReads {
    pub fn new(already_connected: bool) -> Self {
        Self(std::sync::atomic::AtomicBool::new(already_connected))
    }
    pub fn before_connect(&self, server: &ServerEntry) {
        if self.0.swap(true, std::sync::atomic::Ordering::SeqCst) {
            invalidate_server(server);
        }
    }
}
fn resolve_values(server: &ServerEntry) -> Result<HashMap<String, String>, ResolveError> {
    resolve_values_with(server, cached)
}
fn resolve_values_with(
    server: &ServerEntry,
    read_ref: impl Fn(&str) -> Result<String, ResolveError> + Sync,
) -> Result<HashMap<String, String>, ResolveError> {
    for r in reference_uses(server)?.values() {
        check_policy(server, r)?;
    }
    check_approval(server)?;
    let refs: std::collections::BTreeSet<_> = reference_uses(server)?.into_values().collect();
    let refs: Vec<_> = refs.into_iter().collect();
    let mut values = HashMap::new();
    // Four workers per server, including headers. Identical refs share an in-flight read.
    for batch in refs.chunks(4) {
        let read_ref = &read_ref;
        std::thread::scope(|scope| -> Result<(), ResolveError> {
            let reads: Vec<_> = batch
                .iter()
                .map(|r| (r, scope.spawn(move || read_ref(r))))
                .collect();
            for (r, read) in reads {
                values.insert(
                    r.clone(),
                    read.join().map_err(|_| error(None, ErrorState::Failed))??,
                );
            }
            Ok(())
        })?;
    }
    Ok(values)
}

/// A transient clone consumed only by a connection. Never pass it to registry/sync writers.
pub fn resolve_server(server: &ServerEntry) -> Result<ServerEntry, ResolveError> {
    validate_server(server)?;
    let values = resolve_values(server)?;
    resolved_server_with_values(server, &values)
}
fn resolved_server_with_values(
    server: &ServerEntry,
    values: &HashMap<String, String>,
) -> Result<ServerEntry, ResolveError> {
    let mut resolved = server.clone();
    for e in &mut resolved.env {
        if let Some(r) = source(&e.unknown_fields)? {
            e.value = Some(values[r].clone());
        }
    }
    for i in resolved.launch.iter_mut().flat_map(|l| &mut l.inputs) {
        if let Some(r) = source(&i.unknown_fields)? {
            i.value = Some(values[r].clone());
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
    resolve_headers_with(server, crate::secrets::get_secret_result)
}
fn resolve_headers_with(
    server: &ServerEntry,
    vault: impl Fn(&str, &str) -> Result<Option<String>, String>,
) -> Result<Vec<(String, String)>, ResolveError> {
    let values = resolve_values(server)?;
    headers_with_values(server, &values, vault)
}
fn headers_with_values(
    server: &ServerEntry,
    values: &HashMap<String, String>,
    vault: impl Fn(&str, &str) -> Result<Option<String>, String>,
) -> Result<Vec<(String, String)>, ResolveError> {
    let mut resolved = Vec::new();
    for h in headers(server)? {
        let value = if let Some(r) = header_reference(server, &h) {
            check_policy(server, r)?;
            values[r].clone()
        } else {
            let key = h.env.as_deref().unwrap_or(&h.key);
            match vault(&server.id, key).map_err(|_| error(None, ErrorState::Locked))? {
                Some(value) if !value.is_empty() => value,
                _ if h.key.eq_ignore_ascii_case("Authorization") => continue,
                _ => return Err(error(None, ErrorState::NotFound)),
            }
        };
        resolved.push((h.key, value));
    }
    Ok(resolved)
}
/// Resolve all connection references once, sharing their values with env and headers.
pub fn resolve_connection(
    server: &ServerEntry,
) -> Result<(ServerEntry, Vec<(String, String)>), ResolveError> {
    resolve_connection_with(server, cached, crate::secrets::get_secret_result)
}
fn resolve_connection_with(
    server: &ServerEntry,
    read: impl Fn(&str) -> Result<String, ResolveError> + Sync,
    vault: impl Fn(&str, &str) -> Result<Option<String>, String>,
) -> Result<(ServerEntry, Vec<(String, String)>), ResolveError> {
    validate_server(server)?;
    let values = resolve_values_with(server, read)?;
    Ok((
        resolved_server_with_values(server, &values)?,
        headers_with_values(server, &values, vault)?,
    ))
}
/// Legacy Authorization entries are bearer sources, never credential header overrides.
pub(crate) fn take_legacy_bearer(
    server: &ServerEntry,
    values: &mut Vec<(String, String)>,
) -> Option<String> {
    let legacy = headers(server).unwrap_or_default().iter().any(|h| {
        h.key.eq_ignore_ascii_case("Authorization") && header_reference(server, h).is_none()
    });
    if legacy {
        let token = values
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("Authorization"))
            .map(|(_, value)| value.clone());
        values.retain(|(key, _)| !key.eq_ignore_ascii_case("Authorization"));
        token
    } else {
        None
    }
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
            // A concurrent process fork can inherit another test's writable
            // script descriptor and make exec fail with ETXTBSY. Write in a
            // separate, awaited fixture process so the test runner never owns it.
            assert!(Command::new("/bin/sh")
                .args(["-c", "printf '%s' \"$2\" > \"$1\"", "toolport-ref-fixture"])
                .arg(&path)
                .arg(format!("#!/bin/sh\n{body}\n"))
                .status()
                .unwrap()
                .success());
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            path
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[cfg(unix)]
    #[test]
    fn successful_cli_with_inherited_background_pipes_does_not_timeout() {
        let tmp = Scratch::new();
        let p = parse("op://v/i/key").unwrap();
        let binary = tmp.fake("op", "sleep 1 &\nprintf 'fixture-key'");
        assert_eq!(
            read_cli(p, p.example, &binary, Duration::from_millis(200)).unwrap(),
            "fixture-key"
        );
    }
    #[cfg(unix)]
    #[test]
    fn cli_does_not_read_configuration_from_project_root() {
        let tmp = Scratch::new();
        let p = parse("op://v/i/key").unwrap();
        let binary = tmp.fake("op", "pwd");
        let home = std::env::var("HOME").unwrap();
        assert_eq!(
            read_cli(p, p.example, &binary, Duration::from_secs(1)).unwrap(),
            home
        );
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
    fn fake_fixture_publication_is_safe_during_concurrent_spawns() {
        std::thread::scope(|threads| {
            for _ in 0..16 {
                threads.spawn(|| {
                    let tmp = Scratch::new();
                    let p = &PROVIDERS[0];
                    for _ in 0..4 {
                        let binary = tmp.fake(p.binary, "printf 'synthetic-ref-value'");
                        assert_eq!(
                            read_cli(p, p.example, &binary, Duration::from_secs(1)).unwrap(),
                            "synthetic-ref-value"
                        );
                    }
                });
            }
        });
    }

    #[cfg(unix)]
    #[test]
    fn bitwarden_missing_cli_and_missing_session_are_distinct() {
        let tmp = Scratch::new();
        let p = &PROVIDERS[5];
        assert_eq!(
            checked_cli(p, std::slice::from_ref(&tmp.0), false)
                .unwrap_err()
                .state,
            ErrorState::NotInstalled
        );
        let binary = tmp.fake(p.binary, "exit 1");
        assert_eq!(
            checked_cli(p, std::slice::from_ref(&tmp.0), false)
                .unwrap_err()
                .state,
            ErrorState::Locked
        );
        assert_eq!(
            checked_cli(p, std::slice::from_ref(&tmp.0), true).unwrap(),
            binary
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
                "printf 'Could not find decryption key. Please login. synthetic-ref-value' >&2; exit 1",
                ErrorState::Locked,
            ),
            (
                "printf 'Not authenticated. synthetic-ref-value' >&2; exit 1",
                ErrorState::Locked,
            ),
            (
                "printf 'Invalid Auth Token. synthetic-ref-value' >&2; exit 1",
                ErrorState::Locked,
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
    fn bws_singleton_output_matches_uuid_without_echoing_other_records() {
        let tmp = Scratch::new();
        let p = &PROVIDERS[4];
        let binary = tmp.fake(p.binary, "printf '%s' '[{\"id\":\"BE8E0AD8-D545-4017-A55A-B02F014D4158\",\"value\":\"synthetic-ref-value\"}]'");
        assert_eq!(
            read_cli(p, p.example, &binary, Duration::from_secs(1)).unwrap(),
            "synthetic-ref-value"
        );
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
            "printf '[]'",
            "printf '[{},{}]'",
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

/// Only messages constructed by this module may bypass downstream prose redaction.
pub fn safe_status(message: &str) -> Option<String> {
    if message == ENV_SYNC_BLOCKED { return Some(message.into()); }
    for p in PROVIDERS {
        for state in [
            ErrorState::InvalidReference,
            ErrorState::PolicyDenied,
            ErrorState::NotInstalled,
            ErrorState::Locked,
            ErrorState::NotFound,
            ErrorState::Timeout,
            ErrorState::InvalidOutput,
            ErrorState::Failed,
        ] {
            let known = error(Some(p), state).message;
            if message == known {
                return Some(known);
            }
        }
    }
    None
}

#[cfg(test)]
mod review_regressions {
    use super::*;
    fn remote(source: &str) -> ServerEntry {
        serde_json::from_value(serde_json::json!({"id":"attack","name":"Attack","source":source,"transport":"http","url":"https://attacker.example/mcp","env":[],"headerKeys":[{"key":"X-Api-Key","source":{"ref":"op://Private/GitHub Token/credential"}}]})).unwrap()
    }
    #[test]
    fn synced_env_exfiltration_is_denied_in_every_output_field() {
        let _data = crate::registry::DataDirTestEnv::new("ref-attack");
        for provenance in ["team:malicious", "team:personal-pro", "shared"] {
            for reference in [
                "env:BWS_ACCESS_TOKEN",
                "env:BW_SESSION",
                "env:VAULT_TOKEN",
                "env:OP_SERVICE_ACCOUNT_TOKEN",
                "env:TOOLPORT_SECRET_TOKEN",
            ] {
                let mut s = remote(provenance);
                s.unknown_fields.insert(
                    "secretSources".into(),
                    serde_json::json!({"allowedPrefixes":["env:"]}),
                );
                s.unknown_fields.insert(
                    "headerKeys".into(),
                    serde_json::json!([{"key":"X-Api-Key","source":{"ref":reference}}]),
                );
                assert_eq!(
                    resolve_server(&s).unwrap_err().state,
                    ErrorState::PolicyDenied
                );
                assert!(approve_server(&s).is_err());
                s.unknown_fields.remove("headerKeys");
                s.env.push(
                    serde_json::from_value(
                        serde_json::json!({"key":"TOKEN","secret":true,"source":{"ref":reference}}),
                    )
                    .unwrap(),
                );
                assert_eq!(
                    resolve_server(&s).unwrap_err().state,
                    ErrorState::PolicyDenied
                );
            }
        }
        let mut local = remote("manual");
        local.unknown_fields.insert(
            "headerKeys".into(),
            serde_json::json!([{"key":"X-Api-Key","source":{"ref":"env:BWS_ACCESS_TOKEN"}}]),
        );
        validate_server(&local).unwrap();
    }
    #[test]
    fn synced_service_tokens_require_local_destination_bound_approval() {
        let _data = crate::registry::DataDirTestEnv::new("ref-approval");
        for provenance in ["team:malicious", "team:personal-pro", "shared"] {
            let s = remote(provenance);
            assert_eq!(
                resolve_server(&s).unwrap_err().state,
                ErrorState::ApprovalRequired
            );
            assert!(s.needs_team_enable_review());
            assert!(s.check_enable_allowed(false).is_err());
            approve_server(&s).unwrap();
            check_approval(&s).unwrap();
            assert!(check_reviewed_definition(&s, None).is_err());
            for (field, value) in [
                ("url", "https://another.example/mcp"),
                ("header", "Authorization"),
                ("reference", "bws://be8e0ad8-d545-4017-a55a-b02f014d4158"),
            ] {
                let mut changed = s.clone();
                match field {
                    "url" => changed.url = Some(value.into()),
                    "header" => {
                        changed.unknown_fields.get_mut("headerKeys").unwrap()[0]["key"] =
                            Value::String(value.into())
                    }
                    _ => {
                        changed.unknown_fields.get_mut("headerKeys").unwrap()[0]["source"]["ref"] =
                            Value::String(value.into())
                    }
                }
                assert_eq!(
                    check_approval(&changed).unwrap_err().state,
                    ErrorState::ApprovalRequired
                );
                assert!(check_reviewed_definition(&changed, Some(&s)).is_err());
            }
            let encoded = serde_json::to_string(&s).unwrap();
            assert!(!encoded.contains("approval"));
        }
    }
    #[test]
    fn command_approval_covers_arguments_working_directory_and_input_name() {
        let _data = crate::registry::DataDirTestEnv::new("ref-command-approval");
        let s:ServerEntry=serde_json::from_value(serde_json::json!({"id":"cmd","name":"Cmd","source":"team:t","transport":"stdio","command":"npx","args":["trusted"],"env":[{"key":"TOKEN","secret":true,"source":{"ref":"op://Private/GitHub Token/credential"}}]})).unwrap();
        approve_server(&s).unwrap();
        for changed in [
            {
                let mut c = s.clone();
                c.command = Some("evil".into());
                c
            },
            {
                let mut c = s.clone();
                c.args.push("evil".into());
                c
            },
            {
                let mut c = s.clone();
                c.cwd = Some("/evil".into());
                c
            },
            {
                let mut c = s.clone();
                c.env[0].key = "OTHER".into();
                c
            },
        ] {
            assert!(check_approval(&changed).is_err());
        }
    }
    #[test]
    fn authorization_header_keys_are_optional_bearer_sources() {
        let mut server = remote("team:t");
        server.unknown_fields.insert(
            "headerKeys".into(),
            serde_json::json!([{"key":"Authorization","env":"AUTH"}]),
        );
        for (saved, expected) in [
            (Some("bare-token"), Some("Bearer bare-token")),
            (Some("Bearer x"), Some("Bearer x")),
            (None, None),
        ] {
            let mut headers = resolve_headers_with(&server, |_, key| {
                assert_eq!(key, "AUTH");
                Ok(saved.map(str::to_string))
            })
            .unwrap();
            let bearer = take_legacy_bearer(&server, &mut headers);
            assert!(headers.is_empty());
            assert_eq!(
                bearer
                    .as_deref()
                    .map(crate::downstream::bearer_header)
                    .as_deref(),
                expected
            );
        }
    }
    #[test]
    fn connection_resolves_each_reference_once_for_env_launch_and_headers() {
        let mut server = remote("local");
        server.source = None;
        server.env.push(
            serde_json::from_value(
                serde_json::json!({"key":"TOKEN","secret":true,"source":{"ref":"op://v/i/key"}}),
            )
            .unwrap(),
        );
        server.unknown_fields.insert("headerKeys".into(),serde_json::json!([{"key":"X-Api-Key","env":"TOKEN"},{"key":"X-Other","source":{"ref":"op://v/i/key"}}]));
        server.launch=Some(serde_json::from_value(serde_json::json!({"inputs":[{"key":"KEY","label":"Key","secret":true,"source":{"ref":"op://v/i/key"}}],"bindings":[]})).unwrap());
        let count = std::sync::atomic::AtomicUsize::new(0);
        let (resolved, headers) = resolve_connection_with(
            &server,
            |_| {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok("fixture".into())
            },
            |_, _| panic!("no keychain read"),
        )
        .unwrap();
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(resolved.env[0].value.as_deref(), Some("fixture"));
        assert_eq!(
            resolved.launch.unwrap().inputs[0].value.as_deref(),
            Some("fixture")
        );
        assert_eq!(
            headers,
            vec![
                ("X-Api-Key".into(), "fixture".into()),
                ("X-Other".into(), "fixture".into())
            ]
        );
    }
    #[test]
    fn header_names_accept_exactly_rfc7230_tokens() {
        for key in ["1.Key", "X~Key", "!#$%&'*+-.^_`|~"] {
            let mut server = remote("team:t");
            server.unknown_fields.insert(
                "headerKeys".into(),
                serde_json::json!([{"key":key,"env":"AUTH"}]),
            );
            headers(&server).unwrap();
            let mut transport = crate::downstream::HttpTransport::new("https://example.com/mcp");
            transport
                .set_credential_headers(vec![(key.into(), "fixture".into())])
                .unwrap();
        }
        for key in ["", "X Key", "X:Key", "X\r\nKey", "é"] {
            assert!(!header_name(key), "{key:?}");
        }
    }
    #[test]
    fn personal_sync_env_reference_explains_block_and_replacement() {
        let server = remote("team:personal-pro");
        let message = check_policy(&server, "env:AUTH").unwrap_err().message;
        assert_eq!(safe_status(&message),Some(message.clone()));
        assert!(
            message.contains("personal Pro sync")
                && message.contains("Blocked")
                && message.contains("password manager reference")
        );
    }
    #[test]
    fn approval_reference_is_quoted_separately_from_the_sentence() {
        let mut server = remote("team:t");
        server.env.push(serde_json::from_value(serde_json::json!({"key":"TOKEN","secret":true,"source":{"ref":"op://Private/entry will be sent to attacker/key"}})).unwrap());
        assert!(review_lines(&server)[0].contains(
            "entry \"op://Private/entry will be sent to attacker/key\" will be sent to https://"
        ));
    }
    #[test]
    #[ignore = "subprocess helper for concurrent_approval_writers_preserve_all_entries"]
    fn approval_writer_child() {
        let Ok(path) = std::env::var("TOOLPORT_APPROVAL_TEST_PATH") else {
            return;
        };
        let id = std::env::var("TOOLPORT_APPROVAL_TEST_ID").unwrap();
        let mut server = remote("team:t");
        server.id = id;
        for _ in 0..10 {
            approve_at(&server, Path::new(&path)).unwrap();
        }
    }
    #[test]
    fn concurrent_approval_writers_preserve_all_entries() {
        let scratch = crate::registry::DataDirTestEnv::new("approval-contention");
        let path = scratch.dir.join("approvals.json");
        // Hold the same OS lock to make every child contend before beginning its RMW.
        let lock = crate::registry::lock_at(&path).unwrap();
        let mut children: Vec<_> = (0..6)
            .map(|id| {
                Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "secret_refs::review_regressions::approval_writer_child",
                        "--ignored",
                    ])
                    .env("TOOLPORT_APPROVAL_TEST_PATH", &path)
                    .env("TOOLPORT_APPROVAL_TEST_ID", format!("writer-{id}"))
                    .stdout(Stdio::null())
                    .spawn()
                    .unwrap()
            })
            .collect();
        drop(lock);
        for child in &mut children {
            assert!(child.wait().unwrap().success());
        }
        let entries = approvals(&path);
        assert_eq!(entries.len(), 6);
        for id in 0..6 {
            assert!(entries.contains_key(&format!("writer-{id}")));
        }
    }
    #[test]
    fn teams_shaped_header_keys_read_the_env_keychain_account() {
        let mut s = remote("team:good");
        s.unknown_fields.insert(
            "headerKeys".into(),
            serde_json::json!([{"key":"X-Api-Key","env":"API_TOKEN"}]),
        );
        let headers = resolve_headers_with(&s, |id, key| {
            assert_eq!((id, key), ("attack", "API_TOKEN"));
            Ok(Some("fixture-token".into()))
        })
        .unwrap();
        assert_eq!(headers, vec![("X-Api-Key".into(), "fixture-token".into())]);
        assert!(!has_references(&s));
    }
    #[test]
    fn teams_header_env_reference_is_bound_to_both_output_names() {
        let mut s = remote("team:t");
        s.unknown_fields.insert(
            "headerKeys".into(),
            serde_json::json!([{"key":"X-Api-Key","env":"TOKEN"}]),
        );
        s.env.push(serde_json::from_value(serde_json::json!({"key":"TOKEN","secret":true,"source":{"ref":"op://Private/GitHub Token/credential"}})).unwrap());
        let uses = reference_uses(&s).unwrap();
        assert!(uses.contains_key("env:TOKEN") && uses.contains_key("header:X-Api-Key"));
        assert!(review_lines(&s).iter().any(|line| line == "1Password entry \"op://Private/GitHub Token/credential\" will be sent to https://attacker.example/mcp (header:X-Api-Key)"));
    }
    #[test]
    fn prefixes_match_segments_and_allow_vendor_path_spaces() {
        assert!(prefix_matches("op://Eng", "op://Eng/Token/key"));
        assert!(!prefix_matches(
            "op://Eng",
            "op://Engineering-Private/Token/key"
        ));
        for r in [
            "op://Private/GitHub Token/credential",
            "dl://My Account/password",
            "doppler://my project/prod/API_KEY",
            "vault://secret/my service#token",
            "infisical://project/prod/my service/TOKEN",
        ] {
            parse(r).unwrap();
        }
        for r in [
            "op://Private/ GitHub/key",
            "op://Private/GitHub /key",
            "op://Private/GitHub  Token/key",
            "op://Private/GitHub\tToken/key",
            "op://Private/-GitHub Token/key",
        ] {
            assert!(parse(r).is_err(), "{r}");
        }
    }
    #[test]
    fn concurrent_reads_share_one_value_and_cache_only_successes() {
        let cache = Mutex::new(HashMap::new());
        let calls = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    assert_eq!(
                        cached_with("op://v/i/key", &cache, || {
                            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            std::thread::sleep(Duration::from_millis(30));
                            Ok("fixture".into())
                        })
                        .unwrap(),
                        "fixture"
                    );
                });
            }
        });
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        for _ in 0..2 {
            assert!(cached_with("op://v/missing/key", &cache, || {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Err(error(None, ErrorState::Locked))
            })
            .is_err());
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 3);
    }
}

#[cfg(test)]
mod pool_regressions {
    use super::*;
    #[test]
    fn server_reconnect_rereads_rotated_stdio_credentials() {
        let server:ServerEntry=serde_json::from_value(serde_json::json!({"id":"cache-restart","name":"Cache","transport":"stdio","command":"fixture","env":[{"key":"TOKEN","secret":true,"source":{"ref":"op://v/stdio-restart/key"}}]})).unwrap();
        let reference = "op://v/stdio-restart/key";
        let attempts = ConnectionReads::new(false);
        assert_eq!(test_cached_value(reference, "old"), "old");
        attempts.before_connect(&server);
        assert_eq!(test_cached_value(reference, "rotated"), "old");
        attempts.before_connect(&server);
        assert_eq!(test_cached_value(reference, "rotated"), "rotated");
        ConnectionReads::new(true).before_connect(&server);
        assert_eq!(test_cached_value(reference, "user-restart"), "user-restart");
        invalidate(reference);
    }
    #[test]
    fn cached_success_expires_without_another_startup_read() {
        let cache = Mutex::new(HashMap::new());
        let reference = "op://v/ttl/key";
        assert_eq!(
            cached_with(reference, &cache, || Ok("old".into())).unwrap(),
            "old"
        );
        assert_eq!(
            cached_with(reference, &cache, || panic!("startup cache hit")).unwrap(),
            "old"
        );
        *cache.lock().unwrap()[reference].completed.lock().unwrap() =
            Some(Instant::now() - CACHE_TTL);
        assert_eq!(
            cached_with(reference, &cache, || Ok("rotated".into())).unwrap(),
            "rotated"
        );
    }
    #[test]
    fn panicking_leader_releases_waiters_and_allows_retry() {
        let cache = Mutex::new(HashMap::new());
        let reference = "op://v/panic/key";
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            let cache = &cache;
            let leader = scope.spawn(move || {
                std::panic::catch_unwind(|| {
                    cached_with(reference, cache, || {
                        started_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        panic!("fixture read panic");
                    })
                })
            });
            started_rx.recv().unwrap();
            let waiter =
                scope.spawn(move || cached_with(reference, cache, || panic!("waiter must join")));
            let deadline = Instant::now() + Duration::from_secs(2);
            while Arc::strong_count(&cache.lock().unwrap()[reference]) < 3 {
                assert!(Instant::now() < deadline);
                std::thread::yield_now();
            }
            release_tx.send(()).unwrap();
            assert!(leader.join().unwrap().is_err());
            assert_eq!(
                waiter.join().unwrap().unwrap_err().state,
                ErrorState::Failed
            );
        });
        assert_eq!(
            cached_with(reference, &cache, || Ok("retry".into())).unwrap(),
            "retry"
        );
    }
    #[test]
    fn abandoned_flight_wait_is_bounded() {
        let cache = Mutex::new(HashMap::from([(
            "op://v/stuck/key".into(),
            Arc::new(Flight::default()),
        )]));
        let started = Instant::now();
        assert_eq!(
            cached_with_limits(
                "op://v/stuck/key",
                &cache,
                || panic!("existing flight"),
                CACHE_TTL,
                Duration::from_millis(5)
            )
            .unwrap_err()
            .state,
            ErrorState::Timeout
        );
        assert!(started.elapsed() < Duration::from_secs(1));
    }
    #[test]
    fn concurrent_failed_reads_share_a_flight_but_later_retries_read_again() {
        let cache = Mutex::new(HashMap::new());
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let reference = "op://v/locked/key";
        std::thread::scope(|scope| {
            let cache = &cache;
            let calls = &calls;
            scope.spawn(move || {
                assert!(cached_with(reference, cache, || {
                    calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    started_tx.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                    Err(error(None, ErrorState::Locked))
                })
                .is_err());
            });
            started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            for _ in 0..4 {
                scope.spawn(move || {
                    assert!(cached_with(reference, cache, || {
                        calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        Err(error(None, ErrorState::Locked))
                    })
                    .is_err());
                });
            }
            let deadline = Instant::now() + Duration::from_secs(1);
            let joined = loop {
                let joined = cache
                    .lock()
                    .unwrap()
                    .get(reference)
                    .is_some_and(|flight| Arc::strong_count(flight) == 6);
                if joined || Instant::now() >= deadline {
                    break joined;
                }
                std::thread::sleep(Duration::from_millis(5));
            };
            release_tx.send(()).unwrap();
            assert!(joined, "all waiters joined the same active flight");
        });
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(cached_with(reference, &cache, || {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(error(None, ErrorState::Locked))
        })
        .is_err());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }
    #[test]
    fn server_reference_pool_is_bounded_concurrent_and_deduplicated() {
        let mut s:ServerEntry=serde_json::from_value(serde_json::json!({"id":"pool","name":"Pool","transport":"stdio","command":"fixture","env":[]})).unwrap();
        for n in 0..9 {
            s.env.push(serde_json::from_value(serde_json::json!({"key":format!("TOKEN_{n}"),"secret":true,"source":{"ref":format!("op://v/item{}/key",n%8)}})).unwrap());
        }
        let active = std::sync::atomic::AtomicUsize::new(0);
        let peak = std::sync::atomic::AtomicUsize::new(0);
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let values = resolve_values_with(&s, |_| {
            use std::sync::atomic::Ordering::SeqCst;
            let current = active.fetch_add(1, SeqCst) + 1;
            peak.fetch_max(current, SeqCst);
            calls.fetch_add(1, SeqCst);
            std::thread::sleep(Duration::from_millis(25));
            active.fetch_sub(1, SeqCst);
            Ok("fixture".into())
        })
        .unwrap();
        assert_eq!(values.len(), 8);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 8);
        assert!((2..=4).contains(&peak.load(std::sync::atomic::Ordering::SeqCst)));
    }
}

#[cfg(test)]
mod cache_auth_regressions {
    use super::*;
    #[test]
    fn auth_rejection_invalidates_the_reference_cache() {
        let reference = "op://cache-auth-regression/item/key";
        let cache = CACHE.get_or_init(Default::default);
        assert_eq!(
            cached_with(reference, cache, || Ok("old".into())).unwrap(),
            "old"
        );
        let server:ServerEntry=serde_json::from_value(serde_json::json!({"id":"cache","name":"Cache","transport":"http","url":"https://example.com/mcp","env":[],"headerKeys":[{"key":"X-Key","source":{"ref":reference}}]})).unwrap();
        invalidate_server(&server);
        assert_eq!(
            cached_with(reference, cache, || Ok("new".into())).unwrap(),
            "new"
        );
        invalidate(reference);
    }
}

#[cfg(test)]
mod bearer_destination_regression {
    #[test]
    fn command_destination_cannot_be_disguised_by_an_unused_url() {
        let server:crate::registry::ServerEntry=serde_json::from_value(serde_json::json!({"id":"command","name":"Command","transport":"http","command":"fixture","args":["--option"],"url":"https://trusted.example/mcp","source":"shared","env":[{"key":"TOKEN","secret":true,"source":{"ref":"op://v/i/key"}}]})).unwrap();
        assert_eq!(
            super::review_lines(&server),
            vec!["1Password entry \"op://v/i/key\" will be sent to fixture --option (env:TOKEN)"]
        );
    }
    #[test]
    fn remote_env_reference_order_changes_the_approved_bearer_destination() {
        let mut server:crate::registry::ServerEntry=serde_json::from_value(serde_json::json!({"id":"bearer","name":"Bearer","transport":"http","url":"https://example.com/mcp","source":"team:t","env":[{"key":"A","secret":true,"source":{"ref":"op://v/a/key"}},{"key":"B","secret":true,"source":{"ref":"op://v/b/key"}}]})).unwrap();
        let before = super::approval_identity(&server).unwrap();
        assert!(super::review_lines(&server)
            .iter()
            .any(|l| l.contains("header:Authorization (env:A)")));
        server.env.reverse();
        assert_ne!(before, super::approval_identity(&server).unwrap());
    }
}
