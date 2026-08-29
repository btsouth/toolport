//! Versioned gateway publishing for Windows packaged installs.
//!
//! Client MCP configs point at `%APPDATA%\Roaming\Toolport\bin\toolport-gateway-{version}.exe`
//! (legacy leaf `Conduit` is still accepted until launch migrates it) instead of the
//! install-dir copy NSIS must overwrite on update. Publishing copies the bundled gateway
//! to a new versioned filename (never fighting a lock on the old file), records the path
//! in `gateway-manifest.json`, and lets `repoint_stale_gateways` migrate client configs.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MANIFEST_FILE: &str = "gateway-manifest.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GatewayManifest {
    pub version: String,
    pub path: String,
    pub size: u64,
}

/// True on Windows packaged builds (not `cargo run` from `target/`).
pub fn should_publish_client_gateway() -> bool {
    #[cfg(windows)]
    {
        if let Ok(exe) = std::env::current_exe() {
            let lower = exe.to_string_lossy().to_ascii_lowercase();
            return !lower.contains("\\target\\");
        }
    }

    false
}

fn gateway_bin_dir() -> Option<PathBuf> {
    Some(crate::registry::conduit_dir()?.join("bin"))
}

fn manifest_path() -> Option<PathBuf> {
    Some(gateway_bin_dir()?.join(MANIFEST_FILE))
}

fn versioned_dest(version: &str) -> Option<PathBuf> {
    let ext = std::env::consts::EXE_SUFFIX;
    Some(gateway_bin_dir()?.join(format!("toolport-gateway-{version}{ext}")))
}

/// Gateway binary bundled next to the running app (install dir).
pub fn bundled_gateway_source() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let ext = std::env::consts::EXE_SUFFIX;
    let version = env!("CARGO_PKG_VERSION");

    let versioned = dir.join(format!("toolport-gateway-{version}{ext}"));
    if versioned.is_file() {
        return Some(versioned);
    }

    let plain = dir.join(format!("toolport-gateway{ext}"));
    if plain.is_file() {
        return Some(plain);
    }

    let legacy = dir.join(format!("conduit-gateway{ext}"));
    if legacy.is_file() {
        return Some(legacy);
    }

    if let Some(triple) = option_env!("CONDUIT_TARGET_TRIPLE").filter(|t| !t.is_empty()) {
        for name in ["toolport-gateway", "conduit-gateway"] {
            let suffixed = dir.join(format!("{name}-{triple}{ext}"));
            if suffixed.is_file() {
                return Some(suffixed);
            }
        }
    }

    None
}

fn file_size(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|m| m.len())
}

fn file_sha256(path: &Path) -> std::io::Result<String> {
    use std::io::Read as _;

    let mut file = std::fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    let digest = digest.finalize();
    Ok(format!("{digest:x}"))
}

fn existing_file_sha256(path: &Path) -> std::io::Result<Option<String>> {
    match file_sha256(path) {
        Ok(digest) => Ok(Some(digest)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// A second build of the same Cargo version can differ from the gateway already
/// published under the version-only filename. On Windows that old image is commonly
/// locked by Codex/Claude, so replacing it in place fails. Give the rebuilt image a
/// content-addressed leaf instead; the manifest/repoint path can then migrate clients
/// without fighting the running executable.
fn content_addressed_dest(dest: &Path, digest: &str) -> PathBuf {
    let stem = dest
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "toolport-gateway".into());
    let ext = dest
        .extension()
        .map(|s| format!(".{}", s.to_string_lossy()))
        .unwrap_or_default();
    let short = &digest[..digest.len().min(12)];
    dest.with_file_name(format!("{stem}-{short}{ext}"))
}

fn select_publish_dest(base_dest: &Path, src_digest: &str) -> PathBuf {
    match existing_file_sha256(base_dest) {
        Ok(Some(dest_digest)) if dest_digest == src_digest => base_dest.to_path_buf(),
        Ok(Some(_)) | Err(_) => content_addressed_dest(base_dest, src_digest),
        Ok(None) => base_dest.to_path_buf(),
    }
}

fn existing_publish_destination_from(src: &Path, base_dest: &Path) -> Option<PathBuf> {
    let src_digest = file_sha256(src).ok()?;
    let dest = select_publish_dest(base_dest, &src_digest);
    match existing_file_sha256(&dest) {
        Ok(Some(dest_digest)) if dest_digest == src_digest => Some(dest),
        _ => None,
    }
}

/// The destination publication would select, but only when its matching image
/// already exists. Performs no copy and writes no manifest.
pub fn existing_publish_destination() -> Option<PathBuf> {
    if !should_publish_client_gateway() {
        return None;
    }
    let src = bundled_gateway_source()?;
    let base_dest = versioned_dest(env!("CARGO_PKG_VERSION"))?;
    existing_publish_destination_from(&src, &base_dest)
}

/// Copy the install-dir gateway into `Toolport/bin` when needed and write the manifest.
pub fn publish_bundled_gateway() -> Option<PathBuf> {
    if !should_publish_client_gateway() {
        return None;
    }
    let src = bundled_gateway_source()?;
    let version = env!("CARGO_PKG_VERSION").to_string();
    let base_dest = versioned_dest(&version)?;
    let src_size = file_size(&src)?;
    let src_digest = file_sha256(&src).ok()?;

    // Never overwrite a different same-version image in place. Besides being unsafe
    // for a running executable on Unix, Windows rejects it with a sharing violation.
    let dest = select_publish_dest(&base_dest, &src_digest);
    match existing_file_sha256(&dest) {
        Ok(Some(dest_digest)) if dest_digest == src_digest => {}
        Ok(Some(_)) | Err(_) => return None,
        Ok(None) => {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).ok()?;
            }
            std::fs::copy(&src, &dest).ok()?;
            if file_sha256(&dest).ok().as_deref() != Some(src_digest.as_str()) {
                return None;
            }
        }
    }

    let manifest = GatewayManifest {
        version: version.clone(),
        path: dest.to_string_lossy().into_owned(),
        size: file_size(&dest).unwrap_or(src_size),
    };
    if let Some(path) = manifest_path() {
        if let Ok(json) = serde_json::to_string_pretty(&manifest) {
            let _ = crate::registry::atomic_write(&path, &json);
        }
    }

    Some(dest)
}

/// Published client gateway path from the manifest, when it matches this build.
pub fn published_gateway_path() -> Option<PathBuf> {
    if !should_publish_client_gateway() {
        return None;
    }
    let path = manifest_path()?;
    let raw = std::fs::read_to_string(&path).ok()?;
    let manifest: GatewayManifest = serde_json::from_str(&raw).ok()?;
    if manifest.version != env!("CARGO_PKG_VERSION") {
        return None;
    }
    let p = PathBuf::from(&manifest.path);
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

/// Resolve the path MCP clients should spawn: publish if needed, else read manifest.
pub fn client_gateway_path() -> Option<PathBuf> {
    if !should_publish_client_gateway() {
        return None;
    }
    if let Some(p) = published_gateway_path() {
        return Some(p);
    }
    publish_bundled_gateway()
}

// ---------------------------------------------------------------------------
// Cross-platform gateway process reaper (SOU-414 / residual of SOU-306)
//
// Product rule: same outcome on Windows, macOS, and Linux. Staleness is decided
// by process *image identity* (full path + basename rules), not by Windows
// versioned filenames alone. macOS/Linux clients spawn unversioned
// `toolport-gateway`, so a basename-only reaper is a permanent no-op there.
//
// Two modes:
//   * stop_stale_gateways — every launch; keep current/resolved paths, kill obsolete
//   * stop_spawned_gateways — in-app updater; kill every Toolport gateway image so
//     the installer can replace locked files
//
// Parent agent apps (Cursor, Claude, …) are never touched. Clients that auto-respawn
// MCP on a dead stdio pipe pick up the repointed binary on the next tool call.
// ---------------------------------------------------------------------------

/// A running process that looks like a Toolport/Conduit gateway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayProcess {
    pub pid: u32,
    /// Best-effort absolute path of the executable. Missing when the OS denied
    /// query access; decision falls back to basename rules only.
    pub path: Option<PathBuf>,
    /// Process image basename, e.g. `toolport-gateway-1.9.4.exe` or `toolport-gateway`.
    pub basename: String,
    /// The application that spawned this gateway, when it can be attributed.
    /// `None` when the parent is gone, unreadable, or cannot be trusted (see
    /// `windows_parent_predates_child`). Only used to name apps needing a restart;
    /// the keep/kill decision never reads it.
    pub parent: Option<ParentProcess>,
}

/// The application that spawned a gateway, e.g. `claude.exe`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentProcess {
    pub pid: u32,
    pub basename: String,
}

/// Inputs for the pure keep/kill decision. Built at the call site so tests do not
/// need a live process table or a real install layout.
#[derive(Debug, Clone)]
pub struct ReapContext {
    pub current_version: String,
    /// Executable paths that must survive a *stale* reap (current published binary,
    /// nested macOS helper, AppImage stable copy, etc.). Compared case-insensitively
    /// with normalized separators.
    pub keep_paths: Vec<PathBuf>,
    /// PIDs that are never killed, checked before every other rule including
    /// `kill_all` (SOU-432). Always carries the calling process: a reaper must not
    /// kill the process it is running in. `2ba9f95` made that reachable rather than
    /// theoretical by design - a Linux exe ending in ` (deleted)` deliberately
    /// misses keep-paths, so an in-place binary swap turns "our own image" into a
    /// Kill verdict for any caller whose basename looks like a gateway.
    pub keep_pids: Vec<u32>,
    /// When true (updater), every gateway process is killed regardless of path or
    /// version, so locked binaries can be replaced. Still subject to `keep_pids`:
    /// unlocking the binaries is pointless if the process doing it kills itself.
    pub kill_all: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReapDecision {
    Keep,
    Kill,
}

/// Result of a reaper pass, for logging and the Tauri command surface.
///
/// `needs_restart` is carried here rather than queried separately because it is
/// only knowable from the pre-kill process table: once a pass has killed the
/// obsolete gateways, the evidence for which app spawned them is gone. Any
/// caller that asks afterwards necessarily reads an emptier table than the one
/// that produced the advice, which is how #542 shipped a panel that erased
/// itself (see [`RestartAdvice`](../desktop/struct.RestartAdvice.html) for the
/// merge that keeps it).
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReapReport {
    pub killed: Vec<String>,
    pub kept: Vec<String>,
    pub failed: Vec<String>,
    /// Processes that still matched the kill plan after both termination passes.
    /// A successful OS kill call is not proof that the process exited, so updater
    /// callers must gate installation on this final observation too.
    pub remaining: Vec<String>,
    /// Apps still launching an obsolete gateway, from this pass's pre-kill snapshot.
    pub needs_restart: Vec<ClientNeedingRestart>,
    /// External client applications whose gateway accepted a stop request. When an
    /// update later aborts, these apps may need restarting to recreate their stdio
    /// connection. Toolport itself cannot safely recreate a client-owned pipe.
    pub restart_clients: Vec<ClientNeedingRestart>,
    /// Stopped gateways whose parent is provably an external application but
    /// whose executable identity was not readable enough to safely name that
    /// application. Orphans, init-owned helpers, and Toolport-owned processes do
    /// not belong here; updater guidance must not infer external ownership from
    /// the broad `killed` list.
    pub unattributed_external_stopped: Vec<String>,
}

impl ReapReport {
    pub fn killed_labels(&self) -> Vec<String> {
        self.killed.clone()
    }
}

/// An application that keeps relaunching an obsolete gateway and therefore has to
/// be restarted by hand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientNeedingRestart {
    /// Basename of the client application, e.g. `claude.exe`.
    pub client: String,
    /// Pid of that application. Carried so stored advice can be revalidated and
    /// expired: without it the advice can never be cleared once the user complies,
    /// because absence of a respawned gateway is indistinguishable from a client
    /// that simply has not made a tool call yet (#542 review).
    pub client_pid: u32,
    /// The obsolete gateway image it relaunched, e.g. `toolport-gateway-1.9.4.exe`.
    pub gateway: String,
}

/// Build keep-paths known to this crate without calling into `clients` (avoids a
/// module cycle). Callers that can resolve the nested macOS helper should pass
/// extras via [`stop_stale_gateways_with_keep`].
fn default_keep_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let push = |paths: &mut Vec<PathBuf>, p: PathBuf| {
        if !paths.iter().any(|e| paths_equal(e, &p)) {
            paths.push(p);
        }
    };
    if let Some(p) = client_gateway_path() {
        push(&mut paths, p);
    }
    if let Some(p) = published_gateway_path() {
        push(&mut paths, p);
    }
    if let Some(p) = bundled_gateway_source() {
        push(&mut paths, p);
    }
    if let Some(p) = versioned_dest(env!("CARGO_PKG_VERSION")) {
        push(&mut paths, p);
    }
    if let Some(dir) = gateway_bin_dir() {
        let ext = std::env::consts::EXE_SUFFIX;
        push(&mut paths, dir.join(format!("toolport-gateway{ext}")));
        push(&mut paths, dir.join(format!("conduit-gateway{ext}")));
    }
    paths
}

fn paths_equal(a: &Path, b: &Path) -> bool {
    // Resolve symlinks when the files exist so macOS helper vs Contents/MacOS
    // symlink to the same binary both match a keep path.
    let ca = std::fs::canonicalize(a).unwrap_or_else(|_| a.to_path_buf());
    let cb = std::fs::canonicalize(b).unwrap_or_else(|_| b.to_path_buf());
    normalize_path(&ca) == normalize_path(&cb)
}

/// Normalize for comparison only. Always uses `\` so callers check one form.
fn normalize_path(p: &Path) -> String {
    p.to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches(['\\', '/'])
        .to_ascii_lowercase()
}

/// Basename matches a Toolport/Conduit gateway image (versioned or not).
pub fn is_gateway_basename(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let stem = lower.strip_suffix(".exe").unwrap_or(&lower);
    stem == "toolport-gateway"
        || stem == "conduit-gateway"
        || stem.starts_with("toolport-gateway-")
        || stem.starts_with("conduit-gateway-")
}

/// Linux `/proc/<pid>/exe` appends ` (deleted)` after the binary is unlinked
/// (package upgrade). Strip it for *basename* matching so the process is still
/// recognized as a gateway. Leave the marker on the stored path: a `(deleted)`
/// exe must miss keep-paths and be reaped, otherwise an in-place upgrade keeps
/// the old inode running.
#[cfg(any(all(unix, not(target_os = "macos")), test))]
fn strip_deleted_exe_suffix(s: &str) -> &str {
    s.strip_suffix(" (deleted)").unwrap_or(s)
}

