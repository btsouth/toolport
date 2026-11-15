//! T3 Code's provider instances, read from its `settings.json`.
//!
//! T3 Code runs Claude Code, Codex, OpenCode, Cursor and other agents as providers. Each
//! provider reads its own MCP config, so Toolport connects to the providers, not to T3
//! Code. This module finds which client configs T3 Code's instances use, so the Clients
//! page can label those rows and find homes no launcher points at.
//!
//! T3 Code keeps its data in `T3CODE_HOME`, or `<home>/.t3` (Node's `os.homedir()`), on
//! every platform, with settings in `userdata/settings.json`. The file is internal and has
//! no schema version, and some instances hold API keys inline in `environment`. Only an
//! instance's driver, display name, enabled flags, `homePath`, and the values of
//! non-sensitive `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `XDG_CONFIG_HOME`, `GROK_HOME` and
//! `PI_CODING_AGENT_DIR` entries are ever decoded. Every other value, secrets included,
//! is skipped by the parser without being copied, and the file buffer is zeroed once
//! parsing ends. Nothing here is logged except a fixed one-line notice.
use super::*;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// The settings file is a few kilobytes. Anything this large is not T3 Code's.
const SETTINGS_READ_LIMIT: u64 = 8 * 1024 * 1024;
/// A display name is user text; keep it short enough for a row badge and a log-free label.
const NAME_LIMIT: usize = 80;

/// The Toolport client an instance's provider reads its MCP servers from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Provider {
    ClaudeCode,
    Codex,
    OpenCode,
    Cursor,
    Kiro,
    Grok,
    Pi,
}

impl Provider {
    fn of(driver: &str) -> Option<Self> {
        Some(match driver {
            "claudeAgent" => Self::ClaudeCode,
            "codex" => Self::Codex,
            "opencode" => Self::OpenCode,
            "cursor" => Self::Cursor,
            "kiro" => Self::Kiro,
            "grok" => Self::Grok,
            "pi" => Self::Pi,
            _ => return None,
        })
    }

    /// The Toolport client id, which is also the location client type.
    pub(super) fn client_type(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
            Self::Cursor => "cursor",
            Self::Kiro => "kiro",
            Self::Grok => "grok",
            Self::Pi => "pi",
        }
    }

    /// The name T3 Code shows for an instance without its own display name.
    fn default_name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude",
            Self::Codex => "Codex",
            Self::OpenCode => "OpenCode",
            Self::Cursor => "Cursor",
            Self::Kiro => "Kiro",
            Self::Grok => "Grok",
            Self::Pi => "Pi",
        }
    }
}

/// One T3 Code provider instance that Toolport can match to a client config.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Instance {
    /// T3 Code's instance id, the key in `providerInstances`.
    pub id: String,
    pub provider: Provider,
    pub display_name: String,
    pub enabled: bool,
    /// The MCP config file this instance's provider reads. `None` means the client's own
    /// default config, as Toolport resolves it.
    pub config_path: Option<PathBuf>,
}

/// A T3 Code instance that uses a client row, as the Clients page shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct T3CodeUse {
    pub instance_id: String,
    pub display_name: String,
    pub enabled: bool,
}

/// What `settings.json` says, or why it could not be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Reading {
    /// T3 Code is not installed here: no data folder.
    Absent,
    /// The instances Toolport can match, in instance id order.
    Instances(Vec<Instance>),
    /// The file exists but could not be read as T3 Code settings.
    Unreadable,
}

/// T3 Code's data folder: `T3CODE_HOME` (with `~` expanded) or `<home>/.t3`.
pub(super) fn data_dir() -> Option<PathBuf> {
    let home = home()?;
    match client_path_env("T3CODE_HOME") {
        Some(raw) => {
            let raw = raw.to_string_lossy().trim().to_string();
            if raw.is_empty() {
                return Some(home.join(".t3"));
            }
            // T3 Code resolves a relative value against its own working directory, which
            // Toolport cannot know. No guess: such a folder is not read.
            expand(&raw, &home).filter(|path| path.is_absolute())
        }
        None => Some(home.join(".t3")),
    }
}

fn settings_path() -> Option<PathBuf> {
    Some(data_dir()?.join("userdata").join("settings.json"))
}

/// `~`, `~/x` and `~\x` as T3 Code's `expandHomePath` reads them; anything else as is.
fn expand(raw: &str, home: &Path) -> Option<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if raw == "~" {
        return Some(home.to_path_buf());
    }
    if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        return Some(home.join(rest));
    }
    Some(PathBuf::from(raw))
}

