//! Collapse launcher shim chains to a direct `node <script>` spawn.
//!
//! A configured server like `npx -y toolport-mcp-servers vercel` costs four
//! processes on Windows, only the last of which does any work:
//!
//! ```text
//! cmd.exe /c "npx.CMD -y toolport-mcp-servers vercel"   <- Rust runs the .CMD via cmd
//!   node npx-cli.js -y toolport-mcp-servers vercel      <- npx wrapper, stays resident
//!     cmd.exe /d /s /c toolport-mcp-servers vercel      <- the package's .CMD bin shim
//!       node .../toolport-mcp-servers/bin/cli.js vercel <- the actual server
//! ```
//!
//! The three shims are resident only to hold pipes open. Measured on a Windows 11
//! box: 9 gateways x 47 descendants = ~423 processes for ~72 servers. Resolving the
//! package's entry script up front and spawning `node <entry>` directly collapses
//! each chain to one process.
//!
//! Two resolutions, tried in order:
//!
//! 1. [`resolve_direct`] on an `npx`/`npm exec` invocation finds the package that a
//!    previous `npx` run already unpacked under the npm cache's `_npx` tree and
//!    spawns its bin entry directly.
//! 2. Failing that, a command that resolved to an npm-generated `.cmd`/`.bat` shim
//!    is read for the `.js` path it would have run, and that is spawned instead.
//!
//! # What this deliberately gives up
//!
//! `npx` also *installs*. Skipping it means a server stays pinned to whatever version
//! is already in the `_npx` cache: a bare `npx -y pkg` normally means `pkg@latest` and
//! can pick up a new release, and this does not. That is the point (a gateway that
//! re-resolves the registry on every spawn is slow and non-deterministic), but it is a
//! behavior change, so `TOOLPORT_NO_DIRECT_SPAWN=1` turns the whole module off and
//! anything not confidently resolvable falls back to the original command untouched.
//!
//! An explicitly requested version (`pkg@1.2.3`) only resolves when the cached copy is
//! exactly that version; ranges and dist-tags always fall back, because deciding what
//! they mean is `npx`'s job and it needs the registry to do it.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// A launcher invocation rewritten to run its real entry point directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectSpawn {
    /// Absolute path to the `node` executable.
    pub command: String,
    /// The entry script followed by the arguments the launcher would have forwarded.
    pub args: Vec<String>,
    /// The `node_modules/.bin` the launcher would have put on PATH, if any. Servers
    /// that shell out to a sibling bin (a common pattern) break without it.
    pub bin_dir: Option<PathBuf>,
}

/// Off switch for the whole module. Set `TOOLPORT_NO_DIRECT_SPAWN=1` to restore the
/// original `npx`/shim chain, e.g. to let `npx` pick up a newer package version.
pub fn direct_spawn_disabled() -> bool {
    crate::brand::env_flag("TOOLPORT_NO_DIRECT_SPAWN", "CONDUIT_NO_DIRECT_SPAWN")
}

/// Rewrite `command`/`args` to a direct `node` invocation, or `None` to spawn as-is.
///
/// Expects an already-[`normalize_invocation`](crate::downstream::normalize_invocation)d
/// pair. Results are memoized for the life of the process: resolution touches the
/// filesystem, and a gateway respawns the same handful of servers repeatedly.
pub fn resolve_direct(command: &str, args: &[String]) -> Option<DirectSpawn> {
    if direct_spawn_disabled() {
        return None;
    }
    /// Invocation -> resolution, including the negative result: a server that cannot
    /// be rewritten must not re-walk the npx cache on every respawn.
    type Cache = Mutex<HashMap<(String, Vec<String>), Option<DirectSpawn>>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = (command.to_string(), args.to_vec());
    if let Some(hit) = cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&key)
    {
        return hit.clone();
    }
    let resolved = resolve_direct_uncached(command, args);
    cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(key, resolved.clone());
    resolved
}

fn resolve_direct_uncached(command: &str, args: &[String]) -> Option<DirectSpawn> {
    let node = node_executable()?;
    resolve_in(
        command,
        args,
        &npx_package_roots(),
        &node,
        &crate::downstream::resolve_command,
    )
}

/// How the launcher finds a bare command name on this machine. Production passes
/// [`crate::downstream::resolve_command`]; tests pass a fixture-scoped lookup.
///
/// This is the THIRD piece of machine state, and the one that was missed: an
/// `npx_roots` fixture bounds the cache walk, but the shim fallback still asked
/// the host PATH where `npx` lives. On a developer machine that answers, so four
/// tests asserting "this spec must fall back to npx" resolved against whatever
/// npm the developer had installed and failed on a clean checkout of main while
/// CI stayed green (SBS-839).
type CommandLookup<'a> = &'a dyn Fn(&str) -> String;

/// The body of [`resolve_direct`], with the three pieces of machine state - where
/// npm unpacks `_npx` packages, where `node` lives, and how a bare command name is
/// found on PATH - passed in.
///
/// Split out so tests drive the real resolution against a fixture tree instead of the
/// developer's actual npm cache, and without mutating process-wide environment.
fn resolve_in(
    command: &str,
    args: &[String],
    npx_roots: &[PathBuf],
    node: &Path,
    lookup: CommandLookup,
) -> Option<DirectSpawn> {
    if let Some(plan) = parse_launcher(command, args) {
        if let Some(direct) = resolve_npx_plan(&plan, npx_roots, node) {
            return Some(direct);
        }
    }
    resolve_shim(command, args, node, lookup)
}

