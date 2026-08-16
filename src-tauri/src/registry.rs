//! Conduit's own source-of-truth registry.
//!
//! This is independent of any client. It holds the full set of MCP servers the
//! user has in Conduit, plus profiles. A profile is a named set of *enabled*
//! servers (e.g. "Personal", "Work"); toggling a server on/off is just editing
//! the active profile. The gateway exposes whatever the active profile enables.
//!
//! Secrets are never stored here. Env vars marked `secret` keep their value in
//! the OS keychain; this file only records that a secret exists.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::router::sanitize_segment;

use fs2::FileExt;
use serde::{Deserialize, Serialize};

const REGISTRY_VERSION: u32 = 1;

/// Per-process counter for unique atomic-write temp names.
static ATOMIC_WRITE_SEQ: AtomicU64 = AtomicU64::new(0);

trait AtomicWriteOps {
    fn set_owner_only(&self, file: &std::fs::File) -> std::io::Result<()>;
    fn write_all(&self, file: &mut std::fs::File, contents: &[u8]) -> std::io::Result<()>;
    fn sync_all(&self, file: &std::fs::File) -> std::io::Result<()>;
}

struct FsAtomicWriteOps;

impl AtomicWriteOps for FsAtomicWriteOps {
    fn set_owner_only(&self, file: &std::fs::File) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
        }
        #[cfg(not(unix))]
        {
            let _ = file;
            Ok(())
        }
    }

    fn write_all(&self, file: &mut std::fs::File, contents: &[u8]) -> std::io::Result<()> {
        use std::io::Write;
        file.write_all(contents)
    }

    fn sync_all(&self, file: &std::fs::File) -> std::io::Result<()> {
        file.sync_all()
    }
}

struct TempFileCleanup {
    path: PathBuf,
    armed: bool,
}

impl TempFileCleanup {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: false }
    }

    fn arm(&mut self) {
        self.armed = true;
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TempFileCleanup {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Linux `MAXSYMLINKS`. A hop cap is the backstop when a cycle is spelled
/// differently on each visit (`link` vs `dir/../link`) and the seen-set misses it.
const ATOMIC_WRITE_MAX_SYMLINK_HOPS: usize = 40;

/// Where `atomic_write` should create its sibling temp file and `rename`.
///
/// POSIX `rename` does not follow a destination symlink: it replaces the link
/// inode with a regular file and leaves the target unchanged. Stow/chezmoi
/// users then lose the gateway entry on the next apply (SBS-886).
///
/// Walk with `read_link` + parent-join. Do **not** `canonicalize`: that fails
/// on a dangling link (the usual first-Connect case) and would refuse to create
/// the target. A `symlink_metadata` error other than `NotFound` is not "not a
/// symlink" — failing open would clobber a link we could not inspect.
///
/// This is the shared primitive, so following also applies to a Toolport-owned
/// file (`registry.json`, pins, audit, secrets.enc) that is already a symlink.
fn resolve_atomic_write_dest(path: &Path) -> Result<PathBuf, String> {
    let mut current = path.to_path_buf();
    let mut seen = HashSet::new();

    for _ in 0..ATOMIC_WRITE_MAX_SYMLINK_HOPS {
        match std::fs::symlink_metadata(&current) {
            Ok(meta) => {
                if !meta.file_type().is_symlink() {
                    // Followed a config symlink onto a directory (or a further
                    // link that resolved to one). Cannot write file bytes there.
                    if meta.is_dir() && current != path {
                        return Err(format!(
                            "{} is a symlink to the directory {}, which cannot be overwritten as a file",
                            path.display(),
                            current.display()
                        ));
                    }
                    return Ok(current);
                }
                if !seen.insert(current.clone()) {
                    return Err(format!(
                        "symlink loop at {} while resolving atomic write destination",
                        current.display()
                    ));
                }
                let target = std::fs::read_link(&current).map_err(|e| {
                    format!("could not read symlink {}: {e}", current.display())
                })?;
                // Relative targets are relative to the link's parent, not cwd.
                current = match current.parent() {
                    Some(parent) => parent.join(target),
                    None => target,
                };
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Missing original path, or a dangling link's target: write a
                // regular file there. create_dir_all of the dest parent happens
                // at the call site so a first write can create the target.
                return Ok(current);
            }
            Err(e) => {
                return Err(format!(
                    "could not inspect {} before writing: {e}",
                    current.display()
                ));
            }
        }
    }
    Err(format!(
        "too many symlink hops ({ATOMIC_WRITE_MAX_SYMLINK_HOPS}) resolving {}",
        path.display()
    ))
}

/// Write `contents` to `path` atomically: a uniquely-named sibling temp file,
/// then rename over the target. The unique name (pid + per-process sequence)
/// means two writers to the same path can't overwrite each other's half-written
/// temp. The temp sits in the same directory so the rename stays on one
/// filesystem (and is therefore atomic). Once created, the temp is guarded so
/// any permissions, write, sync, or rename failure removes it.
///
/// If `path` is a symlink, the temp and rename target the resolved file so the
/// link inode is left in place (SBS-886).
pub fn atomic_write(path: &Path, contents: &str) -> Result<(), String> {
    atomic_write_with_ops(path, contents, &FsAtomicWriteOps)
}

fn atomic_write_with_ops(
    path: &Path,
    contents: &str,
    ops: &impl AtomicWriteOps,
) -> Result<(), String> {
    // SBS-886: rename(2) replaces a destination symlink. Resolve first so the
    // temp file and rename land next to the real target.
    let dest = resolve_atomic_write_dest(path)?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let seq = ATOMIC_WRITE_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = PathBuf::from(format!(
        "{}.{}.{}.conduit-tmp",
        dest.display(),
        std::process::id(),
        seq
    ));
    let mut cleanup = TempFileCleanup::new(tmp.clone());
    let mut f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
    // Arm cleanup immediately after creation. Declaring the guard before the file makes
    // Rust close the file first on every error path, which is required for removal on Windows.
    cleanup.arm();
    // Restrict to owner-only (0600) BEFORE writing, so secrets.enc, the registry,
    // pins, and the audit log are never world-readable, not even for the brief
    // window before the content lands, under a permissive umask on a shared or
    // headless host. No-op on Windows (NTFS ACLs inherit from the parent dir).
    ops.set_owner_only(&f).map_err(|e| e.to_string())?;
    ops.write_all(&mut f, contents.as_bytes())
        .map_err(|e| e.to_string())?;
    // Flush the data to stable storage BEFORE the rename, so a crash/power loss
    // can't make the rename durable while the file's blocks aren't — which would
    // leave a truncated registry.json. `fs::write` + `rename` alone did not.
    ops.sync_all(&f).map_err(|e| e.to_string())?;
    drop(f);
    std::fs::rename(&tmp, &dest).map_err(|e| e.to_string())?;
    cleanup.disarm();
    // Best-effort: fsync the containing directory so the rename entry itself is durable
    // (Unix). Opening a directory as a File fails on Windows, where NTFS journals the
    // rename anyway, so the error is ignored. Fsync the *resolved* parent so a
    // write-through-symlink still durables the directory that holds the new file.
    if let Some(dir) = dest.parent() {
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}
const DEFAULT_PROFILE_ID: &str = "default";
const INVALID_PROFILE_REF_PREFIX: &str = "__toolport_invalid_profile_ref__:";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EnvVar {
    pub key: String,
    /// Non-secret value, stored inline. For secrets this is `None` and the value
    /// lives in the OS keychain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default)]
    pub secret: bool,
}

// No `Eq`: the `unknown_fields` flatten map (a serde_json::Map) is only `PartialEq`.
// Dropping `Eq` is what lets an older binary preserve per-server fields it doesn't
// recognize on re-save, mirroring the same forward-compat protection already on
// `Registry`. Nothing keys a set/map by `ServerEntry`, so `Eq` was unused.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ServerEntry {
    #[serde(default)]
    pub id: String,
    pub name: String,
    /// "stdio" | "http" | "sse"
    pub transport: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: Vec<EnvVar>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Working directory for a stdio server. Unset means inherit the gateway's
    /// cwd (the previous behavior). A leading `~` expands to the home dir and
    /// `${VAR}` expands from the environment, so a server that operates on the
    /// project (e.g. a grep/filesystem tool) can be pinned to it. The reserved
    /// token `${ROOT}` expands to the upstream MCP client's current project
    /// directory (its first declared root); it is resolved only in stdio-gateway
    /// mode, and falls back to the gateway cwd when no client root is known.
    /// Only applies to stdio servers. See issue #239.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Where this entry came from, e.g. "imported:cursor" or "manual".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Original (downstream) tool names the user has switched off. The gateway
    /// hides these from `tools/list` and rejects calls to them. Default-allow:
    /// an empty list means every tool the server advertises is exposed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_tools: Vec<String>,
    /// Headless outbound OAuth for an HTTP server (SBS-524). Presence of this
    /// block is what selects the client-credentials flow; absence leaves
    /// interactive OAuth and pasted-token behaviour exactly as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_credentials: Option<ClientCredentials>,
    /// Per-server fields written by a newer build that this binary doesn't know
    /// about. Captured on load and re-emitted on save so a mixed-version binary
    /// never strips them (same contract as `Registry::unknown_fields`).
    #[serde(flatten)]
    pub unknown_fields: serde_json::Map<String, serde_json::Value>,
}

impl ServerEntry {
    /// Team-synced local commands and LAN URLs stay off until the member enables
    /// them after review. Enable-all and the playground must not skip that gate.
    pub fn needs_team_enable_review(&self) -> bool {
        let Some(src) = self.source.as_deref() else {
            return false;
        };
        if !src.starts_with("team:") {
            return false;
        }
        if self.transport == "stdio" || self.command.is_some() {
            return true;
        }
        match self.url.as_deref().and_then(crate::oauth::host_of_url) {
            Some(host) => crate::oauth::host_is_private(&host),
            None => false,
        }
    }
}

/// Non-secret configuration for the OAuth client-credentials flow (SBS-524).
///
/// Deliberately holds no secret. The client secret lives in the OS vault under
/// [`crate::secrets::CLIENT_SECRET_KEY`], because this struct is written to
/// `registry.json`, copied into config backups, and included in exports. The
/// client id, scopes and auth method are not credentials and are useful to see
/// in the file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ClientCredentials {
    /// OAuth client identifier issued by the authorization server.
    pub client_id: String,
    /// `client_secret_basic` | `client_secret_post` | `private_key_jwt`.
    /// Unset means negotiate from the server's advertised methods.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_endpoint_auth_method: Option<String>,
    /// Space-delimited scopes to request. Unset means use what discovery
    /// advertises for the protected resource.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Forward-compat, same contract as `ServerEntry::unknown_fields`.
    ///
    /// Sanitized by [`Self::strip_secret_fields`] wherever this crosses a trust
    /// boundary: forward-compat must not become a smuggling channel for a
    /// credential this type is specifically designed never to hold.
    #[serde(flatten)]
    pub unknown_fields: serde_json::Map<String, serde_json::Value>,
}

impl ClientCredentials {
    /// Drop any forward-compat field whose name looks like a credential.
    ///
    /// `unknown_fields` exists so a newer build's additions survive a round trip
    /// through an older one. That is the right default for configuration, and the
    /// wrong one for secrets: a `clientSecret` key written by hand into
    /// registry.json, or present in a team payload, would otherwise be preserved
    /// and then pushed to the org control plane and every teammate by
    /// `team_server_export` -- the exact leak this struct's shape is meant to make
    /// impossible.
    ///
    /// Name-based and deliberately broad. A real forward-compat field is free to
    /// avoid the word; a credential that slips through is not recoverable.
    pub fn strip_secret_fields(&mut self) {
        self.unknown_fields
            .retain(|k, _| !k.to_ascii_lowercase().contains("secret"));
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub enabled_server_ids: Vec<String>,
    /// Optional tool-granular scoping (the "FeatureSet" layer): a per-server allow-list of
    /// the ORIGINAL tool names this profile exposes. A server present here exposes ONLY the
    /// listed tools; a server ABSENT exposes all of its tools, exactly as before. An empty
    /// map means the profile is server-granular only, so this is fully backward compatible.
    /// Keyed by server id -> original tool names (like `pinned_tools`), so a `tool_override`
    /// rename can't slip a tool past the scope. Enforced everywhere the server scope is:
    /// tools/list, search, and the call guard.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub tool_scope: HashMap<String, Vec<String>>,
}

/// Maps a project folder to a profile, so the gateway can auto-scope a client to the right
/// server set based on the working directory (MCP `root`) it reports, instead of a manual
/// profile switch. A client whose reported root is `path` or a descendant of it resolves to
/// `profile`; the longest matching `path` wins. `profile` is a profile id OR name (resolved
/// the same way as `client_scopes`, via `resolve_profile_id`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FolderProfile {
    pub path: String,
    pub profile: String,
}

/// A consumer registered to reach the gateway over the HTTP/OpenAPI bridge with
/// its own bearer token and scope. Lets one bridge process serve several clients
/// (e.g. Open WebUI) with different server sets, resolved per request from the
/// token. The plaintext token is shown once at creation and never stored.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HttpClient {
    pub id: String,
    pub label: String,
    /// SHA-256 (hex) of the bearer token. We store only the hash, like any token.
    pub token_sha256: String,
    /// Profile name this client is scoped to. Empty = the full connected set
    /// (no extra filtering), so it behaves like the legacy single-token bridge.
    #[serde(default)]
    pub profile: String,
}

/// SHA-256 (hex) of a string. Used to hash bearer tokens so plaintext never hits
/// disk; the same hash is recomputed on an incoming token to look up its client.
pub fn sha256_hex(s: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())
}

/// Constant-time byte equality, so a token-hash comparison can't leak the stored
/// hash prefix through early-exit timing (consistent with the gateway's other token
/// checks). A length mismatch short-circuits; length isn't secret for a fixed-width hash.
fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Normalize a filesystem path for prefix comparison WITHOUT touching disk: unify separators
/// to `/`, trim a trailing separator, and lowercase on Windows (its paths are
/// case-insensitive). String-only so it works for a path that doesn't exist on this machine
/// (the client reported it). Not canonicalization, just enough to compare two reported paths.
fn normalize_path(p: &str) -> String {
    let mut s = p.trim().replace('\\', "/");
    while s.len() > 1 && s.ends_with('/') {
        s.pop();
    }
    if cfg!(windows) {
        s = s.to_ascii_lowercase();
    }
    s
}

/// True when `root` is `base` itself or a descendant of it, matched on a path BOUNDARY so
/// base `/a/proj` matches `/a/proj` and `/a/proj/src` but never `/a/project`. Both must
/// already be [`normalize_path`]-ed. An empty `base` matches nothing (an unset mapping path
/// must not swallow every root).
fn path_is_within(base: &str, root: &str) -> bool {
    if base.is_empty() {
        return false;
    }
    if root == base {
        return true;
    }
    root.starts_with(base) && root.as_bytes().get(base.len()) == Some(&b'/')
}

/// A user override for how one tool is exposed to clients, keyed in the registry by the
/// tool's exposed (namespaced `server__tool`) name. Lets the user rename a tool or replace
/// its description - the latter is the security lever: locally neutralize a poisoned or
/// injection-laden description without waiting on the upstream server. Overrides only touch
/// the EXPOSED definition; the call still routes to the original downstream tool. (Pinning
/// input params is a planned follow-up and not in this struct yet.)
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolOverride {
    /// A replacement client-facing name (sanitized to a valid tool name; ignored if it
    /// would collide with another exposed tool).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// A replacement description shown to the client instead of the server's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Registry {
    pub version: u32,
    pub servers: Vec<ServerEntry>,
    pub profiles: Vec<Profile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_profile_id: Option<String>,
    /// Global safety switch: when true, the gateway hides and blocks any tool a
    /// server annotates with `destructiveHint: true` (deletes, drops, writes).
    /// One toggle to keep agents read-only across every connected server.
    #[serde(default)]
    pub deny_destructive: bool,
    /// Per-call confirmation for destructive tools: when true, the gateway
    /// intercepts each call to a destructive-hinted tool, returns a preview
    /// with a confirmation token, and requires `conduit_confirm { token }` to
    /// proceed. The original arguments are replayed exactly — the agent cannot
    /// change them. Unlike `deny_destructive` (which hides tools entirely),
    /// this lets agents use destructive tools — but forces a conscious review
    /// of every call first.
    #[serde(default)]
    pub confirm_destructive: bool,
    /// Human-in-the-loop approval: when true, a *gated* tool call (destructive-hinted, or
    /// from an untrusted-provenance server) is held and surfaced to the Toolport app for a
    /// person to approve or deny before it runs. Unlike `confirm_destructive` (which the
    /// AGENT re-confirms with a token), this puts a HUMAN in the loop: the call blocks until
    /// a decision or a fail-closed timeout. Off by default. Takes precedence over
    /// `confirm_destructive` for the tools it gates (a human decision supersedes the agent's).
    #[serde(default)]
    pub human_approval: bool,
    /// Tools the user chose to "always allow" past human approval, so the HITL gate skips
    /// them. Each entry is a `server/tool` key (see `approval::allow_key`). Persisted so an
    /// always-allowed tool stays allowed across restarts; the broker also keeps a separate
    /// ephemeral per-session allowlist that is NOT saved here.
    #[serde(default)]
    pub human_approval_allow: Vec<String>,
    /// Set true only while an ACTIVE team's screening policy forces human approval
    /// (`forceHumanApproval`). Kept SEPARATE from `human_approval` (the member's own choice)
    /// so the org lock is RELEASABLE: recomputed on every team sync and cleared when the
    /// member leaves or is removed from a team, instead of being baked permanently into the
    /// member's own setting (which had no release path, so an org lock outlived the team).
    /// The gate holds a call when either is true (see [`Registry::human_approval_effective`]).
    #[serde(default)]
    pub team_forced_human_approval: bool,
    /// The same releasable org-lock treatment as [`team_forced_human_approval`] for the other
    /// tighten-only screening flags (`denyDestructive`, `forceContentDefense`,
    /// `forceQuarantineOnDrift`, `forceBlockOnInjection`). Set from the active team's policy,
    /// recomputed each sync and cleared on leave, so an org lock never permanently overwrites
    /// the member's own setting. Enforcement reads `*_effective()` (member's own OR team-forced).
    #[serde(default)]
    pub team_forced_deny_destructive: bool,
    #[serde(default)]
    pub team_forced_content_defense: bool,
    #[serde(default)]
    pub team_forced_quarantine_on_drift: bool,
    #[serde(default)]
    pub team_forced_block_on_injection: bool,
    #[serde(default)]
    pub team_forced_pii_redaction: bool,
    /// Per-tool exposure overrides, keyed by server id then ORIGINAL tool name (not the
    /// exposed name, so a rename or `_2` collision suffix can't misalign the key): rename or
    /// re-describe a tool as clients see it (e.g. neutralize a poisoned description). The
    /// call still routes to the original downstream tool.
    #[serde(default)]
    pub tool_overrides: HashMap<String, HashMap<String, ToolOverride>>,
    /// Tools pinned as lazy-discovery prerequisites, keyed by server id -> original tool
    /// names. Search always surfaces a pinned tool (with its schema) regardless of the
    /// query's match score, so a load-bearing tool (auth, list-before-act, one whose
    /// description doesn't match the user's keywords) is never hidden behind lazy
    /// discovery. Empty = nothing pinned.
    #[serde(default)]
    pub pinned_tools: HashMap<String, Vec<String>>,
    /// Quarantine-on-drift: when true, a high-risk tool (poisoned definition, or a
    /// destructive tool whose definition changed/appeared) that drifts from its pinned
    /// baseline is hidden and blocked from every client until the user re-approves it.
    /// Opt-in, since blocking a tool is more disruptive than just flagging the drift.
    #[serde(default)]
    pub quarantine_on_drift: bool,
    /// Lazy discovery: the gateway exposes 4 meta-tools (status/search/call/fetch)
    /// instead of every downstream tool, so clients with tool-count limits don't
    /// drop tools. The gateway reads this from the registry file it already
    /// loads, so it applies to EVERY client regardless of whether the client
    /// passes the `CONDUIT_DISCOVERY` env var (an explicit env still overrides).
    /// Defaults on, since clients commonly cap the tool list.
    #[serde(default = "default_true")]
    pub lazy_discovery: bool,
    /// Discovery-mode override: `"lazy"` | `"grouped"` | `"full"`. When set, it takes
    /// precedence over `lazy_discovery`; `None` (the default, and every pre-existing
    /// registry) falls back to that bool. An explicit `CONDUIT_DISCOVERY` env var still
    /// overrides this. Lets a user pick grouped mode once - for weak/local models that
    /// browse per-server instead of searching - instead of setting a per-client env var.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery_mode: Option<String>,
    /// Server-side "code mode": advertise the `toolport_run_script` meta-tool so an
    /// agent can orchestrate many downstream tool calls in one sandboxed JS script (a single
    /// round-trip). **On by default** (SOU-397): each in-script call still hits the same
    /// scope / approval gates as `toolport_call_tool`, and Settings is the kill switch.
    /// Code mode is not a security boundary (agent-supplied JS). Shared/HTTP multi-tenant
    /// operators who do not want the surface can turn it off in Settings or set
    /// `"codeMode": false` in the registry. `TOOLPORT_CODE_MODE=1` (legacy
    /// `CONDUIT_CODE_MODE`) still force-enables regardless of the toggle.
    #[serde(default = "default_true")]
    pub code_mode: bool,
    /// Opt-in permission for agents to request persistence of Code Mode routines. Off by
    /// default. This only exposes the save surface; each save still requires a separate,
    /// content-bound human approval. Existing routines may still be listed and run while
    /// writes are disabled.
    #[serde(default)]
    pub allow_routine_writes: bool,
    /// Opt-in agent control: when true, an agent may turn servers on or off via
    /// the gateway's `conduit_enable_server` / `conduit_disable_server` tools.
    /// Off by default. The `deny_destructive` safety switch is never agent-
    /// writable regardless, so granting this cannot let an agent escalate past
    /// the user's governance, only flip which servers are connected.
    #[serde(default)]
    pub allow_agent_control: bool,
    /// Tool-definition integrity: fingerprint each connected tool and flag when a
    /// previously-approved tool's definition changes (a rug-pull signal) or a known
    /// server quietly adds a tool. Detection only, it records a security event and
    /// never blocks. On by default.
    #[serde(default = "default_true")]
    pub integrity_check: bool,
    /// Content defense (anti-agentjacking): scan untrusted tool RESULTS for injection
    /// and label flagged content as data, not instructions, before the agent sees it.
    /// Detection + labeling. On by default. Pair with [`block_on_injection`] to fail closed
    /// on high-confidence hits (SOU-345).
    #[serde(default = "default_true")]
    pub content_defense: bool,
    /// Replace PII in tool results with stable pseudonyms before they reach the model,
    /// re-hydrating them on the way back out (SBS-346). OFF by default: it rewrites tool
    /// output, and unlike content defense a missed value fails OPEN, so it is a reduction
    /// in exposure rather than a guarantee and must be opted into knowingly.
    #[serde(default)]
    pub pii_redaction: bool,
    /// Opt-in fail-closed content defense (SOU-345): when true (or team-forced), a
    /// high-confidence injection hit fails the call instead of only labeling. Off by
    /// default so v1 label-only behavior is preserved. Per-server exempt list:
    /// [`injection_block_exempt`].
    #[serde(default)]
    pub block_on_injection: bool,
    /// Server ids for which block-on-injection never applies (label only), even when
    /// global/org block mode is on. For servers that legitimately return prompty text.
    /// Key present with `true` = exempt. Same shape as other per-server maps.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub injection_block_exempt: HashMap<String, bool>,
    /// Live request/response inspection: when true, the gateway captures each tool
    /// call's arguments and result into a small, separate, ephemeral local ring
    /// (`inspect.jsonl`, last 50 calls, each body size-capped) so the Activity view
    /// can show them. OFF by default and never touches the governance audit log,
    /// which stays free of args/results. This is the ONE place args/results are
    /// captured, and only on the user's machine.
    #[serde(default)]
    pub live_inspect: bool,
    /// Optional semantic re-ranking for tool search (blends embedding similarity
    /// into the lexical ranking). Off by default; when off or unconfigured, search
    /// is pure lexical exactly as before.
    #[serde(default)]
    pub semantic_search: SemanticSettings,
    /// Connection to a Conduit Teams server (the paid config-sync layer), if the user
    /// has joined a team. The member token is NOT stored here, it lives in the OS
    /// keychain like any other secret. Servers pulled from the team are merged into
    /// `servers` tagged `source = "team:<id>"`, non-destructively.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<TeamConnection>,
    /// Per-server result-shaping budgets in bytes, keyed by server id (tier-2
    /// fidelity policy). A server absent from the map uses the global default; a
    /// value of `0` means NEVER shape that server's results (full fidelity, for
    /// financial/compliance APIs); `n` caps that server's results at n bytes.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub result_budgets: HashMap<String, u64>,
    /// Which profile each client was connected with, keyed by client id (e.g.
    /// "cursor" -> "Billing"). This is the binding Conduit wrote into that client's
    /// config as `CONDUIT_PROFILE`; recording it here lets the UI show a connected
    /// client's effective scope and re-scope it in place. Absent / empty value =
    /// the client follows the active profile (all its enabled servers).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub client_scopes: HashMap<String, String>,
    /// Folder -> profile mappings for project-scoped auto-routing: when a client reports a
    /// working directory (MCP `root`), the gateway picks the profile whose mapped path is the
    /// longest prefix of that root, instead of the client's manually-set profile. Empty = no
    /// folder routing (every client follows its configured/active profile as before).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub folder_profiles: Vec<FolderProfile>,
    /// Per-client discovery-mode override, keyed by stable client id (e.g. "cursor" ->
    /// "grouped"). Value is `"full" | "lazy" | "grouped"`; an absent entry means the client
    /// inherits the global mode (`discovery_mode`, else `lazy_discovery`). The gateway
    /// resolves it live via `CONDUIT_CLIENT_ID`, so changing it re-applies without
    /// reinstalling the client (same mechanism as `client_scopes`).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub client_discovery: HashMap<String, String>,
    /// What Toolport last wrote into each client's config as its gateway entry
    /// (command/args/env), keyed by client id. Used to distinguish a managed install
    /// from a hand-edited entry under the same name (SOU-406 / #487). Absent = pre-
    /// ownership install; fall back to the command-basename heuristic.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub client_managed_entries: HashMap<String, ManagedEntry>,
    /// Consumers registered to reach the gateway over the HTTP/OpenAPI bridge,
    /// each with its own hashed bearer token and scope. Empty = the bridge uses
    /// only the legacy single `CONDUIT_HTTP_TOKEN` (back-compat).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub http_clients: Vec<HttpClient>,
    /// Bumped when vaulted secrets change so running gateways reload even when
    /// the rest of the registry JSON is unchanged.
    #[serde(default)]
    pub secrets_generation: u64,
    /// Top-level fields THIS build doesn't know, preserved verbatim across
    /// load -> save. The registry is shared by mixed versions of the app and
    /// long-running gateways (a dev build, the installed release, and gateways
    /// spawned days ago can all touch one file); serde's default is to silently
    /// IGNORE unknown fields, which meant an older binary's next save stripped
    /// every newer-schema field. Capturing them instead makes old binaries
    /// pass-through-safe.
    #[serde(flatten)]
    pub unknown_fields: serde_json::Map<String, serde_json::Value>,
}