/// A cheap token that changes when T3 Code's settings change, for the Clients page to
/// notice an edit while it is open. `None` when there is no settings file.
pub fn revision() -> Option<String> {
    let meta = std::fs::metadata(settings_path()?).ok()?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|time| time.as_nanos())
        .unwrap_or_default();
    Some(format!("{modified}:{}", meta.len()))
}

/// The one line logged when the settings file cannot be used. Fixed text: nothing from the
/// file, not even its path, reaches the log.
pub(super) const UNREADABLE_NOTICE: &str =
    "toolport: T3 Code settings could not be read; its provider instances are not shown on Clients";

type Cached = (PathBuf, Option<std::time::SystemTime>, u64, Arc<Reading>);

static CACHE: Mutex<Option<Cached>> = Mutex::new(None);

/// The current instances, parsed again only when the file changes. When T3 Code is not
/// installed this costs one failed `stat` of its data folder.
pub(super) fn current() -> Arc<Reading> {
    let Some(path) = settings_path() else {
        return Arc::new(Reading::Absent);
    };
    let userdata = path.parent().map(Path::to_path_buf).unwrap_or_default();
    if !userdata.is_dir() {
        return Arc::new(Reading::Absent);
    }
    let meta = std::fs::metadata(&path).ok();
    let modified = meta.as_ref().and_then(|meta| meta.modified().ok());
    let len = meta.as_ref().map(|meta| meta.len()).unwrap_or(u64::MAX);
    let mut cache = CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((cached_path, cached_modified, cached_len, reading)) = cache.as_ref() {
        if *cached_path == path && *cached_modified == modified && *cached_len == len {
            return reading.clone();
        }
    }
    let home = home().unwrap_or_default();
    let reading = Arc::new(match meta {
        // T3 Code writes settings.json only after a setting changes. Until then its two
        // default instances are on, with their default homes.
        None if std::fs::symlink_metadata(&path).is_err() => {
            Reading::Instances(with_defaults(BTreeMap::new()))
        }
        // A link to nothing.
        None => Reading::Unreadable,
        Some(meta) if !meta.is_file() || meta.len() > SETTINGS_READ_LIMIT => Reading::Unreadable,
        Some(_) => match read_contained(&path, &userdata) {
            Some(bytes) => parse(bytes, &home),
            None => Reading::Unreadable,
        },
    });
    if *reading == Reading::Unreadable {
        // One fixed line per changed file. No path contents, no values.
        eprintln!("{UNREADABLE_NOTICE}");
        crate::gatewaylog::append(UNREADABLE_NOTICE);
    }
    *cache = Some((path, modified, len, reading.clone()));
    reading
}

/// Read the settings file only if the file actually opened is inside T3 Code's data
/// folder. The check is made on the open handle, not on the name, so a file or folder
/// swapped for a link out of the folder between any check and the open is never read. A
/// link that stays inside the folder is followed. Reads at most [`SETTINGS_READ_LIMIT`]
/// bytes, whatever the file grew to since it was measured.
fn read_contained(path: &Path, userdata: &Path) -> Option<Vec<u8>> {
    use std::io::Read;
    let root = std::fs::canonicalize(userdata.parent()?).ok()?;
    before_open();
    let file = open_settings(path).ok()?;
    if !file.metadata().ok()?.is_file() || !opened_inside(&file, path, &root) {
        return None;
    }
    // One allocation, so no grown-out copy of the file (and of any key in it) is freed
    // without being zeroed. A file that grew past its size since is cut at the limit.
    let size = file.metadata().ok()?.len().min(SETTINGS_READ_LIMIT) as usize;
    let mut bytes = Vec::with_capacity(size + 1);
    let read = file.take(size as u64 + 1).read_to_end(&mut bytes);
    if read.is_err() || bytes.len() > size || bytes.len() as u64 > SETTINGS_READ_LIMIT {
        bytes.fill(0);
        return None;
    }
    Some(bytes)
}

/// Open for reading without waiting: a name swapped for a FIFO or a device must not hang
/// the scan. The handle is checked to be a plain file before anything is read.
#[cfg(unix)]
fn open_settings(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
        .open(path)
}

#[cfg(not(unix))]
fn open_settings(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
}