// ---------------------------------------------------------------------------
// Launcher argument parsing
// ---------------------------------------------------------------------------

/// What an `npx`-style invocation resolves to, before the filesystem is consulted.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LauncherPlan {
    /// Package name with any version stripped (`@scope/name` or `name`).
    package: String,
    /// Version explicitly requested in the spec, if any.
    requested_version: Option<String>,
    /// Bin name, when `-p/--package` named the package separately.
    bin: Option<String>,
    /// Arguments to pass through to the resolved script.
    forwarded: Vec<String>,
}

/// Strip a Windows extension and any directory from an executable.
fn command_base(command: &str) -> String {
    let base = command
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(command)
        .to_ascii_lowercase();
    for ext in [".exe", ".cmd", ".bat", ".ps1"] {
        if let Some(stripped) = base.strip_suffix(ext) {
            return stripped.to_string();
        }
    }
    base
}

/// Parse `npx [flags] <spec> [args]` (and the `npm exec` / `npm x` spellings).
///
/// Returns `None` for anything whose meaning depends on `npx` itself - `-c` runs a
/// shell string, `--node-arg` changes how node is invoked, a git/URL/path spec is not
/// a cached package, and an unrecognized flag might do any of those. Falling back is
/// always correct; only the confident cases are rewritten.
fn parse_launcher(command: &str, args: &[String]) -> Option<LauncherPlan> {
    let base = command_base(command);
    let args = match base.as_str() {
        "npx" => args,
        // `npm exec -- <pkg>` / `npm x <pkg>`.
        "npm" => match args.first().map(String::as_str) {
            Some("exec") | Some("x") => &args[1..],
            _ => return None,
        },
        _ => return None,
    };

    let mut i = 0;
    let mut package_flag: Option<String> = None;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--" {
            i += 1;
            break;
        }
        if !a.starts_with('-') {
            break;
        }
        match a {
            // Flags that do not change what gets run, or that ask for exactly the
            // cache-only behavior this rewrite provides.
            //
            // `--prefer-online` and `--ignore-existing` are deliberately NOT here:
            // they ask npx to revalidate against the registry or to skip a cached
            // copy, which is the opposite of resolving out of `_npx`. They fall
            // through to the catch-all below and run through npx unchanged.
            "-y" | "--yes" | "-q" | "--quiet" | "--silent" | "--no-install"
            | "--prefer-offline" | "--offline" => {
                i += 1;
            }
            "-p" | "--package" => {
                if package_flag.is_some() {
                    return None; // multi-package env; let npx build it
                }
                package_flag = Some(args.get(i + 1)?.clone());
                i += 2;
            }
            _ if a.starts_with("--package=") => {
                if package_flag.is_some() {
                    return None;
                }
                // strip_prefix, not trim_start_matches: the latter strips every
                // repeat, so `--package=--package=pkg` would parse as `pkg`.
                package_flag = Some(a.strip_prefix("--package=")?.to_string());
                i += 1;
            }
            // -c/--call, --node-arg/-n, --shell, --loglevel, anything unknown.
            _ => return None,
        }
    }

    let rest = args.get(i..)?;
    let (spec, bin, forwarded) = match &package_flag {
        // `-p <spec> <bin> [args]`: the positional is the bin name, not the package.
        Some(spec) => (
            spec.clone(),
            Some(rest.first()?.clone()),
            rest.get(1..)?.to_vec(),
        ),
        None => (rest.first()?.clone(), None, rest.get(1..)?.to_vec()),
    };
    let (package, requested_version) = split_spec(&spec)?;
    Some(LauncherPlan {
        package,
        requested_version,
        bin,
        forwarded,
    })
}

/// Split `name@version` / `@scope/name@version` into its parts.
///
/// Rejects anything that is not a plain registry package: a path, URL, git or
/// `github:` shorthand, or an alias. Those never sit in `_npx/<hash>/node_modules`
/// under a predictable name.
fn split_spec(spec: &str) -> Option<(String, Option<String>)> {
    if spec.is_empty()
        || spec.contains("://")
        || spec.contains(':')
        || spec.contains('#')
        || spec.starts_with('.')
        || spec.starts_with('/')
        || spec.starts_with('\\')
    {
        return None;
    }
    let (name, version) = if let Some(rest) = spec.strip_prefix('@') {
        match rest.find('@') {
            Some(idx) => (
                format!("@{}", &rest[..idx]),
                Some(rest[idx + 1..].to_string()),
            ),
            None => (spec.to_string(), None),
        }
    } else {
        match spec.find('@') {
            Some(idx) => (spec[..idx].to_string(), Some(spec[idx + 1..].to_string())),
            None => (spec.to_string(), None),
        }
    };
    if !is_safe_package_name(&name) {
        return None;
    }
    Some((name, version.filter(|v| !v.is_empty())))
}