/// Snapshot of the gateway entry Toolport last wrote into a client config (SOU-406).
/// Compared against the live entry on detect so a deliberate hand-edit is not treated
/// as a stale install. Bearer tokens are never stored here (stripped from env/args).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ManagedEntry {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Env key→value pairs we wrote (non-secret only: client id / profile).
    /// `BTreeMap` keeps serde order stable for equality checks. Authorization is stripped.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// `"stdio"` (default) or `"sharedHttp"` (SOU-407).
    #[serde(default = "managed_transport_stdio")]
    pub transport: String,
    /// Shared-HTTP MCP URL when `transport` is `sharedHttp`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Unix epoch seconds when we last wrote this entry.
    #[serde(default)]
    pub updated_at: u64,
}

fn managed_transport_stdio() -> String {
    "stdio".into()
}

impl ManagedEntry {
    /// Build a record from the [`ServerEntry`] we are about to (or just did) write.
    /// Strips bearer tokens so the registry never holds HTTP credentials.
    pub fn from_gateway_entry(entry: &ServerEntry) -> Self {
        let env = entry
            .env
            .iter()
            .filter(|e| !e.key.eq_ignore_ascii_case("authorization"))
            .filter_map(|e| e.value.as_ref().map(|v| (e.key.clone(), v.clone())))
            .collect();
        let args = strip_auth_header_args(&entry.args);
        let is_bridge = entry
            .command
            .as_deref()
            .is_some_and(|c| c.eq_ignore_ascii_case("npx"))
            && entry.args.iter().any(|a| a == "mcp-remote");
        let is_shared = entry.url.is_some() || is_bridge;
        Self {
            command: entry.command.clone().unwrap_or_default(),
            args,
            env,
            transport: if is_shared {
                "sharedHttp".into()
            } else {
                "stdio".into()
            },
            url: entry.url.clone().or_else(|| {
                // Bridge form: URL is an mcp-remote arg.
                entry
                    .args
                    .iter()
                    .find(|a| a.starts_with("http://") || a.starts_with("https://"))
                    .cloned()
            }),
            updated_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        }
    }
}

/// Drop `--header` / `Authorization: …` pairs so ownership records stay non-secret.
pub fn strip_auth_header_args(args: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--header" {
            // Skip flag and its value (often `Authorization: Bearer …`).
            i += 2;
            continue;
        }
        if a.to_ascii_lowercase().starts_with("authorization:") {
            i += 1;
            continue;
        }
        out.push(a.clone());
        i += 1;
    }
    out
}

/// A joined Conduit Teams server. Holds only non-secret connection metadata; the
/// member bearer token is vaulted in the OS keychain (see `secrets`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TeamConnection {
    /// Base URL of the conduit-teams server, e.g. `https://teams.example.com`.
    pub server_url: String,
    pub team_id: String,
    /// "admin" | "member".
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member_name: Option<String>,
    /// Last config version pulled, for change display and ETag polling.
    #[serde(default)]
    pub last_version: i64,
    /// The exact ETag the server returned on the last pull. Echoed back as If-None-Match
    /// so the 304 fast-path works even for access-restricted members, whose server ETag
    /// carries a per-member suffix that a reconstructed "v{n}" would never match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_etag: Option<String>,
    /// Usage rows already reported to the team server, "YYYY-MM-DD" (UTC) -> server id ->
    /// [calls, tokens_saved]. The next report sends max(local rollup, this), so a local
    /// log rotation mid-day can never shrink a count the server already has. Pruned to
    /// the report window (today + yesterday) on every successful report.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub usage_reported: HashMap<String, HashMap<String, [u64; 2]>>,
    /// The org instructions content last applied to disk (see [`crate::instructions`]). Persisted
    /// so a steady-state sync (a 304, with no config in hand) can still recompute each client's
    /// coverage for the apply-status receipt, and so the writer skips the client-file writes when
    /// the content is unchanged. `None` = no instructions active (absent/disabled/blank).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_instructions_content: Option<String>,
    /// The config version at which `team_instructions_content` was last written — the version the
    /// on-disk block markers carry, and the one the coverage receipt reports. Advances only when
    /// the content changes, so a server-only config edit doesn't make the marker look stale.
    #[serde(default)]
    pub team_instructions_version: i64,
    /// Absolute paths of the client rules files this member's app actually wrote. Cleanup on
    /// team-leave iterates THIS recorded list rather than re-resolving clients, so a file
    /// survives cleanup even if the client was later uninstalled or its detection changed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub team_instructions_targets: Vec<String>,
    /// Hash of the apply-status receipt last successfully sent to the server, so an unchanged
    /// receipt isn't re-POSTed every ~25s sync. `None` = nothing reported yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_instructions_reported: Option<String>,
    /// Local ms-epoch of the last successful instructions-receipt POST. A matching fingerprint
    /// still re-sends after ~12h so the server's `instructions_status_at` stays fresh and the
    /// dashboard does not mark an actively-syncing member stale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_instructions_reported_at: Option<i64>,
    /// Hash of the screening-policy apply receipt last successfully sent (SOU-339). Same
    /// dedup role as `team_instructions_reported`. `None` = nothing reported yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_policy_reported: Option<String>,
    /// Local ms-epoch of the last successful policy-receipt POST (SOU-339 heartbeat).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_policy_reported_at: Option<i64>,
    /// Highest audit `ts` successfully uploaded for SOU-171 call-event export. `None` = never.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_audit_export_cursor: Option<u64>,
    /// Org opt-in: export per-call audit events to the team server (SOU-171). Set from the
    /// last pulled team config's `callAuditExport` flag.
    #[serde(default)]
    pub call_audit_export: bool,
    /// Tool-call caps from the last config pull (SOU-340). Resolved for this member by
    /// the team server; empty = no org caps. Enforced cooperatively in the local gateway.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rate_limits: Vec<crate::rate_limits::Cap>,
}

/// Settings for embedding-based search re-ranking. The embedding API key, if the
/// endpoint needs one, is read from the `CONDUIT_EMBED_KEY` env var, never stored here.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SemanticSettings {
    #[serde(default)]
    pub enabled: bool,
    /// OpenAI-compatible embeddings endpoint, e.g. http://localhost:1234/v1/embeddings.
    #[serde(default)]
    pub endpoint: String,
    #[serde(default)]
    pub model: String,
    /// Weight of semantic vs lexical, 0.0 (pure lexical) .. 1.0 (pure semantic).
    #[serde(default = "default_blend")]
    pub blend: f32,
}

fn default_blend() -> f32 {
    0.5
}

impl Default for SemanticSettings {
    fn default() -> Self {
        SemanticSettings {
            enabled: false,
            endpoint: String::new(),
            model: String::new(),
            blend: 0.5,
        }
    }
}

fn default_true() -> bool {
    true
}

impl Default for Registry {
    fn default() -> Self {
        Registry {
            version: REGISTRY_VERSION,
            servers: Vec::new(),
            profiles: vec![Profile {
                id: DEFAULT_PROFILE_ID.to_string(),
                name: "Default".to_string(),
                enabled_server_ids: Vec::new(),
                tool_scope: HashMap::new(),
            }],
            active_profile_id: Some(DEFAULT_PROFILE_ID.to_string()),
            deny_destructive: false,
            confirm_destructive: false,
            human_approval: false,
            human_approval_allow: Vec::new(),
            team_forced_human_approval: false,
            team_forced_deny_destructive: false,
            team_forced_content_defense: false,
            team_forced_quarantine_on_drift: false,
            team_forced_block_on_injection: false,
            team_forced_pii_redaction: false,
            pii_redaction: false,
            tool_overrides: HashMap::new(),
            pinned_tools: HashMap::new(),
            quarantine_on_drift: false,
            lazy_discovery: true,
            discovery_mode: None,
            code_mode: true,
            allow_routine_writes: false,
            allow_agent_control: false,
            integrity_check: true,
            content_defense: true,
            block_on_injection: false,
            injection_block_exempt: HashMap::new(),
            live_inspect: false,
            semantic_search: SemanticSettings::default(),
            team: None,
            result_budgets: HashMap::new(),
            client_scopes: HashMap::new(),
            folder_profiles: Vec::new(),
            client_discovery: HashMap::new(),
            client_managed_entries: HashMap::new(),
            http_clients: Vec::new(),
            secrets_generation: 0,
            unknown_fields: serde_json::Map::new(),
        }
    }
}

fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !out.is_empty() && !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

/// A filesystem-safe, collision-resistant key for a stable profile id.
/// Every id uses the same SHA-256 domain so no readable/non-readable branch can
/// collide with another id's literal text (SBS-715).
pub fn profile_store_key(profile_id: &str) -> String {
    sha256_hex(&format!("toolport-profile-id-v1:{profile_id}"))
}

/// True when a v2 store is missing but a pre-migration name-slug file still exists
/// for this profile. Live reads must fail closed rather than treat that as a
/// first run (SBS-715).
pub fn unmigrated_legacy_profile_store(profile: &str, pins: bool) -> bool {
    let Some(dir) = conduit_dir() else {
        return false;
    };
    let v2 = dir.join(format!(
        "{}{}.json",
        if pins {
            "tool-pins-v2-"
        } else {
            "quarantine-v2-"
        },
        profile_store_key(profile)
    ));
    if v2.exists() {
        return false;
    }
    let mut slugs = BTreeSet::from([legacy_profile_store_slug(profile)]);
    // Live callers use stable ids, while pre-v2 files were usually named from the
    // profile's display name. Read the registry without invoking migration again so
    // both historical references are checked (SBS-715 / CodeRabbit).
    let registry = resolved_path().map(|path| load_from(&path));
    match registry {
        Some(Ok(registry)) => {
            if let Some(record) = registry.profiles.iter().find(|record| record.id == profile) {
                slugs.insert(legacy_profile_store_slug(&record.name));
            }
        }
        Some(Err(_)) => {
            // If the registry itself cannot be read, we cannot prove which legacy
            // name belongs to this id. Any leftover store is therefore evidence of
            // an incomplete migration, and the trust read must fail closed.
            let prefix = if pins { "tool-pins-" } else { "quarantine-" };
            return std::fs::read_dir(&dir).is_ok_and(|entries| {
                entries.flatten().any(|entry| {
                    entry.file_name().to_str().is_some_and(|name| {
                        name.starts_with(prefix)
                            && !name.starts_with(&format!("{prefix}v2-"))
                            && name.ends_with(".json")
                    })
                })
            });
        }
        None => {}
    }
    slugs
        .into_iter()
        .filter(|slug| !slug.is_empty())
        .any(|slug| {
            dir.join(format!(
                "{}{slug}.json",
                if pins { "tool-pins-" } else { "quarantine-" }
            ))
            .exists()
        })
}

/// The lossy filename mapping used before profile stores were keyed by stable ids.
fn legacy_profile_store_slug(profile_ref: &str) -> String {
    profile_ref
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProfileStoreKind {
    Cache,
    Pins,
    Quarantine,
}

impl ProfileStoreKind {
    fn legacy_prefix(self) -> &'static str {
        match self {
            Self::Cache => "tool-cache-",
            Self::Pins => "tool-pins-",
            Self::Quarantine => "quarantine-",
        }
    }

    fn stable_prefix(self) -> &'static str {
        match self {
            Self::Cache => "tool-cache-v2-",
            Self::Pins => "tool-pins-v2-",
            Self::Quarantine => "quarantine-v2-",
        }
    }

    fn fallback_file(self) -> &'static str {
        match self {
            Self::Cache => "tool-cache.json",
            Self::Pins => "tool-pins.json",
            Self::Quarantine => "quarantine.json",
        }
    }
}

fn write_new_profile_store(path: &Path, contents: &str) -> Result<(), String> {
    let _lock = lock_at(path)
        .map_err(|e| format!("could not lock profile store {} for migration: {e}", path.display()))?;
    if path.exists() {
        return Ok(());
    }
    atomic_write(path, contents)
        .map_err(|e| format!("could not migrate profile store {}: {e}", path.display()))
}

fn merge_legacy_quarantines(paths: &[PathBuf]) -> Result<String, String> {
    let mut merged = serde_json::Map::new();
    for path in paths {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("could not read legacy quarantine {}: {e}", path.display()))?;
        let value: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
            format!("could not parse legacy quarantine {} during migration: {e}", path.display())
        })?;
        let object = value.as_object().ok_or_else(|| {
            format!("legacy quarantine {} is not an object", path.display())
        })?;
        for (tool, record) in object {
            // Every record blocks the same namespaced tool. Keeping the first
            // record is conservative; ambiguity is resolved by the corrupt-pin
            // marker, which forces a fresh tamper quarantine before trust resumes.
            merged.entry(tool.clone()).or_insert_with(|| record.clone());
        }
    }
    serde_json::to_string_pretty(&serde_json::Value::Object(merged)).map_err(|e| e.to_string())
}