/// Whether the opened file is inside `root`, judged by the handle's own path where the
/// system gives one.
fn opened_inside(file: &std::fs::File, path: &Path, root: &Path) -> bool {
    match handle_path(file) {
        Some(real) => real.starts_with(root),
        None => same_file_inside(file, path, root),
    }
}

/// The path of the open file as the kernel knows it.
#[cfg(target_os = "linux")]
fn handle_path(file: &std::fs::File) -> Option<PathBuf> {
    use std::os::fd::AsRawFd;
    let real = std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).ok()?;
    // Not a path at all ("pipe:[1]", "anon_inode:..."): no answer from the handle.
    real.is_absolute().then_some(real)
}

#[cfg(target_os = "macos")]
fn handle_path(file: &std::fs::File) -> Option<PathBuf> {
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    let mut buffer = vec![0u8; libc::PATH_MAX as usize + 1];
    // SAFETY: F_GETPATH writes a NUL-terminated path of at most MAXPATHLEN (PATH_MAX)
    // bytes into the buffer, which is one byte longer than that.
    let result = unsafe {
        libc::fcntl(
            file.as_raw_fd(),
            libc::F_GETPATH,
            buffer.as_mut_ptr().cast::<libc::c_char>(),
        )
    };
    if result == -1 {
        return None;
    }
    let len = buffer.iter().position(|&b| b == 0)?;
    let real = PathBuf::from(std::ffi::OsStr::from_bytes(&buffer[..len]));
    real.is_absolute().then_some(real)
}

#[cfg(windows)]
fn handle_path(file: &std::fs::File) -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFinalPathNameByHandleW, FILE_NAME_NORMALIZED, VOLUME_NAME_DOS,
    };
    let mut buffer = vec![0u16; 1024];
    loop {
        // SAFETY: the handle is open for the call, and the buffer length passed is its
        // real length in UTF-16 units.
        let len = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
            )
        } as usize;
        if len == 0 {
            return None;
        }
        // Too small: `len` is the size needed, terminator included.
        if len >= buffer.len() {
            buffer.resize(len + 1, 0);
            continue;
        }
        // The same `\\?\C:\...` form `std::fs::canonicalize` gives the root.
        return Some(PathBuf::from(std::ffi::OsString::from_wide(&buffer[..len])));
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn handle_path(_file: &std::fs::File) -> Option<PathBuf> {
    None
}

/// Without the handle's path: the name, resolved after the open, must be inside `root`
/// and be the very file that was opened.
#[cfg(unix)]
fn same_file_inside(file: &std::fs::File, path: &Path, root: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(real) = std::fs::canonicalize(path) else {
        return false;
    };
    let (Ok(opened), Ok(named)) = (file.metadata(), std::fs::symlink_metadata(&real)) else {
        return false;
    };
    real.starts_with(root) && opened.dev() == named.dev() && opened.ino() == named.ino()
}

#[cfg(not(unix))]
fn same_file_inside(_file: &std::fs::File, _path: &Path, _root: &Path) -> bool {
    false
}

#[cfg(test)]
thread_local! {
    /// Runs just before the settings file is opened, so a test can swap the file or its
    /// folder in the window between any check and the open.
    pub(super) static BEFORE_OPEN: std::cell::RefCell<Option<Box<dyn FnMut()>>> =
        const { std::cell::RefCell::new(None) };
}

fn before_open() {
    #[cfg(test)]
    BEFORE_OPEN.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook();
        }
    });
}

/// Parse a settings file. Takes the bytes so it can zero them when it is done.
pub(super) fn parse(mut bytes: Vec<u8>, home: &Path) -> Reading {
    let reading = match std::str::from_utf8(&bytes) {
        Ok(text) => parse_text(text, home),
        Err(_) => Reading::Unreadable,
    };
    // The buffer held every value in the file, API keys included. Clear it before it
    // returns to the allocator.
    bytes.fill(0);
    std::hint::black_box(&bytes);
    reading
}

#[derive(Deserialize)]
struct RawSettings<'a> {
    #[serde(rename = "providerInstances", borrow, default)]
    provider_instances: Option<BTreeMap<String, &'a RawValue>>,
}

/// The allow-listed fields of one instance. `environment` entries keep their value as an
/// undecoded slice of the input until the name says it may be read.
#[derive(Deserialize)]
struct RawInstance<'a> {
    driver: String,
    #[serde(rename = "displayName", default)]
    display_name: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(borrow, default)]
    config: Option<&'a RawValue>,
    #[serde(borrow, default)]
    environment: Option<Vec<RawVariable<'a>>>,
}