/// A package name that is safe to join onto a cache path.
///
/// This is the security-relevant check in the module: the name comes from server
/// config and is turned into a filesystem path, so anything that could escape the
/// cache root (`..`, separators beyond one scope slash, absolute markers) or that npm
/// itself would reject is refused rather than sanitized.
fn is_safe_package_name(name: &str) -> bool {
    let body = match name.strip_prefix('@') {
        Some(scoped) => {
            let mut parts = scoped.splitn(2, '/');
            let scope = parts.next().unwrap_or("");
            let Some(rest) = parts.next() else {
                return false; // `@scope` with no package
            };
            if scope.is_empty() || !is_safe_name_segment(scope) {
                return false;
            }
            rest
        }
        None => name,
    };
    is_safe_name_segment(body)
}

fn is_safe_name_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment.len() <= 214
        && segment != "."
        && segment != ".."
        && !segment.starts_with('.')
        && segment.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.' | '~')
        })
}

// ---------------------------------------------------------------------------
// Filesystem resolution
// ---------------------------------------------------------------------------

/// Roots that may contain an unpacked `_npx` package: `<cache>/_npx/<hash>/node_modules`.
fn npx_package_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let mut push_cache = |cache: PathBuf| {
        let npx = cache.join("_npx");
        if npx.is_dir() && !roots.contains(&npx) {
            roots.push(npx);
        }
    };
    // An explicit cache location wins, and is what the tests would use in anger.
    if let Some(dir) = std::env::var_os("npm_config_cache") {
        push_cache(PathBuf::from(dir));
    }
    if let Some(home) = dirs::home_dir() {
        // npm's default differs by platform: %LOCALAPPDATA%\npm-cache on Windows,
        // ~/.npm elsewhere. Probe both rather than cfg! on it, since a roaming
        // profile or a copied config can produce either.
        push_cache(home.join(".npm"));
        push_cache(home.join("AppData").join("Local").join("npm-cache"));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        push_cache(PathBuf::from(local).join("npm-cache"));
    }
    roots
        .into_iter()
        .flat_map(|npx| {
            std::fs::read_dir(&npx)
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.path().join("node_modules"))
                .filter(|p| p.is_dir())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Locate `node`, preferring the interpreter already running the launcher chain.
fn node_executable() -> Option<PathBuf> {
    // npm sets this for anything it spawns, and it points at the exact node that
    // would have run the script, which is the one we are replacing.
    if let Some(exe) = std::env::var_os("npm_node_execpath") {
        let path = PathBuf::from(exe);
        if path.is_file() {
            return Some(path);
        }
    }
    let resolved = crate::downstream::resolve_command("node");
    let path = PathBuf::from(&resolved);
    // resolve_command echoes its input back on a miss, which would leave a bare
    // "node" that only works via PATH lookup at spawn time. Require a real file so a
    // failed resolution falls back to the launcher instead of a confusing spawn error.
    if path.is_file() {
        Some(path)
    } else {
        None
    }
}

/// Find the package's entry script under one of the `_npx` roots and build the spawn.
fn resolve_npx_plan(
    plan: &LauncherPlan,
    npx_roots: &[PathBuf],
    node: &Path,
) -> Option<DirectSpawn> {
    let mut best: Option<(Version, PathBuf, PathBuf)> = None;
    for root in npx_roots {
        let dir = package_dir(root, &plan.package);
        let Some(manifest) = read_manifest(&dir) else {
            continue;
        };
        if let Some(want) = &plan.requested_version {
            // Only an exact match is safe to shortcut; a range or dist-tag needs the
            // registry, which is precisely what npx is for.
            if manifest.version.as_deref() != Some(want.as_str()) {
                continue;
            }
        }
        let version = Version::parse(manifest.version.as_deref().unwrap_or("0.0.0"));
        if best.as_ref().is_none_or(|(b, _, _)| version > *b) {
            best = Some((version, dir, root.clone()));
        }
    }
    let (_, dir, root) = best?;
    let manifest = read_manifest(&dir)?;
    let entry = entry_script(&manifest, &plan.package, plan.bin.as_deref())?;
    let script = safe_join(&dir, &entry)?;
    if !script.is_file() || !is_js(&script) {
        return None;
    }
    let bin_dir = root.join(".bin");
    let mut args = Vec::with_capacity(plan.forwarded.len() + 1);
    args.push(script.to_string_lossy().into_owned());
    args.extend(plan.forwarded.iter().cloned());
    Some(DirectSpawn {
        command: node.to_string_lossy().into_owned(),
        args,
        bin_dir: bin_dir.is_dir().then_some(bin_dir),
    })
}

fn package_dir(root: &Path, package: &str) -> PathBuf {
    // Join segment by segment so a scoped name never reaches the OS as one string
    // containing a separator.
    let mut dir = root.to_path_buf();
    for segment in package.split('/') {
        dir.push(segment);
    }
    dir
}

struct Manifest {
    version: Option<String>,
    bin: Option<serde_json::Value>,
}

fn read_manifest(dir: &Path) -> Option<Manifest> {
    let text = std::fs::read_to_string(dir.join("package.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    Some(Manifest {
        version: json
            .get("version")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        bin: json.get("bin").cloned(),
    })
}

/// Pick the entry script out of a manifest's `bin` field.
///
/// `bin` is either a string (one bin, named after the package) or a map of bin name
/// to path. When the caller named a bin (`npx -p pkg somebin`) it must match; when it
/// did not, the bin named after the package wins, and a single-entry map is accepted
/// since there is nothing else it could mean.
fn entry_script(manifest: &Manifest, package: &str, wanted_bin: Option<&str>) -> Option<String> {
    let default_name = package.rsplit('/').next().unwrap_or(package);
    match manifest.bin.as_ref()? {
        serde_json::Value::String(path) => {
            match wanted_bin {
                // A string bin is only ever reachable under the package's own name.
                Some(name) if name != default_name => None,
                _ => Some(path.clone()),
            }
        }
        serde_json::Value::Object(map) => {
            let key = wanted_bin.unwrap_or(default_name);
            if let Some(path) = map.get(key).and_then(|v| v.as_str()) {
                return Some(path.to_string());
            }
            if wanted_bin.is_none() && map.len() == 1 {
                return map.values().next()?.as_str().map(str::to_string);
            }
            None
        }
        _ => None,
    }
}

/// Join a manifest-supplied relative path onto the package dir, refusing escapes.
///
/// The `bin` value comes from a downloaded package, so `../../..` in it is treated as
/// hostile rather than resolved.
fn safe_join(dir: &Path, relative: &str) -> Option<PathBuf> {
    let rel = Path::new(relative);
    if rel.is_absolute() {
        return None;
    }
    let mut out = dir.to_path_buf();
    for component in rel.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            // ParentDir, RootDir, Prefix: all either escape or make no sense here.
            _ => return None,
        }
    }
    Some(out)
}