/// Copy legacy name-derived profile stores into stable-id-derived files.
///
/// Legacy files are retained as recovery evidence. A source filename claimed by
/// more than one profile is never trusted as a cache or pin baseline: caches are
/// rebuilt, pin stores receive an intentionally invalid marker (so integrity
/// fails closed), and quarantine records are unioned into every claimant.
pub fn migrate_profile_stores(registry: &Registry) -> Result<(), String> {
    let Some(dir) = conduit_dir() else { return Ok(()) };
    if !dir.exists() {
        return Ok(());
    }

    let mut claims: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut profile_slugs: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for profile in &registry.profiles {
        let slugs = [
            legacy_profile_store_slug(&profile.name),
            legacy_profile_store_slug(&profile.id),
        ]
        .into_iter()
        .filter(|slug| !slug.is_empty())
        .collect::<BTreeSet<_>>();
        for slug in &slugs {
            claims.entry(slug.clone()).or_default().insert(profile.id.clone());
        }
        profile_slugs.insert(profile.id.clone(), slugs);
    }

    for profile in &registry.profiles {
        let Some(slugs) = profile_slugs.get(&profile.id) else { continue };
        for kind in [ProfileStoreKind::Cache, ProfileStoreKind::Pins, ProfileStoreKind::Quarantine]
        {
            let target = dir.join(format!(
                "{}{}.json",
                kind.stable_prefix(),
                profile_store_key(&profile.id)
            ));
            if target.exists() {
                continue;
            }
            let mut sources = slugs
                .iter()
                .map(|slug| {
                    (Some(slug), dir.join(format!("{}{slug}.json", kind.legacy_prefix())))
                })
                .filter(|(_, path)| path.exists())
                .collect::<Vec<_>>();
            let fallback = dir.join(kind.fallback_file());
            if fallback.exists() {
                // The fallback followed whichever profile was active. With more
                // than one profile its ownership is unknowable, so every profile
                // treats it as an ambiguous legacy source and fails closed.
                sources.push((None, fallback));
            }
            if sources.is_empty() {
                continue;
            }
            let mut source_paths = sources.iter().map(|(_, path)| path.clone()).collect::<Vec<_>>();
            source_paths.sort();
            source_paths.dedup();
            let _source_locks = if kind == ProfileStoreKind::Cache {
                Vec::new()
            } else {
                source_paths
                    .iter()
                    .map(|path| {
                        lock_at(path).map_err(|e| {
                            format!(
                                "could not lock legacy profile store {} for migration: {e}",
                                path.display()
                            )
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?
            };
            let unambiguous = sources.len() == 1
                && match sources[0].0 {
                    Some(slug) => claims.get(slug).is_some_and(|owners| owners.len() == 1),
                    None => registry.profiles.len() == 1,
                };
            if unambiguous {
                let raw = std::fs::read_to_string(&sources[0].1).map_err(|e| {
                    format!("could not read legacy profile store {}: {e}", sources[0].1.display())
                })?;
                write_new_profile_store(&target, &raw)?;
                continue;
            }

            match kind {
                ProfileStoreKind::Cache => {
                    // A cache can be rebuilt. Copying an ambiguous catalog could
                    // disclose another profile's tools during cold startup.
                }
                ProfileStoreKind::Pins => {
                    write_new_profile_store(
                        &target,
                        "ambiguous legacy profile pin store; re-approval required",
                    )?;
                }
                ProfileStoreKind::Quarantine => {
                    let paths = sources.into_iter().map(|(_, path)| path).collect::<Vec<_>>();
                    let merged = merge_legacy_quarantines(&paths)?;
                    write_new_profile_store(&target, &merged)?;
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn unique_id(base: &str, existing: &[String]) -> String {
    let base = if base.is_empty() { "item" } else { base };
    if !existing.iter().any(|e| e == base) {
        return base.to_string();
    }
    let mut n = 2;
    loop {
        let candidate = format!("{base}-{n}");
        if !existing.iter().any(|e| e == &candidate) {
            return candidate;
        }
        n += 1;
    }
}

impl Registry {
    fn profile_id_for_ref(&self, profile_ref: &str) -> Option<String> {
        let profile_ref = profile_ref.trim();
        if profile_ref.is_empty() {
            return None;
        }
        if let Some(profile) = self.profiles.iter().find(|p| p.id == profile_ref) {
            return Some(profile.id.clone());
        }
        let mut matches = self
            .profiles
            .iter()
            .filter(|p| p.name.eq_ignore_ascii_case(profile_ref));
        let first = matches.next()?;
        if matches.next().is_some() {
            // A legacy name reference shared by multiple profiles cannot be
            // resolved safely. Leave it dangling so server scope fails closed.
            return None;
        }
        Some(first.id.clone())
    }

    /// Resolve a user/API supplied profile reference to a stable id, rejecting
    /// stale and ambiguous names instead of creating another name-keyed binding.
    pub fn canonical_profile_id(&self, profile_ref: &str) -> Result<String, String> {
        self.profile_id_for_ref(profile_ref)
            .ok_or_else(|| format!("No unique profile matches '{profile_ref}'"))
    }

    /// Rewrite every persisted profile reference to the stable profile id when
    /// it resolves unambiguously. Unknown or ambiguous legacy references remain
    /// dangling and therefore fail closed.
    fn normalize_profile_references(&mut self) {
        let profiles = self.profiles.clone();
        let resolve = |profile_ref: &str| {
            let profile_ref = profile_ref.trim();
            if profile_ref.is_empty() {
                return None;
            }
            if let Some(profile) = profiles.iter().find(|p| p.id == profile_ref) {
                return Some(profile.id.clone());
            }
            let mut matches = profiles
                .iter()
                .filter(|p| p.name.eq_ignore_ascii_case(profile_ref));
            let first = matches.next()?;
            matches.next().is_none().then(|| first.id.clone())
        };

        let normalize = |profile_ref: &str| {
            if profile_ref.trim().is_empty() {
                return String::new();
            }
            resolve(profile_ref).unwrap_or_else(|| {
                format!("{INVALID_PROFILE_REF_PREFIX}{}", sha256_hex(profile_ref.trim()))
            })
        };
        if let Some(active) = self.active_profile_id.clone() {
            // A stale active profile is global state. Clear it so the existing
            // first-profile fallback applies instead of persisting an invalid marker
            // that empties every unscoped client's catalog.
            self.active_profile_id = resolve(&active);
        }
        for scope in self.client_scopes.values_mut() {
            *scope = normalize(scope);
        }
        for mapping in &mut self.folder_profiles {
            mapping.profile = normalize(&mapping.profile);
        }
        for client in &mut self.http_clients {
            client.profile = normalize(&client.profile);
        }
    }

    fn server_ids(&self) -> Vec<String> {
        self.servers.iter().map(|s| s.id.clone()).collect()
    }

    fn profile_ids(&self) -> Vec<String> {
        self.profiles.iter().map(|p| p.id.clone()).collect()
    }

    /// Add a new server, assigning a unique id derived from its name. Returns the id.
    pub fn add_server(&mut self, mut entry: ServerEntry) -> String {
        let id = unique_id(&slugify(&entry.name), &self.server_ids());
        entry.id = id.clone();
        self.servers.push(entry);
        id
    }

    pub fn update_server(&mut self, entry: ServerEntry) -> Result<(), String> {
        let slot = self
            .servers
            .iter_mut()
            .find(|s| s.id == entry.id)
            .ok_or_else(|| format!("No server with id '{}'", entry.id))?;
        *slot = entry;
        Ok(())
    }

    pub fn remove_server(&mut self, id: &str) -> Result<(), String> {
        let before = self.servers.len();
        self.servers.retain(|s| s.id != id);
        if self.servers.len() == before {
            return Err(format!("No server with id '{id}'"));
        }
        for profile in &mut self.profiles {
            profile.enabled_server_ids.retain(|sid| sid != id);
            // Drop any tool-scope allow-list for the removed server so it can't orphan.
            profile.tool_scope.remove(id);
        }
        self.tool_overrides.remove(id);
        self.pinned_tools.remove(id);

        let sanitized_id = sanitize_segment(id);
        let prefix = format!("{sanitized_id}/");
        self.injection_block_exempt.remove(&sanitized_id);
        self.result_budgets.remove(&sanitized_id);
        self.human_approval_allow.retain(|k| !k.starts_with(&prefix));
        Ok(())
    }

    pub fn active_profile_id(&self) -> String {
        self.active_profile_id
            .clone()
            .or_else(|| self.profiles.first().map(|p| p.id.clone()))
            .unwrap_or_else(|| DEFAULT_PROFILE_ID.to_string())
    }

    pub fn is_enabled(&self, profile_id: &str, server_id: &str) -> bool {
        self.profiles
            .iter()
            .find(|p| p.id == profile_id)
            .map(|p| p.enabled_server_ids.iter().any(|s| s == server_id))
            .unwrap_or(false)
    }

    /// Toggle a server's enabled state within a profile.
    pub fn set_server_enabled(
        &mut self,
        profile_id: &str,
        server_id: &str,
        enabled: bool,
    ) -> Result<(), String> {
        if !self.servers.iter().any(|s| s.id == server_id) {
            return Err(format!("No server with id '{server_id}'"));
        }
        let profile = self
            .profiles
            .iter_mut()
            .find(|p| p.id == profile_id)
            .ok_or_else(|| format!("No profile with id '{profile_id}'"))?;
        let present = profile.enabled_server_ids.iter().any(|s| s == server_id);
        if enabled && !present {
            profile.enabled_server_ids.push(server_id.to_string());
        } else if !enabled && present {
            profile.enabled_server_ids.retain(|s| s != server_id);
        }
        Ok(())
    }

    /// Enable or disable every server in a profile at once.
    ///
    /// Enabling skips team-review servers (local command / LAN URL). Those stay
    /// as they were so Enable all cannot bypass the Teams confirm, and so a
    /// server the member already consented to is not wiped.
    pub fn set_all_enabled(&mut self, profile_id: &str, enabled: bool) -> Result<(), String> {
        let ids: Vec<String> = if enabled {
            self.servers
                .iter()
                .filter(|s| !s.needs_team_enable_review())
                .map(|s| s.id.clone())
                .collect()
        } else {
            Vec::new()
        };
        let profile = self
            .profiles
            .iter_mut()
            .find(|p| p.id == profile_id)
            .ok_or_else(|| format!("No profile with id '{profile_id}'"))?;
        if enabled {
            for id in ids {
                if !profile.enabled_server_ids.contains(&id) {
                    profile.enabled_server_ids.push(id);
                }
            }
        } else {
            profile.enabled_server_ids = Vec::new();
        }
        Ok(())
    }

    /// Enable or disable a single tool on a server. Disabling adds it to the
    /// server's `disabled_tools`; enabling removes it. Idempotent.
    pub fn set_tool_enabled(
        &mut self,
        server_id: &str,
        tool: &str,
        enabled: bool,
    ) -> Result<(), String> {
        let server = self
            .servers
            .iter_mut()
            .find(|s| s.id == server_id)
            .ok_or_else(|| format!("No server with id '{server_id}'"))?;
        let present = server.disabled_tools.iter().any(|t| t == tool);
        if enabled && present {
            server.disabled_tools.retain(|t| t != tool);
        } else if !enabled && !present {
            server.disabled_tools.push(tool.to_string());
        }
        Ok(())
    }

    /// Whether a specific tool is enabled (default-allow: unknown tools are on).
    pub fn is_tool_enabled(&self, server_id: &str, tool: &str) -> bool {
        self.servers
            .iter()
            .find(|s| s.id == server_id)
            .map(|s| !s.disabled_tools.iter().any(|t| t == tool))
            .unwrap_or(true)
    }

    /// Whether a profile exposes a given tool (tool-granular scoping / "FeatureSet"). Default-
    /// allow: if the profile has no `tool_scope` allow-list for `server_id`, every tool on
    /// that server is exposed (server-granular behavior, unchanged). If it does, ONLY the
    /// listed tools are. Layered UNDER the server scope, so it only narrows within a profile's
    /// enabled servers. `tool` is the ORIGINAL tool name (as `route_of` yields it). An unknown
    /// profile ref imposes no extra tool restriction here (the server scope already blocks it).
    pub fn profile_allows_tool(&self, profile_ref: &str, server_id: &str, tool: &str) -> bool {
        let id = self.resolve_profile_id(profile_ref);
        match self.profiles.iter().find(|p| p.id == id) {
            Some(p) => match p.tool_scope.get(server_id) {
                Some(allowed) => allowed.iter().any(|t| t == tool),
                None => true,
            },
            None => true,
        }
    }

    /// Set or clear a profile's tool allow-list for one server. `Some(list)` narrows that
    /// server to exactly those ORIGINAL tool names (an EMPTY list is a real state: expose NO
    /// tools on that server, enforced as block-all). `None` removes the entry, restoring "all
    /// tools on that server". Idempotent. The UI sends `None` only when every tool is selected,
    /// so an unnarrowed profile keeps an empty `tool_scope` (backward compatible), and sends
    /// `Some(subset)` otherwise, distinguishing "all" (None) from "none" (empty list).
    pub fn set_profile_server_tools(
        &mut self,
        profile_id: &str,
        server_id: &str,
        tools: Option<Vec<String>>,
    ) -> Result<(), String> {
        let profile = self
            .profiles
            .iter_mut()
            .find(|p| p.id == profile_id)
            .ok_or_else(|| format!("No profile with id '{profile_id}'"))?;
        match tools {
            Some(list) => {
                profile.tool_scope.insert(server_id.to_string(), list);
            }
            None => {
                profile.tool_scope.remove(server_id);
            }
        }
        Ok(())
    }

    /// Pin or unpin a tool as a lazy-discovery prerequisite (by ORIGINAL tool name).
    /// Idempotent; drops the server's entry when its last pin is removed.
    pub fn set_tool_pinned(&mut self, server_id: &str, tool: &str, pinned: bool) {
        let list = self.pinned_tools.entry(server_id.to_string()).or_default();
        let present = list.iter().any(|t| t == tool);
        if pinned && !present {
            list.push(tool.to_string());
        } else if !pinned && present {
            list.retain(|t| t != tool);
        }
        if list.is_empty() {
            self.pinned_tools.remove(server_id);
        }
    }

    /// Whether a tool is pinned as a prerequisite (default: not pinned).
    pub fn is_tool_pinned(&self, server_id: &str, tool: &str) -> bool {
        self.pinned_tools
            .get(server_id)
            .map(|l| l.iter().any(|t| t == tool))
            .unwrap_or(false)
    }

    /// Set the global destructive-tool deny switch. Mutually exclusive with
    /// `confirm_destructive`: enabling deny clears confirm.
    pub fn set_deny_destructive(&mut self, deny: bool) {
        self.deny_destructive = deny;
        if deny {
            self.confirm_destructive = false;
        }
    }

    /// Set per-call confirmation mode for destructive tools. When enabled,
    /// `deny_destructive` is forced off (they're mutually exclusive: deny hides
    /// tools entirely, confirm intercepts them with a preview).
    pub fn set_confirm_destructive(&mut self, confirm: bool) {
        self.confirm_destructive = confirm;
        if confirm {
            self.deny_destructive = false;
        }
    }

    /// Turn human-in-the-loop approval on or off. Independent of deny/confirm: `deny`
    /// hides tools, `confirm` has the agent re-confirm, `human_approval` holds the call
    /// for a person. When it gates a tool it takes precedence over `confirm_destructive`.
    pub fn set_human_approval(&mut self, on: bool) {
        self.human_approval = on;
    }

    /// Whether the HITL gate is active: the member's OWN toggle, OR an active team's forced
    /// policy. The gate reads this instead of `human_approval` directly so an org lock stays
    /// releasable (it lives in `team_forced_human_approval`, cleared on leave) rather than
    /// permanently overwriting the member's own choice.
    pub fn human_approval_effective(&self) -> bool {
        self.human_approval || self.team_forced_human_approval
    }

    /// Effective (member's own OR team-forced) values for the other tighten-only safety flags,
    /// so an org lock is releasable on leave instead of permanently overwriting the member's own.
    pub fn deny_destructive_effective(&self) -> bool {
        self.deny_destructive || self.team_forced_deny_destructive
    }
    pub fn content_defense_effective(&self) -> bool {
        self.content_defense || self.team_forced_content_defense
    }
    /// Member's own OR team-forced PII pseudonymization (SBS-346).
    pub fn pii_redaction_effective(&self) -> bool {
        self.pii_redaction || self.team_forced_pii_redaction
    }
    pub fn quarantine_on_drift_effective(&self) -> bool {
        self.quarantine_on_drift || self.team_forced_quarantine_on_drift
    }
    /// Member's own OR team-forced fail-closed injection block (SOU-345).
    pub fn block_on_injection_effective(&self) -> bool {
        self.block_on_injection || self.team_forced_block_on_injection
    }
    /// Whether this server should fail closed on a high-confidence injection hit:
    /// block mode effective, and the server is not on the exempt list.
    pub fn should_block_injection_for(&self, server: &str) -> bool {
        if !self.block_on_injection_effective() {
            return false;
        }
        // Exempt only when the map explicitly sets the server to true.
        !self
            .injection_block_exempt
            .get(server)
            .copied()
            .unwrap_or(false)
    }

    /// Add a `server/tool` key to the persistent "always allow" list, so the HITL gate
    /// skips it. Idempotent.
    pub fn allow_tool(&mut self, key: String) {
        if !self.human_approval_allow.contains(&key) {
            self.human_approval_allow.push(key);
        }
    }

    /// Remove a key from the persistent allow list (re-require approval for that tool).
    pub fn revoke_tool(&mut self, key: &str) {
        self.human_approval_allow.retain(|k| k != key);
    }

    /// Whether a `server/tool` key is on the persistent always-allow list.
    pub fn is_tool_allowed(&self, key: &str) -> bool {
        self.human_approval_allow.iter().any(|k| k == key)
    }

    /// Set (or replace) the exposure override for a `(server, original tool)`. An override
    /// with both fields cleared is removed rather than stored empty (and the server's map is
    /// dropped when it becomes empty).
    pub fn set_tool_override(&mut self, server: String, tool: String, ov: ToolOverride) {
        if ov.name.is_none() && ov.description.is_none() {
            self.clear_tool_override(&server, &tool);
        } else {
            self.tool_overrides.entry(server).or_default().insert(tool, ov);
        }
    }

    /// Remove any override for a `(server, original tool)` (restore the server's own
    /// definition), dropping the server's map when it becomes empty.
    pub fn clear_tool_override(&mut self, server: &str, tool: &str) {
        if let Some(m) = self.tool_overrides.get_mut(server) {
            m.remove(tool);
            if m.is_empty() {
                self.tool_overrides.remove(server);
            }
        }
    }

    /// Set lazy discovery mode (gateway exposes meta-tools vs the full catalog).
    pub fn set_lazy_discovery(&mut self, lazy: bool) {
        self.lazy_discovery = lazy;
    }

    /// Set the discovery-mode override. `"lazy"`/`"grouped"`/`"full"` are honored;
    /// any other value (including clearing to the empty string) resets to `None`, so
    /// resolution falls back to `lazy_discovery`. Keeps `lazy_discovery` in sync for
    /// the "lazy"/"full" cases so an older gateway reading only that bool still agrees.
    pub fn set_discovery_mode(&mut self, mode: &str) {
        match mode.trim().to_ascii_lowercase().as_str() {
            "lazy" => {
                self.discovery_mode = Some("lazy".into());
                self.lazy_discovery = true;
            }
            "full" => {
                self.discovery_mode = Some("full".into());
                self.lazy_discovery = false;
            }
            "grouped" => self.discovery_mode = Some("grouped".into()),
            _ => self.discovery_mode = None,
        }
    }

    /// Turn live request/response inspection on or off. When on, the gateway
    /// captures each tool call's args + result into the ephemeral `inspect.jsonl`
    /// ring; when off, nothing is captured and no inspect file is written.
    pub fn set_live_inspect(&mut self, on: bool) {
        self.live_inspect = on;
    }

    pub fn add_profile(&mut self, name: &str) -> String {
        let id = unique_id(&slugify(name), &self.profile_ids());
        self.profiles.push(Profile {
            id: id.clone(),
            name: name.to_string(),
            enabled_server_ids: Vec::new(),
            tool_scope: HashMap::new(),
        });
        id
    }

    pub fn remove_profile(&mut self, id: &str) -> Result<(), String> {
        if self.profiles.len() <= 1 {
            return Err("Cannot remove the last profile".to_string());
        }
        let before = self.profiles.len();
        self.profiles.retain(|p| p.id != id);
        if self.profiles.len() == before {
            return Err(format!("No profile with id '{id}'"));
        }
        if self.active_profile_id.as_deref() == Some(id) {
            self.active_profile_id = self.profiles.first().map(|p| p.id.clone());
        }
        Ok(())
    }

    pub fn set_active_profile(&mut self, id: &str) -> Result<(), String> {
        if !self.profiles.iter().any(|p| p.id == id) {
            return Err(format!("No profile with id '{id}'"));
        }
        self.active_profile_id = Some(id.to_string());
        Ok(())
    }

    /// Servers enabled in the active profile - what the gateway should expose.
    pub fn enabled_servers(&self) -> Vec<&ServerEntry> {
        let active = self.active_profile_id();
        self.servers
            .iter()
            .filter(|s| self.is_enabled(&active, &s.id))
            .collect()
    }

    /// Resolve a profile by id or (case-insensitive) name, returning its id.
    ///
    /// An **empty/whitespace** reference means "unscoped": it follows the active
    /// profile. A **named** reference that matches no existing profile (e.g. one
    /// that was deleted or renamed out from under a scoped client) fails CLOSED:
    /// it resolves to itself, which `is_enabled` matches to no servers, so the
    /// client sees an empty set rather than silently widening to the active
    /// profile's servers. Only ever widen scope on an explicit unscoped request,
    /// never on a dangling reference.
    pub fn resolve_profile_id(&self, profile_ref: &str) -> String {
        if profile_ref.trim().is_empty() {
            return self.active_profile_id();
        }
        self.profile_id_for_ref(profile_ref)
            .unwrap_or_else(|| profile_ref.to_string())
    }

    /// Servers enabled in a specific profile (id or name). Powers per-client
    /// scoping: each gateway can expose only the slice its client needs, so
    /// overlapping verbs from unrelated servers never share one tool surface.
    pub fn enabled_servers_for(&self, profile_ref: &str) -> Vec<&ServerEntry> {
        let id = self.resolve_profile_id(profile_ref);
        self.servers
            .iter()
            .filter(|s| self.is_enabled(&id, &s.id))
            .collect()
    }

    /// Resolve the folder-scoped profile for a client's reported root path, if any
    /// [`folder_profiles`](Self::folder_profiles) mapping matches. The longest matching
    /// mapped path wins (so a nested mapping overrides its parent). Returns the mapped
    /// profile string (id or name, resolved like `client_scopes`), or `None` to fall back to
    /// the client's configured/active profile. Path-only string matching, never touches disk:
    /// canonicalizing would fail for a root that doesn't exist on THIS machine, and the point
    /// is to match what the client reported.
    pub fn profile_for_root(&self, root: &str) -> Option<String> {
        let root = normalize_path(root);
        if root.is_empty() {
            return None;
        }
        self.folder_profiles
            .iter()
            .filter_map(|fp| {
                let base = normalize_path(&fp.path);
                path_is_within(&base, &root).then_some((base.len(), fp.profile.clone()))
            })
            .max_by_key(|(len, _)| *len)
            .map(|(_, profile)| self.resolve_profile_id(&profile))
    }

    /// Replace the folder -> profile routing mappings (the UI edits the list wholesale). Drops
    /// entries with a blank path or profile; stores paths verbatim (normalized only at match
    /// time in [`profile_for_root`]).
    pub fn set_folder_profiles(&mut self, mappings: Vec<FolderProfile>) {
        self.folder_profiles = mappings
            .into_iter()
            .filter(|m| !m.path.trim().is_empty() && !m.profile.trim().is_empty())
            .map(|mut mapping| {
                mapping.profile = self.resolve_profile_id(&mapping.profile);
                mapping
            })
            .collect();
    }

    /// Servers the multi-tenant HTTP bridge must connect: the union of the base
    /// profile's enabled servers and every registered HTTP client's profile, so a
    /// scoped client's servers are always actually connected (per-request
    /// filtering then narrows each client's view). An empty-profile client is
    /// unscoped and contributes nothing beyond the base. Deduplicated by id;
    /// registry order is preserved.
    pub fn bridge_enabled_servers(&self, base: Option<&str>) -> Vec<&ServerEntry> {
        use std::collections::HashSet;
        let base_id = match base {
            Some(p) => self.resolve_profile_id(p),
            None => self.active_profile_id(),
        };
        let mut profile_ids: Vec<String> = vec![base_id];
        for c in &self.http_clients {
            if c.profile.trim().is_empty() {
                continue; // unscoped client: sees the union, adds nothing to it
            }
            let pid = self.resolve_profile_id(&c.profile);
            if !profile_ids.contains(&pid) {
                profile_ids.push(pid);
            }
        }
        let mut ids: HashSet<&str> = HashSet::new();
        for pid in &profile_ids {
            for s in &self.servers {
                if self.is_enabled(pid, &s.id) {
                    ids.insert(s.id.as_str());
                }
            }
        }
        self.servers
            .iter()
            .filter(|s| ids.contains(s.id.as_str()))
            .collect()
    }

    /// Record (or clear) which profile a client was connected with, mirroring the
    /// `CONDUIT_PROFILE` env Conduit wrote into that client's config. `None` or an
    /// empty/whitespace profile clears the binding (the client follows the active
    /// profile). Lets the UI show and re-apply a connected client's scope.
    pub fn set_client_scope(&mut self, client_id: &str, profile: Option<&str>) {
        match profile.map(str::trim).filter(|p| !p.is_empty()) {
            Some(p) => {
                self.client_scopes
                    .insert(client_id.to_string(), self.resolve_profile_id(p));
            }
            None => {
                self.client_scopes.remove(client_id);
            }
        }
    }

    /// Set (or clear) a client's discovery-mode override. `Some("full"|"lazy"|"grouped")`
    /// pins that mode for the client; `None`, an empty/whitespace value, `"inherit"`, or any
    /// unrecognized value clears the entry so the client inherits the global mode.
    pub fn set_client_discovery(&mut self, client_id: &str, mode: Option<&str>) {
        let valid = mode
            .map(|m| m.trim().to_ascii_lowercase())
            .filter(|m| matches!(m.as_str(), "full" | "lazy" | "grouped"));
        match valid {
            Some(m) => {
                self.client_discovery.insert(client_id.to_string(), m);
            }
            None => {
                self.client_discovery.remove(client_id);
            }
        }
    }

    /// This client's discovery-mode override, if any (`None` = inherit the global mode).
    pub fn client_discovery_mode(&self, client_id: &str) -> Option<&str> {
        self.client_discovery.get(client_id).map(String::as_str)
    }

    /// Record what we just wrote into a client's gateway entry (SOU-406).
    pub fn set_client_managed_entry(&mut self, client_id: &str, entry: ManagedEntry) {
        self.client_managed_entries
            .insert(client_id.to_string(), entry);
    }

    /// Clear the ownership record (uninstall or explicit forget).
    pub fn clear_client_managed_entry(&mut self, client_id: &str) {
        self.client_managed_entries.remove(client_id);
    }

    /// What we last wrote for this client, if anything.
    pub fn client_managed_entry(&self, client_id: &str) -> Option<&ManagedEntry> {
        self.client_managed_entries.get(client_id)
    }

    /// Record that a client is *explicitly* unscoped: it follows the active
    /// profile (the full connected set), and we want that to apply live. This is
    /// deliberately distinct from having no entry at all: an empty-string marker
    /// means "follow the active profile now", so a running gateway can drop its
    /// previous scope on the next reload; a missing entry means "no recorded
    /// scope, fall back to the CONDUIT_PROFILE this process booted with" (e.g. an
    /// install from before CONDUIT_CLIENT_ID existed). Without this distinction,
    /// re-scoping a client from a named profile to "all servers" wouldn't take
    /// effect until the client restarted. The frontend already reads a missing
    /// or empty scope identically (`clientScopes?.[id] ?? ""`), so this needs no
    /// UI change.
    pub fn set_client_unscoped(&mut self, client_id: &str) {
        self.client_scopes
            .insert(client_id.to_string(), String::new());
    }

    /// Find the registered HTTP client whose stored hash matches `token`'s
    /// SHA-256, if any. The bridge uses this to resolve a bearer to its scope.
    pub fn http_client_for_token(&self, token: &str) -> Option<&HttpClient> {
        let h = sha256_hex(token);
        self.http_clients.iter().find(|c| ct_eq(&c.token_sha256, &h))
    }
}

/// How [`conduit_dir`] was resolved, for startup diagnostics. Only Windows has
/// interesting cases (MSIX app containers); everywhere else it is `Direct`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirResolution {
    /// Normal process: the natural path IS the real directory.
    Direct,
    /// Running inside an MSIX app container: using the loopback-UNC view of the
    /// real directory, bypassing the package's virtualized shadow copy.
    Devirtualized,
    /// Running inside an MSIX app container but the UNC view is unreachable
    /// (admin share disabled/inaccessible), so we are stuck with the natural
    /// path, which the container may redirect to a STALE shadow copy. The
    /// gateway warns loudly when it sees this - see its startup path.
    VirtualizedFallback,
}

/// Toolport's data dir, anchored so every process agrees regardless of launch
/// context.
///
/// On Windows this is `%USERPROFILE%\AppData\Roaming\Toolport` (migrated from the
/// pre-rename `…\Conduit` leaf when safe). Spelling the path out (instead of the
/// APPDATA known folder) is NOT enough to agree across processes: a gateway
/// spawned by an MSIX-packaged client (e.g. Claude Desktop) runs inside that app's
/// container, whose filesystem filter redirects opens under `AppData\Roaming` -
/// by ANY path spelling, home-derived or not - into the package's `LocalCache`
/// shadow copy, which can be days stale. (Verified empirically 2026-07-05: a
/// probe file written to `%APPDATA%` from inside the Claude container landed in
/// the package `LocalCache`. An earlier version of this comment claimed
/// home-derived paths escape the redirect; that is false.)
/// A shadowed gateway reads a frozen `registry.json` (server/profile edits never
/// arrive) and a stale `approval-endpoint.json` (HITL approvals fail closed
/// against a dead broker port).
///
/// The fix: when this process has MSIX package identity - meaning it was spawned
/// inside a packaged app's container, since Toolport's own binaries never ship as
/// MSIX - address the SAME directory through its loopback-UNC twin
/// (`\\localhost\C$\Users\...`). SMB serves those opens from the real filesystem,
/// outside the virtualization filter's reach (verified on the same machine). If
/// the UNC view is unreachable we fall back to the natural path, no worse than
/// before; see [`DirResolution`] and [`conduit_dir_resolution`].
///
/// Public so every Toolport file (registry, tool cache, audit log, approval
/// endpoint, debug logs) derives from the same anchor - otherwise the app and a
/// client-spawned gateway would read/write different dirs.
pub fn conduit_dir() -> Option<PathBuf> {
    resolve_conduit_dir().0
}

/// How [`conduit_dir`] was resolved. Cached with it; the answer cannot change
/// mid-process (package identity and the home dir are fixed at spawn).
pub fn conduit_dir_resolution() -> DirResolution {
    resolve_conduit_dir().1
}

/// Process-global test override for [`conduit_dir`]. See [`DataDirOverride`].
///
/// The `AtomicBool` is a fast path so debug runs barely pay for the lock: it is only
/// ever flipped by a test, so a normal run does one relaxed load per lookup and skips
/// the `RwLock` entirely. Production release builds compile the whole mechanism out;
/// release-profile *tests* of the gateway binary opt in with `--features test-support`.
#[cfg(any(debug_assertions, test, feature = "test-support"))]
static DATA_DIR_OVERRIDE_ACTIVE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
#[cfg(any(debug_assertions, test, feature = "test-support"))]
static DATA_DIR_OVERRIDE: std::sync::RwLock<Option<PathBuf>> = std::sync::RwLock::new(None);
#[cfg(test)]
static DATA_DIR_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Serialize tests that resolve [`conduit_dir`] with tests that override it.
///
/// The override is process-global, so this lock is required even for tests that only
/// read the normal data directory; otherwise they can observe another test's scratch
/// directory while that test holds a [`DataDirOverride`].
#[cfg(test)]
pub(crate) fn data_dir_test_lock() -> std::sync::MutexGuard<'static, ()> {
    DATA_DIR_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Points [`conduit_dir`] at a scratch directory until the guard drops. **Tests only.**
///
/// Setting `TOOLPORT_DATA_DIR` / `CONDUIT_DATA_DIR` does NOT work from a test:
/// [`resolve_conduit_dir`] memoizes in a `OnceLock`, so whichever test resolves the
/// dir first wins and every later `set_var` is silently ignored, leaving the test
/// reading and writing the developer's REAL data dir. That made suite results
/// order-dependent and leaked fixture files into the debug data dir (SOU-301).
///
/// Deliberately not `#[cfg(test)]`-only: the gateway binary's tests link this library
/// compiled WITHOUT `cfg(test)`, so a cfg-gated hook would be invisible in exactly the
/// place that needs it. Production release binaries omit the hook (`debug_assertions`
/// is off and `test-support` is not enabled). Release-profile binary tests pass
/// `--features test-support`.
///
/// The override is process-global. Every test in the same test binary that resolves
/// [`conduit_dir`] directly or indirectly must hold `data_dir_test_lock`, whether
/// or not that test installs an override itself.
#[cfg(any(debug_assertions, test, feature = "test-support"))]
#[doc(hidden)]
#[must_use = "the override is reverted when the guard drops, so it must be bound"]
pub struct DataDirOverride(());

#[cfg(any(debug_assertions, test, feature = "test-support"))]
impl DataDirOverride {
    pub fn set(path: impl Into<PathBuf>) -> Self {
        *DATA_DIR_OVERRIDE
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(path.into());
        DATA_DIR_OVERRIDE_ACTIVE.store(true, Ordering::SeqCst);
        Self(())
    }
}

#[cfg(any(debug_assertions, test, feature = "test-support"))]
impl Drop for DataDirOverride {
    fn drop(&mut self) {
        // Clear the flag first so a lookup racing the drop falls through to the real
        // resolution rather than reading a half-cleared override.
        DATA_DIR_OVERRIDE_ACTIVE.store(false, Ordering::SeqCst);
        *DATA_DIR_OVERRIDE
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}

/// Resolve the data dir and cache it: the container check and the UNC
/// reachability probe should not run on every path lookup, and a stable answer
/// keeps every consumer (registry, watcher, tool cache, approval endpoint) on
/// one directory for the process lifetime.
///
/// Cache is an `RwLock` (not `OnceLock`) so a desktop-launch data-dir migration
/// can [`invalidate_data_dir_cache`] after renaming `Conduit` → `Toolport`;
/// otherwise a pre-migration resolution would keep pointing at the old path.
fn resolve_conduit_dir() -> (Option<PathBuf>, DirResolution) {
    // Checked ahead of the memoized value so a test can redirect the dir even after
    // something else in the process has already resolved it. Debug-only: release
    // builds have no override mechanism at all.
    #[cfg(any(debug_assertions, test, feature = "test-support"))]
    if DATA_DIR_OVERRIDE_ACTIVE.load(Ordering::SeqCst) {
        if let Some(p) = DATA_DIR_OVERRIDE
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            return (Some(p), DirResolution::Direct);
        }
    }
    {
        let cached = DATA_DIR_RESOLVED
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((path, resolution)) = cached.as_ref() {
            // Re-resolve when the cached path was removed (e.g. legacy leaf renamed).
            let still_valid = path.as_ref().map(|p| p.exists()).unwrap_or(true);
            if still_valid {
                return (path.clone(), *resolution);
            }
        }
    }
    let fresh = compute_conduit_dir();
    *DATA_DIR_RESOLVED
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(fresh.clone());
    fresh
}

static DATA_DIR_RESOLVED: std::sync::RwLock<Option<(Option<PathBuf>, DirResolution)>> =
    std::sync::RwLock::new(None);

/// Drop the memoized data-dir path so the next lookup re-runs resolution.
/// Used after a successful Conduit → Toolport leaf rename on desktop launch.
pub fn invalidate_data_dir_cache() {
    *DATA_DIR_RESOLVED
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
}

fn compute_conduit_dir() -> (Option<PathBuf>, DirResolution) {
    if let Some(dir) = crate::brand::env_var("TOOLPORT_DATA_DIR", "CONDUIT_DATA_DIR") {
        return (Some(PathBuf::from(dir)), DirResolution::Direct);
    }
    #[cfg(windows)]
    {
        let Some(home) = dirs::home_dir() else {
            return (None, DirResolution::Direct);
        };
        let under_roaming = |base: &Path| {
            crate::brand::resolve_data_dir_under(&crate::brand::windows_roaming_base(base))
        };
        if !msix::has_package_identity() {
            return (Some(under_roaming(&home)), DirResolution::Direct);
        }
        match msix::unc_twin(&home) {
            // The profile dir always exists, so a metadata success proves the
            // UNC view actually works before we commit every file to it.
            Some(unc_home) if std::fs::metadata(&unc_home).is_ok() => {
                (Some(under_roaming(&unc_home)), DirResolution::Devirtualized)
            }
            _ => (
                Some(under_roaming(&home)),
                DirResolution::VirtualizedFallback,
            ),
        }
    }
    #[cfg(not(windows))]
    {
        (
            dirs::config_dir().map(|d| crate::brand::resolve_data_dir_under(&d)),
            DirResolution::Direct,
        )
    }
}

/// Best-effort desktop-launch migration of the legacy data-dir leaf (`Conduit`
/// / `Conduit-dev`) to `Toolport` / `Toolport-dev`. Invalidates the path cache
/// on success so subsequent lookups use the new leaf. Returns the new path when
/// a rename happened.
pub fn migrate_legacy_data_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let base = dirs::home_dir().map(|h| crate::brand::windows_roaming_base(&h))?;
    #[cfg(not(windows))]
    let base = dirs::config_dir()?;
    let migrated = crate::brand::migrate_legacy_data_dir_under(&base)?;
    invalidate_data_dir_cache();
    Some(migrated)
}

/// MSIX app-container detection and escape hatch (see [`conduit_dir`]).
#[cfg(windows)]
mod msix {
    use std::path::{Path, PathBuf};

    /// True when this process runs with MSIX package identity, i.e. it was
    /// spawned inside a packaged app's container (child processes inherit the
    /// container). Conduit's own binaries are never packaged, so identity here
    /// always means "inside ANOTHER app's container" - exactly the situation
    /// where `AppData\Roaming` opens get redirected to that package's shadow.
    pub fn has_package_identity() -> bool {
        #[link(name = "kernel32")]
        extern "system" {
            fn GetCurrentPackageFamilyName(length: *mut u32, family_name: *mut u16) -> i32;
        }
        // Per appmodel.h: "The process has no package identity."
        const APPMODEL_ERROR_NO_PACKAGE: i32 = 15700;
        let mut len: u32 = 0;
        let rc = unsafe { GetCurrentPackageFamilyName(&mut len, std::ptr::null_mut()) };
        rc != APPMODEL_ERROR_NO_PACKAGE
    }

    /// The loopback-UNC twin of a local drive path: `C:\Users\x` becomes
    /// `\\localhost\C$\Users\x`. SMB requests are served from the real
    /// filesystem, outside the MSIX virtualization filter, so from inside a
    /// container this reaches the REAL directory. `None` for paths without a
    /// drive root (already UNC, relative); callers then stay on the natural path.
    pub fn unc_twin(p: &Path) -> Option<PathBuf> {
        let s = p.to_str()?;
        let b = s.as_bytes();
        if b.len() < 3
            || !b[0].is_ascii_alphabetic()
            || b[1] != b':'
            || (b[2] != b'\\' && b[2] != b'/')
        {
            return None;
        }
        Some(PathBuf::from(format!(
            r"\\localhost\{}$\{}",
            b[0].to_ascii_uppercase() as char,
            &s[3..]
        )))
    }
}

/// Default path: `<conduit dir>/registry.json`.
pub fn registry_path() -> Option<PathBuf> {
    Some(conduit_dir()?.join("registry.json"))
}

const RECOVERY_NOTICE_FILE: &str = "registry-recovery.json";

/// Written when `load_from` recovers from `registry.json.bak` so the app can
/// surface a one-time notice. Consumed by [`take_recovery_notice`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RegistryRecoveryNotice {
    pub recovered_at_ms: u128,
    /// `"missing"` when the primary was absent; `"corrupt"` when it was unreadable.
    pub reason: String,
    pub quarantine_path: Option<String>,
}

fn recovery_notice_path() -> Option<PathBuf> {
    Some(conduit_dir()?.join(RECOVERY_NOTICE_FILE))
}

fn record_registry_recovery(reason: &str, quarantine: Option<PathBuf>) {
    let Some(path) = recovery_notice_path() else {
        return;
    };
    let recovered_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let notice = RegistryRecoveryNotice {
        recovered_at_ms,
        reason: reason.to_string(),
        quarantine_path: quarantine.as_ref().map(|p| p.to_string_lossy().into_owned()),
    };
    if let Ok(json) = serde_json::to_string_pretty(&notice) {
        let _ = atomic_write(&path, &json);
    }
    eprintln!(
        "toolport: recovered registry from backup ({reason}){}",
        quarantine
            .as_ref()
            .map(|p| format!("; quarantined copy at {}", p.display()))
            .unwrap_or_default()
    );
}

/// Read and delete the pending recovery notice (at most once per recovery).
pub fn take_recovery_notice() -> Option<RegistryRecoveryNotice> {
    let path = recovery_notice_path()?;
    let raw = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    serde_json::from_str(&raw).ok()
}

/// The always-on gateway log (connection lifecycle: starts, connect successes
/// and failures). Shared by the gateway (writer) and the diagnostics command
/// (reader) so the path can't drift between them.
pub fn gateway_log_path() -> Option<PathBuf> {
    Some(conduit_dir()?.join("gateway.log"))
}

/// Sibling `<registry>.bak` path holding the last-known-good registry.
fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".bak");
    PathBuf::from(name)
}

/// Sequence recorded alongside `.bak`, allowing recovery to distinguish two
/// snapshots whose filesystem mtimes collapse into the same coarse bucket.
fn backup_sequence_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".bak.seq");
    PathBuf::from(name)
}

/// How many rolling backup generations `save_to` keeps beyond the single `.bak`.
/// The registry is a few KB, so 5 generations is negligible on disk but means
/// recovery has several recent snapshots to fall back to, not one file that (as
/// in the 2026-07-07 incident) may itself be stale.
const BACKUP_GENERATIONS: usize = 5;

/// The rolling journal generations written by `save_to`, named
/// `<registry>.bak.<sequence>`. The sequence starts from epoch milliseconds and
/// advances monotonically, so name order is age order; returned oldest-first. Excludes the single
/// `<registry>.bak` (no trailing timestamp) and the `.unreadable-*` quarantine
/// files, which use a different prefix.
fn backup_generations(path: &Path) -> Vec<PathBuf> {
    let (Some(dir), Some(base)) = (path.parent(), path.file_name().and_then(|f| f.to_str()))
    else {
        return Vec::new();
    };
    let prefix = format!("{base}.bak.");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut gens: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|f| f.to_str())
                .and_then(|f| f.strip_prefix(&prefix))
                .is_some_and(|suffix| suffix.parse::<u128>().is_ok())
        })
        .collect();
    gens.sort();
    gens
}