/// The config fields T3 Code uses for a home folder. Other config fields, such as an
/// OpenCode server password, are skipped by the parser.
#[derive(Deserialize, Default)]
struct RawConfig {
    #[serde(rename = "homePath", default)]
    home_path: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
}

impl<'a> RawInstance<'a> {
    /// serde reads a struct from an array too; an instance is always an object.
    fn of(raw: &'a RawValue) -> Option<Self> {
        if !raw.get().trim_start().starts_with('{') {
            return None;
        }
        serde_json::from_str(raw.get()).ok()
    }
}

#[derive(Deserialize)]
struct RawVariable<'a> {
    #[serde(default)]
    name: Option<String>,
    #[serde(borrow, default)]
    value: Option<&'a RawValue>,
    #[serde(default)]
    sensitive: Option<bool>,
}

fn parse_text(text: &str, home: &Path) -> Reading {
    // serde reads a struct from an array too; T3 Code's settings are always an object.
    if !text.trim_start().starts_with('{') {
        return Reading::Unreadable;
    }
    let Ok(settings) = serde_json::from_str::<RawSettings>(text) else {
        return Reading::Unreadable;
    };
    let mut found = BTreeMap::new();
    for (id, raw) in settings.provider_instances.unwrap_or_default() {
        // One odd instance must not hide the rest. T3 Code keeps unknown-driver configs
        // verbatim, so shapes Toolport does not know are expected. An instance Toolport
        // cannot use labels no row, and still stops T3 Code's default for that id from
        // standing in for it.
        let instance = RawInstance::of(raw).and_then(|raw| {
            let provider = Provider::of(raw.driver.trim())?;
            instance_of(&id, provider, raw, home)
        });
        found.insert(id.clone(), instance);
    }
    Reading::Instances(with_defaults(found))
}

/// The enabled rule T3 Code's `resolveProviderInstanceEnabled` applies: either flag off
/// wins, then the explicit flag, then the driver's default.
fn resolve_enabled(top: Option<bool>, config: Option<bool>, default: bool) -> bool {
    if top == Some(false) || config == Some(false) {
        false
    } else {
        top.or(config).unwrap_or(default)
    }
}

/// T3 Code's unconfigured `claudeAgent` and `codex` instances are on by default.
fn with_defaults(mut found: BTreeMap<String, Option<Instance>>) -> Vec<Instance> {
    for (id, provider) in [
        ("claudeAgent", Provider::ClaudeCode),
        ("codex", Provider::Codex),
    ] {
        found.entry(id.to_string()).or_insert_with(|| {
            Some(Instance {
                id: id.into(),
                provider,
                display_name: provider.default_name().into(),
                enabled: true,
                config_path: None,
            })
        });
    }
    found.into_values().flatten().collect()
}

/// An instance's `config`, when it is an object of the expected shape.
fn config_of(config: &RawValue) -> Option<RawConfig> {
    if !config.get().trim_start().starts_with('{') {
        return None;
    }
    serde_json::from_str(config.get()).ok()
}