/// Last path component of an `exe` symlink target (or any display path), after
/// stripping a trailing ` (deleted)` marker. Pure for tests (WS4-3).
#[cfg(any(all(unix, not(target_os = "macos")), test))]
fn basename_from_exe_link(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let cleaned = strip_deleted_exe_suffix(raw.as_ref());
    Path::new(cleaned)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| cleaned.to_string())
}

/// Parse one line of `ps -ax -o pid= -o ppid= -o ucomm=` into `(pid, ppid, name)`.
///
/// Pure helper for the macOS enumerator line format. Does **not** prove the `ps`
/// argv itself is correct — a broken `-axo pid= comm=` still needs a macOS
/// smoke / CI job (WS4-1 / WS4-8).
///
/// Both leading columns are required. A row missing the ppid column must fail
/// rather than slide the name left, which is how a wrong `-o` argv would quietly
/// produce nonsense parent attributions instead of an empty list (SOU-435).
///
/// Compiled under `test` on every platform, not just macOS, so the parser is
/// covered on the machines that actually run the suite. Gating it to macOS alone
/// is how the previous attempt ended up with a test that only ran where the bug
/// could not appear.
#[cfg(any(target_os = "macos", test))]
fn parse_ps_pid_ppid_name_line(line: &str) -> Option<(u32, u32, String)> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let mut parts = line.split_whitespace();
    let pid = parts.next()?.parse().ok()?;
    let ppid = parts.next()?.parse().ok()?;
    let name = parts.collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        return None;
    }
    Some((pid, ppid, name))
}

/// True only for names like `toolport-gateway-1.9.4`, not `toolport-gateway-shim`.
fn is_versioned_gateway_basename(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let stem = lower.strip_suffix(".exe").unwrap_or(&lower);
    let rest = if let Some(r) = stem.strip_prefix("toolport-gateway-") {
        r
    } else if let Some(r) = stem.strip_prefix("conduit-gateway-") {
        r
    } else {
        return false;
    };
    looks_like_version_suffix(rest)
}

/// Version suffix: starts with a digit, contains `.`, and only version-ish chars
/// (e.g. `1.9.4`, `1.9.4-beta.1`). Rejects `shim`, `helper`, target triples alone.
fn looks_like_version_suffix(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_digit() || !s.contains('.') {
        return false;
    }
    s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | '_'))
}

fn basename_matches_current_version(name: &str, version: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let stem = lower.strip_suffix(".exe").unwrap_or(&lower);
    let want_tp = format!("toolport-gateway-{version}").to_ascii_lowercase();
    let want_cd = format!("conduit-gateway-{version}").to_ascii_lowercase();
    [want_tp, want_cd].iter().any(|want| {
        stem == want
            || stem
                .strip_prefix(&format!("{want}-"))
                .map(|suffix| {
                    suffix.len() == 12 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
                })
                .unwrap_or(false)
    })
}

/// Path looks like a cargo `target/...` dev binary — keep under stale mode so
/// `tauri dev` is not murdered by a packaged Toolport on the same machine.
fn is_dev_target_path(path: &Path) -> bool {
    let n = normalize_path(path);
    n.contains("\\target\\debug\\") || n.contains("\\target\\release\\")
}

/// Path is under a Toolport/Conduit product location (data dir bin, install leaf,
/// macOS helper bundle, AppImage stable copy). Used to avoid killing a foreign
/// binary that merely starts with a similar name.
fn path_looks_like_our_install(path: &Path) -> bool {
    let n = normalize_path(path);
    n.contains("\\toolport\\")
        || n.contains("\\conduit\\")
        || n.contains("toolportgateway.app")
        || n.contains("conduitgateway.app")
        || n.contains("toolport.app")
        || n.contains("conduit.app")
}

/// Pure keep/kill decision. Getting this wrong is expensive both ways: too eager
/// kills the bridge we just started; too shy leaves users on old gateway code.
pub fn decide_reap(proc: &GatewayProcess, ctx: &ReapContext) -> ReapDecision {
    // Before anything else, including kill_all: never kill a protected pid
    // (the calling process itself) (SOU-432).
    if ctx.keep_pids.contains(&proc.pid) {
        return ReapDecision::Keep;
    }
    if !is_gateway_basename(&proc.basename) {
        return ReapDecision::Keep;
    }
    if ctx.kill_all {
        return ReapDecision::Kill;
    }

    if let Some(ref path) = proc.path {
        if ctx.keep_paths.iter().any(|k| paths_equal(k, path)) {
            return ReapDecision::Keep;
        }
        if is_dev_target_path(path) {
            return ReapDecision::Keep;
        }
        if basename_matches_current_version(&proc.basename, &ctx.current_version) {
            // Same version, different install path: still our build; keep unless
            // we have keep_paths that explicitly name a different current (handled above).
            return ReapDecision::Keep;
        }
        if is_versioned_gateway_basename(&proc.basename) {
            // Only kill versioned images under our product paths (not toolport-gateway-shim
            // in /opt/other-vendor, and not non-version suffixes that snuck through).
            if path_looks_like_our_install(path) {
                return ReapDecision::Kill;
            }
            return ReapDecision::Keep;
        }
        // Unversioned basename (macOS/Linux/AppImage/dev packaged): path is the
        // identity. Not equal to keep_paths and looks like ours → obsolete copy.
        if path_looks_like_our_install(path) {
            if ctx.keep_paths.is_empty() {
                // No known current path — refuse to guess.
                return ReapDecision::Keep;
            }
            return ReapDecision::Kill;
        }
        // Unknown location with gateway basename: do not kill strangers.
        return ReapDecision::Keep;
    }

    // Path unavailable: basename-only fallback (Windows ACL edge cases).
    if basename_matches_current_version(&proc.basename, &ctx.current_version) {
        return ReapDecision::Keep;
    }
    if is_versioned_gateway_basename(&proc.basename) {
        return ReapDecision::Kill;
    }
    // Unversioned without path: cannot tell current from stale.
    ReapDecision::Keep
}

/// Is `basename` one of our own processes rather than a third-party MCP client?
///
/// The supervised HTTP bridge is spawned by the Toolport app itself, so it would
/// otherwise be reported as a client the user must restart.
fn is_our_own_process(basename: &str) -> bool {
    let lower = basename.to_ascii_lowercase();
    lower.starts_with("toolport") || lower.starts_with("conduit")
}

/// Is this "parent" the init system or the kernel rather than an application?
///
/// An orphaned gateway is reparented to pid 1 on Unix, and Windows reports pid 0 /
/// pid 4 for its kernel processes. None of them is something a user can restart,
/// and each platform previously guarded a different subset of these, so the same
/// orphan produced "restart systemd" on Linux and nothing on macOS. Name-matched
/// as well as pid-matched because a systemd *user* session is a child subreaper
/// with a pid of its own (#542 review).
fn is_init_like(parent: &ParentProcess) -> bool {
    if parent.pid <= 1 || parent.pid == 4 {
        return true;
    }
    let lower = parent.basename.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "systemd" | "init" | "launchd" | "system" | "[system process]"
    )
}

/// Will the path this client cached start yielding new code by itself?
///
/// Only one case self-heals, and the ` (deleted)` marker alone does not identify
/// it. On Linux `/proc/<pid>/exe` gains that marker whenever the file was
/// *unlinked*, which covers two opposite outcomes:
///
/// * **Replaced in place** (a package upgrade wrote a new file at the same path):
///   the cached path now resolves to the new binary, so the next spawn picks it up
///   and there is nothing to advise.
/// * **Moved or removed** (a stale install location that an upgrade abandoned):
///   the cached path resolves to nothing, so the client cannot spawn a gateway at
///   all. That is worse, not better, and the user still has to restart the app.
///
/// Treating the marker itself as "self-heals" silently dropped the second case.
/// The test that was supposed to cover it passed only because it ran on macOS,
/// where no marker appears and the entry survives for unrelated reasons; on Linux,
/// the platform where the marker is real, it was silent (#542 review). Probing the
/// stripped path tells the two apart. A path with no marker is unchanged on disk
/// and never self-heals, which is why non-Linux platforms fall through to `false`.
fn cached_path_self_heals(proc: &GatewayProcess) -> bool {
    let Some(path) = proc.path.as_ref().and_then(|p| p.to_str()) else {
        return false;
    };
    match path.strip_suffix(" (deleted)") {
        Some(stripped) => Path::new(stripped).exists(),
        None => false,
    }
}

/// Applications running an obsolete gateway they will relaunch from a cached path.
///
/// An MCP client reads its config once at its own startup and caches the spawn
/// command; see [`cached_path_self_heals`] for when that traps it on old code.
///
/// Deduplicated per (client pid, gateway) so one entry per app the user has to act
/// on, not one per respawn. Pure so the grouping is testable without a process table.
fn clients_needing_restart(
    procs: &[GatewayProcess],
    ctx: &ReapContext,
) -> Vec<ClientNeedingRestart> {
    // The updater kills every gateway including the current one, so every process
    // reaches a Kill verdict and "obsolete" stops meaning anything. Computing advice
    // there yields a list of apps to restart because they are running the CURRENT
    // gateway, which is nonsense the user would act on (#542 review).
    if ctx.kill_all {
        return Vec::new();
    }
    let mut out: Vec<ClientNeedingRestart> = Vec::new();
    for proc in procs {
        if decide_reap(proc, ctx) != ReapDecision::Kill {
            continue;
        }
        // A path we could not read means a process we could not inspect - a
        // foreign user's session, or one whose ACL denied us. `decide_reap` still
        // reaches Kill there via the basename-only fallback, but that path skips
        // the `path_looks_like_our_install` guard, and we cannot stop the process
        // either. Advising a restart for someone else's app is worse than silence.
        if proc.path.is_none() {
            // Logged rather than dropped silently: this is the one branch that
            // withholds advice for a reason the user cannot see, so a support case
            // ("why does it keep coming back and say nothing?") is otherwise
            // untraceable (#542 review).
            eprintln!(
                "toolport: {} (pid {}) looks obsolete but its path could not be read, \
                 so it is not attributed to an app",
                proc.basename, proc.pid
            );
            continue;
        }
        if cached_path_self_heals(proc) {
            continue;
        }
        // No parent means no name to show. Staying silent beats telling someone to
        // restart "something".
        let Some(parent) = proc.parent.as_ref() else {
            continue;
        };
        if is_our_own_process(&parent.basename) || is_init_like(parent) {
            continue;
        }
        let advice = ClientNeedingRestart {
            client: parent.basename.clone(),
            client_pid: parent.pid,
            gateway: proc.basename.clone(),
        };
        if !out.contains(&advice) {
            out.push(advice);
        }
    }
    out
}

/// What a reaper pass will do, decided before it does any of it.
#[derive(Debug, Clone, Default)]
pub struct ReapPlan {
    pub to_kill: Vec<GatewayProcess>,
    pub kept: Vec<String>,
    pub needs_restart: Vec<ClientNeedingRestart>,
}

/// Pure planner: classify a process table, and derive restart advice from that same
/// pre-kill snapshot.
///
/// Exists so the "advice must be computed before anything is killed" requirement
/// holds *by construction* rather than by where a line sits in the reaping loop.
/// Three review rounds of #542 each moved that requirement somewhere new instead of
/// removing it, and each move looked correct in isolation. Here the snapshot is an
/// argument, so there is no later table to accidentally read.
pub fn plan_reap(procs: &[GatewayProcess], ctx: &ReapContext) -> ReapPlan {
    let mut plan = ReapPlan {
        needs_restart: clients_needing_restart(procs, ctx),
        ..Default::default()
    };
    for proc in procs {
        match decide_reap(proc, ctx) {
            ReapDecision::Keep => plan.kept.push(label_process(proc)),
            ReapDecision::Kill => plan.to_kill.push(proc.clone()),
        }
    }
    plan
}

fn label_process(proc: &GatewayProcess) -> String {
    match &proc.path {
        Some(p) => format!("{} (pid {} @ {})", proc.basename, proc.pid, p.display()),
        None => format!("{} (pid {})", proc.basename, proc.pid),
    }
}

/// Recovery target for a gateway stopped on behalf of the updater.
///
/// Only attribute a process when both its executable and parent were inspectable.
/// That is the same trust boundary as stale-gateway restart advice: an unreadable
/// process may belong to another user session, and naming its parent would turn
/// weak basename evidence into confident recovery guidance.
fn restart_client_for(proc: &GatewayProcess) -> Option<ClientNeedingRestart> {
    proc.path.as_ref()?;
    let parent = proc.parent.as_ref()?;
    if is_our_own_process(&parent.basename) || is_init_like(parent) {
        return None;
    }
    Some(ClientNeedingRestart {
        client: parent.basename.clone(),
        client_pid: parent.pid,
        gateway: proc.basename.clone(),
    })
}

fn record_restart_client(report: &mut ReapReport, proc: &GatewayProcess) {
    let Some(client) = restart_client_for(proc) else {
        let provably_external = proc
            .parent
            .as_ref()
            .is_some_and(|parent| !is_our_own_process(&parent.basename) && !is_init_like(parent));
        if provably_external {
            let label = label_process(proc);
            if !report
                .unattributed_external_stopped
                .iter()
                .any(|existing| existing == &label)
            {
                report.unattributed_external_stopped.push(label);
            }
        }
        return;
    };
    if !report
        .restart_clients
        .iter()
        .any(|existing| existing.client_pid == client.client_pid)
    {
        report.restart_clients.push(client);
    }
}

fn reap_with_context(ctx: &ReapContext) -> ReapReport {
    reap_listed(ctx, list_gateway_processes)
}