/// Append the current good registry to the rolling journal and prune to the
/// newest `BACKUP_GENERATIONS`. Best-effort: a failure here never fails a save -
/// the primary write and the single `.bak` remain the durability guarantees, and
/// this only adds recovery depth on top of them.
fn next_backup_sequence(path: &Path) -> u128 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let latest = backup_generations(path)
        .into_iter()
        .filter_map(|generation| {
            generation
                .file_name()?
                .to_str()?
                .rsplit_once(".bak.")?
                .1
                .parse::<u128>()
                .ok()
        })
        .max()
        .unwrap_or(0);
    now.max(latest.saturating_add(1))
}

fn write_backup_generation(path: &Path, content: &str, sequence: u128) {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".bak.{sequence}"));
    if atomic_write(&PathBuf::from(name), content).is_err() {
        return;
    }
    let mut gens = backup_generations(path);
    while gens.len() > BACKUP_GENERATIONS {
        let _ = std::fs::remove_file(gens.remove(0));
    }
}

/// Recover the registry from the backups `save_to` maintains, newest-first by
/// filesystem modification time across both `.bak` and rolling generations.
/// Returns the first that parses (and
/// best-effort rewrites the primary from it so a later read self-heals), or None
/// when nothing usable remains. Walking the journal means one stale or corrupt
/// `.bak` no longer strands recovery when fresher snapshots exist.
fn restore_from_backup(path: &Path) -> Option<Registry> {
    let single_backup = backup_path(path);
    let single_sequence = std::fs::read_to_string(backup_sequence_path(path))
        .ok()
        .and_then(|raw| raw.trim().parse::<u128>().ok());
    let mut candidates = vec![single_backup.clone()];
    candidates.extend(backup_generations(path));
    candidates.sort_by(|a, b| {
        let modified = |candidate: &Path| {
            std::fs::metadata(candidate)
                .and_then(|metadata| metadata.modified())
                .unwrap_or(std::time::UNIX_EPOCH)
        };
        let sequence = |candidate: &Path| {
            if candidate == single_backup {
                single_sequence
            } else {
                candidate
                    .file_name()?
                    .to_str()?
                    .rsplit_once(".bak.")?
                    .1
                    .parse::<u128>()
                    .ok()
            }
        };
        match (sequence(a), sequence(b)) {
            (Some(a_sequence), Some(b_sequence)) => b_sequence.cmp(&a_sequence),
            // Legacy backups have no sequence sidecar. Preserve mtime ordering
            // for them, preferring the journal only when the coarse times tie.
            _ => modified(b)
                .cmp(&modified(a))
                .then_with(|| b.as_os_str().len().cmp(&a.as_os_str().len())),
        }
    });

    for candidate in candidates {
        let Ok(content) = std::fs::read_to_string(&candidate) else {
            continue;
        };
        if content.trim().is_empty() {
            continue;
        }
        if let Ok(registry) = serde_json::from_str::<Registry>(&content) {
            // Best-effort: restore the primary so we don't keep reading a backup.
            // Recovery still succeeds if this write fails.
            let _ = atomic_write(path, &content);
            return Some(registry);
        }
    }
    None
}