/// The instance, or `None` when Toolport cannot tell which config it reads: its config is
/// not an object of the expected shape, or its home folder is hidden or relative.
fn instance_of(id: &str, provider: Provider, raw: RawInstance, home: &Path) -> Option<Instance> {
    let display_name = raw
        .display_name
        .as_deref()
        .map(display_name)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| provider.default_name().into());
    let default_on = matches!(provider, Provider::ClaudeCode | Provider::Codex);
    // A config Toolport cannot read says nothing reliable about the home: no guess.
    let config = match raw.config {
        Some(config) => config_of(config)?,
        None => RawConfig::default(),
    };
    let enabled = resolve_enabled(raw.enabled, config.enabled, default_on);
    // `Err` when the folder is hidden (sensitive or not text) or relative to T3 Code's
    // working directory, which Toolport cannot know.
    let folder = |value: Result<Option<String>, ()>| -> Result<Option<PathBuf>, ()> {
        match value?
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            None => Ok(None),
            Some(value) => match expand(value, home) {
                Some(path) if path.is_absolute() => Ok(Some(path)),
                _ => Err(()),
            },
        }
    };
    // Claude's and Codex's home: `homePath`, else the instance environment's variable,
    // which T3 Code passes to the provider as it is, so a `~` there is not expanded
    // (ClaudeHome.ts `resolveClaudeHomePath`, CodexAdapterV2's `CODEX_HOME`).
    let home_or = |variable: &str| -> Result<Option<PathBuf>, ()> {
        if let Some(path) = folder(Ok(config.home_path.clone()))? {
            return Ok(Some(path));
        }
        match environment_value(&raw.environment, variable)?
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            None => Ok(None),
            Some(value) if Path::new(value).is_absolute() => Ok(Some(PathBuf::from(value))),
            Some(_) => Err(()),
        }
    };
    let config_path = match provider {
        Provider::ClaudeCode => home_or("CLAUDE_CONFIG_DIR")
            .ok()?
            .map(|dir| claude_code_config_path(&dir)),
        // A shadow home links the shared home's config.toml (T3 Code's
        // CodexHomeLayout), so the instance reads the shared home's MCP servers.
        Provider::Codex => home_or("CODEX_HOME")
            .ok()?
            .map(|dir| codex_config_path(&dir)),
        Provider::OpenCode => folder(environment_value(&raw.environment, "XDG_CONFIG_HOME"))
            .ok()?
            .map(|dir| dir.join("opencode").join("opencode.json")),
        // T3 Code's Grok driver merges the instance environment over its own, and Grok
        // reads `GROK_HOME/config.toml`, `~/.grok` by default.
        Provider::Grok => folder(environment_value(&raw.environment, "GROK_HOME"))
            .ok()?
            .map(|dir| dir.join("config.toml")),
        // Pi reads its MCP servers from `PI_CODING_AGENT_DIR/mcp.json`, `~/.pi/agent` by
        // default.
        Provider::Pi => folder(environment_value(&raw.environment, "PI_CODING_AGENT_DIR"))
            .ok()?
            .map(|dir| dir.join("mcp.json")),
        // T3 Code's Cursor and Kiro use the user's own default MCP config.
        Provider::Cursor | Provider::Kiro => None,
    };
    Some(Instance {
        id: id.into(),
        provider,
        display_name,
        enabled,
        config_path,
    })
}

/// The value an instance's environment gives `name`: the last entry wins, as T3 Code merges
/// them. `Ok(None)` when the instance does not set it. `Err` when the value is hidden
/// (marked sensitive) or not a string: the folder is then unknown, and no guess is made.
fn environment_value(
    environment: &Option<Vec<RawVariable>>,
    name: &str,
) -> Result<Option<String>, ()> {
    let Some(variable) = environment
        .iter()
        .flatten()
        .rfind(|variable| variable.name.as_deref().map(str::trim) == Some(name))
    else {
        return Ok(None);
    };
    if variable.sensitive == Some(true) {
        return Err(());
    }
    match variable.value {
        None => Ok(None),
        Some(value) => serde_json::from_str::<String>(value.get())
            .map(Some)
            .map_err(|_| ()),
    }
}

/// A user-entered name, made safe for a single-line label.
fn display_name(raw: &str) -> String {
    let name: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    name.chars().take(NAME_LIMIT).collect()
}

/// The instances, or nothing when T3 Code is absent or its settings are unreadable.
pub(super) fn instances() -> Vec<Instance> {
    match &*current() {
        Reading::Instances(instances) => instances.clone(),
        _ => Vec::new(),
    }
}

/// The config path an instance uses: its own, or its client's default.
fn resolved_path(instance: &Instance) -> Option<PathBuf> {
    instance
        .config_path
        .clone()
        .or_else(|| default_config_path(instance.provider))
}

fn default_config_path(provider: Provider) -> Option<PathBuf> {
    match provider {
        // With no homePath and no CLAUDE_CONFIG_DIR or CODEX_HOME in the instance
        // environment, the provider reads its documented default, whatever Toolport was
        // started with.
        Provider::ClaudeCode => Some(home()?.join(".claude.json")),
        Provider::Codex => Some(codex_config_path(&home()?.join(".codex"))),
        Provider::OpenCode => client_config_path("opencode"),
        Provider::Cursor => client_config_path("cursor"),
        Provider::Kiro => client_config_path("kiro"),
        Provider::Grok => client_config_path("grok"),
        Provider::Pi => client_config_path("pi"),
    }
}

/// The location id for a T3 Code home no other rule finds: `claude-code@t3-<hash>`.
pub(super) fn location_id(client_type: &str, config: &Path) -> String {
    let key = crate::registry::sha256_hex(&config.to_string_lossy());
    format!("{client_type}@t3-{}", &key[..16])
}