/// Body of [`reap_with_context`], taking the enumerator so a caller can bound which
/// processes the pass may consider.
///
/// Production always passes [`list_gateway_processes`]. Tests pass an enumerator
/// scoped to their own fixtures: driving the real plan/kill/verify path against the
/// *global* process table would otherwise mean a real gateway that starts during the
/// pass (including inside the 150ms verify window below, which re-enumerates) is not
/// in `keep_pids` and gets killed. Pinning pids before the pass narrows that window
/// but cannot close it, because the verify re-enumeration happens later than any
/// snapshot the caller can take. Scoping the enumerator closes it by construction.
fn reap_listed(ctx: &ReapContext, list: impl Fn() -> Vec<GatewayProcess>) -> ReapReport {
    let mut report = ReapReport::default();
    let plan = plan_reap(&list(), ctx);
    report.kept = plan.kept;
    report.needs_restart = plan.needs_restart;
    for proc in plan.to_kill {
        let label = label_process(&proc);
        if kill_gateway_process(&proc) {
            report.killed.push(label);
            if ctx.kill_all {
                record_restart_client(&mut report, &proc);
            }
        } else {
            report.failed.push(label);
        }
    }
    // Verify: anything still present that should be gone?
    if !report.killed.is_empty() || !report.failed.is_empty() {
        std::thread::sleep(std::time::Duration::from_millis(150));
        let still = list();
        for proc in still {
            if decide_reap(&proc, ctx) == ReapDecision::Kill {
                let label = label_process(&proc);
                if kill_gateway_process(&proc) {
                    // A first termination attempt may fail transiently. Once the
                    // retry is accepted, do not leave a stale failure that would
                    // incorrectly block the updater; the final observation below
                    // is authoritative about whether the process actually exited.
                    report.failed.retain(|failed| failed != &label);
                    if !report.killed.iter().any(|k| k == &label) {
                        report.killed.push(format!("{label} [retry]"));
                    }
                    if ctx.kill_all {
                        record_restart_client(&mut report, &proc);
                    }
                } else if !report.failed.iter().any(|f| f == &label) {
                    report.failed.push(label);
                }
            }
        }

        // A kill API reporting success only means the signal/request was accepted.
        // Re-enumerate once more so update installation never races a process that
        // still has the gateway binary open.
        std::thread::sleep(std::time::Duration::from_millis(150));
        report.remaining = list()
            .into_iter()
            .filter(|proc| decide_reap(proc, ctx) == ReapDecision::Kill)
            .map(|proc| label_process(&proc))
            .collect();

        // Keep failed termination attempts even when the final enumerator no longer
        // sees the pid. Enumeration is best-effort on every platform; only a later
        // successful kill is strong enough evidence to clear a failure. Updaters
        // must fail closed rather than replace files after an ambiguous shutdown.
    }
    report
}

fn log_reap_report(kind: &str, report: &ReapReport) {
    if !report.killed.is_empty() {
        eprintln!(
            "toolport: {kind} stopped {} gateway process(es): {}",
            report.killed.len(),
            report.killed.join("; ")
        );
    }
    if !report.failed.is_empty() {
        eprintln!(
            "toolport: {kind} failed to stop {} gateway process(es): {}",
            report.failed.len(),
            report.failed.join("; ")
        );
    }
    if !report.remaining.is_empty() {
        eprintln!(
            "toolport: {kind} still sees {} gateway process(es) alive: {}",
            report.remaining.len(),
            report.remaining.join("; ")
        );
    }
}

/// Terminate every Toolport/Conduit gateway process (all platforms). Used before
/// in-app update so locked binaries can be replaced. Does not touch parent apps.
/// Returns the complete shutdown report. The updater must refuse installation
/// while either `failed` or `remaining` is non-empty.
pub fn stop_spawned_gateways() -> ReapReport {
    let ctx = ReapContext {
        current_version: env!("CARGO_PKG_VERSION").to_string(),
        keep_paths: Vec::new(),
        // Even the updater's kill-all must not kill the process running it.
        keep_pids: vec![std::process::id()],
        kill_all: true,
    };
    let report = reap_with_context(&ctx);
    log_reap_report("updater reaper", &report);
    report
}

/// Terminate gateway processes that are not the current install. Safe on every
/// launch: keeps processes whose path matches the current resolved/published
/// binary (and current-version basenames). Returns labels of killed processes.
pub fn stop_stale_gateways() -> Vec<String> {
    stop_stale_gateways_with_keep(&[])
}

/// Like [`stop_stale_gateways`], with extra keep-paths (e.g. nested macOS helper
/// from `clients::resolve_gateway_path`).
pub fn stop_stale_gateways_with_keep(extra_keep: &[PathBuf]) -> Vec<String> {
    reap_stale(extra_keep).killed_labels()
}

/// Full-report variant of [`stop_stale_gateways_with_keep`].
///
/// Callers that surface anything beyond the killed list want this: the restart
/// advice cannot be recovered after the fact, and `failed` is the difference
/// between "nothing was stale" and "something is stale and we could not stop it".
pub fn reap_stale(extra_keep: &[PathBuf]) -> ReapReport {
    let mut keep = default_keep_paths();
    for p in extra_keep {
        if !keep.iter().any(|e| paths_equal(e, p)) {
            keep.push(p.clone());
        }
    }
    let ctx = ReapContext {
        current_version: env!("CARGO_PKG_VERSION").to_string(),
        keep_paths: keep,
        keep_pids: vec![std::process::id()],
        kill_all: false,
    };
    let report = reap_with_context(&ctx);
    log_reap_report("stale reaper", &report);
    report
}

// ---------------------------------------------------------------------------
// Pruning published gateway binaries (SOU-484)
//
// The reaper stops stale *processes*; nothing ever deleted a stale *file*, so
// `%APPDATA%\Toolport\bin` accumulates every gateway ever published (~18 MB a
// release). The naive fix is worse than the leak: a client caches its spawn
// command at its own startup, so deleting a binary it still names converts
// "silently runs old code" - a surfaced, recoverable state since SOU-435 - into
// "cannot start the gateway at all", which looks like a broken client with no
// cause. Every rule below therefore fails closed: anything we cannot prove is
// unused is kept, and pruning is never urgent enough to guess.
// ---------------------------------------------------------------------------

/// How many non-current versions to keep regardless of evidence.
///
/// Evidence-based rules cover every client we can *observe*, but a client that has
/// been idle since before the app started has spawned nothing and appears in no
/// live-process list, while a repoint has already removed its path from the config.
/// Its cached path is almost always the version we just upgraded from, so the two
/// newest non-current binaries stay whatever the evidence says. Costs ~36 MB of the
/// ~200 MB this reclaims.
const PRUNE_KEEP_RECENT: usize = 2;

/// Inputs for the pure delete/keep decision, built at the call site so tests need
/// no real install layout.
#[derive(Debug, Clone, Default)]
pub struct PruneContext {
    pub current_version: String,
    /// Never deleted: current published binary, app-local copy, macOS helper, etc.
    pub keep_paths: Vec<PathBuf>,
    /// Backing a live process right now.
    pub live_paths: Vec<PathBuf>,
    /// Named by some client's config. See `clients::referenced_gateway_paths`.
    pub referenced_paths: Vec<PathBuf>,
    /// Gateway basenames from current restart advice: a client is known to be
    /// relaunching these from a cached path even though the config no longer names
    /// them and no process may be alive at this instant. This is the case that
    /// makes evidence-based pruning safe at all, and it only became knowable with
    /// SOU-435 (`toolport-gateway-1.9.7-rc.1.exe` was serving a live Claude Code
    /// two minutes after the 1.10.0 upgrade reaped it).
    pub advised_basenames: Vec<String>,
    /// Newest non-current versions to keep unconditionally, see [`PRUNE_KEEP_RECENT`].
    pub keep_recent: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PruneDecision {
    /// Kept, with the reason, so a log explains why 200 MB is still there.
    Keep(&'static str),
    Delete,
}

/// Pure keep/delete decision for one file in the published gateway directory.
pub fn decide_prune(path: &Path, ctx: &PruneContext) -> PruneDecision {
    let basename = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // Only ever delete versioned images we published. This is what protects the
    // unversioned `toolport-gateway.exe` / `conduit-gateway.exe`, the manifest
    // json, and anything else that happens to share the directory.
    if !is_gateway_basename(&basename) || !is_versioned_gateway_basename(&basename) {
        return PruneDecision::Keep("not a versioned gateway image");
    }
    if basename_matches_current_version(&basename, &ctx.current_version) {
        return PruneDecision::Keep("current version");
    }
    if ctx.keep_paths.iter().any(|k| paths_equal(k, path)) {
        return PruneDecision::Keep("protected path");
    }
    if ctx.live_paths.iter().any(|k| paths_equal(k, path)) {
        return PruneDecision::Keep("backing a running process");
    }
    if ctx.referenced_paths.iter().any(|k| paths_equal(k, path)) {
        return PruneDecision::Keep("named by a client config");
    }
    if ctx
        .advised_basenames
        .iter()
        .any(|b| b.eq_ignore_ascii_case(&basename))
    {
        return PruneDecision::Keep("a client is still relaunching it");
    }
    if ctx.keep_recent.iter().any(|k| paths_equal(k, path)) {
        return PruneDecision::Keep("recent enough to still be cached somewhere");
    }
    PruneDecision::Delete
}

/// The `PRUNE_KEEP_RECENT` newest non-current versions among `paths`.
///
/// Ordered by parsed version rather than mtime: a re-published binary can be
/// rewritten at any time, and publish order is what "recent" means here.
pub fn newest_non_current(paths: &[PathBuf], current_version: &str) -> Vec<PathBuf> {
    let mut versioned: Vec<(Vec<u64>, PathBuf)> = paths
        .iter()
        .filter_map(|p| {
            let name = p.file_name()?.to_string_lossy().into_owned();
            if !is_versioned_gateway_basename(&name)
                || basename_matches_current_version(&name, current_version)
            {
                return None;
            }
            Some((version_sort_key(&name), p.clone()))
        })
        .collect();
    versioned.sort_by(|a, b| b.0.cmp(&a.0));
    versioned
        .into_iter()
        .take(PRUNE_KEEP_RECENT)
        .map(|(_, p)| p)
        .collect()
}

/// Numeric sort key from a versioned gateway basename.
///
/// `toolport-gateway-1.10.0.exe` must sort above `-1.9.6`, so components are
/// compared numerically rather than lexically. A pre-release suffix
/// (`1.9.7-rc.1`) sorts just below its release, which is what the trailing
/// sentinel achieves without parsing semver properly.
fn version_sort_key(basename: &str) -> Vec<u64> {
    let lower = basename.to_ascii_lowercase();
    let stem = lower.strip_suffix(".exe").unwrap_or(&lower);
    let version = stem
        .rsplit_once("-gateway-")
        .map(|(_, v)| v)
        .unwrap_or(stem);
    let (release, pre) = match version.split_once('-') {
        Some((release, pre)) => (release, Some(pre)),
        None => (version, None),
    };
    let numbers = |s: &str| {
        s.split('.')
            .map(|part| part.parse::<u64>().unwrap_or(0))
            .collect::<Vec<u64>>()
    };
    let mut key = numbers(release);
    // Sentinel first, so a release outranks every pre-release of the same version
    // regardless of what follows.
    key.push(if pre.is_some() { 0 } else { 1 });
    // Then the pre-release's own numbers, so `rc.2` outranks `rc.1`. Without this
    // the two collapse to an identical key and `newest_non_current` falls back to
    // `read_dir` order, which can protect the older candidate and delete the newer
    // one. Reachable: the reported directory contains `-1.9.7-rc.1.exe`.
    //
    // Non-numeric identifiers parse to 0, so `rc.1` and `beta.1` still tie. That is
    // deliberate: ordering two different pre-release *channels* of one version needs
    // real semver precedence rules, and the recency floor is a safety net, not a
    // release-management feature. Ties only mean both may be kept.
    if let Some(pre) = pre {
        key.extend(numbers(pre));
    }
    key
}

/// Result of a prune pass.
#[derive(Debug, Clone, Default)]
pub struct PruneReport {
    pub deleted: Vec<String>,
    pub reclaimed_bytes: u64,
    /// Files we tried to delete and could not. Expected on Windows, where a running
    /// image is locked; the next launch retries. Logged, never surfaced as an error.
    pub failed: Vec<String>,
}

/// Delete published gateway binaries that nothing can still be using.
///
/// `referenced_paths` comes from `clients::referenced_gateway_paths`; pass `None`
/// when a client config could not be read, and the pass is skipped entirely rather
/// than deleting against an incomplete picture.
///
/// `advised_basenames` are the gateway images from current restart advice (SOU-435).
pub fn prune_published_gateways(
    referenced_paths: Option<Vec<PathBuf>>,
    advised_basenames: Vec<String>,
) -> PruneReport {
    let mut report = PruneReport::default();
    let Some(referenced_paths) = referenced_paths else {
        eprintln!(
            "toolport: skipping gateway binary prune - at least one client config could not \
             be read, so its cached gateway path is unknown"
        );
        return report;
    };
    let Some(dir) = gateway_bin_dir() else {
        return report;
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return report;
    };
    let on_disk: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();

    let ctx = PruneContext {
        current_version: env!("CARGO_PKG_VERSION").to_string(),
        keep_paths: default_keep_paths(),
        live_paths: list_gateway_processes()
            .into_iter()
            .filter_map(|p| p.path)
            .collect(),
        referenced_paths,
        advised_basenames,
        keep_recent: newest_non_current(&on_disk, env!("CARGO_PKG_VERSION")),
    };

    for path in on_disk {
        if decide_prune(&path, &ctx) != PruneDecision::Delete {
            continue;
        }
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let label = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        match std::fs::remove_file(&path) {
            Ok(()) => {
                report.deleted.push(label);
                report.reclaimed_bytes += size;
            }
            // A locked image on Windows is the normal case, not an error: the file
            // is in use, so keeping it is the correct outcome anyway.
            Err(e) => report.failed.push(format!("{label} ({e})")),
        }
    }
    if !report.deleted.is_empty() {
        eprintln!(
            "toolport: pruned {} old gateway binar(ies), reclaiming {} MB: {}",
            report.deleted.len(),
            report.reclaimed_bytes / 1_048_576,
            report.deleted.join("; ")
        );
    }
    if !report.failed.is_empty() {
        eprintln!(
            "toolport: could not delete {} old gateway binar(ies) (in use; will retry next \
             launch): {}",
            report.failed.len(),
            report.failed.join("; ")
        );
    }
    report
}

/// Is this pid still running?
///
/// Used to expire stored restart advice once the user actually restarts the app.
/// Fails *safe* by returning true on an inconclusive answer: keeping stale advice
/// visible one pass longer is recoverable, dropping live advice is the failure that
/// leaves a user pinned to an obsolete gateway with the UI saying nothing.
pub fn pid_is_running(pid: u32) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if !handle.is_null() {
                CloseHandle(handle);
                return true;
            }
            // Only ERROR_INVALID_PARAMETER positively means "no such pid". Access
            // denied means it exists and we cannot see it.
            windows_sys::Win32::Foundation::GetLastError() != ERROR_INVALID_PARAMETER
        }
    }
    #[cfg(unix)]
    {
        // Linux exposes a definitive answer without spawning anything.
        #[cfg(not(target_os = "macos"))]
        {
            return Path::new(&format!("/proc/{pid}")).exists();
        }
        // macOS has no /proc, so `kill -0` it is: the existence/permission check
        // without delivering a signal. Shelled out rather than via libc to match
        // `unix_kill_pid`, which does the same deliberately so this module needs no
        // libc dependency.
        //
        // A non-zero exit means either "no such process" or "not permitted", and
        // only the first means gone. They are told apart by stderr because the exit
        // code alone cannot: treating EPERM as gone would expire advice for a live
        // app, the exact direction this function must not fail in.
        //
        // LC_ALL=C pins that stderr to English. Without it a non-English locale
        // makes the match fail, which reports a live app as gone and silently drops
        // its restart advice - the same failure the stderr check exists to avoid.
        #[cfg(target_os = "macos")]
        {
            match std::process::Command::new("kill")
                .args(["-0", &pid.to_string()])
                .env("LC_ALL", "C")
                .output()
            {
                Ok(out) if out.status.success() => true,
                Ok(out) => String::from_utf8_lossy(&out.stderr)
                    .to_ascii_lowercase()
                    .contains("permitted"),
                Err(_) => true,
            }
        }
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = pid;
        true
    }
}