fn is_js(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "js" | "mjs" | "cjs"))
}

/// A comparable version, so the newest cached copy of a package wins.
///
/// Deliberately not a full semver implementation: it only has to order the handful of
/// copies of one package sitting in the npx cache. A prerelease sorts below the same
/// release, and anything unparseable sorts lowest.
#[derive(Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct Version(u64, u64, u64, bool);

impl Version {
    fn parse(text: &str) -> Self {
        let core = text
            .split(['-', '+'])
            .next()
            .unwrap_or(text)
            .trim_start_matches(['v', '=']);
        let mut parts = core.split('.').map(|p| p.parse::<u64>().unwrap_or(0));
        Version(
            parts.next().unwrap_or(0),
            parts.next().unwrap_or(0),
            parts.next().unwrap_or(0),
            !text.contains('-'), // release sorts above prerelease
        )
    }
}

// ---------------------------------------------------------------------------
// npm .cmd shim resolution (Windows)
// ---------------------------------------------------------------------------

/// Read an npm-generated `.cmd`/`.bat` shim for the `.js` it would run.
///
/// Covers the second shape of the same problem: a globally or locally installed
/// server named directly in config (`"command": "toolport-mcp-servers"`) resolves to
/// a `.CMD`, which Rust runs through `cmd.exe`, costing a shell hop per server.
///
/// npm's shims end with a line that invokes node on a `"%dp0%\..\<pkg>\bin\cli.js"`
/// path. That is a generated format rather than a documented one, so this only
/// rewrites when it finds exactly one such path and that path is a real `.js` file.
fn resolve_shim(
    command: &str,
    args: &[String],
    node: &Path,
    lookup: CommandLookup,
) -> Option<DirectSpawn> {
    let resolved = lookup(command);
    let path = Path::new(&resolved);
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if ext != "cmd" && ext != "bat" {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    let dir = path.parent()?;
    // A shim lives in `node_modules/.bin` (or a global bin dir) and climbs one level
    // to reach its package, so containment is enforced against the parent rather
    // than the shim's own directory.
    let contains = dir.parent().unwrap_or(dir).to_path_buf();
    let mut found: Option<PathBuf> = None;
    for candidate in shim_script_candidates(&text) {
        let script = safe_join_shim(dir, &candidate)?;
        if script.starts_with(&contains) && script.is_file() && is_js(&script) {
            if found.as_ref().is_some_and(|f| *f != script) {
                return None; // ambiguous shim; let cmd.exe sort it out
            }
            found = Some(script);
        }
    }
    let script = found?;
    let mut out = Vec::with_capacity(args.len() + 1);
    out.push(script.to_string_lossy().into_owned());
    out.extend(args.iter().cloned());
    let bin_dir = dir.to_path_buf();
    Some(DirectSpawn {
        command: node.to_string_lossy().into_owned(),
        args: out,
        bin_dir: bin_dir.is_dir().then_some(bin_dir),
    })
}

/// Pull `%dp0%\..\path\to\entry.js` occurrences out of a shim's text.
fn shim_script_candidates(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for chunk in text.split("%dp0%").skip(1) {
        // The path runs to the closing quote of the argument npm generated.
        let Some(end) = chunk.find('"') else { continue };
        let candidate = chunk[..end].trim_start_matches(['\\', '/']);
        if candidate.is_empty() {
            continue;
        }
        let lower = candidate.to_ascii_lowercase();
        if lower.ends_with(".js") || lower.ends_with(".mjs") || lower.ends_with(".cjs") {
            out.push(candidate.to_string());
        }
    }
    out
}

/// Like [`safe_join`] but tolerates the `..` npm shims legitimately use to climb from
/// `node_modules/.bin` to the package.
///
/// This function itself allows unbounded `..`; containment is the caller's job.
/// [`resolve_shim`] requires the result to sit under the shim directory's parent and
/// to be a real `.js` file.
fn safe_join_shim(dir: &Path, relative: &str) -> Option<PathBuf> {
    let mut out = dir.to_path_buf();
    for part in relative.split(['\\', '/']) {
        match part {
            "" | "." => {}
            ".." => {
                if !out.pop() {
                    return None;
                }
            }
            other => out.push(other),
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    /// A throwaway `_npx/<hash>/node_modules` tree, cleaned up on drop.
    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "toolport-launcher-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).expect("fixture root");
            Fixture { root }
        }

        /// Install a package with a `bin` field and a real entry file.
        fn package(&self, hash: &str, name: &str, version: &str, bin: serde_json::Value) -> &Self {
            let mut dir = self.root.join(hash).join("node_modules");
            for segment in name.split('/') {
                dir.push(segment);
            }
            std::fs::create_dir_all(&dir).expect("package dir");
            let manifest = serde_json::json!({ "name": name, "version": version, "bin": bin });
            std::fs::write(
                dir.join("package.json"),
                serde_json::to_string(&manifest).unwrap(),
            )
            .expect("manifest");
            // Create every file the bin field points at.
            let paths: Vec<String> = match &bin {
                serde_json::Value::String(p) => vec![p.clone()],
                serde_json::Value::Object(map) => map
                    .values()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect(),
                _ => vec![],
            };
            for p in paths {
                let file = super::safe_join(&dir, &p).expect("safe bin path");
                if let Some(parent) = file.parent() {
                    std::fs::create_dir_all(parent).expect("bin parent");
                }
                std::fs::write(&file, "#!/usr/bin/env node\n").expect("bin file");
            }
            self
        }

        fn roots(&self) -> Vec<PathBuf> {
            std::fs::read_dir(&self.root)
                .expect("fixture roots")
                .flatten()
                .map(|e| e.path().join("node_modules"))
                .filter(|p| p.is_dir())
                .collect()
        }

        /// A stand-in for the node executable; only its path is used.
        fn node(&self) -> PathBuf {
            let path = self.root.join("node.exe");
            std::fs::write(&path, "").expect("node stub");
            path
        }
    }

    /// A PATH on which nothing is installed. `resolve_command` echoes its input
    /// back on a miss, so this is exactly what the production lookup returns for a
    /// command that is not on the machine.
    ///
    /// Every fixture-driven resolution passes this. Without it the shim fallback
    /// asks the DEVELOPER's PATH where `npx` is, which on a machine with npm
    /// installed answers with a real shim - so four tests that assert a spec must
    /// fall back to npx instead resolved against that install and failed on a
    /// clean checkout, while CI (no npm shim in scope) stayed green (SBS-839).
    fn nothing_on_path(command: &str) -> String {
        command.to_string()
    }

    /// The production lookup, for the shim tests: they pass an ABSOLUTE path, which
    /// `resolve_command` returns untouched on both platforms, so those tests drive
    /// `resolve_shim` for real without a PATH search.
    fn host_lookup(command: &str) -> String {
        crate::downstream::resolve_command(command)
    }

    /// [`resolve_in`] against a fixture, with the host PATH out of scope.
    fn resolve_fixture(
        command: &str,
        args: &[String],
        roots: &[PathBuf],
        node: &Path,
    ) -> Option<DirectSpawn> {
        resolve_in(command, args, roots, node, &nothing_on_path)
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    // ----- argument parsing ------------------------------------------------

    #[test]
    fn parses_the_common_npx_invocation() {
        let plan = parse_launcher("npx", &s(&["-y", "toolport-mcp-servers", "vercel"]))
            .expect("plain npx must parse");
        assert_eq!(plan.package, "toolport-mcp-servers");
        assert_eq!(plan.requested_version, None);
        assert_eq!(plan.bin, None);
        assert_eq!(plan.forwarded, s(&["vercel"]));
    }

    #[test]
    fn parses_scoped_names_versions_and_windows_shims() {
        // A scoped name keeps its scope; the version splits off the second `@`.
        let plan = parse_launcher(
            "npx.CMD",
            &s(&["-y", "@scope/server@1.2.3", "--port", "8080"]),
        )
        .expect("scoped spec must parse");
        assert_eq!(plan.package, "@scope/server");
        assert_eq!(plan.requested_version.as_deref(), Some("1.2.3"));
        assert_eq!(plan.forwarded, s(&["--port", "8080"]));

        // `-p <spec> <bin>`: the positional names the bin, not the package.
        let plan = parse_launcher("npx", &s(&["-p", "@scope/tools", "other-bin", "x"]))
            .expect("-p form must parse");
        assert_eq!(plan.package, "@scope/tools");
        assert_eq!(plan.bin.as_deref(), Some("other-bin"));
        assert_eq!(plan.forwarded, s(&["x"]));

        // `npm exec` is the same thing spelled differently.
        let plan = parse_launcher("npm", &s(&["exec", "-y", "pkg", "arg"])).expect("npm exec");
        assert_eq!(plan.package, "pkg");
        assert_eq!(plan.forwarded, s(&["arg"]));
    }

    /// Everything whose meaning lives inside npx must fall back, because rewriting it
    /// would change what runs.
    #[test]
    fn refuses_invocations_it_cannot_faithfully_rewrite() {
        // -c runs a shell string: rewriting it would drop the shell entirely.
        assert!(parse_launcher("npx", &s(&["-c", "echo hi"])).is_none());
        // --node-arg changes how node itself is invoked.
        assert!(parse_launcher("npx", &s(&["--node-arg=--inspect", "pkg"])).is_none());
        // An unknown flag might be any of the above.
        assert!(parse_launcher("npx", &s(&["--mystery", "pkg"])).is_none());
        // Two packages: npx builds a combined env we cannot reproduce.
        assert!(parse_launcher("npx", &s(&["-p", "a", "-p", "b", "bin"])).is_none());
        // Non-registry specs are not cached under a predictable name.
        assert!(parse_launcher("npx", &s(&["github:user/repo"])).is_none());
        assert!(parse_launcher("npx", &s(&["https://example.invalid/x.tgz"])).is_none());
        assert!(parse_launcher("npx", &s(&["./local-thing"])).is_none());
        // Not a launcher at all.
        assert!(parse_launcher("node", &s(&["server.js"])).is_none());
        assert!(parse_launcher("npm", &s(&["run", "start"])).is_none());
        // Nothing to run.
        assert!(parse_launcher("npx", &s(&["-y"])).is_none());
        // These ask npx to revalidate against the registry or ignore a cached copy.
        // A cache-only rewrite cannot honor either, so both must fall back.
        assert!(parse_launcher("npx", &s(&["--prefer-online", "pkg"])).is_none());
        assert!(parse_launcher("npx", &s(&["--ignore-existing", "pkg"])).is_none());
        // `--package=` must strip exactly one prefix. With trim_start_matches this
        // parsed as `pkg`; the repeated form is not a package name, so it falls back.
        assert!(parse_launcher("npx", &s(&["--package=--package=pkg", "bin"])).is_none());
    }

    /// The attached `--package=<spec>` spelling, which nothing else covers.
    #[test]
    fn parses_the_attached_package_flag() {
        let plan = parse_launcher("npx", &s(&["--package=@scope/tools", "other-bin", "x"]))
            .expect("--package= form must parse");
        assert_eq!(plan.package, "@scope/tools");
        assert_eq!(plan.bin.as_deref(), Some("other-bin"));
        assert_eq!(plan.forwarded, s(&["x"]));
    }

    /// The name is concatenated into a filesystem path, so traversal attempts must be
    /// refused outright rather than cleaned up.
    #[test]
    fn refuses_package_names_that_could_escape_the_cache() {
        for bad in [
            "../../etc/passwd",
            "..",
            ".",
            ".hidden",
            "a/b",
            "@scope/../x",
            "@scope",
            "UPPER",
            "has space",
            "C:\\evil",
        ] {
            assert!(
                split_spec(bad).is_none(),
                "{bad:?} must not resolve to a package path"
            );
        }
        // The legitimate shapes still pass.
        assert!(split_spec("pkg").is_some());
        assert!(split_spec("@scope/pkg").is_some());
        assert!(split_spec("pkg.js").is_some());
    }

    // ----- filesystem resolution -------------------------------------------

    #[test]
    fn resolves_a_cached_package_to_a_direct_node_spawn() {
        let fx = Fixture::new("basic");
        fx.package(
            "abc123",
            "toolport-mcp-servers",
            "1.4.0",
            serde_json::json!({ "toolport-mcp-servers": "bin/cli.js" }),
        );
        let node = fx.node();

        let direct = resolve_fixture(
            "npx",
            &s(&["-y", "toolport-mcp-servers", "vercel"]),
            &fx.roots(),
            &node,
        )
        .expect("a cached package must resolve");

        assert_eq!(direct.command, node.to_string_lossy());
        assert_eq!(direct.args.len(), 2, "entry script plus the forwarded arg");
        assert!(direct.args[0].replace('\\', "/").ends_with("bin/cli.js"));
        assert_eq!(direct.args[1], "vercel");
    }

    #[test]
    fn a_string_bin_field_resolves_and_the_newest_version_wins() {
        let fx = Fixture::new("newest");
        fx.package("old", "srv", "1.9.0", serde_json::json!("index.js"));
        fx.package("new", "srv", "1.10.0", serde_json::json!("index.js"));
        let node = fx.node();

        let direct =
            resolve_fixture("npx", &s(&["srv"]), &fx.roots(), &node).expect("string bin resolves");
        // 1.10.0 > 1.9.0 numerically, which a string comparison would get wrong.
        assert!(
            direct.args[0].replace('\\', "/").contains("/new/"),
            "expected the 1.10.0 copy, got {}",
            direct.args[0]
        );
    }

    /// npm publishes `1.4.0-rc.1` *before* `1.4.0`, so both can sit in the cache. A
    /// plain numeric compare would call them equal and let the prerelease win by
    /// arrival order.
    #[test]
    fn a_release_sorts_above_its_own_prereleases() {
        let release = Version::parse("1.4.0");
        for pre in ["1.4.0-rc.1", "1.4.0-rc.10", "1.4.0-beta", "1.4.0-alpha.0"] {
            assert!(
                release > Version::parse(pre),
                "1.4.0 must outrank {pre}, got {release:?} vs {:?}",
                Version::parse(pre)
            );
        }
        // The numeric core still dominates: a later prerelease beats an earlier release.
        assert!(Version::parse("1.5.0-rc.1") > Version::parse("1.4.0"));
        // Build metadata is not a prerelease marker.
        assert!(Version::parse("1.4.0+build.7") > Version::parse("1.4.0-rc.1"));
    }

    #[test]
    fn a_cached_prerelease_never_beats_the_cached_release() {
        let fx = Fixture::new("prerelease");
        fx.package("rc", "srv", "1.4.0-rc.1", serde_json::json!("index.js"));
        fx.package("rel", "srv", "1.4.0", serde_json::json!("index.js"));
        let node = fx.node();

        let direct = resolve_fixture("npx", &s(&["srv"]), &fx.roots(), &node)
            .expect("cached package resolves");
        assert!(
            direct.args[0].replace('\\', "/").contains("/rel/"),
            "expected the 1.4.0 release copy, got {}",
            direct.args[0]
        );
    }

    #[test]
    fn an_exact_version_request_must_match_the_cached_copy() {
        let fx = Fixture::new("version");
        fx.package("h", "srv", "2.0.0", serde_json::json!("index.js"));
        let node = fx.node();

        assert!(
            resolve_fixture("npx", &s(&["srv@2.0.0"]), &fx.roots(), &node).is_some(),
            "an exact match is safe to shortcut"
        );
        // A different version, a range, or a dist-tag all need the registry.
        for spec in ["srv@2.0.1", "srv@^2.0.0", "srv@latest"] {
            assert!(
                resolve_fixture("npx", &s(&[spec]), &fx.roots(), &node).is_none(),
                "{spec} must fall back to npx"
            );
        }
    }

    #[test]
    fn falls_back_when_the_package_is_not_cached_or_the_entry_is_missing() {
        let fx = Fixture::new("missing");
        fx.package(
            "h",
            "srv",
            "1.0.0",
            serde_json::json!({ "srv": "bin/cli.js" }),
        );
        let node = fx.node();

        // Never installed.
        assert!(resolve_fixture("npx", &s(&["other-srv"]), &fx.roots(), &node).is_none());
        // Installed, but the bin the caller asked for is not one of its bins.
        assert!(resolve_fixture("npx", &s(&["-p", "srv", "nope"]), &fx.roots(), &node).is_none());

        // Manifest promises an entry that is not on disk.
        let fx2 = Fixture::new("dangling");
        let dir = fx2.root.join("h").join("node_modules").join("srv");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("package.json"),
            r#"{"name":"srv","version":"1.0.0","bin":{"srv":"bin/gone.js"}}"#,
        )
        .unwrap();
        assert!(resolve_fixture("npx", &s(&["srv"]), &fx2.roots(), &fx2.node()).is_none());
    }

    /// A downloaded package's `bin` is untrusted input; it must not be able to point
    /// the spawn at a file outside its own directory.
    #[test]
    fn a_bin_field_cannot_escape_its_package_directory() {
        assert!(safe_join(Path::new("/pkg"), "../../../etc/passwd").is_none());
        assert!(safe_join(Path::new("/pkg"), "/etc/passwd").is_none());
        assert!(safe_join(Path::new("/pkg"), "./bin/cli.js").is_some());
        assert!(safe_join(Path::new("/pkg"), "bin/cli.js").is_some());
    }

    /// The guard above, reached the way an attacker would reach it: a real manifest
    /// in a real cache tree, resolved through `resolve_in`. Asserting on `safe_join`
    /// alone would keep passing if the resolution stopped calling it.
    #[test]
    fn an_escaping_bin_path_is_refused_by_the_real_resolution() {
        let fx = Fixture::new("escape");
        let outside = fx.root.join("outside.js");
        std::fs::write(&outside, "// not ours\n").expect("decoy");

        // Hand-written manifest: `package()` would refuse to create this bin path.
        let dir = fx.root.join("h").join("node_modules").join("srv");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("package.json"),
            r#"{"name":"srv","version":"1.0.0","bin":{"srv":"../../../outside.js"}}"#,
        )
        .unwrap();

        assert!(
            resolve_fixture("npx", &s(&["srv"]), &fx.roots(), &fx.node()).is_none(),
            "a bin field pointing outside the package must not be spawned"
        );
    }

    #[test]
    fn only_javascript_entries_are_rewritten() {
        let fx = Fixture::new("nonjs");
        // A package whose bin is a shell script, not something node can run.
        fx.package(
            "h",
            "srv",
            "1.0.0",
            serde_json::json!({ "srv": "bin/cli.sh" }),
        );
        assert!(resolve_fixture("npx", &s(&["srv"]), &fx.roots(), &fx.node()).is_none());
    }

    // ----- .cmd shim parsing -----------------------------------------------

    #[test]
    fn extracts_the_script_path_from_an_npm_shim() {
        // The tail of a real npm-generated .cmd shim.
        let shim = r#"@ECHO off
SETLOCAL
CALL :find_dp0
IF EXIST "%dp0%\node.exe" (
  SET "_prog=%dp0%\node.exe"
) ELSE (
  SET "_prog=node"
)
endLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & "%_prog%"  "%dp0%\..\toolport-mcp-servers\bin\cli.js" %*
"#;
        let found = shim_script_candidates(shim);
        assert_eq!(
            found,
            vec!["..\\toolport-mcp-servers\\bin\\cli.js".to_string()]
        );
        // node.exe references are not .js and must not be mistaken for the entry.
        assert!(!found.iter().any(|f| f.contains("node.exe")));
    }

    #[test]
    fn a_shim_with_no_script_path_yields_nothing() {
        assert!(shim_script_candidates("@ECHO off\r\nnpm.cmd %*\r\n").is_empty());
        assert!(shim_script_candidates("").is_empty());
    }

    /// Build a `.bin` directory holding `shim.cmd` plus the given sibling scripts,
    /// and return the shim's absolute path. `resolve_command` passes an absolute
    /// path through untouched on both platforms, so this drives `resolve_shim` for
    /// real rather than through PATH lookup.
    fn shim_fixture(fx: &Fixture, body: &str, scripts: &[&str]) -> String {
        let bin = fx.root.join("node_modules").join(".bin");
        std::fs::create_dir_all(&bin).expect("bin dir");
        for script in scripts {
            let path = super::safe_join_shim(&bin, script).expect("script path");
            std::fs::create_dir_all(path.parent().unwrap()).expect("script parent");
            std::fs::write(&path, "// entry\n").expect("script");
        }
        let shim = bin.join("shim.cmd");
        std::fs::write(&shim, body).expect("shim");
        shim.to_string_lossy().into_owned()
    }

    /// The shim path end to end. The isolated `shim_script_candidates` /
    /// `safe_join_shim` assertions keep passing if `resolve_shim` stops consulting
    /// them, so the ambiguity refusal and the extension gate need driving for real.
    #[test]
    fn a_well_formed_shim_resolves_to_its_script() {
        let fx = Fixture::new("shim-ok");
        let node = fx.node();
        let shim = shim_fixture(
            &fx,
            "@ECHO off\r\n\"%_prog%\"  \"%dp0%\\..\\pkg\\bin\\cli.js\" %*\r\n",
            &["..\\pkg\\bin\\cli.js"],
        );

        let direct =
            resolve_shim(&shim, &s(&["--flag"]), &node, &host_lookup).expect("shim must resolve");
        assert_eq!(direct.command, node.to_string_lossy());
        assert!(direct.args[0]
            .replace('\\', "/")
            .ends_with("pkg/bin/cli.js"));
        assert_eq!(direct.args[1], "--flag", "shim args pass through");
    }

    #[test]
    fn an_ambiguous_shim_refuses_to_guess() {
        let fx = Fixture::new("shim-ambiguous");
        let node = fx.node();
        // Two different existing .js targets: which one cmd.exe would pick depends on
        // batch control flow we are not interpreting, so rewriting is not safe.
        let shim = shim_fixture(
            &fx,
            "@ECHO off\r\n\"%dp0%\\..\\pkg\\bin\\one.js\"\r\n\"%dp0%\\..\\pkg\\bin\\two.js\"\r\n",
            &["..\\pkg\\bin\\one.js", "..\\pkg\\bin\\two.js"],
        );

        assert!(resolve_shim(&shim, &[], &node, &host_lookup).is_none());
    }

    /// A shim that climbs out of its own `node_modules` is refused, matching what
    /// `safe_join_shim`'s doc comment says the caller enforces.
    #[test]
    fn a_shim_escaping_its_node_modules_is_refused() {
        let fx = Fixture::new("shim-escape");
        let node = fx.node();
        let outside = fx.root.join("outside.js");
        std::fs::write(&outside, "// not ours\n").expect("decoy");

        let shim = shim_fixture(
            &fx,
            "@ECHO off\r\n\"%dp0%\\..\\..\\outside.js\" %*\r\n",
            &[],
        );
        assert!(
            outside.is_file(),
            "the decoy must exist for this to prove anything"
        );
        assert!(resolve_shim(&shim, &[], &node, &host_lookup).is_none());
    }

    #[test]
    fn a_shim_naming_a_non_javascript_target_is_refused() {
        let fx = Fixture::new("shim-nonjs");
        let node = fx.node();
        let shim = shim_fixture(
            &fx,
            "@ECHO off\r\n\"%dp0%\\..\\pkg\\bin\\cli.sh\" %*\r\n",
            &["..\\pkg\\bin\\cli.sh"],
        );
        assert!(resolve_shim(&shim, &[], &node, &host_lookup).is_none());
    }

    #[test]
    fn a_shim_relative_path_climbs_out_of_dot_bin() {
        let joined = safe_join_shim(Path::new("/root/node_modules/.bin"), "..\\pkg\\bin\\cli.js")
            .expect("npm shims legitimately use ..");
        assert_eq!(
            joined.to_string_lossy().replace('\\', "/"),
            "/root/node_modules/pkg/bin/cli.js"
        );
    }
}