/// What a (retried) read of the registry file actually found.
enum ReadOutcome {
    Content(String),
    /// Still missing or empty after retries: genuinely absent, not a race.
    Absent,
}

/// Read the registry tolerating the transient states a concurrent `atomic_write`
/// (or an SMB view of one - packaged gateways reach this file over the
/// `\\localhost\C$` twin, where rename windows are wider) can expose: a brief
/// not-found, empty, or sharing-violation moment during the rename. A reader
/// that mistakes that moment for "the registry is gone" used to fall into
/// `restore_from_backup`, which REWRITES the primary from a possibly-days-old
/// .bak - the exact mechanism that destroyed a real user registry (manual
/// servers added over three days lost to a self-heal from a stale backup).
/// Retrying a few times before concluding anything makes that race unloseable.
fn read_registry_file(path: &Path) -> ReadOutcome {
    const ATTEMPTS: u32 = 4;
    const BACKOFF_MS: u64 = 75;
    for attempt in 0..ATTEMPTS {
        match std::fs::read_to_string(path) {
            Ok(content) if !content.trim().is_empty() => return ReadOutcome::Content(content),
            // Empty, missing, locked (sharing violation), or any other error:
            // all indistinguishable from a rename in flight. Wait and re-look.
            _ => {}
        }
        if attempt + 1 < ATTEMPTS {
            std::thread::sleep(std::time::Duration::from_millis(BACKOFF_MS));
        }
    }
    ReadOutcome::Absent
}

/// Preserve an unreadable registry file next to the original before anything
/// overwrites it. "Unreadable" does NOT always mean corrupt: on a machine
/// running mixed builds it can be a NEWER schema this binary can't parse, and
/// destroying it silently loses whatever the newer build stored. Best-effort;
/// keeps the most recent few so a repeating failure can't fill the disk.
/// Returns the quarantine file path when a copy was written.
fn quarantine_unreadable(path: &Path, content: &str) -> Option<PathBuf> {
    const KEEP: usize = 3;
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".unreadable-{ts}"));
    let dest = PathBuf::from(name);
    atomic_write(&dest, content).ok()?;
    // Prune older quarantine files beyond the newest KEEP.
    let (Some(dir), Some(base)) = (path.parent(), path.file_name().and_then(|f| f.to_str()))
    else {
        return None;
    };
    let prefix = format!("{base}.unreadable-");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return None;
    };
    let mut quarantined: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|f| f.to_str())
                .is_some_and(|f| f.starts_with(&prefix))
        })
        .collect();
    // Timestamps are fixed-width for any realistic epoch, so name order = age order.
    quarantined.sort();
    while quarantined.len() > KEEP {
        let _ = std::fs::remove_file(quarantined.remove(0));
    }
    Some(dest)
}

fn load_from_inner(path: &Path) -> Result<Registry, String> {
    let mut registry = match read_registry_file(path) {
        // Genuinely missing or empty (not a rename race - read_registry_file
        // already waited that out): recover the last-known-good from the .bak
        // sibling if one survived, else this is a first run.
        ReadOutcome::Absent => {
            if let Some(reg) = restore_from_backup(path) {
                record_registry_recovery("missing", None);
                Ok(reg)
            } else {
                Ok(Registry::default())
            }
        }
        ReadOutcome::Content(content) => match serde_json::from_str(&content) {
            Ok(reg) => Ok(reg),
            // Present but unparseable by THIS build: corrupt, or a newer schema.
            // Quarantine the evidence BEFORE restore_from_backup self-heals the
            // primary from .bak, so nothing is ever silently destroyed.
            Err(e) => {
                let quarantine = quarantine_unreadable(path, &content);
                match restore_from_backup(path) {
                    Some(reg) => {
                        record_registry_recovery("corrupt", quarantine);
                        Ok(reg)
                    }
                    None => Err(format!("Corrupt registry: {e}")),
                }
            }
        },
    }?;
    registry.normalize_profile_references();
    Ok(registry)
}

/// Load an explicit registry while holding its cross-process lock across the full
/// read/recovery path. Recovery can rewrite the primary from a backup, so even a caller
/// that only intends to read must serialize with writers (SOU-330).
pub fn load_from(path: &Path) -> Result<Registry, String> {
    let lock = lock_for(path, registry_lock_timeout())?;
    load_from_locked(path, &lock)
}

/// Load while the caller already holds a registry lock. Requiring the guard by reference
/// prevents nested acquisition in read-modify-write paths while making the lock requirement
/// explicit at call sites that need to keep it through a later save.
pub fn load_from_locked(path: &Path, _lock: &FileLock) -> Result<Registry, String> {
    load_from_inner(path)
}

pub fn save_to(path: &Path, registry: &Registry) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(registry).map_err(|e| e.to_string())?;
    let existing = std::fs::read_to_string(path).ok();
    // No-op guard: if the on-disk registry is already semantically identical to what we
    // would write, skip the whole save so we never bump the file's mtime. This is NOT just
    // an IO optimization. The gateway watches this file's mtime and, on ANY change, does a
    // full rebuild that re-spawns every stdio MCP server. The team sync loop calls save()
    // every ~25s even on a no-op (304) pull, so without this guard each cycle bumped the
    // mtime and made every gateway respawn every server; the orphaned npx/node children
    // piled up until the machine ran out of RAM. Compare as PARSED JSON, not raw bytes, so
    // HashMap key-order jitter across a load->save round-trip can't masquerade as a change.
    if let Some(cur) = existing.as_deref() {
        if let Ok(cur_val) = serde_json::from_str::<serde_json::Value>(cur) {
            if serde_json::to_value(registry).map(|v| v == cur_val).unwrap_or(false) {
                return Ok(());
            }
        }
    }
    // Snapshot the current on-disk registry to a `.bak` sibling before overwriting,
    // but only if it still parses, so a bad write or an accidental deletion of
    // registry.json has a last-known-good to recover from (see load_from). An
    // existing file that does NOT parse is quarantined instead of silently
    // overwritten: on a mixed-version machine it may be a newer build's registry,
    // and this save (from an older binary) must not be the thing that destroys it.
    if let Some(existing) = existing {
        if !existing.trim().is_empty() {
            if serde_json::from_str::<Registry>(&existing).is_ok() {
                // Single last-known-good (compat + the load_from fast path)...
                let sequence = next_backup_sequence(path);
                if atomic_write(&backup_path(path), &existing).is_ok() {
                    let sequence_path = backup_sequence_path(path);
                    if atomic_write(&sequence_path, &sequence.to_string()).is_err() {
                        // Never leave older metadata attached to newer `.bak`
                        // contents; mtime fallback is safer than a stale sequence.
                        let _ = std::fs::remove_file(sequence_path);
                    }
                }
                // ...plus a rolling journal generation, so recovery can fall back
                // to the immediately-previous state and a few before it, not just
                // whatever the one .bak happens to hold.
                write_backup_generation(path, &existing, sequence);
            } else {
                quarantine_unreadable(path, &existing);
            }
        }
    }
    // The registry is the single source of truth for every server, so a crash,
    // power loss, or full disk mid-write must not be able to truncate it.
    atomic_write(path, &json)
}

pub fn load() -> Result<Registry, String> {
    load_resolved()
}

pub fn save(registry: &Registry) -> Result<(), String> {
    let path = resolved_path().ok_or("Could not resolve registry path")?;
    save_to(&path, registry)
}

/// A held cross-process exclusive lock over a file's sibling `<path>.lock`, released on drop
/// (and by the OS if the holding process exits). Serializes a read-modify-write section across
/// the desktop app, the gateway binary, and the team-sync worker, so no writer's save can
/// revert another process's concurrent change (SOU-23). Used for the registry (via `update` /
/// `update_at` / `lock_at`) and the integrity pins/quarantine stores (SOU-165). Advisory: it
/// only excludes other holders of THIS lock, which every writer of the guarded file takes.
pub struct FileLock(std::fs::File);

impl Drop for FileLock {
    fn drop(&mut self) {
        // Also released when the File closes / the process exits; explicit for clarity.
        let _ = self.0.unlock();
    }
}

/// The sibling lock file for the registry at `path` (`<registry>.lock`). A dedicated file,
/// not registry.json itself, so locking never races the atomic temp+rename that swaps the
/// registry inode on every save.
fn lock_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".lock");
    PathBuf::from(s)
}

/// Acquire the exclusive registry lock, retrying briefly under contention. Registry writes
/// are sub-millisecond, so a real conflict clears at once; a holder stuck past the deadline
/// surfaces as an error rather than hanging the caller indefinitely.
const DEFAULT_REGISTRY_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// The contention deadline, overridable via `TOOLPORT_LOCK_TIMEOUT_MS`.
///
/// The default is a PRODUCTION budget: giving up is the right answer when a holder is
/// stuck, because hanging the app is worse. But the multi-process concurrency tests
/// deliberately run eight or more writers at once and assert an INVARIANT ("no update is
/// lost"), not a latency budget. On a loaded CI runner the 5s default expires, one writer
/// correctly gives up, and the test fails for the machine's timing rather than for the
/// thing it exists to catch. That flake hit three separate tests and made a clean PR look
/// broken (SBS-895).
///
/// An env override rather than `cfg!(test)` because the rate-limit repro spawns real child
/// processes, which are not test builds and inherit only the environment.
fn registry_lock_timeout() -> std::time::Duration {
    crate::brand::env_var("TOOLPORT_LOCK_TIMEOUT_MS", "CONDUIT_LOCK_TIMEOUT_MS")
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map(std::time::Duration::from_millis)
        .unwrap_or(DEFAULT_REGISTRY_LOCK_TIMEOUT)
}

/// Raise the store-lock deadline for a test that deliberately runs many concurrent
/// writers, restoring the previous value on drop.
///
/// Those tests assert that no update is LOST, not that every writer wins the lock inside
/// the production budget. Raising the deadline is monotonically safe: it can only make a
/// concurrently-running test more tolerant, never less. Child processes inherit it, which
/// is why this is an env var and not a `cfg!(test)` branch (SBS-895).
#[cfg(any(debug_assertions, test, feature = "test-support"))]
#[doc(hidden)]
#[must_use = "the override is reverted when the guard drops, so it must be bound"]
pub struct LockTimeoutOverride(Option<std::ffi::OsString>);

#[cfg(any(debug_assertions, test, feature = "test-support"))]
impl LockTimeoutOverride {
    pub fn generous() -> Self {
        let previous = std::env::var_os("TOOLPORT_LOCK_TIMEOUT_MS");
        std::env::set_var("TOOLPORT_LOCK_TIMEOUT_MS", "60000");
        Self(previous)
    }
}

#[cfg(any(debug_assertions, test, feature = "test-support"))]
impl Drop for LockTimeoutOverride {
    fn drop(&mut self) {
        match &self.0 {
            Some(value) => std::env::set_var("TOOLPORT_LOCK_TIMEOUT_MS", value),
            None => std::env::remove_var("TOOLPORT_LOCK_TIMEOUT_MS"),
        }
    }
}