// ----- OS process list / kill ------------------------------------------------

#[cfg(windows)]
fn list_gateway_processes() -> Vec<GatewayProcess> {
    windows_list_gateway_processes()
}

#[cfg(windows)]
fn kill_gateway_process(proc: &GatewayProcess) -> bool {
    windows_kill_pid(proc.pid)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn list_gateway_processes() -> Vec<GatewayProcess> {
    linux_list_gateway_processes()
}

#[cfg(target_os = "macos")]
fn list_gateway_processes() -> Vec<GatewayProcess> {
    macos_list_gateway_processes()
}

#[cfg(unix)]
fn kill_gateway_process(proc: &GatewayProcess) -> bool {
    unix_kill_pid(proc.pid)
}

#[cfg(windows)]
fn windows_list_gateway_processes() -> Vec<GatewayProcess> {
    use std::mem::{size_of, zeroed};
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return Vec::new();
        }
        let mut entry: PROCESSENTRY32W = zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut out: Vec<(GatewayProcess, u32)> = Vec::new();
        // Every pid in the snapshot, so a gateway's parent can be named even though
        // the walk may reach the parent after the child.
        let mut names: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
        if Process32FirstW(snap, &mut entry) != 0 {
            loop {
                let basename = widestr_to_string(&entry.szExeFile);
                names.insert(entry.th32ProcessID, basename.clone());
                if is_gateway_basename(&basename) {
                    let pid = entry.th32ProcessID;
                    let path = windows_process_path(pid);
                    out.push((
                        GatewayProcess {
                            pid,
                            path,
                            basename,
                            parent: None,
                        },
                        entry.th32ParentProcessID,
                    ));
                }
                if Process32NextW(snap, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
        // Resolve after the walk: a parent can appear later in the snapshot than
        // its child, so this cannot be done inline.
        out.into_iter()
            .map(|(mut proc, ppid)| {
                proc.parent = names
                    .get(&ppid)
                    .filter(|_| windows_parent_predates_child(ppid, proc.pid))
                    .map(|basename| ParentProcess {
                        pid: ppid,
                        basename: basename.clone(),
                    });
                proc
            })
            .collect()
    }
}

/// Guard against a recycled parent pid naming an unrelated application.
///
/// Unlike Unix, Windows never reparents an orphan and never clears the dead
/// parent's pid from the child, so `th32ParentProcessID` dangles once the parent
/// exits - and orphaned gateways are exactly the population the reaper exists to
/// clean up. Windows then recycles pids freely, so the dangling value can point at
/// something started later, and the user is told to restart an app that never
/// spawned a gateway (#542 review).
///
/// A real parent always starts before its child. Fails closed: if either creation
/// time is unreadable we drop the attribution rather than guess, because a wrong
/// app name is worse than none.
#[cfg(windows)]
fn windows_parent_predates_child(ppid: u32, child_pid: u32) -> bool {
    match (
        windows_process_start_time(ppid),
        windows_process_start_time(child_pid),
    ) {
        (Some(parent), Some(child)) => parent <= child,
        _ => false,
    }
}

/// Process creation time as a raw FILETIME tick count, for ordering only.
#[cfg(windows)]
fn windows_process_start_time(pid: u32) -> Option<u64> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut created: FILETIME = std::mem::zeroed();
        let mut exited: FILETIME = std::mem::zeroed();
        let mut kernel: FILETIME = std::mem::zeroed();
        let mut user: FILETIME = std::mem::zeroed();
        let ok = GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user);
        CloseHandle(handle);
        if ok == 0 {
            return None;
        }
        Some(((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64)
    }
}

#[cfg(windows)]
fn widestr_to_string(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

#[cfg(windows)]
fn windows_process_path(pid: u32) -> Option<PathBuf> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut size);
        CloseHandle(handle);
        if ok == 0 || size == 0 {
            return None;
        }
        Some(PathBuf::from(String::from_utf16_lossy(
            &buf[..size as usize],
        )))
    }
}

#[cfg(windows)]
fn windows_kill_pid(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if handle.is_null() {
            // Fall back to taskkill when OpenProcess is denied.
            return windows_taskkill_pid(pid);
        }
        let ok = TerminateProcess(handle, 1) != 0;
        CloseHandle(handle);
        if ok {
            true
        } else {
            windows_taskkill_pid(pid)
        }
    }
}