/// Whether an id has the shape [`location_id`] makes.
pub(super) fn is_location_id(id: &str) -> bool {
    id.split_once('@')
        .and_then(|(kind, key)| Some((kind, key.strip_prefix("t3-")?)))
        .is_some_and(|(kind, hash)| {
            matches!(kind, "claude-code" | "codex" | "opencode" | "grok" | "pi")
                && hash.len() == 16
                && hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
}

/// A T3 Code home's config is an ordinary file in an ordinary folder inside the user's own
/// home, or not written yet. The folder is already resolved (no links, no `..`); the
/// config file itself is never a link. A settings file that names `/etc` or another
/// user's folder therefore never becomes a row Toolport could write to. A folder outside
/// home can still be added by hand as a manual location.
pub(super) fn path_is_safe(config: &Path) -> bool {
    let Some(folder) = config.parent() else {
        return false;
    };
    let Some(root) = home().and_then(|home| std::fs::canonicalize(home).ok()) else {
        return false;
    };
    config.is_absolute()
        && std::fs::canonicalize(folder)
            .is_ok_and(|real| real == folder && real.is_dir() && real.starts_with(&root))
        && match std::fs::symlink_metadata(config) {
            Ok(meta) => meta.is_file(),
            Err(error) => error.kind() == std::io::ErrorKind::NotFound,
        }
}

/// Locations for T3 Code homes that are not a client's main config and that `known` does
/// not already list. Homes whose folder does not exist yet are left out until it does.
pub(super) fn automatic(known: &[locations::ClientLocation]) -> Vec<locations::ClientLocation> {
    let mut rows: Vec<locations::ClientLocation> = Vec::new();
    for instance in instances() {
        let Some(path) = instance.config_path.as_ref() else {
            continue;
        };
        let Some(folder) = path
            .parent()
            .and_then(|folder| std::fs::canonicalize(folder).ok())
        else {
            continue;
        };
        let Some(file) = path.file_name() else {
            continue;
        };
        let config_path = folder.join(file);
        let client_type = instance.provider.client_type();
        let main = client_config_path_for_type(client_type);
        if main
            .as_ref()
            .is_some_and(|main| locations::same_path(main, &config_path))
            || known
                .iter()
                .chain(rows.iter())
                .any(|row| locations::same_path(&row.config_path, &config_path))
            || !path_is_safe(&config_path)
        {
            continue;
        }
        rows.push(locations::ClientLocation {
            id: location_id(client_type, &config_path),
            manual: false,
            client_type: client_type.into(),
            name: locations::t3_location_name(client_type, &config_path),
            config_path,
        });
    }
    rows
}

/// The config path Toolport's own row for a client type uses.
fn client_config_path_for_type(client_type: &str) -> Option<PathBuf> {
    match client_type {
        "claude-code" => client_config_path("claude-code"),
        "codex" => codex_path(),
        other => client_config_path(other),
    }
}

/// Whether T3 Code still lists the home behind a saved T3 location.
pub(super) fn lists(config: &Path) -> bool {
    instances().iter().any(|instance| {
        instance
            .config_path
            .as_ref()
            .is_some_and(|path| locations::same_path(path, config))
    })
}

/// Label each client row with the T3 Code instances that use its config.
pub(super) fn annotate(clients: &mut [DetectedClient]) {
    let instances = instances();
    if instances.is_empty() {
        return;
    }
    annotate_with(clients, &instances);
}

pub(super) fn annotate_with(clients: &mut [DetectedClient], instances: &[Instance]) {
    for instance in instances {
        let Some(path) = resolved_path(instance) else {
            continue;
        };
        let client_type = instance.provider.client_type();
        let Some(client) = clients.iter_mut().find(|client| {
            (client.id == client_type || client.id.starts_with(&format!("{client_type}@")))
                && !client.config_path.is_empty()
                && locations::same_path(Path::new(&client.config_path), &path)
        }) else {
            continue;
        };
        if client
            .t3_code
            .iter()
            .any(|known| known.instance_id == instance.id)
        {
            continue;
        }
        client.t3_code.push(T3CodeUse {
            instance_id: instance.id.clone(),
            display_name: instance.display_name.clone(),
            enabled: instance.enabled,
        });
    }
}

#[cfg(test)]
#[path = "t3code_tests.rs"]
mod tests;