fn lock_for(path: &Path, timeout: std::time::Duration) -> Result<FileLock, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(lock_path(path))
        .map_err(|e| format!("Could not open the registry lock: {e}"))?;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(FileLock(file)),
            // Contended. The error KIND for "already locked" differs by platform (`WouldBlock`
            // on Unix, a lock-violation OS error on Windows), so do NOT gate the retry on it:
            // the lock file already opened above, so any try-lock failure here is contention.
            // Retry briefly, then surface it rather than hang the caller indefinitely.
            Err(e) => {
                if std::time::Instant::now() >= deadline {
                    return Err(format!(
                        "The registry is locked by another Toolport process ({e}); try again."
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
    }
}

/// Load-modify-save the resolved registry while holding the cross-process lock, so the
/// write reflects (and can't clobber) any change another process made since this one last
/// read. `f` mutates a FRESH on-disk copy; the persisted registry and `f`'s value are
/// returned. Every registry writer (app commands, the gateway toggle, team sync) goes
/// through this or [`update_at`] — that is what makes the lock effective.
pub fn update<T>(
    f: impl FnOnce(&mut Registry) -> Result<T, String>,
) -> Result<(Registry, T), String> {
    let path = resolved_path().ok_or("Could not resolve registry path")?;
    let lock = lock_for(&path, registry_lock_timeout())?;
    let mut reg = load_from_locked(&path, &lock)?;
    let out = f(&mut reg)?;
    // Save to the exact path we locked and loaded. Re-resolving after `f` would let a
    // runtime env override change redirect this write to a different, unlocked registry.
    save_to(&path, &reg)?;
    Ok((reg, out))
}

/// Acquire the cross-process lock guarding an explicit path (its sibling `<path>.lock`), for a
/// caller that runs its own load-modify-save rather than using [`update_at`]: the gateway's
/// agent toggle (which interleaves audit + early returns), and the integrity pins/quarantine
/// stores (SOU-165). Hold the returned guard across the entire read-decide-write.
pub fn lock_at(path: &Path) -> Result<FileLock, String> {
    lock_for(path, registry_lock_timeout())
}

/// Acquire an explicit-path lock with a caller-appropriate contention deadline.
/// Registry operations use the short default above; operations that deliberately
/// hold a lock across network I/O need to cover that I/O's timeout instead.
pub(crate) fn lock_at_for(path: &Path, timeout: std::time::Duration) -> Result<FileLock, String> {
    lock_for(path, timeout)
}

/// Like [`update`] but for a caller that already resolved an explicit path (the gateway
/// binary), locking the same sibling lock file so it serializes with the app's `update`.
pub fn update_at<T>(
    path: &Path,
    f: impl FnOnce(&mut Registry) -> Result<T, String>,
) -> Result<(Registry, T), String> {
    let lock = lock_for(path, registry_lock_timeout())?;
    let mut reg = load_from_locked(path, &lock)?;
    let out = f(&mut reg)?;
    save_to(path, &reg)?;
    Ok((reg, out))
}

/// The path the registry actually resolves to, honoring `TOOLPORT_REGISTRY`
/// (legacy: `CONDUIT_REGISTRY`).
pub fn resolved_path() -> Option<PathBuf> {
    if let Some(path) = crate::brand::env_var("TOOLPORT_REGISTRY", "CONDUIT_REGISTRY") {
        return Some(PathBuf::from(path));
    }
    registry_path()
}

/// Load honoring the `TOOLPORT_REGISTRY` / `CONDUIT_REGISTRY` env override (used
/// by the gateway and tests), falling back to the default path.
pub fn load_resolved() -> Result<Registry, String> {
    let registry = match resolved_path() {
        Some(path) => load_from(&path),
        None => Ok(Registry::default()),
    }?;
    migrate_profile_stores(&registry)?;
    Ok(registry)
}

/// True when a command argument looks like it carries a secret: an inline
/// credential param (password=, token=, ...) or a connection URI with embedded
/// userinfo (scheme://user:pass@host). Used to redact args before sharing, since
/// some servers (e.g. Postgres) take a connection string with a password in args.
/// Biased toward over-redacting: for a share, a false positive is harmless.
pub(crate) fn arg_looks_secret(arg: &str) -> bool {
    let lower = arg.to_ascii_lowercase();
    let trimmed = lower.trim();
    const NEEDLES: [&str; 8] = [
        "password=", "pwd=", "token=", "apikey=", "api_key=", "secret=", "accountkey=",
        "access_key",
    ];
    if NEEDLES.iter().any(|n| lower.contains(n)) {
        return true;
    }
    // Common remote-MCP launchers pass an HTTP auth header as one argument.
    // It has no key=value marker, but sharing it would disclose the credential.
    for header in ["authorization:", "proxy-authorization:"] {
        if trimmed
            .strip_prefix(header)
            .is_some_and(|value| !value.trim().is_empty())
        {
            return true;
        }
    }
    // Some launchers split the header name and value into separate arguments.
    // Catch the credential-bearing value even when "Authorization:" is adjacent.
    for scheme in ["bearer", "basic", "digest"] {
        if let Some(value) = trimmed.strip_prefix(scheme) {
            if value.chars().next().is_some_and(char::is_whitespace) && !value.trim().is_empty() {
                return true;
            }
        }
    }
    // A connection URI with embedded userinfo: scheme://user:pass@host/...
    if let Some((_, rest)) = arg.split_once("://") {
        let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
        if authority.contains('@') {
            return true;
        }
    }
    false
}

/// Redact credentials embedded in a URL's authority: `scheme://user:pass@host/x`
/// (or `scheme://token@host/x`) becomes `scheme://<redacted>@host/x`. A URL is a
/// legitimate place for a secret (HTTP basic, token-as-username), so it must be
/// stripped anywhere a setup leaves the machine - env/arg redaction alone misses the
/// `url` field. Returns the input unchanged when there is no userinfo. Best-effort
/// string surgery (no URL-crate dependency): only the span between `://` and the
/// first `/?#` is touched, and ANY `@` there is treated as a userinfo separator.
pub(crate) fn redact_url_userinfo(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let auth_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(auth_end);
    match authority.rsplit_once('@') {
        Some((_userinfo, host)) => format!("{scheme}://<redacted>@{host}{tail}"),
        None => url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approval::fingerprint_allow_key;

    static REGISTRY_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Pins the plumbing the concurrency tests rely on (SBS-895). Without this, a typo in
    /// the env-var name would silently leave those tests on the 5s production budget and
    /// they would go on flaking under load with nothing to point at.
    #[test]
    fn lock_timeout_honors_the_env_override_and_ignores_junk() {
        let _env = REGISTRY_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = std::env::var_os("TOOLPORT_LOCK_TIMEOUT_MS");
        std::env::remove_var("TOOLPORT_LOCK_TIMEOUT_MS");

        assert_eq!(registry_lock_timeout(), DEFAULT_REGISTRY_LOCK_TIMEOUT);

        {
            let _guard = LockTimeoutOverride::generous();
            assert_eq!(
                registry_lock_timeout(),
                std::time::Duration::from_millis(60_000),
                "the override must actually reach the lock, or the concurrency tests \
                 silently keep the production budget"
            );
        }
        // Dropping the guard restores, so one test cannot leak a long budget into another.
        assert_eq!(registry_lock_timeout(), DEFAULT_REGISTRY_LOCK_TIMEOUT);

        // Junk and zero fall back rather than producing a 0ms (instantly-expiring) budget.
        for bad in ["", "   ", "abc", "0", "-5", "9999999999999999999999"] {
            std::env::set_var("TOOLPORT_LOCK_TIMEOUT_MS", bad);
            assert_eq!(
                registry_lock_timeout(),
                DEFAULT_REGISTRY_LOCK_TIMEOUT,
                "{bad:?} must fall back to the default, not disable locking"
            );
        }

        match previous {
            Some(value) => std::env::set_var("TOOLPORT_LOCK_TIMEOUT_MS", value),
            None => std::env::remove_var("TOOLPORT_LOCK_TIMEOUT_MS"),
        }
    }

    fn sample_server(name: &str) -> ServerEntry {
        ServerEntry {
            id: String::new(),
            name: name.to_string(),
            transport: "stdio".to_string(),
            command: Some("npx".to_string()),
            args: vec!["-y".to_string(), format!("@scope/{name}")],
            env: vec![],
            url: None,
            source: Some("manual".to_string()),
            disabled_tools: vec![],
            cwd: None,
            client_credentials: None,
            unknown_fields: serde_json::Map::new(),
        }
    }

    #[test]
    fn default_has_one_active_profile() {
        let r = Registry::default();
        assert_eq!(r.profiles.len(), 1);
        assert_eq!(r.active_profile_id(), DEFAULT_PROFILE_ID);
        assert!(r.enabled_servers().is_empty());
    }

    #[test]
    fn update_at_loads_fresh_and_preserves_a_concurrent_write() {
        // The core SOU-23 property: because `update_at` load-modify-saves a FRESH on-disk
        // copy (under the cross-process lock), a write another process made to a DIFFERENT
        // field between this process's reads is preserved, not reverted. Uses an explicit
        // path (no CONDUIT_REGISTRY env), so it's independent of other tests.
        let dir = std::env::temp_dir().join(format!("conduit-sou23-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("registry.json");
        save_to(&path, &Registry::default()).unwrap();

        // Simulate a concurrent external writer flipping `allow_agent_control` on disk.
        let mut disk = load_from(&path).unwrap();
        disk.allow_agent_control = true;
        save_to(&path, &disk).unwrap();

        // Our update touches a different field. Loading fresh must keep the concurrent change.
        let (out, ()) = update_at(&path, |r| {
            r.deny_destructive = true;
            Ok(())
        })
        .unwrap();
        assert!(out.deny_destructive, "our change applied");
        assert!(out.allow_agent_control, "the concurrent write was NOT reverted");

        let reloaded = load_from(&path).unwrap();
        assert!(reloaded.deny_destructive && reloaded.allow_agent_control);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// One `secrets_generation` increment, retrying the way a real caller would.
    ///
    /// `lock_for` gives up after a bounded wait and returns "The registry is locked
    /// by another Toolport process ...; try again." That is an expected outcome
    /// under sustained contention, not a failure: every writer here holds the lock
    /// across an fsync-ing `save_to`, so on a slow filesystem the queue can exceed
    /// the acquisition deadline. Unwrapping it made the test assert "the lock is
    /// always acquired within the deadline" on top of the property it actually
    /// means to assert, which is "no update is lost" — and that is why it failed
    /// under `cargo test --lib update_at_serializes_concurrent_writers` yet passed
    /// in the full suite, where unrelated work spaced the writers out (#652).
    ///
    /// Only the contention error is retried; any other error still fails the test.
    fn increment_with_retry(path: &Path) {
        // Bound the retries by wall clock, not by a count: each attempt already
        // spins inside `lock_for` for its own deadline, so a fixed attempt count
        // would multiply out to minutes before the test admitted defeat.
        let give_up_at = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            match update_at(path, |r| {
                r.secrets_generation += 1;
                Ok(())
            }) {
                Ok(_) => return,
                Err(e) if e.contains("is locked by another") => {
                    assert!(
                        std::time::Instant::now() < give_up_at,
                        "registry lock stayed contended for 60s; this is a real hang, not scheduling"
                    );
                    // Let the current holder finish; `lock_for` already spun.
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(e) => panic!("update_at failed for a reason other than contention: {e}"),
            }
        }
    }

    #[test]
    fn update_at_serializes_concurrent_writers_with_no_lost_updates() {
        // The definitive lock check: many threads each read-increment-write via update_at.
        // Because each increment is a load-modify-save under the exclusive lock, none are
        // lost, so the final count equals the total number of writes. Without the lock, the
        // interleaved read-modify-write would drop increments. This exercises the same file
        // lock used cross-process: each update_at opens its own handle and contends on it.
        let dir = std::env::temp_dir().join(format!("conduit-sou23-conc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("registry.json");
        save_to(&path, &Registry::default()).unwrap(); // secrets_generation starts at 0

        const THREADS: u64 = 4;
        const PER: u64 = 30;
        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let p = path.clone();
                std::thread::spawn(move || {
                    for _ in 0..PER {
                        increment_with_retry(&p);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        let final_reg = load_from(&path).unwrap();
        assert_eq!(
            final_reg.secrets_generation,
            THREADS * PER,
            "every increment persisted; the lock prevented lost updates"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn add_server_assigns_unique_slug_ids() {
        let mut r = Registry::default();
        let a = r.add_server(sample_server("File System"));
        let b = r.add_server(sample_server("File System"));
        assert_eq!(a, "file-system");
        assert_eq!(b, "file-system-2");
        assert_eq!(r.servers.len(), 2);
    }

    #[test]
    fn remove_server_cleans_up_server_state() {
        let mut r = Registry::default();
        let id = r.add_server(sample_server("Github MCP"));
        // `tool_overrides` / `pinned_tools` are keyed by the RAW registry id, while
        // `injection_block_exempt` / `result_budgets` / the allow-list are keyed by
        // `sanitize_segment(id)` because that is what the gateway looks them up with
        // (`toolport-gateway.rs`: `let srv_owned = sanitize_segment(server_id)`).
        // Cleaning the wrong one of the two is a silent no-op, so keep both shapes
        // exercised here.
        let sanitized_id = sanitize_segment(&id);
        r.set_tool_override(
            id.clone(),
            "search".to_string(),
            ToolOverride {
                name: Some("repo-search".to_string()),
                description: None,
            },
        );
        r.set_tool_pinned(&id, "create_issue", true);
        let key = fingerprint_allow_key(&sanitized_id, "create_issue", "v2:testfp");
        r.allow_tool(key.clone());
        r.injection_block_exempt.insert(sanitized_id.clone(), true);
        r.result_budgets.insert(sanitized_id.clone(), 100);
        // Block mode on, so `should_block_injection_for` reports the exemption
        // instead of short-circuiting on the global flag.
        r.block_on_injection = true;

        // Assert setup
        assert!(r.tool_overrides.get(&id).is_some());
        assert!(r.is_tool_pinned(&id, "create_issue"));
        assert!(r.is_tool_allowed(&key));
        assert!(
            !r.should_block_injection_for(&sanitized_id),
            "an exempt server must not be blocked while it is registered"
        );
        assert!(r.result_budgets.contains_key(&sanitized_id));

        // Act
        r.remove_server(&id).unwrap();

        // Assert cleanup
        assert!(r.tool_overrides.get(&id).is_none());
        assert!(!r.is_tool_pinned(&id, "create_issue"));
        assert!(!r.is_tool_allowed(&key));
        // Through the reader the gateway actually calls, not the raw map: a stale
        // exemption surviving removal would silently disable injection blocking for
        // whatever server next takes this id.
        assert!(
            r.should_block_injection_for(&sanitized_id),
            "the injection exemption must not survive removing the server"
        );
        assert!(!r.result_budgets.contains_key(&sanitized_id));
    }

    #[test]
    fn remove_server_does_not_restore_state_when_id_is_reused() {
        let mut r = Registry::default();
        let id = r.add_server(sample_server("Github MCP"));
        let sanitized_id = sanitize_segment(&id);
        r.set_tool_override(
        id.clone(),
        "search".to_string(),
        ToolOverride {
            name: Some("repo-search".to_string()),
            description: None,
          },
        );
        r.set_tool_pinned(&id, "create_issue", true);
        let key = fingerprint_allow_key(&sanitized_id, "create_issue", "v2:testfp");
        r.allow_tool(key.clone());
        r.injection_block_exempt.insert(sanitized_id.clone(), true);
        r.result_budgets.insert(sanitized_id.clone(), 100);

        // Act
        r.remove_server(&id).unwrap();

        let new_id = r.add_server(sample_server("GitHub MCP"));
        // Assert
        assert_eq!(new_id, id);
        assert!(r.tool_overrides.get(&new_id).is_none());
        assert!(!r.is_tool_pinned(&new_id, "create_issue"));
        let new_key = fingerprint_allow_key(&sanitize_segment(&new_id), "create_issue", "v2:testfp");
        assert!(!r.is_tool_allowed(&new_key));
        assert!(!r.injection_block_exempt.contains_key(&sanitized_id));
        assert!(!r.result_budgets.contains_key(&sanitized_id));
    }

    #[test]
    fn toggle_drives_active_profile_membership() {
        let mut r = Registry::default();
        let id = r.add_server(sample_server("github"));
        let profile = r.active_profile_id();
        assert!(!r.is_enabled(&profile, &id));
        r.set_server_enabled(&profile, &id, true).unwrap();
        assert!(r.is_enabled(&profile, &id));
        assert_eq!(r.enabled_servers().len(), 1);
        r.set_server_enabled(&profile, &id, false).unwrap();
        assert!(r.enabled_servers().is_empty());
    }

    #[test]
    fn set_all_enabled_skips_unreviewed_team_servers() {
        let mut r = Registry::default();
        let own = r.add_server(sample_server("github"));
        let mut team_cmd = sample_server("team-npx");
        team_cmd.source = Some("team:t1".into());
        let team_cmd_id = r.add_server(team_cmd);
        let mut team_lan = sample_server("team-lan");
        team_lan.transport = "http".into();
        team_lan.command = None;
        team_lan.args = vec![];
        team_lan.url = Some("http://10.0.0.5:8080/mcp".into());
        team_lan.source = Some("team:t1".into());
        let team_lan_id = r.add_server(team_lan);
        let mut team_public = sample_server("team-public");
        team_public.transport = "http".into();
        team_public.command = None;
        team_public.args = vec![];
        team_public.url = Some("https://1.2.3.4/mcp".into());
        team_public.source = Some("team:t1".into());
        let team_public_id = r.add_server(team_public);

        r.set_all_enabled("default", true).unwrap();
        assert!(r.is_enabled("default", &own));
        assert!(r.is_enabled("default", &team_public_id));
        assert!(
            !r.is_enabled("default", &team_cmd_id),
            "team stdio stays off until explicit enable"
        );
        assert!(
            !r.is_enabled("default", &team_lan_id),
            "team LAN URL stays off until explicit enable"
        );

        r.set_server_enabled("default", &team_cmd_id, true).unwrap();
        r.set_all_enabled("default", true).unwrap();
        assert!(
            r.is_enabled("default", &team_cmd_id),
            "consented review server stays on"
        );
        assert!(!r.is_enabled("default", &team_lan_id));
    }

    #[test]
    fn profiles_isolate_enabled_sets() {
        let mut r = Registry::default();
        let id = r.add_server(sample_server("postgres"));
        let work = r.add_profile("Work");
        r.set_server_enabled("default", &id, true).unwrap();
        assert!(r.is_enabled("default", &id));
        assert!(!r.is_enabled(&work, &id));
        r.set_active_profile(&work).unwrap();
        assert!(r.enabled_servers().is_empty());
    }

    #[test]
    fn enabled_servers_for_scopes_by_profile_id_or_name() {
        let mut r = Registry::default();
        let db = r.add_server(sample_server("postgres"));
        let pay = r.add_server(sample_server("stripe"));
        let billing = r.add_profile("Billing");
        // default enables only postgres; Billing enables only stripe.
        r.set_server_enabled("default", &db, true).unwrap();
        r.set_server_enabled(&billing, &pay, true).unwrap();

        // Resolve by name (case-insensitive) and by id, independent of active.
        let by_name: Vec<_> = r.enabled_servers_for("billing").iter().map(|s| s.id.clone()).collect();
        assert_eq!(by_name, vec![pay.clone()]);
        let by_id: Vec<_> = r.enabled_servers_for("default").iter().map(|s| s.id.clone()).collect();
        assert_eq!(by_id, vec![db]);
        // A NAMED reference that matches no profile (deleted/renamed) fails CLOSED:
        // an empty set, NOT a silent widening to the active profile's servers.
        assert!(
            r.enabled_servers_for("nope").is_empty(),
            "unknown profile must fail closed, not fall back to active"
        );
    }

    #[test]
    fn profile_for_root_longest_prefix_wins_on_a_path_boundary() {
        let mut r = Registry::default();
        r.folder_profiles = vec![
            FolderProfile {
                path: "/home/me/work".into(),
                profile: "Work".into(),
            },
            FolderProfile {
                path: "/home/me/work/client-a".into(),
                profile: "ClientA".into(),
            },
            FolderProfile {
                path: "/home/me/personal".into(),
                profile: "Personal".into(),
            },
        ];
        // Exact match, and a descendant picks the parent mapping.
        assert_eq!(r.profile_for_root("/home/me/work"), Some("Work".into()));
        assert_eq!(r.profile_for_root("/home/me/work/src"), Some("Work".into()));
        // A more specific nested mapping wins over its parent.
        assert_eq!(
            r.profile_for_root("/home/me/work/client-a/repo"),
            Some("ClientA".into())
        );
        assert_eq!(r.profile_for_root("/home/me/personal/notes"), Some("Personal".into()));
        // No mapping -> None (caller falls back to the configured profile).
        assert_eq!(r.profile_for_root("/tmp/other"), None);
        // Boundary: a sibling sharing a NAME prefix must not match ("work" vs "workspace").
        assert_eq!(r.profile_for_root("/home/me/workspace"), None);
        // Empty root never matches.
        assert_eq!(r.profile_for_root(""), None);
    }

    #[test]
    fn tool_scope_narrows_a_profile_to_specific_tools() {
        let mut r = Registry::default();
        let gh = r.add_server(sample_server("github"));
        let db = r.add_server(sample_server("postgres"));
        // Default-allow: no scope -> every tool exposed on every server.
        assert!(r.profile_allows_tool("default", &gh, "search"));
        assert!(r.profile_allows_tool("default", &db, "query"));

        // Narrow github to only `search`; postgres untouched.
        r.set_profile_server_tools("default", &gh, Some(vec!["search".into()]))
            .unwrap();
        assert!(r.profile_allows_tool("default", &gh, "search"));
        assert!(!r.profile_allows_tool("default", &gh, "create_issue")); // not allow-listed
        assert!(r.profile_allows_tool("default", &db, "query")); // no scope on db -> all allowed
        // Resolves by profile NAME too, like the other scope lookups.
        assert!(!r.profile_allows_tool("Default", &gh, "create_issue"));

        // Clearing restores all-allowed and leaves the map empty (backward compatible).
        r.set_profile_server_tools("default", &gh, None).unwrap();
        assert!(r.profile_allows_tool("default", &gh, "create_issue"));
        assert!(r.profiles[0].tool_scope.is_empty());
    }

    #[test]
    fn empty_allow_list_exposes_no_tools_distinct_from_clear() {
        let mut r = Registry::default();
        let gh = r.add_server(sample_server("github"));
        // Some(empty) = expose NO tools on this server (not the same as "all tools").
        r.set_profile_server_tools("default", &gh, Some(vec![])).unwrap();
        assert!(!r.profile_allows_tool("default", &gh, "search"));
        assert!(!r.profile_allows_tool("default", &gh, "anything"));
        assert!(r.profiles[0].tool_scope.contains_key(&gh));
        // None = clear the narrowing, back to all tools.
        r.set_profile_server_tools("default", &gh, None).unwrap();
        assert!(r.profile_allows_tool("default", &gh, "search"));
        assert!(r.profiles[0].tool_scope.is_empty());
    }

    #[test]
    fn removing_a_server_drops_its_tool_scope() {
        let mut r = Registry::default();
        let gh = r.add_server(sample_server("github"));
        r.set_profile_server_tools("default", &gh, Some(vec!["search".into()]))
            .unwrap();
        assert!(!r.profiles[0].tool_scope.is_empty());
        r.remove_server(&gh).unwrap();
        assert!(
            r.profiles[0].tool_scope.is_empty(),
            "tool_scope must not orphan a removed server"
        );
    }

    #[test]
    fn tool_scope_omitted_from_json_when_empty() {
        // Back-compat: a profile with no tool scope serializes without the field.
        let r = Registry::default();
        assert!(!serde_json::to_string(&r).unwrap().contains("toolScope"));
    }

    #[test]
    fn unknown_profile_ref_imposes_no_extra_tool_restriction() {
        // The server scope already fails an unknown profile closed; this must not add a
        // second, confusing block, so it is default-allow for an unresolved profile.
        let r = Registry::default();
        assert!(r.profile_allows_tool("nope", "github", "anything"));
    }

    #[test]
    fn set_folder_profiles_drops_blank_entries() {
        let mut r = Registry::default();
        r.set_folder_profiles(vec![
            FolderProfile {
                path: "/a".into(),
                profile: "P".into(),
            },
            FolderProfile {
                path: "  ".into(),
                profile: "P".into(),
            }, // blank path
            FolderProfile {
                path: "/b".into(),
                profile: " ".into(),
            }, // blank profile
        ]);
        assert_eq!(r.folder_profiles.len(), 1);
        assert_eq!(r.folder_profiles[0].path, "/a");
    }

    #[test]
    fn profile_for_root_normalizes_separators_and_trailing_slash() {
        let mut r = Registry::default();
        r.folder_profiles = vec![FolderProfile {
            path: "/home/me/work/".into(),
            profile: "Work".into(),
        }];
        // A trailing slash on the mapping and backslash separators in the root both normalize.
        assert_eq!(r.profile_for_root("/home/me/work"), Some("Work".into()));
        assert_eq!(r.profile_for_root(r"\home\me\work\sub"), Some("Work".into()));
    }

    #[test]
    fn folder_profiles_omitted_from_json_when_empty() {
        // Back-compat: a registry with no folder routing serializes without the field, so
        // existing registry.json files round-trip unchanged.
        let r = Registry::default();
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("folderProfiles"));
    }

    #[test]
    fn unknown_profile_fails_closed_but_empty_ref_follows_active() {
        let mut r = Registry::default();
        let db = r.add_server(sample_server("postgres"));
        r.set_server_enabled("default", &db, true).unwrap();

        // A deleted/renamed profile a scoped client still references resolves to
        // nothing (fail closed), so the client can't inherit the active profile.
        assert!(r.enabled_servers_for("deleted-profile").is_empty());
        assert_eq!(r.resolve_profile_id("deleted-profile"), "deleted-profile");

        // An empty/whitespace ref is the *unscoped* case and still follows active.
        assert_eq!(r.resolve_profile_id(""), r.active_profile_id());
        assert_eq!(r.resolve_profile_id("   "), r.active_profile_id());
        assert_eq!(r.enabled_servers_for("").len(), 1);
    }

    #[test]
    fn client_scope_records_and_clears() {
        let mut r = Registry::default();
        r.set_client_scope("cursor", Some("Billing"));
        assert_eq!(r.client_scopes.get("cursor").map(String::as_str), Some("Billing"));
        // Whitespace-only / empty / None all clear the binding.
        r.set_client_scope("cursor", Some("  "));
        assert!(!r.client_scopes.contains_key("cursor"));
        r.set_client_scope("claude", Some("Work"));
        r.set_client_scope("claude", None);
        assert!(!r.client_scopes.contains_key("claude"));
    }

    #[test]
    fn client_managed_entries_set_and_clear() {
        let mut r = Registry::default();
        assert!(r.client_managed_entry("claude-desktop").is_none());
        let entry = ManagedEntry {
            command: "/opt/toolport/toolport-gateway".into(),
            args: vec![],
            env: [("TOOLPORT_CLIENT_ID".into(), "claude-desktop".into())]
                .into_iter()
                .collect(),
            transport: "stdio".into(),
            url: None,
            updated_at: 42,
        };
        r.set_client_managed_entry("claude-desktop", entry.clone());
        assert_eq!(r.client_managed_entry("claude-desktop"), Some(&entry));
        r.clear_client_managed_entry("claude-desktop");
        assert!(r.client_managed_entry("claude-desktop").is_none());
    }

    #[test]
    fn client_discovery_records_normalizes_and_clears() {
        let mut r = Registry::default();
        assert_eq!(r.client_discovery_mode("cursor"), None);
        // Recorded case-insensitively / trimmed to a canonical lowercase mode.
        r.set_client_discovery("cursor", Some("  LAZY "));
        assert_eq!(r.client_discovery_mode("cursor"), Some("lazy"));
        r.set_client_discovery("cursor", Some("Grouped"));
        assert_eq!(r.client_discovery_mode("cursor"), Some("grouped"));
        // Unknown / empty / None all clear the override (client inherits the global mode).
        r.set_client_discovery("cursor", Some("nonsense"));
        assert_eq!(r.client_discovery_mode("cursor"), None);
        r.set_client_discovery("claude", Some("full"));
        r.set_client_discovery("claude", None);
        assert_eq!(r.client_discovery_mode("claude"), None);
        // Empty map is omitted from serialization (skip_serializing_if).
        assert!(r.client_discovery.is_empty());
    }

    #[test]
    fn explicit_unscoped_is_distinct_from_no_entry() {
        let mut r = Registry::default();
        // Explicit-unscoped is recorded as an empty-string entry, NOT a removal, so
        // the gateway can tell "follow the active profile now" (present, empty)
        // apart from "no recorded scope, fall back to boot env" (absent).
        r.set_client_unscoped("cursor");
        assert_eq!(r.client_scopes.get("cursor").map(String::as_str), Some(""));
        assert!(r.client_scopes.contains_key("cursor"));
        // Re-scoping to a named profile replaces the marker; uninstall clears it.
        r.set_client_scope("cursor", Some("Billing"));
        assert_eq!(r.client_scopes.get("cursor").map(String::as_str), Some("Billing"));
        r.set_client_scope("cursor", None);
        assert!(!r.client_scopes.contains_key("cursor"));
    }

    #[test]
    fn http_client_lookup_by_token_hash() {
        let mut r = Registry::default();
        let token = "tok_abc123";
        r.http_clients.push(HttpClient {
            id: "c1".into(),
            label: "Open WebUI".into(),
            token_sha256: sha256_hex(token),
            profile: "Billing".into(),
        });
        // The plaintext token resolves to its client; a wrong token doesn't.
        assert_eq!(r.http_client_for_token(token).map(|c| c.profile.as_str()), Some("Billing"));
        assert!(r.http_client_for_token("tok_wrong").is_none());
        // The hash is deterministic and not the plaintext.
        assert_eq!(sha256_hex(token), sha256_hex(token));
        assert_ne!(sha256_hex(token), token);
    }

    #[test]
    fn bridge_union_connects_every_clients_servers() {
        let mut r = Registry::default();
        let a = r.add_server(sample_server("alpha"));
        let b = r.add_server(sample_server("bravo"));
        let c = r.add_server(sample_server("charlie"));
        let billing = r.add_profile("Billing");
        let support = r.add_profile("Support");
        // default (active) enables alpha; Billing -> bravo; Support -> charlie.
        r.set_server_enabled("default", &a, true).unwrap();
        r.set_server_enabled(&billing, &b, true).unwrap();
        r.set_server_enabled(&support, &c, true).unwrap();
        // Base alone (no clients) connects only the active profile's server.
        assert_eq!(
            r.bridge_enabled_servers(None).iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
            vec![a.clone()]
        );
        // Two clients scoped to Billing and Support -> the bridge connects the union.
        r.http_clients.push(HttpClient {
            id: "1".into(), label: "x".into(), token_sha256: "h1".into(), profile: "Billing".into(),
        });
        r.http_clients.push(HttpClient {
            id: "2".into(), label: "y".into(), token_sha256: "h2".into(), profile: "Support".into(),
        });
        let ids: Vec<_> = r.bridge_enabled_servers(None).iter().map(|s| s.id.clone()).collect();
        assert!(ids.contains(&a) && ids.contains(&b) && ids.contains(&c));
        assert_eq!(ids.len(), 3);
        // An unscoped (empty-profile) client adds nothing beyond the union.
        r.http_clients.push(HttpClient {
            id: "3".into(), label: "z".into(), token_sha256: "h3".into(), profile: String::new(),
        });
        assert_eq!(r.bridge_enabled_servers(None).len(), 3);
    }

    #[test]
    fn tool_disable_is_default_allow_and_idempotent() {
        let mut r = Registry::default();
        let id = r.add_server(sample_server("github"));
        // Unknown tools are enabled by default.
        assert!(r.is_tool_enabled(&id, "create_issue"));
        // Disable, then confirm; double-disable doesn't duplicate.
        r.set_tool_enabled(&id, "create_issue", false).unwrap();
        r.set_tool_enabled(&id, "create_issue", false).unwrap();
        assert!(!r.is_tool_enabled(&id, "create_issue"));
        let server = r.servers.iter().find(|s| s.id == id).unwrap();
        assert_eq!(server.disabled_tools, vec!["create_issue".to_string()]);
        // Re-enable removes it.
        r.set_tool_enabled(&id, "create_issue", true).unwrap();
        assert!(r.is_tool_enabled(&id, "create_issue"));
        assert!(r.servers.iter().find(|s| s.id == id).unwrap().disabled_tools.is_empty());
    }

    #[test]
    fn tool_pin_is_idempotent_and_prunes_empty() {
        let mut r = Registry::default();
        let id = r.add_server(sample_server("github"));
        // Default: nothing pinned.
        assert!(!r.is_tool_pinned(&id, "create_issue"));
        // Pin, then double-pin doesn't duplicate.
        r.set_tool_pinned(&id, "create_issue", true);
        r.set_tool_pinned(&id, "create_issue", true);
        assert!(r.is_tool_pinned(&id, "create_issue"));
        assert_eq!(r.pinned_tools.get(&id).map(Vec::len), Some(1));
        // A second pin adds to the same server's list.
        r.set_tool_pinned(&id, "list_issues", true);
        assert_eq!(r.pinned_tools.get(&id).map(Vec::len), Some(2));
        // Unpinning the last one prunes the server entry entirely.
        r.set_tool_pinned(&id, "create_issue", false);
        r.set_tool_pinned(&id, "list_issues", false);
        assert!(!r.is_tool_pinned(&id, "create_issue"));
        assert!(r.pinned_tools.get(&id).is_none());
    }

    #[test]
    fn deny_destructive_round_trips_through_disk() {
        let mut r = Registry::default();
        let id = r.add_server(sample_server("postgres"));
        r.set_tool_enabled(&id, "drop_table", false).unwrap();
        r.set_deny_destructive(true);

        let mut path = std::env::temp_dir();
        path.push(format!("conduit-policy-test-{}.json", std::process::id()));
        save_to(&path, &r).unwrap();
        let loaded = load_from(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert!(loaded.deny_destructive);
        assert!(!loaded.is_tool_enabled(&id, "drop_table"));
    }

    #[test]
    fn removing_server_cleans_profiles() {
        let mut r = Registry::default();
        let id = r.add_server(sample_server("linear"));
        r.set_server_enabled("default", &id, true).unwrap();
        r.remove_server(&id).unwrap();
        assert!(r.servers.is_empty());
        assert!(r.profiles[0].enabled_server_ids.is_empty());
    }

    #[test]
    fn cannot_remove_last_profile() {
        let mut r = Registry::default();
        assert!(r.remove_profile("default").is_err());
    }

    #[test]
    fn discovery_mode_setter_and_backcompat() {
        let mut r = Registry::default();
        // Absent by default (and every pre-existing registry).
        assert_eq!(r.discovery_mode, None);
        assert!(r.lazy_discovery);

        r.set_discovery_mode("grouped");
        assert_eq!(r.discovery_mode.as_deref(), Some("grouped"));
        // grouped doesn't touch the bool; lazy/full keep it in sync for old gateways.
        r.set_discovery_mode("full");
        assert_eq!(r.discovery_mode.as_deref(), Some("full"));
        assert!(!r.lazy_discovery);
        r.set_discovery_mode("lazy");
        assert_eq!(r.discovery_mode.as_deref(), Some("lazy"));
        assert!(r.lazy_discovery);
        // An unknown value clears the override (falls back to lazy_discovery).
        r.set_discovery_mode("nonsense");
        assert_eq!(r.discovery_mode, None);

        // Serde: None is skipped, so a default registry serializes exactly as before
        // (no new key), and that JSON - which lacks the field, like every old registry -
        // round-trips back to None.
        let json = serde_json::to_string(&Registry::default()).unwrap();
        assert!(!json.contains("discovery_mode"), "None must not be serialized");
        let back: Registry = serde_json::from_str(&json).unwrap();
        assert_eq!(back.discovery_mode, None);
    }

    #[test]
    fn round_trips_through_disk() {
        let mut r = Registry::default();
        let id = r.add_server(sample_server("vercel"));
        r.set_server_enabled("default", &id, true).unwrap();
        r.add_profile("Work");

        let mut path = std::env::temp_dir();
        path.push(format!("conduit-test-{}.json", std::process::id()));
        save_to(&path, &r).unwrap();
        let loaded = load_from(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(loaded.servers, r.servers);
        assert_eq!(loaded.profiles, r.profiles);
        assert_eq!(loaded.active_profile_id, r.active_profile_id);
    }

    #[test]
    fn load_and_save_resolved_honor_registry_override() {
        let _guard = REGISTRY_ENV_LOCK.lock().unwrap();

        let mut path = std::env::temp_dir();
        path.push(format!("conduit-registry-override-{}.json", std::process::id()));
        let previous = std::env::var_os("CONDUIT_REGISTRY");
        struct RestoreEnv(Option<std::ffi::OsString>);
        impl Drop for RestoreEnv {
            fn drop(&mut self) {
                match &self.0 {
                    Some(value) => std::env::set_var("CONDUIT_REGISTRY", value),
                    None => std::env::remove_var("CONDUIT_REGISTRY"),
                }
            }
        }
        let _restore = RestoreEnv(previous);
        std::env::set_var("CONDUIT_REGISTRY", &path);

        let mut r = Registry::default();
        let id = r.add_server(sample_server("oauth"));
        r.set_server_enabled("default", &id, true).unwrap();
        save(&r).unwrap();

        let loaded = load().unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(loaded.servers, r.servers);
        assert_eq!(loaded.profiles, r.profiles);
        assert_eq!(loaded.active_profile_id, r.active_profile_id);
    }

    #[test]
    fn update_saves_to_the_same_resolved_path_it_locked() {
        let _guard = REGISTRY_ENV_LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!(
            "toolport-update-path-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let locked_path = dir.join("locked.json");
        let redirected_path = dir.join("redirected.json");

        let previous = std::env::var_os("TOOLPORT_REGISTRY");
        struct RestoreEnv(Option<std::ffi::OsString>);
        impl Drop for RestoreEnv {
            fn drop(&mut self) {
                match &self.0 {
                    Some(value) => std::env::set_var("TOOLPORT_REGISTRY", value),
                    None => std::env::remove_var("TOOLPORT_REGISTRY"),
                }
            }
        }
        let _restore = RestoreEnv(previous);
        std::env::set_var("TOOLPORT_REGISTRY", &locked_path);

        update(|registry| {
            registry.deny_destructive = true;
            std::env::set_var("TOOLPORT_REGISTRY", &redirected_path);
            Ok(())
        })
        .unwrap();

        let persisted = load_from(&locked_path).unwrap();
        assert!(persisted.deny_destructive);
        assert!(
            !redirected_path.exists(),
            "update wrote to a path whose lock it never held"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn missing_file_yields_default() {
        let path = std::env::temp_dir().join("conduit-does-not-exist-xyz.json");
        let r = load_from(&path).unwrap();
        assert_eq!(r.profiles.len(), 1);
    }

    /// The MSIX escape hatch: a drive-rooted path maps to its `\\localhost\<D>$`
    /// admin-share twin; anything without a drive root is refused so callers
    /// stay on the natural path.
    #[cfg(windows)]
    #[test]
    fn unc_twin_maps_drive_paths_and_rejects_others() {
        assert_eq!(
            super::msix::unc_twin(Path::new(r"C:\Users\alice")).unwrap(),
            Path::new(r"\\localhost\C$\Users\alice")
        );
        // Lowercase drive letters normalize; forward slashes count as a root.
        assert_eq!(
            super::msix::unc_twin(Path::new("d:/stuff")).unwrap(),
            Path::new(r"\\localhost\D$\stuff")
        );
        assert!(super::msix::unc_twin(Path::new(r"\\server\share\home")).is_none());
        assert!(super::msix::unc_twin(Path::new(r"relative\path")).is_none());
        assert!(super::msix::unc_twin(Path::new("C:")).is_none());
    }

    /// `cargo test` never runs with package identity, so resolution must be
    /// Direct and the dir the natural home-derived path (not a UNC one).
    #[cfg(windows)]
    #[test]
    fn conduit_dir_is_direct_outside_a_container() {
        let _data_dir = data_dir_test_lock();
        assert_eq!(conduit_dir_resolution(), DirResolution::Direct);
        let dir = conduit_dir().expect("home dir resolves");
        let s = dir.to_string_lossy();
        // Prefer Toolport; existing installs may still resolve under the legacy leaf
        // until desktop launch migrates it.
        assert!(
            s.ends_with(&format!(
                "AppData\\Roaming\\{}",
                crate::brand::data_dir_leaf_name()
            ))
                || s.ends_with(&format!(
                    "AppData\\Roaming\\{}",
                    crate::brand::legacy_data_dir_leaf_name()
                )),
            "unexpected data dir: {s}"
        );
        assert!(!s.starts_with(r"\\"));
    }

    #[test]
    fn atomic_write_replaces_and_leaves_no_temp() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("conduit-aw-{}.json", std::process::id()));
        atomic_write(&path, "first").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first");
        // Overwrite replaces the contents in place.
        atomic_write(&path, "second").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
        // A successful write leaves no .conduit-tmp sibling behind.
        let prefix = format!("conduit-aw-{}.json.", std::process::id());
        let leftover = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().starts_with(&prefix));
        assert!(!leftover, "temp file left behind after a successful write");
        std::fs::remove_file(&path).ok();
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum FailingAtomicWriteStep {
        Permissions,
        Write,
        Sync,
    }

    struct FailingAtomicWriteOps(FailingAtomicWriteStep);

    impl AtomicWriteOps for FailingAtomicWriteOps {
        fn set_owner_only(&self, _file: &std::fs::File) -> std::io::Result<()> {
            if self.0 == FailingAtomicWriteStep::Permissions {
                return Err(std::io::Error::other("injected permissions failure"));
            }
            Ok(())
        }

        fn write_all(&self, file: &mut std::fs::File, contents: &[u8]) -> std::io::Result<()> {
            if self.0 == FailingAtomicWriteStep::Write {
                let partial_len = contents.len().min(3);
                std::io::Write::write_all(file, &contents[..partial_len])?;
                return Err(std::io::Error::other("injected write failure"));
            }
            std::io::Write::write_all(file, contents)
        }

        fn sync_all(&self, file: &std::fs::File) -> std::io::Result<()> {
            if self.0 == FailingAtomicWriteStep::Sync {
                return Err(std::io::Error::other("injected sync failure"));
            }
            file.sync_all()
        }
    }

    fn atomic_temp_files(path: &Path) -> Vec<PathBuf> {
        let prefix = format!("{}.", path.file_name().unwrap().to_string_lossy());
        std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|candidate| {
                candidate
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(".conduit-tmp"))
            })
            .collect()
    }

    #[test]
    fn atomic_write_cleans_temp_after_each_post_create_failure() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "toolport-atomic-failures-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        for (label, step) in [
            ("permissions", FailingAtomicWriteStep::Permissions),
            ("write", FailingAtomicWriteStep::Write),
            ("sync", FailingAtomicWriteStep::Sync),
        ] {
            let path = dir.join(format!("{label}.json"));
            std::fs::write(&path, "original").unwrap();
            let error = atomic_write_with_ops(&path, "replacement", &FailingAtomicWriteOps(step))
                .expect_err("injected operation must fail");

            assert!(error.contains(label), "unexpected {label} error: {error}");
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                "original",
                "failed {label} stage replaced the destination"
            );
            assert!(
                atomic_temp_files(&path).is_empty(),
                "failed {label} stage left a temp file behind"
            );
        }

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn file_lock_excludes_a_second_holder_and_releases_on_drop() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("conduit-lock-{}.json", std::process::id()));
        std::fs::remove_file(lock_path(&path)).ok();
        let guard = lock_at(&path).expect("first lock acquires");
        // A second, independent handle on the same sibling .lock must NOT acquire while held -
        // this is what serializes two Toolport processes' read-modify-write (SOU-23 / SOU-165).
        let second = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .open(lock_path(&path))
            .unwrap();
        assert!(
            second.try_lock_exclusive().is_err(),
            "a second holder acquired the lock while it was still held"
        );
        drop(guard);
        // Once the guard drops the OS releases the advisory lock, so the same handle can take it.
        assert!(
            second.try_lock_exclusive().is_ok(),
            "lock was not released after the guard dropped"
        );
        let _ = second.unlock();
        std::fs::remove_file(lock_path(&path)).ok();
    }

    #[test]
    fn recovery_waits_for_the_registry_lock_before_rewriting_primary() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "toolport-locked-recovery-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("registry.json");

        let mut stale = Registry::default();
        stale.allow_agent_control = false;
        let mut latest = Registry::default();
        latest.allow_agent_control = true;
        atomic_write(
            &backup_path(&path),
            &serde_json::to_string_pretty(&stale).unwrap(),
        )
        .unwrap();
        atomic_write(&path, "{ corrupt primary").unwrap();

        // Simulate a writer that already owns the registry lock. A concurrent reader must
        // wait rather than restoring the stale backup over the writer's newer primary.
        let guard = lock_at(&path).expect("writer acquires registry lock");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let reader_barrier = std::sync::Arc::clone(&barrier);
        let reader_path = path.clone();
        let reader = std::thread::spawn(move || {
            reader_barrier.wait();
            load_from(&reader_path)
        });
        barrier.wait();
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(
            !reader.is_finished(),
            "reader must remain blocked while the writer owns the registry lock"
        );
        atomic_write(&path, &serde_json::to_string_pretty(&latest).unwrap()).unwrap();
        drop(guard);

        let loaded = reader.join().unwrap().expect("reader loads newest primary");
        assert!(loaded.allow_agent_control, "reader must not return the stale backup");
        let persisted: Registry =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(
            persisted.allow_agent_control,
            "recovery must not overwrite the newer primary after the writer releases"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn atomic_write_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir();
        let path = dir.join(format!("conduit-perm-{}.json", std::process::id()));
        atomic_write(&path, "secret").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "atomic_write must produce an owner-only file");
        // Re-writing an existing file keeps it owner-only.
        atomic_write(&path, "secret2").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "overwrite must stay owner-only");
        std::fs::remove_file(&path).ok();
    }

    #[cfg(unix)]
    fn atomic_write_symlink_scratch(label: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "toolport-aw-symlink-{}-{}-{stamp}",
            std::process::id(),
            label
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(unix)]
    fn assert_is_symlink_to(link: &Path, target: &Path) {
        let meta = std::fs::symlink_metadata(link).expect("link must still exist");
        assert!(
            meta.file_type().is_symlink(),
            "expected {} to remain a symlink, became {:?}",
            link.display(),
            meta.file_type()
        );
        assert_eq!(
            std::fs::read_link(link).unwrap(),
            target,
            "symlink target must be left unchanged"
        );
    }

    /// SBS-886: rename(2) over a symlink dest replaces the link inode and leaves
    /// the target bytes unchanged. Connect then "succeeds" while the file in the
    /// dotfiles repo still has the old content.
    #[cfg(unix)]
    #[test]
    fn atomic_write_follows_existing_symlink() {
        let dir = atomic_write_symlink_scratch("existing");
        let target = dir.join("dotfiles").join("config.toml");
        let link = dir.join("home").join("config.toml");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::fs::write(&target, "old").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        atomic_write(&link, "new").unwrap();

        assert_is_symlink_to(&link, &target);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
        assert!(
            atomic_temp_files(&target).is_empty(),
            "temp must land next to the resolved dest and be cleaned up"
        );
        assert!(
            atomic_temp_files(&link).is_empty(),
            "must not leave a temp next to the symlink"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    /// SBS-886: a dangling stow/chezmoi link (repo file not created yet) must
    /// still stay a link; the write creates the target instead of replacing the
    /// inode under home.
    #[cfg(unix)]
    #[test]
    fn atomic_write_dangling_symlink_creates_target_and_keeps_link() {
        let dir = atomic_write_symlink_scratch("dangling");
        let target = dir
            .join("dotfiles")
            .join("codex")
            .join("config.toml");
        let link = dir.join("home").join(".codex").join("config.toml");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        // Target parent is missing on purpose: first Connect should create it.
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            std::fs::symlink_metadata(&target).unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );

        atomic_write(&link, "created").unwrap();

        assert_is_symlink_to(&link, &target);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "created");
        std::fs::remove_dir_all(dir).ok();
    }

    /// SBS-886: relative link targets are relative to the link's parent, not cwd.
    #[cfg(unix)]
    #[test]
    fn atomic_write_relative_symlink_target() {
        let dir = atomic_write_symlink_scratch("relative");
        let target = dir.join("dotfiles").join("config.toml");
        let link = dir.join("home").join("config.toml");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::fs::write(&target, "old").unwrap();
        std::os::unix::fs::symlink("../dotfiles/config.toml", &link).unwrap();

        atomic_write(&link, "via-relative").unwrap();

        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            std::fs::read_link(&link).unwrap(),
            Path::new("../dotfiles/config.toml")
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "via-relative");
        std::fs::remove_dir_all(dir).ok();
    }

    /// SBS-886: a chain A -> B -> file must walk to the file, leaving every
    /// link inode in place.
    #[cfg(unix)]
    #[test]
    fn atomic_write_walks_nested_symlinks() {
        let dir = atomic_write_symlink_scratch("nested");
        let file = dir.join("real.toml");
        let mid = dir.join("mid.toml");
        let link = dir.join("home.toml");
        std::fs::write(&file, "old").unwrap();
        std::os::unix::fs::symlink(&file, &mid).unwrap();
        std::os::unix::fs::symlink(&mid, &link).unwrap();

        atomic_write(&link, "nested").unwrap();

        assert_is_symlink_to(&link, &mid);
        assert_is_symlink_to(&mid, &file);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "nested");
        std::fs::remove_dir_all(dir).ok();
    }

    /// SBS-886: a loop is its own state. Do not treat it as "not a symlink"
    /// and replace one of the link inodes.
    #[cfg(unix)]
    #[test]
    fn atomic_write_symlink_loop_errors() {
        let dir = atomic_write_symlink_scratch("loop");
        let a = dir.join("a.toml");
        let b = dir.join("b.toml");
        std::os::unix::fs::symlink(&b, &a).unwrap();
        std::os::unix::fs::symlink(&a, &b).unwrap();

        let err = atomic_write(&a, "loop").expect_err("a symlink loop must fail");
        assert!(
            err.contains("symlink loop") || err.contains("too many symlink hops"),
            "unexpected loop error: {err}"
        );
        assert_is_symlink_to(&a, &b);
        assert_is_symlink_to(&b, &a);
        std::fs::remove_dir_all(dir).ok();
    }

    /// SBS-886: writing file bytes over a symlink-to-directory must error
    /// rather than rename a regular file onto a directory inode.
    #[cfg(unix)]
    #[test]
    fn atomic_write_symlink_to_dir_errors() {
        let dir = atomic_write_symlink_scratch("to-dir");
        let target_dir = dir.join("dotfiles");
        let link = dir.join("config.toml");
        std::fs::create_dir_all(&target_dir).unwrap();
        std::fs::write(target_dir.join("keep"), "safe").unwrap();
        std::os::unix::fs::symlink(&target_dir, &link).unwrap();

        let err = atomic_write(&link, "nope").expect_err("symlink-to-dir must fail");
        assert!(
            err.contains("directory"),
            "unexpected symlink-to-dir error: {err}"
        );
        assert_is_symlink_to(&link, &target_dir);
        assert_eq!(
            std::fs::read_to_string(target_dir.join("keep")).unwrap(),
            "safe"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    /// A missing path is not a symlink: create a regular file, same as before.
    #[cfg(unix)]
    #[test]
    fn atomic_write_missing_path_creates_regular_file() {
        let dir = atomic_write_symlink_scratch("missing");
        let path = dir.join("new").join("config.toml");
        atomic_write(&path, "fresh").unwrap();
        let meta = std::fs::symlink_metadata(&path).unwrap();
        assert!(
            meta.file_type().is_file() && !meta.file_type().is_symlink(),
            "a first write to a missing path must create a regular file"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "fresh");
        std::fs::remove_dir_all(dir).ok();
    }

    /// Regular-file dest is unchanged: still overwrite in place, never invent
    /// a symlink, never write beside a different path.
    #[cfg(unix)]
    #[test]
    fn atomic_write_regular_file_destination_unchanged() {
        let dir = atomic_write_symlink_scratch("regular");
        let path = dir.join("config.toml");
        std::fs::write(&path, "old").unwrap();
        atomic_write(&path, "new").unwrap();
        let meta = std::fs::symlink_metadata(&path).unwrap();
        assert!(
            meta.file_type().is_file() && !meta.file_type().is_symlink(),
            "a regular file dest must stay a regular file"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        std::fs::remove_dir_all(dir).ok();
    }

    /// SBS-886: `symlink_metadata` failing for a reason other than NotFound is
    /// unknown, not "not a symlink". Collapsing those would let the write
    /// proceed against a path we could not inspect.
    #[cfg(unix)]
    #[test]
    fn atomic_write_inspect_error_is_not_treated_as_missing() {
        let dir = atomic_write_symlink_scratch("inspect");
        let not_a_dir = dir.join("file");
        std::fs::write(&not_a_dir, "regular").unwrap();
        // Parent is a regular file: lstat of child is ENOTDIR, never NotFound.
        let child = not_a_dir.join("config.toml");

        let err = resolve_atomic_write_dest(&child)
            .expect_err("an inspect failure must not collapse to missing");
        assert!(
            err.contains("could not inspect"),
            "inspect failure must be reported as inspect, not as a write: {err}"
        );
        assert_eq!(std::fs::read_to_string(&not_a_dir).unwrap(), "regular");
        std::fs::remove_dir_all(dir).ok();
    }

    /// A failed write through a symlink must not replace the link and must
    /// leave the target bytes and no sibling temp.
    #[cfg(unix)]
    #[test]
    fn atomic_write_failed_write_through_symlink_keeps_link_and_target() {
        let dir = atomic_write_symlink_scratch("fail-keep");
        let target = dir.join("target.toml");
        let link = dir.join("link.toml");
        std::fs::write(&target, "original").unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let err = atomic_write_with_ops(
            &link,
            "replacement",
            &FailingAtomicWriteOps(FailingAtomicWriteStep::Write),
        )
        .expect_err("injected write must fail");
        assert!(err.contains("write"), "unexpected error: {err}");
        assert_is_symlink_to(&link, &target);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "original");
        assert!(atomic_temp_files(&target).is_empty());
        assert!(atomic_temp_files(&link).is_empty());
        std::fs::remove_dir_all(dir).ok();
    }

    fn quarantine_files(path: &Path) -> Vec<PathBuf> {
        let dir = path.parent().unwrap();
        let prefix = format!("{}.unreadable-", path.file_name().unwrap().to_str().unwrap());
        let mut out: Vec<PathBuf> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|f| f.to_str())
                    .is_some_and(|f| f.starts_with(&prefix))
            })
            .collect();
        out.sort();
        out
    }

    fn cleanup_quarantine(path: &Path) {
        for q in quarantine_files(path) {
            std::fs::remove_file(q).ok();
        }
    }

    #[test]
    fn corrupt_primary_is_quarantined_before_selfheal() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("conduit-reg-quar-{}.json", std::process::id()));
        let bak = backup_path(&path);
        cleanup_quarantine(&path);
        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&bak).ok();

        let mut reg = Registry::default();
        reg.add_server(sample_server("alpha"));
        save_to(&path, &reg).unwrap();
        // A second, DIFFERENT save snapshots the prior {alpha} state into .bak before
        // overwriting. (An identical re-save is now a no-op and writes no backup.)
        reg.add_server(sample_server("beta"));
        save_to(&path, &reg).unwrap();

        // A newer build (or corruption) leaves bytes this build can't parse. The
        // self-heal from .bak must PRESERVE those bytes, not destroy them - they
        // may be three days of a newer build's data (this happened for real).
        std::fs::write(&path, r#"{"servers": "future-shape"}"#).unwrap();
        let recovered = load_from(&path).unwrap();
        assert_eq!(recovered.servers.len(), 1, "self-healed from .bak");
        let q = quarantine_files(&path);
        assert_eq!(q.len(), 1, "unreadable primary must be quarantined");
        let kept = std::fs::read_to_string(&q[0]).unwrap();
        assert!(kept.contains("future-shape"), "quarantine holds the exact bytes");

        cleanup_quarantine(&path);
        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&bak).ok();
    }

    #[test]
    fn identical_save_is_a_noop_leaving_the_file_untouched() {
        // The gateway rebuilds (re-spawning every stdio MCP server) on any registry
        // mtime change, and the team sync loop save()s every cycle even when the pull was
        // a 304. A save whose content already matches disk must therefore be a complete
        // no-op: no rewrite, no mtime bump, no backup. Without this, each idle sync cycle
        // triggered every gateway to respawn every server, orphaning npx/node children
        // until the machine ran out of RAM.
        let dir = std::env::temp_dir();
        let path = dir.join(format!("conduit-reg-noop-{}.json", std::process::id()));
        let bak = backup_path(&path);
        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&bak).ok();
        for g in backup_generations(&path) {
            std::fs::remove_file(g).ok();
        }

        let mut reg = Registry::default();
        reg.add_server(sample_server("alpha"));
        save_to(&path, &reg).unwrap();
        let mtime1 = std::fs::metadata(&path).unwrap().modified().unwrap();

        // Re-save the SAME registry (freshly re-serialized, exactly as the sync loop does
        // after a load): a complete no-op.
        std::thread::sleep(std::time::Duration::from_millis(15));
        save_to(&path, &reg).unwrap();
        let mtime2 = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(mtime1, mtime2, "an identical save must not rewrite the file");
        assert!(!bak.exists(), "an identical save must not create a backup");
        assert!(
            backup_generations(&path).is_empty(),
            "an identical save must not add a journal generation"
        );

        // A genuine change still writes (and snapshots the prior state).
        reg.add_server(sample_server("beta"));
        save_to(&path, &reg).unwrap();
        assert!(bak.exists(), "a real change snapshots the prior state to .bak");
        assert_eq!(
            load_from(&path).unwrap().servers.len(),
            2,
            "a real change is persisted"
        );

        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&bak).ok();
        for g in backup_generations(&path) {
            std::fs::remove_file(g).ok();
        }
    }

    #[test]
    fn save_over_unparseable_existing_quarantines_instead_of_clobbering() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("conduit-reg-savequar-{}.json", std::process::id()));
        let bak = backup_path(&path);
        cleanup_quarantine(&path);
        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&bak).ok();

        std::fs::write(&path, r#"{"servers": 42}"#).unwrap();
        save_to(&path, &Registry::default()).unwrap();
        assert!(!bak.exists(), "unparseable existing must never become the .bak");
        let q = quarantine_files(&path);
        assert_eq!(q.len(), 1, "the bytes we overwrote must survive in quarantine");
        assert!(std::fs::read_to_string(&q[0]).unwrap().contains("42"));

        cleanup_quarantine(&path);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn unknown_top_level_fields_survive_a_round_trip() {
        // An OLDER binary loading and re-saving a NEWER build's registry must not
        // strip fields it doesn't understand (mixed versions share this file).
        let dir = std::env::temp_dir();
        let path = dir.join(format!("conduit-reg-fwd-{}.json", std::process::id()));
        std::fs::remove_file(&path).ok();
        std::fs::remove_file(backup_path(&path)).ok();

        let mut json = serde_json::to_value(Registry::default()).unwrap();
        json["someFutureFeature"] = serde_json::json!({ "enabled": true, "level": 3 });
        std::fs::write(&path, serde_json::to_string(&json).unwrap()).unwrap();

        let reg = load_from(&path).unwrap();
        save_to(&path, &reg).unwrap();
        let round: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            round["someFutureFeature"]["level"], 3,
            "unknown fields must round-trip, not be stripped"
        );

        std::fs::remove_file(&path).ok();
        std::fs::remove_file(backup_path(&path)).ok();
    }

    #[test]
    fn unknown_server_fields_survive_a_round_trip() {
        // Same forward-compat contract as the top-level test, at the per-SERVER
        // level: an older binary loading and re-saving a newer build's registry
        // must not strip a `ServerEntry` field it doesn't understand.
        let dir = std::env::temp_dir();
        let path = dir.join(format!("conduit-reg-srv-fwd-{}.json", std::process::id()));
        std::fs::remove_file(&path).ok();
        std::fs::remove_file(backup_path(&path)).ok();

        let mut reg = Registry::default();
        reg.servers.push(ServerEntry {
            id: "s1".into(),
            name: "s1".into(),
            transport: "stdio".into(),
            command: Some("x".into()),
            args: vec![],
            env: vec![],
            url: None,
            source: None,
            disabled_tools: vec![],
            cwd: None,
            client_credentials: None,
            unknown_fields: serde_json::Map::new(),
        });
        // Inject a per-server field this binary's ServerEntry doesn't define.
        let mut json = serde_json::to_value(&reg).unwrap();
        json["servers"][0]["futureServerFlag"] = serde_json::json!({ "enabled": true });
        std::fs::write(&path, serde_json::to_string(&json).unwrap()).unwrap();

        let loaded = load_from(&path).unwrap();
        save_to(&path, &loaded).unwrap();
        let round: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            round["servers"][0]["futureServerFlag"]["enabled"],
            true,
            "unknown per-server fields must round-trip, not be stripped"
        );

        std::fs::remove_file(&path).ok();
        std::fs::remove_file(backup_path(&path)).ok();
    }

    #[test]
    fn quarantine_prunes_to_the_newest_three() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("conduit-reg-prune-{}.json", std::process::id()));
        cleanup_quarantine(&path);
        for i in 0..5 {
            quarantine_unreadable(&path, &format!("junk-{i}"));
            // Distinct millisecond timestamps so each call gets its own file.
            std::thread::sleep(std::time::Duration::from_millis(3));
        }
        let q = quarantine_files(&path);
        assert_eq!(q.len(), 3, "quarantine must stay bounded");
        let newest = std::fs::read_to_string(q.last().unwrap()).unwrap();
        assert_eq!(newest, "junk-4", "pruning removes the oldest, keeps the newest");
        cleanup_quarantine(&path);
    }

    #[test]
    fn save_keeps_backup_and_load_recovers_from_it() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("conduit-reg-bak-{}.json", std::process::id()));
        let bak = backup_path(&path);
        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&bak).ok();

        // First save: one server. No prior file, so nothing to snapshot yet.
        let mut reg = Registry::default();
        reg.add_server(sample_server("alpha"));
        save_to(&path, &reg).unwrap();
        assert!(!bak.exists(), "no backup on the first save");

        // Second save snapshots the one-server registry into .bak before overwriting
        // it with an empty one.
        save_to(&path, &Registry::default()).unwrap();
        assert_eq!(
            load_from(&bak).unwrap().servers.len(),
            1,
            ".bak holds the pre-overwrite registry"
        );

        // A corrupt primary recovers its server list from the backup.
        std::fs::write(&path, "{ not valid json").unwrap();
        assert_eq!(
            load_from(&path).unwrap().servers.len(),
            1,
            "recovered from .bak when the primary is corrupt"
        );

        // A missing primary also recovers from the backup.
        std::fs::remove_file(&path).ok();
        assert_eq!(
            load_from(&path).unwrap().servers.len(),
            1,
            "recovered from .bak when the primary is missing"
        );

        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&bak).ok();
    }

    #[test]
    fn save_journal_prunes_to_the_generation_cap() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("conduit-reg-journal-{}.json", std::process::id()));
        std::fs::remove_file(&path).ok();
        std::fs::remove_file(backup_path(&path)).ok();
        for g in backup_generations(&path) {
            std::fs::remove_file(g).ok();
        }

        // The first save has no prior file to snapshot; every save after that
        // writes one generation. Well past the cap, the journal must stay bounded
        // to the newest BACKUP_GENERATIONS.
        let mut reg = Registry::default();
        for i in 0..(BACKUP_GENERATIONS + 3) {
            reg.add_server(sample_server(&format!("s{i}")));
            save_to(&path, &reg).unwrap();
            // Distinct millisecond timestamps so each generation gets its own file.
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let gens = backup_generations(&path);
        assert_eq!(
            gens.len(),
            BACKUP_GENERATIONS,
            "journal must be pruned to the newest {BACKUP_GENERATIONS} generations"
        );

        std::fs::remove_file(&path).ok();
        std::fs::remove_file(backup_path(&path)).ok();
        for g in backup_generations(&path) {
            std::fs::remove_file(g).ok();
        }
    }

    #[test]
    fn recovery_uses_the_journal_when_bak_is_gone() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("conduit-reg-recover-journal-{}.json", std::process::id()));
        std::fs::remove_file(&path).ok();
        std::fs::remove_file(backup_path(&path)).ok();
        for g in backup_generations(&path) {
            std::fs::remove_file(g).ok();
        }

        // Six saves: the last on-disk state has 6 servers; the immediately-previous
        // state (in .bak and the newest journal generation) has 5.
        let mut reg = Registry::default();
        for i in 0..6 {
            reg.add_server(sample_server(&format!("s{i}")));
            save_to(&path, &reg).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        // Corrupt the primary AND remove the single .bak, so recovery has only the
        // rolling journal to fall back to. This is the acceptance case: recover the
        // immediately-previous state, not a stale one.
        std::fs::write(&path, "{ not json").unwrap();
        std::fs::remove_file(backup_path(&path)).ok();

        let recovered = load_from(&path).unwrap();
        assert_eq!(
            recovered.servers.len(),
            5,
            "recovered the immediately-previous state from the journal"
        );

        std::fs::remove_file(&path).ok();
        std::fs::remove_file(backup_path(&path)).ok();
        for g in backup_generations(&path) {
            std::fs::remove_file(g).ok();
        }
    }

    #[test]
    fn recovery_prefers_a_fresher_journal_over_a_stale_parseable_bak() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "conduit-reg-recover-freshest-{}.json",
            std::process::id()
        ));
        std::fs::remove_file(&path).ok();
        std::fs::remove_file(backup_path(&path)).ok();
        for generation in backup_generations(&path) {
            std::fs::remove_file(generation).ok();
        }

        let mut stale = Registry::default();
        stale.add_server(sample_server("stale"));
        std::fs::write(backup_path(&path), serde_json::to_string(&stale).unwrap()).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));

        let mut fresh = stale.clone();
        fresh.add_server(sample_server("fresh"));
        let mut generation_name = path.as_os_str().to_owned();
        generation_name.push(".bak.9999999999999");
        std::fs::write(
            PathBuf::from(generation_name),
            serde_json::to_string(&fresh).unwrap(),
        )
        .unwrap();
        std::fs::write(&path, "{ corrupt").unwrap();

        let recovered = load_from(&path).unwrap();
        assert_eq!(recovered.servers.len(), 2);
        assert!(recovered.servers.iter().any(|server| server.name == "fresh"));

        std::fs::remove_file(&path).ok();
        std::fs::remove_file(backup_path(&path)).ok();
        for generation in backup_generations(&path) {
            std::fs::remove_file(generation).ok();
        }
    }

    #[test]
    fn recovery_uses_backup_sequence_when_mtimes_tie() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "conduit-reg-recover-sequence-{}.json",
            std::process::id()
        ));
        let bak = backup_path(&path);
        let sequence_path = backup_sequence_path(&path);
        let mut generation_name = path.as_os_str().to_owned();
        generation_name.push(".bak.100");
        let generation = PathBuf::from(generation_name);

        for candidate in [&path, &bak, &sequence_path, &generation] {
            std::fs::remove_file(candidate).ok();
        }

        let mut stale = Registry::default();
        stale.add_server(sample_server("stale"));
        std::fs::write(&generation, serde_json::to_string(&stale).unwrap()).unwrap();

        let mut fresh = stale.clone();
        fresh.add_server(sample_server("fresh"));
        std::fs::write(&bak, serde_json::to_string(&fresh).unwrap()).unwrap();
        std::fs::write(&sequence_path, "200").unwrap();

        let tied_time = std::time::SystemTime::UNIX_EPOCH
            + std::time::Duration::from_secs(1_700_000_000);
        let times = std::fs::FileTimes::new().set_modified(tied_time);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&bak)
            .unwrap()
            .set_times(times)
            .unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&generation)
            .unwrap()
            .set_times(times)
            .unwrap();
        std::fs::write(&path, "{ corrupt").unwrap();

        let recovered = load_from(&path).unwrap();
        assert_eq!(recovered.servers.len(), 2);
        assert!(recovered.servers.iter().any(|server| server.name == "fresh"));

        for candidate in [&path, &bak, &sequence_path, &generation] {
            std::fs::remove_file(candidate).ok();
        }
    }

    #[test]
    fn profile_store_keys_do_not_collide_for_slug_equivalent_names() {
        let work_prod = profile_store_key("work-prod");
        let work_slash = profile_store_key("work/prod");
        let work_space = profile_store_key("Work Prod");
        assert_ne!(work_prod, work_slash);
        assert_ne!(work_prod, work_space);
        assert_ne!(work_slash, work_space);
        assert_eq!(
            legacy_profile_store_slug("Work Prod"),
            legacy_profile_store_slug("Work/Prod")
        );
        assert_eq!(legacy_profile_store_slug("Work Prod"), "work-prod");
    }

    #[test]
    fn canonical_profile_id_rejects_ambiguous_display_names() {
        let mut registry = Registry::default();
        let first = registry.add_profile("Work Prod");
        let second = registry.add_profile("Work/Prod");
        assert_eq!(registry.canonical_profile_id(&first).unwrap(), first);
        assert_eq!(registry.canonical_profile_id("default").unwrap(), "default");
        assert!(registry.canonical_profile_id("Work Prod").is_ok());
        registry.profiles[1].name = "Work Prod".into();
        registry.profiles[2].name = "Work Prod".into();
        assert!(
            registry.canonical_profile_id("Work Prod").is_err(),
            "duplicate case-insensitive names must not resolve"
        );
        assert_eq!(registry.canonical_profile_id(&second).unwrap(), second);
    }

    #[test]
    fn normalize_rewrites_unique_name_scope_and_leaves_ambiguous_dangling() {
        let mut registry = Registry::default();
        let work = registry.add_profile("Work");
        registry
            .client_scopes
            .insert("cursor".into(), "work".into());
        registry
            .client_scopes
            .insert("zed".into(), "Missing".into());
        registry.normalize_profile_references();
        assert_eq!(registry.client_scopes.get("cursor").unwrap(), &work);
        assert!(
            registry
                .client_scopes
                .get("zed")
                .unwrap()
                .starts_with(INVALID_PROFILE_REF_PREFIX),
            "unknown names stay dangling instead of widening to the active profile"
        );
    }

    #[test]
    fn normalize_clears_a_stale_active_profile_for_first_profile_fallback() {
        let mut registry = Registry::default();
        registry.active_profile_id = Some("deleted-profile".into());
        registry.normalize_profile_references();
        assert_eq!(registry.active_profile_id, None);
        assert_eq!(registry.active_profile_id(), DEFAULT_PROFILE_ID);
    }

    #[test]
    fn migrate_copies_unambiguous_legacy_files_and_fails_closed_on_slug_collisions() {
        let _lock = data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!(
            "toolport-sbs-715-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let _override = DataDirOverride::set(&dir);

        let mut registry = Registry::default();
        let work_space = registry.add_profile("Work Prod");
        let work_slash = registry.add_profile("Work/Prod");
        let solo = registry.add_profile("Solo");

        std::fs::write(dir.join("tool-cache-work-prod.json"), r#"{"owner":"collided"}"#).unwrap();
        std::fs::write(dir.join("tool-pins-work-prod.json"), r#"{"shared":{"fp":"x"}}"#).unwrap();
        std::fs::write(
            dir.join("quarantine-work-prod.json"),
            r#"{"alpha__tool":{"tool":"alpha__tool","reason":"a"},"beta__tool":{"tool":"beta__tool","reason":"b"}}"#,
        )
        .unwrap();
        std::fs::write(dir.join("tool-cache-solo.json"), r#"{"owner":"solo"}"#).unwrap();
        std::fs::write(dir.join("tool-pins-solo.json"), r#"{"solo":{"fp":"y"}}"#).unwrap();

        migrate_profile_stores(&registry).unwrap();

        let space_cache = dir.join(format!(
            "tool-cache-v2-{}.json",
            profile_store_key(&work_space)
        ));
        let slash_cache = dir.join(format!(
            "tool-cache-v2-{}.json",
            profile_store_key(&work_slash)
        ));
        let space_pins = dir.join(format!(
            "tool-pins-v2-{}.json",
            profile_store_key(&work_space)
        ));
        let slash_pins = dir.join(format!(
            "tool-pins-v2-{}.json",
            profile_store_key(&work_slash)
        ));
        let space_q = dir.join(format!(
            "quarantine-v2-{}.json",
            profile_store_key(&work_space)
        ));
        let slash_q = dir.join(format!(
            "quarantine-v2-{}.json",
            profile_store_key(&work_slash)
        ));
        let solo_cache = dir.join(format!("tool-cache-v2-{}.json", profile_store_key(&solo)));
        let solo_pins = dir.join(format!("tool-pins-v2-{}.json", profile_store_key(&solo)));

        assert!(!space_cache.exists(), "collided cache must not be copied");
        assert!(!slash_cache.exists(), "collided cache must not be copied");
        assert_eq!(
            std::fs::read_to_string(&space_pins).unwrap(),
            "ambiguous legacy profile pin store; re-approval required"
        );
        assert_eq!(
            std::fs::read_to_string(&slash_pins).unwrap(),
            "ambiguous legacy profile pin store; re-approval required"
        );
        let space_quarantine: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&space_q).unwrap()).unwrap();
        let slash_quarantine: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&slash_q).unwrap()).unwrap();
        assert!(space_quarantine.get("alpha__tool").is_some());
        assert!(space_quarantine.get("beta__tool").is_some());
        assert_eq!(space_quarantine, slash_quarantine);
        assert_eq!(
            std::fs::read_to_string(&solo_cache).unwrap(),
            r#"{"owner":"solo"}"#
        );
        assert_eq!(
            std::fs::read_to_string(&solo_pins).unwrap(),
            r#"{"solo":{"fp":"y"}}"#
        );
        // Legacy files stay as recovery evidence.
        assert!(dir.join("tool-cache-work-prod.json").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unmigrated_legacy_pin_store_is_detected() {
        let _lock = data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!(
            "toolport-sbs-715-unmigrated-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let _override = DataDirOverride::set(&dir);
        let mut registry = Registry::default();
        let billing = registry.add_profile("Customer Billing");
        save_to(&dir.join("registry.json"), &registry).unwrap();
        std::fs::write(
            dir.join("tool-pins-customer-billing.json"),
            r#"{"x":{}}"#,
        )
        .unwrap();
        assert!(unmigrated_legacy_profile_store(&billing, true));
        assert!(!unmigrated_legacy_profile_store(&billing, false));
        let v2 = dir.join(format!(
            "tool-pins-v2-{}.json",
            profile_store_key(&billing)
        ));
        std::fs::write(&v2, "{}").unwrap();
        assert!(!unmigrated_legacy_profile_store(&billing, true));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn data_dir_leaf_is_dev_in_debug_builds() {
        assert_eq!(
            crate::brand::data_dir_leaf_name(),
            if cfg!(debug_assertions) {
                "Toolport-dev"
            } else {
                "Toolport"
            }
        );
    }
}