#[cfg(windows)]
fn windows_taskkill_pid(pid: u32) -> bool {
    use std::os::windows::process::CommandExt;
    let mut cmd = std::process::Command::new("taskkill");
    cmd.args(["/F", "/PID", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    cmd.status().map(|s| s.success()).unwrap_or(false)
}

/// Linux: `/proc/<pid>/exe` + `/proc/<pid>/comm`.
#[cfg(all(unix, not(target_os = "macos")))]
fn linux_list_gateway_processes() -> Vec<GatewayProcess> {
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for ent in rd.flatten() {
        let name = ent.file_name();
        let name = name.to_string_lossy();
        if !name.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let pid: u32 = match name.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        let comm_path = ent.path().join("comm");
        let basename = std::fs::read_to_string(&comm_path)
            .unwrap_or_default()
            .trim()
            .to_string();
        if !is_gateway_basename(&basename) {
            // Also check exe basename — comm is truncated to 15 chars
            // (`toolport-gateway` is 16), so post-upgrade detection depends on
            // the exe symlink. Strip ` (deleted)` for basename matching only;
            // keep it on `path` so keep-paths miss and the old inode is reaped
            // (WS4-3).
            let exe = std::fs::read_link(ent.path().join("exe")).ok();
            let Some(ref exe_path) = exe else {
                continue;
            };
            let exe_base = basename_from_exe_link(exe_path);
            if !is_gateway_basename(&exe_base) {
                continue;
            }
            out.push(GatewayProcess {
                pid,
                path: exe,
                basename: exe_base,
                parent: linux_parent_of(pid),
            });
            continue;
        }
        let path = std::fs::read_link(ent.path().join("exe")).ok();
        out.push(GatewayProcess {
            pid,
            path,
            basename,
            parent: linux_parent_of(pid),
        });
    }
    out
}

/// Parent application of a pid, from `/proc/<pid>/status`.
#[cfg(all(unix, not(target_os = "macos")))]
fn linux_parent_of(pid: u32) -> Option<ParentProcess> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let ppid: u32 = status
        .lines()
        .find_map(|line| line.strip_prefix("PPid:"))?
        .trim()
        .parse()
        .ok()?;
    // pid 0 is the kernel scheduler, never an application to restart.
    if ppid == 0 {
        return None;
    }
    // `comm` is truncated to 15 chars, which is fine for a display name.
    let basename = std::fs::read_to_string(format!("/proc/{ppid}/comm"))
        .ok()?
        .trim()
        .to_string();
    if basename.is_empty() {
        return None;
    }
    Some(ParentProcess {
        pid: ppid,
        basename,
    })
}

/// macOS: `ps` for pid + accounting name (`ucomm`), then `proc_pidpath` for the
/// real executable path. Do not use `-axo pid= comm=` as a single argv word —
/// Apple's `ps` treats that as a broken `-o` operand and exits 1 with empty
/// stdout (WS4-1). Prefer `ucomm` (basename) over `comm` (argv[0] full path).
#[cfg(target_os = "macos")]
fn macos_list_gateway_processes() -> Vec<GatewayProcess> {
    let Ok(out) = std::process::Command::new("ps")
        .args(["-ax", "-o", "pid=", "-o", "ppid=", "-o", "ucomm="])
        .output()
    else {
        return Vec::new();
    };
    // Command::output returns Ok even when ps exits non-zero; a usage error
    // previously returned an empty list with no log (WS4-1 / WS4-8).
    if !out.status.success() {
        eprintln!(
            "toolport: ps failed listing gateway processes (status={}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&out.stdout);
    // One pass over the whole table: `-ax` already lists every process, so the
    // parent's name is here and needs no second `ps` (SOU-435).
    let rows: Vec<(u32, u32, String)> = text
        .lines()
        .filter_map(parse_ps_pid_ppid_name_line)
        .collect();
    let names: std::collections::HashMap<u32, &str> = rows
        .iter()
        .map(|(pid, _, ucomm)| (*pid, ucomm.as_str()))
        .collect();

    let mut procs = Vec::new();
    for (pid, ppid, ucomm) in &rows {
        // ucomm is the accounting basename; filter before the more expensive path lookup.
        if !is_gateway_basename(ucomm) {
            continue;
        }
        let path = macos_proc_pidpath(*pid);
        let basename = path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| ucomm.clone());
        if !is_gateway_basename(&basename) {
            continue;
        }
        // pid 1 is launchd, never an application a user can restart.
        let parent = (*ppid > 1)
            .then(|| names.get(ppid))
            .flatten()
            .map(|name| ParentProcess {
                pid: *ppid,
                basename: (*name).to_string(),
            });
        procs.push(GatewayProcess {
            pid: *pid,
            path,
            basename,
            parent,
        });
    }
    procs
}

/// Full executable path for a pid via libproc (handles spaces and symlinks).
#[cfg(target_os = "macos")]
fn macos_proc_pidpath(pid: u32) -> Option<PathBuf> {
    // proc_pidpath is in libSystem on macOS; no extra crate needed.
    extern "C" {
        fn proc_pidpath(pid: i32, buffer: *mut std::ffi::c_void, buffersize: u32) -> i32;
    }
    let mut buf = [0u8; 4096];
    let n = unsafe {
        proc_pidpath(
            pid as i32,
            buf.as_mut_ptr() as *mut std::ffi::c_void,
            buf.len() as u32,
        )
    };
    if n <= 0 {
        return None;
    }
    let s = std::str::from_utf8(&buf[..n as usize]).ok()?;
    if s.is_empty() {
        return None;
    }
    Some(PathBuf::from(s))
}

#[cfg(unix)]
fn unix_kill_pid(pid: u32) -> bool {
    // SIGTERM first, then SIGKILL — matches polite shutdown without depending on libc.
    let term = std::process::Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !term {
        return std::process::Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
    }
    std::thread::sleep(std::time::Duration::from_millis(100));
    // Still alive?
    let alive = std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if alive {
        return std::process::Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
    }
    true
}

/// Last path segment, regardless of OS path separator (client configs store Windows paths).
fn path_basename(stored: &str) -> &str {
    stored.rsplit(['\\', '/']).next().unwrap_or(stored)
}

/// Whether a stored client config still points at an install-dir (unversioned) gateway.
pub fn is_unversioned_install_gateway_path(stored: &str) -> bool {
    let lower = stored.to_ascii_lowercase();
    if lower.contains("conduit-gateway") {
        return true;
    }
    let name_lower = path_basename(stored).to_ascii_lowercase();
    if name_lower != "toolport-gateway.exe" && name_lower != "conduit-gateway.exe" {
        return false;
    }
    // Unversioned basename outside Conduit/bin → install dir or other stale layout.
    !(lower.contains("\\conduit\\bin\\") || lower.contains("/conduit/bin/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(version: &str, keep: &[&str], kill_all: bool) -> ReapContext {
        ReapContext {
            current_version: version.into(),
            keep_paths: keep.iter().map(PathBuf::from).collect(),
            keep_pids: Vec::new(),
            kill_all,
        }
    }

    fn proc(pid: u32, basename: &str, path: Option<&str>) -> GatewayProcess {
        GatewayProcess {
            pid,
            basename: basename.into(),
            path: path.map(PathBuf::from),
            parent: None,
        }
    }

    /// Same, with an attributed parent application.
    fn proc_with_parent(
        pid: u32,
        basename: &str,
        path: Option<&str>,
        parent_pid: u32,
        parent_name: &str,
    ) -> GatewayProcess {
        GatewayProcess {
            parent: Some(ParentProcess {
                pid: parent_pid,
                basename: parent_name.into(),
            }),
            ..proc(pid, basename, path)
        }
    }

    #[test]
    fn updater_report_serializes_complete_shutdown_and_recovery_evidence() {
        let client = ClientNeedingRestart {
            client: "Cursor.exe".into(),
            client_pid: 77,
            gateway: "toolport-gateway.exe".into(),
        };
        let report = ReapReport {
            killed: vec!["toolport-gateway.exe (pid 10)".into()],
            kept: vec![],
            failed: vec!["toolport-gateway.exe (pid 11)".into()],
            remaining: vec!["toolport-gateway.exe (pid 11)".into()],
            needs_restart: vec![],
            restart_clients: vec![client],
            unattributed_external_stopped: vec!["toolport-gateway.exe (pid 12)".into()],
        };

        let value = serde_json::to_value(report).expect("serialize updater reaper report");
        assert_eq!(value["killed"][0], "toolport-gateway.exe (pid 10)");
        assert_eq!(value["failed"][0], "toolport-gateway.exe (pid 11)");
        assert_eq!(value["remaining"][0], "toolport-gateway.exe (pid 11)");
        assert_eq!(value["restartClients"][0]["client"], "Cursor.exe");
        assert_eq!(value["restartClients"][0]["clientPid"], 77);
        assert_eq!(
            value["unattributedExternalStopped"][0],
            "toolport-gateway.exe (pid 12)"
        );
    }

    #[test]
    fn updater_recovery_names_only_safely_attributed_external_clients() {
        let external = proc_with_parent(
            10,
            "toolport-gateway.exe",
            Some(r"C:\Toolport\toolport-gateway.exe"),
            77,
            "Cursor.exe",
        );
        assert_eq!(
            restart_client_for(&external),
            Some(ClientNeedingRestart {
                client: "Cursor.exe".into(),
                client_pid: 77,
                gateway: "toolport-gateway.exe".into(),
            })
        );

        let unreadable = proc_with_parent(11, "toolport-gateway.exe", None, 78, "Claude.exe");
        assert!(restart_client_for(&unreadable).is_none());
        let mut report = ReapReport::default();
        record_restart_client(&mut report, &unreadable);
        assert_eq!(
            report.unattributed_external_stopped,
            vec!["toolport-gateway.exe (pid 11)"]
        );

        let supervised = proc_with_parent(
            12,
            "toolport-gateway.exe",
            Some(r"C:\Toolport\toolport-gateway.exe"),
            79,
            "toolport.exe",
        );
        assert!(restart_client_for(&supervised).is_none());
        record_restart_client(&mut report, &supervised);
        assert_eq!(report.unattributed_external_stopped.len(), 1);

        let orphan = proc(13, "toolport-gateway.exe", None);
        record_restart_client(&mut report, &orphan);
        assert_eq!(report.unattributed_external_stopped.len(), 1);
    }

    #[test]
    fn unversioned_install_path_detected() {
        assert!(is_unversioned_install_gateway_path(
            r"C:\Users\me\AppData\Local\Toolport\toolport-gateway.exe"
        ));
        assert!(!is_unversioned_install_gateway_path(
            r"C:\Users\me\AppData\Roaming\Conduit\bin\toolport-gateway-1.6.0.exe"
        ));
        assert!(is_unversioned_install_gateway_path(
            "/Applications/Toolport.app/Contents/MacOS/conduit-gateway"
        ));
    }

    #[test]
    fn decide_keeps_current_versioned_basename() {
        // SOU-306 regression: current versioned Windows binary must survive.
        let c = ctx(
            "1.9.6",
            &[r"C:\Users\me\AppData\Roaming\Toolport\bin\toolport-gateway-1.9.6.exe"],
            false,
        );
        assert_eq!(
            decide_reap(
                &proc(
                    1,
                    "toolport-gateway-1.9.6.exe",
                    Some(r"C:\Users\me\AppData\Roaming\Toolport\bin\toolport-gateway-1.9.6.exe")
                ),
                &c
            ),
            ReapDecision::Keep
        );
    }

    #[test]
    fn decide_keeps_current_content_addressed_basename_without_a_path() {
        let c = ctx("1.9.6", &[], false);
        assert_eq!(
            decide_reap(
                &proc(10, "toolport-gateway-1.9.6-0123456789ab.exe", None),
                &c
            ),
            ReapDecision::Keep
        );
        assert!(
            !basename_matches_current_version("toolport-gateway-1.9.6-not-a-digest.exe", "1.9.6"),
            "only the publisher's 12-hex content suffix is current"
        );
    }

    #[test]
    fn decide_kills_older_versioned_basenames() {
        let c = ctx(
            "1.9.6",
            &[r"C:\Users\me\AppData\Roaming\Toolport\bin\toolport-gateway-1.9.6.exe"],
            false,
        );
        for (name, path) in [
            (
                "toolport-gateway-1.9.4.exe",
                r"C:\Users\me\AppData\Roaming\Toolport\bin\toolport-gateway-1.9.4.exe",
            ),
            (
                "toolport-gateway-1.9.5.exe",
                r"C:\Users\me\AppData\Roaming\Toolport\bin\toolport-gateway-1.9.5.exe",
            ),
            (
                "conduit-gateway-1.7.2.exe",
                r"C:\Users\me\AppData\Roaming\Conduit\bin\conduit-gateway-1.7.2.exe",
            ),
        ] {
            assert_eq!(
                decide_reap(&proc(2, name, Some(path)), &c),
                ReapDecision::Kill,
                "{name}"
            );
        }
    }

    #[test]
    fn decide_kills_unversioned_when_path_differs_from_keep() {
        // macOS / AppImage: same basename, obsolete path.
        let current = "/Users/me/Library/Application Support/Toolport/bin/toolport-gateway";
        let c = ctx("1.9.6", &[current], false);
        assert_eq!(
            decide_reap(
                &proc(
                    3,
                    "toolport-gateway",
                    Some("/Applications/Toolport.app/Contents/MacOS/toolport-gateway")
                ),
                &c
            ),
            ReapDecision::Kill
        );
        assert_eq!(
            decide_reap(&proc(4, "toolport-gateway", Some(current)), &c),
            ReapDecision::Keep
        );
    }

    #[test]
    fn decide_keeps_dev_target_paths() {
        let c = ctx(
            "1.9.6",
            &[r"C:\Users\me\AppData\Roaming\Toolport\bin\toolport-gateway-1.9.6.exe"],
            false,
        );
        assert_eq!(
            decide_reap(
                &proc(
                    5,
                    "toolport-gateway.exe",
                    Some(r"C:\projects\toolport\src-tauri\target\debug\toolport-gateway.exe")
                ),
                &c
            ),
            ReapDecision::Keep
        );
    }

    #[test]
    fn decide_keeps_unknown_location_unversioned() {
        // Do not kill a foreign binary that happens to share the name.
        let c = ctx(
            "1.9.6",
            &[r"C:\Users\me\AppData\Roaming\Toolport\bin\toolport-gateway-1.9.6.exe"],
            false,
        );
        assert_eq!(
            decide_reap(
                &proc(
                    6,
                    "toolport-gateway",
                    Some("/opt/other-vendor/toolport-gateway")
                ),
                &c
            ),
            ReapDecision::Keep
        );
    }

    #[test]
    fn decide_basename_only_kills_old_versioned() {
        let c = ctx("1.9.6", &[], false);
        assert_eq!(
            decide_reap(&proc(7, "toolport-gateway-1.9.4.exe", None), &c),
            ReapDecision::Kill
        );
        assert_eq!(
            decide_reap(&proc(8, "toolport-gateway-1.9.6.exe", None), &c),
            ReapDecision::Keep
        );
        assert_eq!(
            decide_reap(&proc(9, "toolport-gateway.exe", None), &c),
            ReapDecision::Keep
        );
    }

    #[test]
    fn decide_keeps_versioned_looking_stranger_outside_install() {
        // CodeRabbit: versioned branch must not kill /opt/other-vendor/toolport-gateway-1.0.0
        // style strangers (and non-version suffixes must not count as versioned).
        let c = ctx(
            "1.9.6",
            &[r"C:\Users\me\AppData\Roaming\Toolport\bin\toolport-gateway-1.9.6.exe"],
            false,
        );
        assert_eq!(
            decide_reap(
                &proc(
                    11,
                    "toolport-gateway-1.0.0",
                    Some("/opt/other-vendor/toolport-gateway-1.0.0")
                ),
                &c
            ),
            ReapDecision::Keep
        );
        assert_eq!(
            decide_reap(
                &proc(
                    12,
                    "toolport-gateway-shim",
                    Some(r"C:\Users\me\AppData\Roaming\Toolport\bin\toolport-gateway-shim.exe")
                ),
                &c
            ),
            // Non-version suffix under our path, not in keep list → unversioned-path kill
            ReapDecision::Kill
        );
    }

    #[test]
    fn version_suffix_requires_digit_and_dot() {
        assert!(looks_like_version_suffix("1.9.4"));
        assert!(looks_like_version_suffix("1.9.4-beta.1"));
        assert!(!looks_like_version_suffix("shim"));
        assert!(!looks_like_version_suffix("helper"));
        assert!(!looks_like_version_suffix("aarch64-apple-darwin"));
        assert!(!is_versioned_gateway_basename("toolport-gateway-shim.exe"));
        assert!(is_versioned_gateway_basename("toolport-gateway-1.9.4.exe"));
    }

    #[test]
    fn version_parsing_survives_a_two_digit_minor() {
        // 1.9 -> 1.10 is the transition where a version parser usually bites, and
        // every other case here stops at a single-digit minor. Nothing compares
        // versions by ordering (a lexical compare would put "1.10.0" before
        // "1.9.7"), so equality is all that has to hold, but it has to hold for
        // the reaper to recognise its own image after the bump.
        assert!(looks_like_version_suffix("1.10.0"));
        assert!(looks_like_version_suffix("1.10.0-rc.1"));
        assert!(is_versioned_gateway_basename("toolport-gateway-1.10.0.exe"));
        assert!(is_versioned_gateway_basename("conduit-gateway-1.10.0"));

        // The current image is kept and an older one is killed across the boundary,
        // in both directions, so neither is mistaken for the other.
        assert!(basename_matches_current_version(
            "toolport-gateway-1.10.0.exe",
            "1.10.0"
        ));
        assert!(!basename_matches_current_version(
            "toolport-gateway-1.9.7.exe",
            "1.10.0"
        ));
        assert!(!basename_matches_current_version(
            "toolport-gateway-1.10.0.exe",
            "1.9.7"
        ));

        let c = ctx(
            "1.10.0",
            &[r"C:\Users\me\AppData\Roaming\Toolport\bin\toolport-gateway-1.10.0.exe"],
            false,
        );
        assert_eq!(
            decide_reap(
                &proc(
                    1,
                    "toolport-gateway-1.10.0.exe",
                    Some(r"C:\Users\me\AppData\Roaming\Toolport\bin\toolport-gateway-1.10.0.exe")
                ),
                &c
            ),
            ReapDecision::Keep,
            "the current 1.10.0 image must survive its own reaper"
        );
        assert_eq!(
            decide_reap(
                &proc(
                    2,
                    "toolport-gateway-1.9.7.exe",
                    Some(r"C:\Users\me\AppData\Roaming\Toolport\bin\toolport-gateway-1.9.7.exe")
                ),
                &c
            ),
            ReapDecision::Kill,
            "a pre-bump 1.9.7 image is obsolete once 1.10.0 is current"
        );
    }

    #[test]
    fn decide_kill_all_kills_everything_gateway() {
        let c = ctx("1.9.6", &[r"C:\keep\toolport-gateway-1.9.6.exe"], true);
        assert_eq!(
            decide_reap(
                &proc(
                    10,
                    "toolport-gateway-1.9.6.exe",
                    Some(r"C:\keep\toolport-gateway-1.9.6.exe")
                ),
                &c
            ),
            ReapDecision::Kill
        );
    }

    /// SOU-432: a protected pid outranks every other rule, including `kill_all`
    /// and the Linux ` (deleted)` policy that deliberately misses keep-paths.
    #[test]
    fn keep_pids_outrank_every_kill_rule() {
        let deleted = PathBuf::from("/home/u/.local/share/toolport/toolport-gateway (deleted)");
        let mut stale = ctx(
            "1.9.6",
            &["/home/u/.local/share/toolport/toolport-gateway"],
            false,
        );
        let me = proc(4242, "toolport-gateway", deleted.to_str());
        // Control: unprotected, this is exactly the WS4-3 Kill case.
        assert_eq!(decide_reap(&me, &stale), ReapDecision::Kill);
        stale.keep_pids = vec![4242];
        assert_eq!(
            decide_reap(&me, &stale),
            ReapDecision::Keep,
            "a reaper must never kill the process it runs in"
        );
        // A different pid at the same path is still reaped.
        assert_eq!(
            decide_reap(&proc(99, "toolport-gateway", deleted.to_str()), &stale),
            ReapDecision::Kill
        );
        // kill_all does not override the guard either.
        let mut all = ctx("1.9.6", &[], true);
        all.keep_pids = vec![4242];
        assert_eq!(decide_reap(&me, &all), ReapDecision::Keep);
        assert_eq!(
            decide_reap(&proc(99, "toolport-gateway", deleted.to_str()), &all),
            ReapDecision::Kill
        );
    }

    /// The shipped call sites must actually seed the guard, not just support it.
    #[test]
    fn production_reap_contexts_protect_the_current_process() {
        for ctx in [
            ReapContext {
                current_version: "1.9.6".into(),
                keep_paths: Vec::new(),
                keep_pids: vec![std::process::id()],
                kill_all: true,
            },
            ReapContext {
                current_version: "1.9.6".into(),
                keep_paths: default_keep_paths(),
                keep_pids: vec![std::process::id()],
                kill_all: false,
            },
        ] {
            assert_eq!(
                decide_reap(
                    &proc(
                        std::process::id(),
                        "toolport-gateway",
                        Some("/opt/toolport/toolport-gateway")
                    ),
                    &ctx
                ),
                ReapDecision::Keep
            );
        }
    }

    #[test]
    fn is_gateway_basename_accepts_versioned_and_plain() {
        assert!(is_gateway_basename("toolport-gateway.exe"));
        assert!(is_gateway_basename("toolport-gateway-1.9.6.exe"));
        assert!(is_gateway_basename("conduit-gateway"));
        assert!(!is_gateway_basename("cursor.exe"));
        assert!(!is_gateway_basename("my-toolport-gateway-shim"));
        // Prefix only — must be exact stem or stem-version
        assert!(!is_gateway_basename("toolport-gatewayed"));
    }

    /// WS4-3: post-upgrade `/proc/.../exe` ends with ` (deleted)`. Strip it for
    /// basename detection only — leave it on the stored path so keep-paths miss
    /// and the pre-upgrade inode is killed.
    #[test]
    fn strip_deleted_exe_suffix_and_basename() {
        assert_eq!(
            strip_deleted_exe_suffix("/usr/bin/toolport-gateway (deleted)"),
            "/usr/bin/toolport-gateway"
        );
        assert_eq!(
            strip_deleted_exe_suffix("/usr/bin/toolport-gateway"),
            "/usr/bin/toolport-gateway"
        );
        let deleted = PathBuf::from("/home/u/.local/share/toolport/toolport-gateway (deleted)");
        assert_eq!(basename_from_exe_link(&deleted), "toolport-gateway");
        assert!(is_gateway_basename(&basename_from_exe_link(&deleted)));
        // Without the strip, file_name keeps the marker and matching fails.
        assert!(!is_gateway_basename("toolport-gateway (deleted)"));
        let live = PathBuf::from("/usr/bin/toolport-gateway-1.9.4");
        assert_eq!(basename_from_exe_link(&live), "toolport-gateway-1.9.4");

        let keep = PathBuf::from("/home/u/.local/share/toolport/toolport-gateway");
        let c = ReapContext {
            current_version: "1.9.6".into(),
            keep_paths: vec![keep.clone()],
            keep_pids: Vec::new(),
            kill_all: false,
        };
        // Fresh process at the keep path survives.
        assert_eq!(
            decide_reap(
                &GatewayProcess {
                    pid: 1,
                    path: Some(keep),
                    basename: "toolport-gateway".into(),
                    parent: None,
                },
                &c
            ),
            ReapDecision::Keep
        );
        // In-place upgrade: raw `(deleted)` path must miss keep-paths → Kill.
        assert_eq!(
            decide_reap(
                &GatewayProcess {
                    pid: 2,
                    path: Some(deleted),
                    basename: "toolport-gateway".into(),
                    parent: None,
                },
                &c
            ),
            ReapDecision::Kill
        );
    }

    /// WS4-1 / WS4-8: pure parse of `ps -o pid= -o ppid= -o ucomm=` rows.
    /// Does not prove the `ps` argv itself - that still needs a macOS smoke.
    #[test]
    fn parse_ps_pid_ppid_name_line_accepts_padded_columns_and_ucomm() {
        assert_eq!(
            parse_ps_pid_ppid_name_line("  123   1 toolport-gateway"),
            Some((123, 1, "toolport-gateway".into()))
        );
        assert_eq!(
            parse_ps_pid_ppid_name_line("45678 4321 toolport-gateway-1.9.4"),
            Some((45678, 4321, "toolport-gateway-1.9.4".into()))
        );
        assert_eq!(parse_ps_pid_ppid_name_line(""), None);
        assert_eq!(
            parse_ps_pid_ppid_name_line("not-a-pid 1 toolport-gateway"),
            None
        );
        // A missing ppid column must not be read as the name, which is how a wrong
        // `-o` argv would silently produce nonsense parents rather than nothing.
        assert_eq!(parse_ps_pid_ppid_name_line("99 toolport-gateway"), None);
        assert_eq!(parse_ps_pid_ppid_name_line("99 1"), None);
        assert_eq!(parse_ps_pid_ppid_name_line("99"), None);
        // Full-path comm= style still parses; basename filter is applied by the caller.
        assert_eq!(
            parse_ps_pid_ppid_name_line(
                "  42 7 /Applications/Toolport.app/Contents/MacOS/toolport-gateway"
            ),
            Some((
                42,
                7,
                "/Applications/Toolport.app/Contents/MacOS/toolport-gateway".into()
            ))
        );
        // A name with spaces survives, since only the first two columns are positional.
        assert_eq!(
            parse_ps_pid_ppid_name_line("5 2 Some App Helper"),
            Some((5, 2, "Some App Helper".into()))
        );
    }

    #[test]
    fn manifest_roundtrip_fields() {
        let m = GatewayManifest {
            version: "1.6.0".into(),
            path: r"C:\x\toolport-gateway-1.6.0.exe".into(),
            size: 42,
        };
        let json = serde_json::to_string(&m).unwrap();
        let back: GatewayManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn same_version_rebuild_gets_content_addressed_leaf() {
        let base =
            PathBuf::from(r"C:\Users\u\AppData\Roaming\Toolport\bin\toolport-gateway-1.12.0.exe");
        assert_eq!(
            content_addressed_dest(
                &base,
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
            ),
            PathBuf::from(
                r"C:\Users\u\AppData\Roaming\Toolport\bin\toolport-gateway-1.12.0-0123456789ab.exe"
            )
        );
    }

    #[test]
    fn readonly_publish_selection_finds_the_image_publication_would_reuse() {
        let dir = ScratchDir::new("readonly-publish-dest");
        let src = dir.join("bundled-gateway");
        let base = dir.join("toolport-gateway-1.12.0");
        std::fs::write(&src, b"current image").unwrap();
        std::fs::write(&base, b"current image").unwrap();
        assert_eq!(
            existing_publish_destination_from(&src, &base),
            Some(base.clone())
        );

        std::fs::write(&base, b"older image").unwrap();
        let digest = file_sha256(&src).unwrap();
        let addressed = content_addressed_dest(&base, &digest);
        std::fs::write(&addressed, b"current image").unwrap();
        assert_eq!(
            existing_publish_destination_from(&src, &base),
            Some(addressed)
        );
    }

    #[test]
    fn unreadable_same_version_destination_uses_content_addressed_leaf() {
        let dir = ScratchDir::new("publish-unreadable-base");
        let base = dir.join("toolport-gateway-1.12.0.exe");
        std::fs::create_dir(&base).expect("directory stand-in should be unreadable as a file");
        let digest = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        assert_eq!(
            select_publish_dest(&base, digest),
            dir.join("toolport-gateway-1.12.0-0123456789ab.exe")
        );
    }

    /// A small, long-running binary to stand in for a gateway image, and the
    /// arguments that keep it alive.
    ///
    /// Searches `PATH` rather than assuming `/bin/sleep`, so the tests still run on
    /// layouts that put it elsewhere (NixOS, busybox images, minimal containers).
    ///
    /// The candidate is *probed*, never trusted. Ubuntu 26.04 ships uutils coreutils
    /// as a single multi-call binary that every `/usr/bin/<tool>` hardlinks to, and it
    /// dispatches on `argv[0]`: copied to `toolport-gateway` it prints
    /// `coreutils: unknown program 'toolport-gateway'` and exits, so every process
    /// [`SpawnedGateway`] spawns is already dead by the time the enumeration under
    /// test runs and three reaper tests fail for a reason that has nothing to do with
    /// the reaper. Nothing on disk gives it away - the copy is a plain ELF, the link
    /// is a hardlink rather than a symlink, and `canonicalize` still ends in `sleep` -
    /// so the only reliable check is behavioural: copy the candidate under a foreign
    /// name, run it, and see whether it is still alive a moment later.
    ///
    /// The fallback is a shell blocking on `read`, for two properties `sleep` cannot
    /// be relied on for here: a shell must ignore `argv[0]` (POSIX gives it meaning
    /// only for a leading `-`), and reading a pipe this process holds open blocks
    /// forever without forking a child that would outlive the kill.
    ///
    /// Probed once per test binary and memoized, so the 250ms costs one test run, not
    /// one spawn.
    #[cfg(all(unix, not(target_os = "macos")))]
    fn stand_in_binary() -> &'static (PathBuf, Vec<&'static str>) {
        static CHOICE: std::sync::OnceLock<(PathBuf, Vec<&'static str>)> =
            std::sync::OnceLock::new();
        CHOICE.get_or_init(|| {
            let mut candidates: Vec<(PathBuf, Vec<&'static str>)> = Vec::new();
            let mut push = |path: PathBuf, args: Vec<&'static str>| {
                if path.is_file() && !candidates.iter().any(|(p, _)| *p == path) {
                    candidates.push((path, args));
                }
            };
            if let Some(path) = std::env::var_os("PATH") {
                for dir in std::env::split_paths(&path) {
                    push(dir.join("sleep"), vec!["300"]);
                }
            }
            for fixed in ["/bin/sleep", "/usr/bin/sleep"] {
                push(PathBuf::from(fixed), vec!["300"]);
            }
            for shell in ["/bin/sh", "/bin/dash", "/bin/bash", "/usr/bin/sh"] {
                push(PathBuf::from(shell), vec!["-c", "read _standin"]);
            }
            candidates
                .into_iter()
                .find(|(path, args)| stand_in_survives_rename(path, args))
                .expect(
                    "no binary on this system stays alive when copied to a gateway's \
                     name, so no reaper test can spawn anything to enumerate",
                )
        })
    }

    /// True when `source`, copied under a gateway's name and run with `args`, is still
    /// running a moment later. The behavioural half of [`stand_in_binary`].
    ///
    /// Every failure is a `false` rather than a panic: a candidate that cannot be
    /// copied, chmod'd or spawned is simply not the one to use, and the next one gets
    /// a turn. That includes a spurious `ETXTBSY` from another thread's fork, which
    /// costs a usable `sleep` its place and falls through to the shell - a worse
    /// stand-in, not a broken one.
    #[cfg(all(unix, not(target_os = "macos")))]
    fn stand_in_survives_rename(source: &Path, args: &[&str]) -> bool {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = std::env::temp_dir().join(format!(
            "toolport-standin-probe-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        if std::fs::create_dir_all(&dir).is_err() {
            return false;
        }
        let exe = dir.join("toolport-gateway");
        let alive = (|| {
            std::fs::copy(source, &exe).ok()?;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).ok()?;
            let mut child = std::process::Command::new(&exe)
                .args(args)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .ok()?;
            // Long enough for a multi-call binary to reject the name and exit, short
            // enough that a healthy candidate is not worth optimising further.
            std::thread::sleep(std::time::Duration::from_millis(250));
            let alive = matches!(child.try_wait(), Ok(None));
            let _ = child.kill();
            let _ = child.wait();
            Some(alive)
        })()
        .unwrap_or(false);
        let _ = std::fs::remove_dir_all(&dir);
        alive
    }

    /// Scratch tree removed on drop.
    ///
    /// Every test in this file that needs real files on disk goes through this, so
    /// cleanup happens even when an assertion panics and the whole unique root is
    /// removed rather than just its `Toolport` leaf. Doing it by hand at the end of
    /// each test got both of those wrong: a failing test leaked its tree, and a
    /// passing one still leaked the tree's parent, once per test per run.
    struct ScratchDir {
        /// Handed to tests. Ends in a literal `Toolport` segment on purpose:
        /// `decide_reap` only kills an image under a path
        /// `path_looks_like_our_install` recognizes, so a bare temp dir would be
        /// treated as a stranger's binary and kept.
        dir: PathBuf,
        /// The unique root actually removed on drop. Dropping only `dir` would leave
        /// its parent behind, leaking one empty directory per tag per run.
        root: PathBuf,
    }

    impl ScratchDir {
        /// Thread id as well as pid: the advice/prune tests run in parallel and each
        /// needs its own tree.
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "toolport-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).expect("create scratch root");
            // Canonicalize the ROOT and derive everything else from it, so `root`,
            // `dir` and every path handed to a test are in one form.
            //
            // It has to be canonical because `/proc/<pid>/exe` reports the kernel's
            // own resolved path: where TMPDIR resolves through a symlink (a container
            // layout, or a custom TMPDIR) an uncanonical path makes every
            // `read_link(...) == exe` comparison fail despite a successful exec.
            //
            // Canonicalizing only `dir` and falling back to the uncanonical path on
            // error was worse than not trying: it left `dir` and `root` in different
            // forms, and a failure surfaced 3s later as an unexplained readiness-poll
            // panic. The directory was just created, so failure here means a broken
            // environment and should say so.
            let root = match std::fs::canonicalize(&root) {
                Ok(canonical) => canonical,
                Err(e) => {
                    // `Self` does not exist yet, so `Drop` cannot clean up the
                    // directory just created. Remove it before unwinding.
                    let _ = std::fs::remove_dir_all(&root);
                    panic!("canonicalize scratch root {}: {e}", root.display());
                }
            };
            let dir = root.join("Toolport");
            std::fs::create_dir_all(&dir).expect("create scratch dir");
            Self { dir, root }
        }
    }

    /// So a `ScratchDir` can be used anywhere the old `PathBuf` helper was, without
    /// rewriting every call site.
    impl std::ops::Deref for ScratchDir {
        type Target = Path;

        fn deref(&self) -> &Path {
            &self.dir
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// A stand-in gateway process running from a scratch path.
    ///
    /// Kills and reaps the child on drop, so a failing assertion cannot leave a
    /// stray `sleep` behind.
    #[cfg(all(unix, not(target_os = "macos")))]
    struct SpawnedGateway {
        child: std::process::Child,
        exe: PathBuf,
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    impl SpawnedGateway {
        /// Copy the stand-in binary to `exe`, run it, and block until the exec has
        /// actually happened.
        ///
        /// `spawn()` returns before exec completes, so `/proc/<pid>` is not populated
        /// yet; polling the exe symlink is what makes the enumeration assertions
        /// deterministic rather than racy.
        fn at(exe: PathBuf) -> Self {
            use std::os::unix::fs::PermissionsExt as _;

            if let Some(parent) = exe.parent() {
                std::fs::create_dir_all(parent).expect("create gateway dir");
            }
            let (stand_in, stand_in_args) = stand_in_binary();
            std::fs::copy(stand_in, &exe).expect("copy stand-in binary");
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755))
                .expect("chmod +x");

            // Retry ETXTBSY rather than fail.
            //
            // Any thread that forks while this copy's write fd is open passes that fd
            // to its child, and the descriptor stays open across the child's own
            // fork-to-exec window. A file some process holds open for writing cannot
            // be exec'd, so the spawn here fails with "Text file busy".
            //
            // A mutex around copy-through-exec used to guard this, but it could only
            // ever serialize spawns going through *this helper*. The rest of the suite
            // forks freely, which is why the failure never appeared running
            // `--lib gateway_publish` alone and did appear in a full-suite run.
            // Retrying covers every forker, not just the cooperating ones. Measured:
            // with the mutex removed, retrying alone is 30/30 clean where the
            // unprotected version was 5/8.
            let mut waited = std::time::Duration::ZERO;
            let child = loop {
                match std::process::Command::new(&exe)
                    .args(stand_in_args)
                    // The stand-in may block on stdin rather than on a timer. `Child`
                    // owns the write end and nothing takes it, so the pipe stays open
                    // for exactly as long as this `SpawnedGateway` does.
                    .stdin(std::process::Stdio::piped())
                    .spawn()
                {
                    Ok(child) => break child,
                    // ETXTBSY. Matched on the raw errno so this does not depend on
                    // `ErrorKind::ExecutableFileBusy`, and the block is Linux-only.
                    Err(e)
                        if e.raw_os_error() == Some(26)
                            && waited < std::time::Duration::from_secs(5) =>
                    {
                        let step = std::time::Duration::from_millis(10);
                        std::thread::sleep(step);
                        waited += step;
                    }
                    Err(e) => panic!("spawn stand-in gateway {}: {e}", exe.display()),
                }
            };
            let me = Self { child, exe };

            let link = PathBuf::from(format!("/proc/{}/exe", me.pid()));
            for _ in 0..300 {
                if std::fs::read_link(&link)
                    .map(|p| p == me.exe)
                    .unwrap_or(false)
                {
                    return me;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            panic!("child never exec'd into {}", me.exe.display());
        }

        fn pid(&self) -> u32 {
            self.child.id()
        }

        /// Unlink the running image, which is what an in-place binary replacement
        /// (package upgrade, installer overwrite) does. `/proc/<pid>/exe` carries the
        /// ` (deleted)` marker from here on.
        fn unlink_image(&self) {
            std::fs::remove_file(&self.exe).expect("unlink running image");
        }

        /// Did the process exit within `budget`? Reaps the zombie, so a later `/proc`
        /// walk cannot see it.
        fn exited_within(&mut self, budget: std::time::Duration) -> bool {
            let deadline = std::time::Instant::now() + budget;
            loop {
                match self.child.try_wait() {
                    Ok(Some(_)) => return true,
                    Ok(None) => {}
                    Err(_) => return false,
                }
                if std::time::Instant::now() >= deadline {
                    return false;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }

        fn still_running(&mut self) -> bool {
            matches!(self.child.try_wait(), Ok(None))
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    impl Drop for SpawnedGateway {
        /// Only signals a child that has not already been reaped.
        ///
        /// `exited_within` reaps via `try_wait`, which frees the pid for reuse. A
        /// bare `kill()` afterwards would signal whatever now holds that number.
        /// `std` does not save us here: reaping with `try_wait` and then calling
        /// `Child::kill()` returns `Ok(())` rather than refusing, so the guard has to
        /// be explicit.
        fn drop(&mut self) {
            if matches!(self.child.try_wait(), Ok(None)) {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
    }

    /// Drives the real Linux enumerator against a real process.
    ///
    /// Linux is the one platform where detection has no margin. `/proc/<pid>/comm`
    /// is capped at 15 usable characters and `toolport-gateway` is 16, so `comm`
    /// can never match a Toolport-named gateway and detection depends *entirely*
    /// on the `/proc/<pid>/exe` fallback. (macOS differs: `ucomm` allows 16, so the
    /// name fits there exactly.) Every other test in this file is a pure fixture
    /// over `is_gateway_basename` / `decide_reap`, and none of them can catch a
    /// broken fallback, because the fallback is the part that reads the OS.
    ///
    /// That gap is not hypothetical: the sibling macOS path shipped with an argv
    /// Apple's `ps` rejects, returning zero processes, and the fixture tests stayed
    /// green throughout. CI runs ubuntu, so this path *can* be covered for real.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn linux_enumeration_finds_a_versioned_gateway_via_the_exe_fallback() {
        let dir = ScratchDir::new("reaper-enum");
        // Deliberately longer than comm's 15-char window so the truncation this
        // test exists for actually happens.
        let exe = dir.join("toolport-gateway-9.9.9");
        let gw = SpawnedGateway::at(exe.clone());
        let pid = gw.pid();

        // The premise: comm is truncated past recognition, so a hit below can only
        // have come from the exe fallback. If a future kernel widens comm this
        // assertion fires, which is the correct signal that the premise changed.
        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm"))
            .expect("read comm")
            .trim()
            .to_string();
        assert!(
            !is_gateway_basename(&comm),
            "comm {comm:?} matched is_gateway_basename on its own, so this test no \
             longer proves the /proc/<pid>/exe fallback works. Check whether comm's \
             width changed before touching the fallback."
        );

        let found = linux_list_gateway_processes();
        let hit = found.iter().find(|p| p.pid == pid).unwrap_or_else(|| {
            panic!(
                "linux_list_gateway_processes() missed pid {pid} running {}. comm was \
                 {comm:?}, which cannot match, so the /proc/<pid>/exe fallback is broken \
                 and no Toolport-named gateway would be reaped on Linux.",
                exe.display()
            )
        });

        assert_eq!(
            hit.basename, "toolport-gateway-9.9.9",
            "basename must come from the exe link, not truncated comm"
        );
        assert_eq!(
            hit.path.as_deref(),
            Some(exe.as_path()),
            "path must be the resolved exe so keep-path comparisons work"
        );
    }

    /// SBS-418: the `(deleted)` policy from #505, against a real unlinked process.
    ///
    /// Two halves that pull in opposite directions and have to hold at once:
    ///
    /// * the marker is **stripped for basename matching**, so an unlinked image is
    ///   still recognized as a gateway and enumerated at all;
    /// * the marker is **kept on the stored path**, so that image misses keep-paths
    ///   and is reaped even when the path it was launched from *is* the current
    ///   keep-path.
    ///
    /// The second half is why this needs a real process. It reversed twice inside 16
    /// minutes during development (e1242eb added a path strip, 2ba9f95 reverted it),
    /// and until now only `decide_reap` fixtures covered it — which cannot catch the
    /// enumerator handing `decide_reap` an already-stripped path, since the fixtures
    /// build that path themselves.
    ///
    /// Uses the unversioned name deliberately. `toolport-gateway` is 16 chars and
    /// `comm` caps at 15, so detection can only come from `/proc/<pid>/exe`, and the
    /// verdict can only come from the unversioned path-identity branch. The legacy
    /// `conduit-gateway` is exactly 15 and *does* match `comm`, which would mask a
    /// broken fallback.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn linux_deleted_image_is_enumerated_but_misses_its_own_keep_path() {
        let dir = ScratchDir::new("reaper-deleted");
        let exe = dir.join("bin/toolport-gateway");
        let gw = SpawnedGateway::at(exe.clone());
        gw.unlink_image();

        let found = linux_list_gateway_processes();
        let hit = found.iter().find(|p| p.pid == gw.pid()).unwrap_or_else(|| {
            panic!(
                "linux_list_gateway_processes() lost pid {} once its image was unlinked. \
                 The ` (deleted)` marker must be stripped for basename matching, or an \
                 in-place upgrade leaves the old inode running forever.",
                gw.pid()
            )
        });

        assert_eq!(
            hit.basename, "toolport-gateway",
            "basename must have the ` (deleted)` marker stripped"
        );
        let stored = hit
            .path
            .as_ref()
            .expect("an unlinked image must still carry a path");
        assert!(
            stored.to_string_lossy().ends_with(" (deleted)"),
            "path must KEEP the ` (deleted)` marker so the keep-path comparison misses; \
             got {}",
            stored.display()
        );

        // The launch path is the current keep-path and the process must still be
        // killed. That is the entire point of leaving the marker on.
        let ctx = ReapContext {
            current_version: "9.9.9".into(),
            keep_paths: vec![exe.clone()],
            keep_pids: Vec::new(),
            kill_all: false,
        };
        assert_eq!(
            decide_reap(hit, &ctx),
            ReapDecision::Kill,
            "an unlinked image launched from the keep path {} must still be reaped, or \
             an in-place upgrade keeps serving the old binary",
            exe.display()
        );

        // Control: same name, same kind of path, image intact. The marker is the only
        // difference between this and the case above, so it must flip the verdict.
        let live_dir = ScratchDir::new("reaper-deleted-control");
        let live_exe = live_dir.join("bin/toolport-gateway");
        let live_gw = SpawnedGateway::at(live_exe.clone());
        let live_hit = linux_list_gateway_processes()
            .into_iter()
            .find(|p| p.pid == live_gw.pid())
            .expect("control gateway was not enumerated");
        assert_eq!(
            decide_reap(
                &live_hit,
                &ReapContext {
                    keep_paths: vec![live_exe],
                    ..ctx
                }
            ),
            ReapDecision::Keep,
            "an intact image at its keep path must survive"
        );
    }

    /// `ScratchDir` removes its whole root, including when a test panics.
    ///
    /// The panic half is the point: cleanup used to be a `remove_dir_all` on the last
    /// line of each test, which is exactly the line an assertion failure skips, so a
    /// failing test leaked its tree and the binaries in it.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn scratch_dir_removes_its_root_on_drop_and_on_panic() {
        let root = {
            let dir = ScratchDir::new("reaper-cleanup");
            std::fs::write(dir.join("file"), b"x").expect("write into scratch dir");
            dir.root.clone()
        };
        assert!(
            !root.exists(),
            "scratch root {} survived a normal drop",
            root.display()
        );

        // The parent, not just the `Toolport` leaf: removing only the leaf is what
        // leaked one directory per test per run.
        let leaked = std::panic::catch_unwind(|| {
            let dir = ScratchDir::new("reaper-cleanup-panic");
            let root = dir.root.clone();
            std::fs::write(dir.join("file"), b"x").expect("write into scratch dir");
            std::panic::panic_any(root);
        })
        .expect_err("the closure must panic");
        let root = leaked
            .downcast::<PathBuf>()
            .expect("panic payload is the scratch root");
        assert!(
            !root.exists(),
            "scratch root {} survived a panicking test",
            root.display()
        );
    }

    /// SBS-418: a real `reap_with_context` pass over real processes.
    ///
    /// The decision tests are pure. This one proves the whole path — enumerate, plan,
    /// signal — actually does what the plan says, including the row that matters most
    /// operationally: **a live gateway at the keep path is still running afterwards.**
    /// Every other test in this file could pass while the reaper killed the bridge it
    /// had just started.
    ///
    /// Every gateway already running when the test starts is pinned into `keep_pids`,
    /// so running the suite on a developer's Linux box cannot kill their real Toolport
    /// gateway. Only the three processes spawned here are candidates.
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn linux_reap_keeps_the_live_keep_path_and_kills_stale_and_unlinked_images() {
        let dir = ScratchDir::new("reaper-reap");
        let keep_exe = dir.join("bin/toolport-gateway");
        let stale_exe = dir.join("old/toolport-gateway");
        let upgraded_exe = dir.join("upgraded/toolport-gateway");

        let mut keep = SpawnedGateway::at(keep_exe.clone());
        let mut stale = SpawnedGateway::at(stale_exe.clone());
        let mut upgraded = SpawnedGateway::at(upgraded_exe.clone());

        // An in-place upgrade: the file at a path we still consider current was
        // replaced, so this process now holds an unlinked inode.
        upgraded.unlink_image();

        // Real enumeration, scoped to this test's own processes. Everything the pass
        // sees is therefore something this test spawned, so it cannot reap a real
        // Toolport gateway on a developer's machine no matter when one starts. A
        // `keep_pids` snapshot cannot achieve that: the verify pass re-enumerates
        // 150ms later, after any snapshot the caller could have taken.
        let mine = [keep.pid(), stale.pid(), upgraded.pid()];
        let report = reap_listed(
            &ReapContext {
                current_version: "9.9.9".into(),
                keep_paths: vec![keep_exe.clone(), upgraded_exe.clone()],
                keep_pids: vec![std::process::id()],
                kill_all: false,
            },
            || {
                linux_list_gateway_processes()
                    .into_iter()
                    .filter(|p| mine.contains(&p.pid))
                    .collect()
            },
        );

        let budget = std::time::Duration::from_secs(5);
        assert!(
            stale.exited_within(budget),
            "a gateway at {} is not at any keep path and must be reaped; report: {report:?}",
            stale_exe.display()
        );
        assert!(
            upgraded.exited_within(budget),
            "the unlinked image must be reaped even though it was launched from the keep \
             path {}, otherwise an in-place upgrade keeps the old inode serving traffic; \
             report: {report:?}",
            upgraded_exe.display()
        );
        assert!(
            keep.still_running(),
            "the live gateway at the keep path {} must survive the reap; report: {report:?}",
            keep_exe.display()
        );
        assert!(
            report.failed.is_empty(),
            "the reaper could not stop processes it planned to kill: {:?}",
            report.failed
        );
        // The keep-path process must not appear in the killed list under any label,
        // including the verify pass's `[retry]` form. Asserting a count instead would
        // be both weaker and flaky: a fixture the kernel has not finished reaping
        // within the 150ms verify window legitimately adds a `[retry]` entry.
        assert!(
            !report
                .killed
                .iter()
                .any(|label| label.contains(&keep_exe.display().to_string())),
            "the keep-path gateway appears in the killed list: {:?}",
            report.killed
        );
    }

    // ----- SOU-435: restart advice -------------------------------------------

    /// A context where anything not at `keep` and not current-version is obsolete.
    fn advice_ctx(keep: &Path) -> ReapContext {
        ReapContext {
            current_version: "9.9.9".into(),
            keep_paths: vec![keep.to_path_buf()],
            keep_pids: Vec::new(),
            kill_all: false,
        }
    }

    #[test]
    fn advice_names_the_parent_app_and_carries_its_pid() {
        let dir = ScratchDir::new("advice-names-parent");
        let keep = dir.join("toolport-gateway-9.9.9.exe");
        let stale = dir.join("toolport-gateway-1.9.4.exe");
        let ctx = advice_ctx(&keep);

        let advice = clients_needing_restart(
            &[proc_with_parent(
                10,
                "toolport-gateway-1.9.4.exe",
                stale.to_str(),
                77,
                "claude.exe",
            )],
            &ctx,
        );

        assert_eq!(
            advice,
            vec![ClientNeedingRestart {
                client: "claude.exe".into(),
                client_pid: 77,
                gateway: "toolport-gateway-1.9.4.exe".into(),
            }],
            "an obsolete gateway with a named parent must be attributed to that app"
        );
    }

    #[test]
    fn advice_is_empty_for_the_updater_kill_all_pass() {
        let dir = ScratchDir::new("advice-kill-all");
        let keep = dir.join("toolport-gateway-9.9.9.exe");
        let mut ctx = advice_ctx(&keep);
        ctx.kill_all = true;

        // The CURRENT gateway, which kill_all also reaps so the installer can
        // replace it. Advising a restart because an app runs the current binary is
        // nonsense the user would act on.
        let procs = [proc_with_parent(
            10,
            "toolport-gateway-9.9.9.exe",
            keep.to_str(),
            77,
            "claude.exe",
        )];
        assert_eq!(decide_reap(&procs[0], &ctx), ReapDecision::Kill);
        assert!(
            clients_needing_restart(&procs, &ctx).is_empty(),
            "kill_all gives every process a Kill verdict, so obsolescence means nothing there"
        );
    }

    /// The case #542 got backwards. ` (deleted)` means *unlinked*, which covers an
    /// in-place replacement (self-heals, stay silent) and an abandoned install
    /// location (does not self-heal, must advise). The old check treated the marker
    /// alone as self-healing and so was silent for the second, and its test only
    /// passed because it ran on macOS where no marker appears at all.
    #[test]
    fn deleted_marker_advises_only_when_the_path_is_really_gone() {
        let dir = ScratchDir::new("advice-deleted-marker");
        let keep = dir.join("toolport-gateway-9.9.9");
        let ctx = advice_ctx(&keep);

        // Replaced in place: the file at the stripped path exists again, so the
        // cached spawn command already resolves to the new binary.
        let replaced = dir.join("toolport-gateway-1.9.4");
        std::fs::write(&replaced, b"new code").unwrap();
        let replaced_marked = format!("{} (deleted)", replaced.display());
        let replaced_proc = proc_with_parent(
            11,
            "toolport-gateway-1.9.4",
            Some(&replaced_marked),
            77,
            "claude.exe",
        );
        assert_eq!(
            decide_reap(&replaced_proc, &ctx),
            ReapDecision::Kill,
            "the marked path must still miss keep-paths so the old inode is reaped"
        );
        assert!(
            cached_path_self_heals(&replaced_proc),
            "a replaced-in-place binary self-heals on the next spawn"
        );
        assert!(
            clients_needing_restart(&[replaced_proc], &ctx).is_empty(),
            "nothing to advise when the cached path already yields new code"
        );

        // Moved or removed: nothing at the stripped path. The client cannot spawn a
        // gateway at all, which is worse than running an old one.
        let removed = dir.join("toolport-gateway-1.9.5");
        let removed_marked = format!("{} (deleted)", removed.display());
        assert!(!removed.exists());
        let removed_proc = proc_with_parent(
            12,
            "toolport-gateway-1.9.5",
            Some(&removed_marked),
            78,
            "grok.exe",
        );
        assert!(
            !cached_path_self_heals(&removed_proc),
            "an abandoned install location never starts yielding new code"
        );
        assert_eq!(
            clients_needing_restart(&[removed_proc], &ctx),
            vec![ClientNeedingRestart {
                client: "grok.exe".into(),
                client_pid: 78,
                gateway: "toolport-gateway-1.9.5".into(),
            }],
            "the stale-install-location case must be reported, on every platform"
        );
    }

    #[test]
    fn advice_skips_unattributable_and_non_app_parents() {
        let dir = ScratchDir::new("advice-skips");
        let keep = dir.join("toolport-gateway-9.9.9.exe");
        let stale = dir.join("toolport-gateway-1.9.4.exe");
        let ctx = advice_ctx(&keep);
        let stale_str = stale.to_str();

        // No parent at all: nothing to name.
        assert!(clients_needing_restart(
            &[proc(10, "toolport-gateway-1.9.4.exe", stale_str)],
            &ctx
        )
        .is_empty());

        // Our own supervised HTTP bridge is not a client the user restarts.
        assert!(clients_needing_restart(
            &[proc_with_parent(
                10,
                "toolport-gateway-1.9.4.exe",
                stale_str,
                5,
                "toolport.exe"
            )],
            &ctx
        )
        .is_empty());

        // Init/kernel parents are not restartable applications. Both pid- and
        // name-matched, so a systemd *user* session with its own pid is caught too.
        for (ppid, name) in [
            (1u32, "launchd"),
            (4, "System"),
            (900, "systemd"),
            (0, "kernel"),
        ] {
            assert!(
                clients_needing_restart(
                    &[proc_with_parent(
                        10,
                        "toolport-gateway-1.9.4.exe",
                        stale_str,
                        ppid,
                        name
                    )],
                    &ctx
                )
                .is_empty(),
                "parent {name} (pid {ppid}) is not an app a user can restart"
            );
        }

        // Unreadable path: decide_reap still reaches Kill via the basename fallback,
        // but that skips the install-location guard and we cannot stop it either, so
        // it may be a foreign user's process. Silence beats naming someone else's app.
        assert!(clients_needing_restart(
            &[proc_with_parent(
                10,
                "toolport-gateway-1.9.4.exe",
                None,
                77,
                "claude.exe"
            )],
            &ctx
        )
        .is_empty());
    }

    #[test]
    fn advice_dedupes_repeat_respawns_of_the_same_app() {
        let dir = ScratchDir::new("advice-dedupe");
        let keep = dir.join("toolport-gateway-9.9.9.exe");
        let stale = dir.join("toolport-gateway-1.9.4.exe");
        let ctx = advice_ctx(&keep);
        let stale_str = stale.to_str();

        // One app that respawned the same gateway three times is one thing to do.
        let advice = clients_needing_restart(
            &[
                proc_with_parent(
                    10,
                    "toolport-gateway-1.9.4.exe",
                    stale_str,
                    77,
                    "claude.exe",
                ),
                proc_with_parent(
                    11,
                    "toolport-gateway-1.9.4.exe",
                    stale_str,
                    77,
                    "claude.exe",
                ),
                proc_with_parent(
                    12,
                    "toolport-gateway-1.9.4.exe",
                    stale_str,
                    77,
                    "claude.exe",
                ),
            ],
            &ctx,
        );
        assert_eq!(
            advice.len(),
            1,
            "one entry per app to act on, not per respawn"
        );
    }

    /// The ordering requirement that three review rounds kept relocating: advice is
    /// derived from the same snapshot the kill list is, so it cannot be read from a
    /// table a previous pass already cleared.
    #[test]
    fn plan_reap_derives_advice_from_the_same_prekill_snapshot() {
        let dir = ScratchDir::new("advice-plan");
        let keep = dir.join("toolport-gateway-9.9.9.exe");
        let stale = dir.join("toolport-gateway-1.9.4.exe");
        let ctx = advice_ctx(&keep);

        let procs = vec![
            proc_with_parent(
                10,
                "toolport-gateway-1.9.4.exe",
                stale.to_str(),
                77,
                "claude.exe",
            ),
            proc_with_parent(
                11,
                "toolport-gateway-9.9.9.exe",
                keep.to_str(),
                78,
                "cursor.exe",
            ),
        ];
        let plan = plan_reap(&procs, &ctx);

        assert_eq!(plan.to_kill.len(), 1, "only the obsolete gateway is killed");
        assert_eq!(plan.to_kill[0].pid, 10);
        assert_eq!(plan.kept.len(), 1, "the current gateway is kept");
        assert_eq!(
            plan.needs_restart,
            vec![ClientNeedingRestart {
                client: "claude.exe".into(),
                client_pid: 77,
                gateway: "toolport-gateway-1.9.4.exe".into(),
            }],
            "advice comes from the pre-kill snapshot, alongside the kill list"
        );

        // The post-kill table: the obsolete process is gone and its client has not
        // respawned yet. Planning against it yields nothing, which is exactly why
        // the advice must not be recomputed after a reap.
        let after = plan_reap(&procs[1..], &ctx);
        assert!(
            after.needs_restart.is_empty(),
            "a post-kill table cannot produce the advice, so it must never be the source"
        );
    }

    // ----- SOU-484: pruning published gateway binaries ------------------------

    fn bin(dir: &Path, name: &str) -> PathBuf {
        dir.join(name)
    }

    /// A context where nothing is in use, so every rule below is the only thing
    /// standing between a file and deletion.
    fn prune_ctx(current: &str) -> PruneContext {
        PruneContext {
            current_version: current.into(),
            ..Default::default()
        }
    }

    #[test]
    fn prune_deletes_only_old_versioned_images() {
        let dir = ScratchDir::new("advice-prune-basic");
        let ctx = prune_ctx("1.10.0");

        assert_eq!(
            decide_prune(&bin(&dir, "toolport-gateway-1.9.4.exe"), &ctx),
            PruneDecision::Delete,
            "an old versioned image with no evidence against it is the whole point"
        );
        // The unversioned app-local copy is what clients fall back to; deleting it
        // would break every client at once.
        assert!(matches!(
            decide_prune(&bin(&dir, "toolport-gateway.exe"), &ctx),
            PruneDecision::Keep(_)
        ));
        assert!(matches!(
            decide_prune(&bin(&dir, "conduit-gateway.exe"), &ctx),
            PruneDecision::Keep(_)
        ));
        // Anything else sharing the directory, including our own manifest.
        assert!(matches!(
            decide_prune(&bin(&dir, "gateway-manifest.json"), &ctx),
            PruneDecision::Keep(_)
        ));
        assert!(matches!(
            decide_prune(&bin(&dir, "some-other-vendor-1.2.3.exe"), &ctx),
            PruneDecision::Keep(_)
        ));
        // Never the current version, however the evidence looks.
        assert!(matches!(
            decide_prune(&bin(&dir, "toolport-gateway-1.10.0.exe"), &ctx),
            PruneDecision::Keep(_)
        ));
        assert_eq!(
            decide_prune(&bin(&dir, "toolport-gateway-1.10.0-0123456789ab.exe"), &ctx),
            PruneDecision::Keep("current version")
        );
    }

    /// The exact situation from the SOU-484 report: on the machine where this was
    /// found, `toolport-gateway-1.9.7-rc.1.exe` was serving a live Claude Code two
    /// minutes after the 1.10.0 upgrade reaped it, and no config named it any more.
    /// Deleting it there would have broken Claude Code rather than updated it.
    #[test]
    fn prune_keeps_a_binary_a_live_process_is_running() {
        let dir = ScratchDir::new("advice-prune-live");
        let rc = bin(&dir, "toolport-gateway-1.9.7-rc.1.exe");
        let mut ctx = prune_ctx("1.10.0");
        // No config references it; only the live process speaks for it.
        ctx.live_paths = vec![rc.clone()];

        assert_eq!(
            decide_prune(&rc, &ctx),
            PruneDecision::Keep("backing a running process")
        );
    }

    #[test]
    fn prune_keeps_a_binary_a_client_config_still_names() {
        let dir = ScratchDir::new("advice-prune-referenced");
        let old = bin(&dir, "toolport-gateway-1.8.0.exe");
        let mut ctx = prune_ctx("1.10.0");
        // Nothing is running it, but a client will spawn exactly this path.
        ctx.referenced_paths = vec![old.clone()];

        assert_eq!(
            decide_prune(&old, &ctx),
            PruneDecision::Keep("named by a client config")
        );
    }

    /// The window evidence alone misses: the client was reaped, has not respawned
    /// yet, and the repoint already removed its path from the config. Nothing is
    /// running and nothing references it, yet it is precisely the binary that client
    /// will spawn next. Restart advice is the only witness, which is why this became
    /// safe to do only after SOU-435.
    #[test]
    fn prune_keeps_a_binary_a_client_is_still_relaunching() {
        let dir = ScratchDir::new("advice-prune-advised");
        let old = bin(&dir, "toolport-gateway-1.9.6.exe");
        let mut ctx = prune_ctx("1.10.0");
        assert_eq!(
            decide_prune(&old, &ctx),
            PruneDecision::Delete,
            "with no evidence at all this file looks deletable"
        );

        ctx.advised_basenames = vec!["toolport-gateway-1.9.6.exe".into()];
        assert_eq!(
            decide_prune(&old, &ctx),
            PruneDecision::Keep("a client is still relaunching it"),
            "restart advice must veto deletion even with no process and no config"
        );
    }

    #[test]
    fn prune_keeps_the_two_newest_non_current_versions() {
        let dir = ScratchDir::new("advice-prune-recent");
        let all: Vec<PathBuf> = [
            "toolport-gateway-1.6.2.exe",
            "toolport-gateway-1.9.0.exe",
            "toolport-gateway-1.9.6.exe",
            "toolport-gateway-1.9.7-rc.1.exe",
            "toolport-gateway-1.10.0.exe",
        ]
        .iter()
        .map(|n| bin(&dir, n))
        .collect();

        let recent = newest_non_current(&all, "1.10.0");
        let names: Vec<String> = recent
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec![
                "toolport-gateway-1.9.7-rc.1.exe".to_string(),
                "toolport-gateway-1.9.6.exe".to_string(),
            ],
            "the current version is excluded, and a pre-release sorts just under its release"
        );

        let mut ctx = prune_ctx("1.10.0");
        ctx.keep_recent = recent;
        // The two newest survive on the recency floor alone.
        assert!(matches!(
            decide_prune(&bin(&dir, "toolport-gateway-1.9.6.exe"), &ctx),
            PruneDecision::Keep(_)
        ));
        // Everything older is genuinely gone.
        assert_eq!(
            decide_prune(&bin(&dir, "toolport-gateway-1.9.0.exe"), &ctx),
            PruneDecision::Delete
        );
        assert_eq!(
            decide_prune(&bin(&dir, "toolport-gateway-1.6.2.exe"), &ctx),
            PruneDecision::Delete
        );
    }

    /// `1.10.0` must sort above `1.9.6`, which a lexical compare gets backwards and
    /// would make the recency floor protect the wrong two files.
    #[test]
    fn version_sort_is_numeric_not_lexical() {
        assert!(
            version_sort_key("toolport-gateway-1.10.0.exe")
                > version_sort_key("toolport-gateway-1.9.6.exe")
        );
        assert!(
            version_sort_key("toolport-gateway-1.9.7.exe")
                > version_sort_key("toolport-gateway-1.9.7-rc.1.exe"),
            "a release outranks its own pre-release"
        );
        assert!(
            version_sort_key("toolport-gateway-1.9.7-rc.1.exe")
                > version_sort_key("toolport-gateway-1.9.6.exe")
        );
        // Two candidates of the same version must be ordered, or the recency floor
        // falls back to `read_dir` order and can keep rc.1 while deleting rc.2.
        assert!(
            version_sort_key("toolport-gateway-1.9.7-rc.2.exe")
                > version_sort_key("toolport-gateway-1.9.7-rc.1.exe")
        );
        assert!(
            version_sort_key("toolport-gateway-1.9.7-rc.10.exe")
                > version_sort_key("toolport-gateway-1.9.7-rc.9.exe"),
            "pre-release numbers compare numerically too"
        );
    }

    /// The floor must protect the newest candidate, not whichever one the directory
    /// listing happened to yield first.
    #[test]
    fn recency_floor_picks_the_newest_of_two_candidates() {
        let dir = ScratchDir::new("advice-prune-two-rcs");
        // Deliberately listed oldest-first, the order that hid this.
        let all: Vec<PathBuf> = [
            "toolport-gateway-1.9.6.exe",
            "toolport-gateway-1.9.7-rc.1.exe",
            "toolport-gateway-1.9.7-rc.2.exe",
        ]
        .iter()
        .map(|n| bin(&dir, n))
        .collect();

        let names: Vec<String> = newest_non_current(&all, "1.10.0")
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec![
                "toolport-gateway-1.9.7-rc.2.exe".to_string(),
                "toolport-gateway-1.9.7-rc.1.exe".to_string(),
            ],
            "the two newest are both candidates of 1.9.7, newest first"
        );
    }

    /// The whole reported directory, end to end: 14 binaries, one current, one held
    /// by a live client, and the rest reclaimable.
    #[test]
    fn prune_plan_over_the_reported_directory() {
        let dir = ScratchDir::new("advice-prune-whole");
        let names = [
            "toolport-gateway-1.6.2.exe",
            "toolport-gateway-1.7.0.exe",
            "toolport-gateway-1.7.1.exe",
            "toolport-gateway-1.7.2.exe",
            "toolport-gateway-1.8.0.exe",
            "toolport-gateway-1.9.0.exe",
            "toolport-gateway-1.9.1.exe",
            "toolport-gateway-1.9.2.exe",
            "toolport-gateway-1.9.3.exe",
            "toolport-gateway-1.9.4.exe",
            "toolport-gateway-1.9.5.exe",
            "toolport-gateway-1.9.6.exe",
            "toolport-gateway-1.9.7-rc.1.exe",
            "toolport-gateway-1.10.0.exe",
        ];
        let all: Vec<PathBuf> = names.iter().map(|n| bin(&dir, n)).collect();

        let mut ctx = prune_ctx("1.10.0");
        ctx.keep_recent = newest_non_current(&all, "1.10.0");
        // Claude Code is still running the rc, as it was on the real machine.
        ctx.live_paths = vec![bin(&dir, "toolport-gateway-1.9.7-rc.1.exe")];

        let kept: Vec<&str> = names
            .iter()
            .copied()
            .filter(|n| decide_prune(&bin(&dir, n), &ctx) != PruneDecision::Delete)
            .collect();
        assert_eq!(
            kept,
            vec![
                "toolport-gateway-1.9.6.exe",
                "toolport-gateway-1.9.7-rc.1.exe",
                "toolport-gateway-1.10.0.exe",
            ],
            "current, the live rc, and the recency floor survive; the other 11 go"
        );
    }
}
