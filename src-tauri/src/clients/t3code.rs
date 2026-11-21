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
//! instance's driver, display name, enabled flags, `homePath`, an ACP Registry `agentId`,
//! and the values of non-sensitive `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `XDG_CONFIG_HOME`,
//! `GROK_HOME` and `PI_CODING_AGENT_DIR` entries are ever decoded; of an entry with another
//! shape, only a plain-text driver, display name and enabled flag. Every other value,
//! secrets included, is skipped by the parser without being copied, and the file buffer is
//! zeroed once parsing ends. Nothing here is logged except a fixed one-line notice.
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
    Antigravity,
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
            "antigravity" => Self::Antigravity,
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
            Self::Antigravity => "antigravity",
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
            Self::Antigravity => "Antigravity",
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

/// Why Toolport cannot connect a T3 Code instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum T3CodeGap {
    /// Toolport has no client for this provider yet.
    Unsupported,
    /// T3 Code's settings file could not be read, so no instance is known. The entry's
    /// display name is the file, with `~` for home.
    Unreadable,
    /// This instance's entry, or its `config`, is not in a shape Toolport can read. Only
    /// its id, and its display name and driver when they are plain text, are shown.
    Malformed,
    /// The variable that names this instance's home folder is marked sensitive or is not
    /// plain text, so Toolport cannot tell which config it reads. Its value is never read.
    HiddenHome,
    /// This instance's home folder is a relative path, which T3 Code resolves against a
    /// working directory Toolport cannot know.
    RelativeHome,
    /// This instance's home folder is not plainly inside the user's home: outside it, another
    /// user's, a network or device path, or with `..`. Judged by its text alone; the path
    /// is never opened and never shown.
    OutsideHome,
    /// This instance's home folder, or the config file in it, is a link or not a plain
    /// folder or file, so Toolport does not make it a row it could write to.
    LinkedHome,
    /// This instance's home folder does not exist yet. For OpenCode, the `opencode`
    /// folder inside its `XDG_CONFIG_HOME`.
    MissingHome,
}

/// A T3 Code instance Toolport cannot connect, shown on the T3 Code card.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct T3CodeOther {
    pub instance_id: String,
    /// The provider as T3 Code names it, such as "Muse Code".
    pub provider_name: String,
    pub display_name: String,
    pub enabled: bool,
    pub gap: T3CodeGap,
}

/// T3 Code's drivers that Toolport cannot connect, with T3 Code's own provider names
/// (`PROVIDER_DISPLAY_NAMES`) and whether the driver is on before it is configured.
fn other_driver(driver: &str) -> Option<(&'static str, T3CodeGap, bool)> {
    Some(match driver {
        "fx" => ("fx", T3CodeGap::Unsupported, false),
        "muse" => ("Muse Code", T3CodeGap::Unsupported, false),
        "acpRegistry" => ("ACP Registry", T3CodeGap::Unsupported, true),
        _ => return None,
    })
}

/// What `settings.json` says, or why it could not be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Reading {
    /// T3 Code is not installed here: no data folder.
    Absent,
    /// The instances Toolport can match, in instance id order, and the ones it cannot,
    /// with the config each [`T3CodeGap::OutsideHome`] one names.
    Instances(Vec<Instance>, Vec<T3CodeOther>, Vec<Outside>),
    /// The file exists but could not be read as T3 Code settings.
    Unreadable,
}

/// The config a [`T3CodeGap::OutsideHome`] instance names, kept as text only: it is
/// compared with saved locations by [`same_text`] and never opened, resolved or shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Outside {
    /// Its entry in the reading's list of instances Toolport cannot connect.
    pub index: usize,
    pub client_type: &'static str,
    pub config: PathBuf,
}

/// T3 Code's data folder: `T3CODE_HOME` (with `~` expanded) or `<home>/.t3`.
pub(super) fn data_dir() -> Option<PathBuf> {
    data_dir_for(&home()?)
}

/// [`data_dir`] for a given home folder.
fn data_dir_for(home: &Path) -> Option<PathBuf> {
    match client_path_env("T3CODE_HOME") {
        Some(raw) => {
            let raw = raw.to_string_lossy().trim().to_string();
            if raw.is_empty() {
                return Some(home.join(".t3"));
            }
            // T3 Code resolves a relative value against its own working directory, which
            // Toolport cannot know. No guess: such a folder is not read.
            expand(&raw, home).filter(|path| path.is_absolute())
        }
        None => Some(home.join(".t3")),
    }
}

fn settings_path() -> Option<PathBuf> {
    Some(data_dir()?.join("userdata").join("settings.json"))
}

/// Absolute, or rooted on the current drive (`\net\x` on Windows): a fixed folder, never
/// one below T3 Code's working directory. A rooted one is never inside home by its text.
fn rooted(path: &Path) -> bool {
    path.is_absolute() || path.has_root()
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

/// [`expand`] by text alone, for a home that is not plain text: `~//x` stays below home
/// (as Node's `path.join` reads it) rather than becoming `/x`. Only ever compared.
fn expand_text(raw: &str, home: &Path) -> PathBuf {
    let raw = raw.trim();
    if raw == "~" {
        return home.to_path_buf();
    }
    match raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        Some(rest) => PathBuf::from(format!(
            "{}{}{rest}",
            home.display(),
            std::path::MAIN_SEPARATOR
        )),
        None => PathBuf::from(raw),
    }
}

/// Whether `path` is inside `home` by its text: absolute, no `.` or `..`, and below
/// `home`. A UNC, device or verbatim path never starts with an ordinary home folder.
/// Windows compares names without regard to case, as Windows does.
pub(super) fn lexically_inside(path: &Path, home: &Path) -> bool {
    inside_by_text(path, home, cfg!(windows))
}

/// [`lexically_inside`] with the case rule given, so both rules run in tests on any OS.
pub(super) fn inside_by_text(path: &Path, home: &Path, ignore_case: bool) -> bool {
    use std::path::Component;
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return false;
    }
    let mut parts = path.components();
    home.components().all(|want| {
        parts
            .next()
            .is_some_and(|part| same_name(part.as_os_str(), want.as_os_str(), ignore_case))
    }) && parts.next().is_some()
}

/// Two path texts name the same place, by the platform's rules and by text alone: on
/// Windows without regard to case, and with a verbatim prefix (`\\?\C:\`,
/// `\\?\UNC\host\`, what canonicalizing gives) the same as the ordinary one the
/// Clients rows show. Nothing on disk is looked at.
pub(super) fn same_text(a: &Path, b: &Path) -> bool {
    let windows = cfg!(windows);
    path_key(&a.to_string_lossy(), windows) == path_key(&b.to_string_lossy(), windows)
}

/// The text [`same_text`] compares, with Windows rules when `windows` (so both rules run
/// in tests on any OS).
pub(super) fn path_key(text: &str, windows: bool) -> String {
    if !windows {
        let rooted = text.starts_with('/');
        let names: Vec<&str> = text.split('/').filter(|name| !name.is_empty()).collect();
        return format!("{}{}", if rooted { "/" } else { "" }, names.join("/"));
    }
    let text = text.replace('/', "\\");
    let ordinary = if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = text.strip_prefix(r"\\?\").filter(|rest| {
        let bytes = rest.as_bytes();
        bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
    }) {
        rest.to_string()
    } else {
        text
    };
    let lead = if ordinary.starts_with(r"\\") {
        r"\\"
    } else if ordinary.starts_with('\\') {
        "\\"
    } else {
        ""
    };
    let names: Vec<String> = ordinary
        .split('\\')
        .filter(|name| !name.is_empty())
        .map(str::to_lowercase)
        .collect();
    format!("{lead}{}", names.join("\\"))
}

fn same_name(a: &std::ffi::OsStr, b: &std::ffi::OsStr, ignore_case: bool) -> bool {
    a == b
        || (ignore_case && a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase())
}

/// Whether a home folder exactly as T3 Code's settings spell it is plain text Toolport
/// may use, checked before any `Path` parsing (which would drop `.` segments). Rejects
/// `.` and `..` segments, empty segments in the middle (`//`), `~user`, and on Windows
/// (`windows`, so both rules run in tests) `\` as a separator too, a segment ending in a
/// dot or space, and a `:` anywhere but a leading drive (alternate data streams).
pub(super) fn plain_path_text(raw: &str, windows: bool) -> bool {
    let separator = |c: char| c == '/' || (windows && c == '\\');
    let (tilde, body) = match raw.strip_prefix('~') {
        Some("") => return true,
        Some(rest) if rest.starts_with(separator) => (true, &rest[1..]),
        Some(_) => return false,
        None => (false, raw),
    };
    if tilde && body.is_empty() {
        return true;
    }
    let segments: Vec<&str> = body.split(separator).collect();
    let last = segments.len() - 1;
    segments.iter().enumerate().all(|(i, segment)| {
        if segment.is_empty() {
            // The root of an absolute path, or one trailing separator.
            return (i == 0 && !tilde) || (i == last && i > 0);
        }
        if *segment == "." || *segment == ".." {
            return false;
        }
        if !windows {
            return true;
        }
        let drive = i == 0
            && !tilde
            && segment.len() == 2
            && segment.as_bytes()[0].is_ascii_alphabetic()
            && segment.as_bytes()[1] == b':';
        (drive || !segment.contains(':')) && !segment.ends_with(['.', ' '])
    })
}

/// What [`walk_below_home`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HomeWalk {
    /// Every folder below home is a plain folder, and the config is a plain file or is
    /// not written yet.
    Plain,
    /// A folder on the way does not exist.
    Missing,
    /// A folder on the way, or the config, is a link (or a Windows reparse point).
    Link,
    /// Not below home by its text, something that is not a folder where one should be,
    /// or a folder Toolport cannot look at.
    NotPlain,
}

/// Walk a T3 Code-named config path from `home` down, one name at a time, without
/// following anything: each name is looked at with `symlink_metadata`, and the walk stops
/// at the first link. Nothing that follows a link (canonicalize, metadata, exists, read)
/// may run on such a path until this returns [`HomeWalk::Plain`]. Home itself may be a
/// link (`/home` to `/var/home`); only names below it are checked.
pub(super) fn walk_below_home(config: &Path, home: &Path) -> HomeWalk {
    if !lexically_inside(config, home) {
        return HomeWalk::NotPlain;
    }
    let below: Vec<_> = config
        .components()
        .skip(home.components().count())
        .collect();
    let mut at = home.to_path_buf();
    for (i, name) in below.iter().enumerate() {
        let is_config = i + 1 == below.len();
        at.push(name);
        match std::fs::symlink_metadata(&at) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return if is_config {
                    HomeWalk::Plain
                } else {
                    HomeWalk::Missing
                };
            }
            Err(_) => return HomeWalk::NotPlain,
            Ok(meta) if is_link(&meta) => return HomeWalk::Link,
            Ok(meta) if is_config && !meta.is_file() => return HomeWalk::NotPlain,
            Ok(meta) if !is_config && !meta.is_dir() => return HomeWalk::NotPlain,
            Ok(_) => {}
        }
    }
    HomeWalk::Plain
}

/// A link, or on Windows any reparse point (a junction or mount point too).
fn is_link(meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    meta.file_type().is_symlink()
}

/// The config an instance uses, if Toolport may look at it: its own path only once
/// [`walk_below_home`] has passed, else its client's default (Toolport's own path, not
/// one T3 Code named). Every use of an instance's path goes through here.
fn checked_path(instance: &Instance) -> Option<PathBuf> {
    match &instance.config_path {
        Some(path) => (walk_below_home(path, &home()?) == HomeWalk::Plain).then(|| path.clone()),
        None => default_config_path(instance.provider),
    }
}

/// An instance's own path with its folder resolved, once the walk has passed.
fn checked_canonical(instance: &Instance) -> Option<PathBuf> {
    let path = instance.config_path.as_ref()?;
    checked_path(instance)?;
    let folder = std::fs::canonicalize(path.parent()?).ok()?;
    Some(folder.join(path.file_name()?))
}

/// A cheap token that changes when T3 Code's settings change, for the Clients page to
/// notice an edit while it is open. `None` when there is no settings file.
pub fn revision() -> Option<String> {
    let path = settings_path()?;
    // A link to nothing still counts, so the page asks why it can't be read. Neither
    // call reads the file.
    let meta = std::fs::metadata(&path)
        .or_else(|_| std::fs::symlink_metadata(&path))
        .ok()?;
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
            Reading::Instances(with_defaults(BTreeMap::new()), Vec::new(), Vec::new())
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
    // without being zeroed. A file that grew while it was read is skipped this time; its
    // new size makes the next scan read it again.
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
    /// The ACP Registry agent an `acpRegistry` instance runs.
    #[serde(rename = "agentId", default)]
    agent_id: Option<String>,
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

/// What can still be shown of an instance that does not read as [`RawInstance`]: each
/// field is kept undecoded and read only when it is plain text (or a flag).
#[derive(Deserialize, Default)]
struct LooseInstance<'a> {
    #[serde(borrow, default)]
    driver: Option<&'a RawValue>,
    #[serde(rename = "displayName", borrow, default)]
    display_name: Option<&'a RawValue>,
    #[serde(borrow, default)]
    enabled: Option<&'a RawValue>,
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
    let mut others = Vec::new();
    let mut outside = Vec::new();
    for (id, raw) in settings.provider_instances.unwrap_or_default() {
        // One odd instance must not hide the rest. T3 Code keeps unknown-driver configs
        // verbatim, so shapes Toolport does not know are expected.
        // An instance Toolport cannot use is listed on the card with why, and still stops
        // T3 Code's default for that id from standing in for it.
        let instance = match RawInstance::of(raw) {
            Some(raw) => match Provider::of(raw.driver.trim()) {
                Some(provider) => {
                    let mut named = None;
                    match instance_of(&id, provider, raw, home, &mut named) {
                        Ok(instance) => Some(instance),
                        Err(other) => {
                            if let Some(config) =
                                named.filter(|_| other.gap == T3CodeGap::OutsideHome)
                            {
                                outside.push(Outside {
                                    index: others.len(),
                                    client_type: provider.client_type(),
                                    config,
                                });
                            }
                            others.push(other);
                            None
                        }
                    }
                }
                None => {
                    others.push(other_of(&id, raw));
                    None
                }
            },
            None => {
                others.push(malformed(&id, raw));
                None
            }
        };
        found.insert(id.clone(), instance);
    }
    Reading::Instances(with_defaults(found), others, outside)
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

/// An instance entry Toolport cannot read, for the card: its id, and its display name,
/// driver and flag only when each is plain text. Nothing else of it is decoded.
fn malformed(id: &str, raw: &RawValue) -> T3CodeOther {
    let loose = if raw.get().trim_start().starts_with('{') {
        serde_json::from_str::<LooseInstance>(raw.get()).unwrap_or_default()
    } else {
        LooseInstance::default()
    };
    let text = |value: Option<&RawValue>| {
        value.and_then(|value| serde_json::from_str::<String>(value.get()).ok())
    };
    let driver = text(loose.driver);
    let driver = driver.as_deref().map(str::trim);
    let (provider_name, default_on) = match driver {
        Some(driver) => match (Provider::of(driver), other_driver(driver)) {
            (Some(provider), _) => (
                provider.default_name().to_string(),
                matches!(provider, Provider::ClaudeCode | Provider::Codex),
            ),
            (None, Some(known)) => (known.0.to_string(), known.2),
            (None, None) if plain_identifier(driver) => (driver.to_string(), false),
            // Unknown drivers are off unless the file says otherwise, as in `other_of`.
            (None, None) => (String::new(), false),
        },
        // Nothing says what it is: shown as it is, not marked off.
        None => (String::new(), true),
    };
    let enabled = loose
        .enabled
        .and_then(|value| serde_json::from_str::<bool>(value.get()).ok());
    let instance_id = display_name_id(id);
    let display_name = text(loose.display_name)
        .as_deref()
        .map(display_name)
        .filter(|name| !name.is_empty())
        .or_else(|| (!provider_name.is_empty()).then(|| provider_name.clone()))
        .unwrap_or_else(|| instance_id.clone());
    T3CodeOther {
        instance_id,
        provider_name,
        display_name,
        enabled: enabled.unwrap_or(default_on),
        gap: T3CodeGap::Malformed,
    }
}

/// An instance of a T3 Code driver Toolport cannot connect. A driver T3 Code adds later
/// shows under its own id until Toolport learns its name; one whose id is not a plain
/// identifier shows by its display name or instance id alone.
fn other_of(id: &str, raw: RawInstance) -> T3CodeOther {
    let driver = raw.driver.trim();
    let config = match raw.config {
        Some(config) => serde_json::from_str::<RawConfig>(config.get()).unwrap_or_default(),
        None => RawConfig::default(),
    };
    let (provider_name, gap, default_on) = match other_driver(driver) {
        Some(known) => (known.0.to_string(), known.1, known.2),
        // A driver id is code, not prose: only a plain identifier is shown. Unknown
        // drivers are off unless the file says otherwise.
        None if plain_identifier(driver) => (driver.to_string(), T3CodeGap::Unsupported, false),
        // Still listed, by its display name or id, without the driver.
        None => (String::new(), T3CodeGap::Unsupported, false),
    };
    let provider_name = match config.agent_id.as_deref().map(str::trim) {
        Some(agent) if driver == "acpRegistry" && plain_identifier(agent) => {
            format!("{provider_name}: {agent}")
        }
        _ => provider_name,
    };
    let instance_id = display_name_id(id);
    let display_name = raw
        .display_name
        .as_deref()
        .map(display_name)
        .filter(|name| !name.is_empty())
        .or_else(|| (!provider_name.is_empty()).then(|| provider_name.clone()))
        .unwrap_or_else(|| instance_id.clone());
    T3CodeOther {
        instance_id,
        provider_name,
        display_name,
        enabled: resolve_enabled(raw.enabled, config.enabled, default_on),
        gap,
    }
}

/// A short ASCII identifier such as a driver or ACP agent id.
fn plain_identifier(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// An instance id made safe to show and to key UI rows by.
fn display_name_id(id: &str) -> String {
    display_name(id)
}

/// An instance's `config`, when it is an object of the expected shape.
fn config_of(config: &RawValue) -> Option<RawConfig> {
    if !config.get().trim_start().starts_with('{') {
        return None;
    }
    serde_json::from_str(config.get()).ok()
}

/// The instance, or why Toolport cannot tell which config it reads. For a home that is
/// not plainly inside `home`, `named` gets the config it names, as text only.
fn instance_of(
    id: &str,
    provider: Provider,
    raw: RawInstance,
    home: &Path,
    named: &mut Option<PathBuf>,
) -> Result<Instance, T3CodeOther> {
    let display_name = raw
        .display_name
        .as_deref()
        .map(display_name)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| provider.default_name().into());
    let default_on = matches!(provider, Provider::ClaudeCode | Provider::Codex);
    let gap = |gap, enabled| T3CodeOther {
        instance_id: display_name_id(id),
        provider_name: provider.default_name().into(),
        display_name: display_name.clone(),
        enabled,
        gap,
    };
    // A config Toolport cannot read says nothing reliable about the home: no guess.
    let config = match raw.config {
        Some(config) => match config_of(config) {
            Some(config) => config,
            None => {
                return Err(gap(
                    T3CodeGap::Malformed,
                    resolve_enabled(raw.enabled, None, default_on),
                ))
            }
        },
        None => RawConfig::default(),
    };
    let enabled = resolve_enabled(raw.enabled, config.enabled, default_on);
    // Set when the home is not plain text. It is still worked out, by text, so a location
    // added by hand can be matched with it; the instance is listed as outside home.
    let not_plain = std::cell::Cell::new(false);
    let folder = |value: Result<Option<String>, ()>| -> Result<Option<PathBuf>, T3CodeOther> {
        let value = value.map_err(|()| gap(T3CodeGap::HiddenHome, enabled))?;
        match value
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            None => Ok(None),
            Some(value) if !plain_path_text(value, cfg!(windows)) => {
                not_plain.set(true);
                Ok(Some(expand_text(value, home)))
            }
            Some(value) => match expand(value, home) {
                Some(path) if rooted(&path) => Ok(Some(path)),
                // Relative to T3 Code's working directory, which Toolport cannot know.
                _ => Err(gap(T3CodeGap::RelativeHome, enabled)),
            },
        }
    };
    // Claude's and Codex's home: `homePath`, else the instance environment's variable,
    // which T3 Code passes to the provider as it is, so a `~` there is not expanded
    // (ClaudeHome.ts `resolveClaudeHomePath`, CodexAdapterV2's `CODEX_HOME`).
    let home_or = |variable: &str| -> Result<Option<PathBuf>, T3CodeOther> {
        if let Some(path) = folder(Ok(config.home_path.clone()))? {
            return Ok(Some(path));
        }
        let value = environment_value(&raw.environment, variable)
            .map_err(|()| gap(T3CodeGap::HiddenHome, enabled))?;
        match value.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
            None => Ok(None),
            Some(value) if !plain_path_text(value, cfg!(windows)) => {
                not_plain.set(true);
                Ok(Some(PathBuf::from(value)))
            }
            Some(value) if rooted(Path::new(value)) => Ok(Some(PathBuf::from(value))),
            Some(_) => Err(gap(T3CodeGap::RelativeHome, enabled)),
        }
    };
    let config_path = match provider {
        Provider::ClaudeCode => {
            home_or("CLAUDE_CONFIG_DIR")?.map(|dir| claude_code_config_path(&dir))
        }
        // A shadow home links the shared home's config.toml (T3 Code's
        // CodexHomeLayout), so the instance reads the shared home's MCP servers.
        Provider::Codex => home_or("CODEX_HOME")?.map(|dir| codex_config_path(&dir)),
        Provider::OpenCode => folder(environment_value(&raw.environment, "XDG_CONFIG_HOME"))?
            .map(|dir| dir.join("opencode").join("opencode.json")),
        // T3 Code's Grok driver merges the instance environment over its own, and Grok
        // reads `GROK_HOME/config.toml`, `~/.grok` by default.
        Provider::Grok => folder(environment_value(&raw.environment, "GROK_HOME"))?
            .map(|dir| dir.join("config.toml")),
        // Pi reads its MCP servers from `PI_CODING_AGENT_DIR/mcp.json`, `~/.pi/agent` by
        // default.
        Provider::Pi => folder(environment_value(&raw.environment, "PI_CODING_AGENT_DIR"))?
            .map(|dir| dir.join("mcp.json")),
        // T3 Code's Cursor and Kiro use the user's own default MCP config.
        Provider::Cursor | Provider::Kiro => None,
        // T3 Code gives each Antigravity instance a private profile named by the SHA-256
        // of its id (`antigravityAuthSupport.ts`), and `agy acp --gemini-dir` reads its
        // MCP servers from `config/mcp_config.json` there. That file is the only one
        // Toolport touches: T3 Code manages the rest of the profile, such as its
        // `settings.json` and the `config/skills` link.
        Provider::Antigravity => antigravity_config_path(home, id),
    };
    // Checked by its text alone, before anything touches the file system: a network or
    // device path, `..`, or a folder outside the user's home is never opened, resolved or
    // listed. The card still lists the instance, by its name only.
    if not_plain.get()
        || config_path
            .as_deref()
            .is_some_and(|path| !lexically_inside(path, home))
    {
        *named = config_path;
        return Err(gap(T3CodeGap::OutsideHome, enabled));
    }
    Ok(Instance {
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
        Reading::Instances(instances, _, _) => instances.clone(),
        _ => Vec::new(),
    }
}

/// T3 Code instances Toolport cannot connect, for the T3 Code card. Empty when T3 Code is
/// absent or its settings are unreadable.
pub fn other_instances() -> Vec<T3CodeOther> {
    match &*current() {
        Reading::Instances(instances, others, outside) => {
            let covered = outside_covered(outside);
            let mut others: Vec<T3CodeOther> = others
                .iter()
                .enumerate()
                .filter(|(index, _)| !covered.contains(index))
                .map(|(_, other)| other.clone())
                .collect();
            others.extend(home_gaps(instances));
            others.sort_by(|a, b| a.instance_id.cmp(&b.instance_id));
            others
        }
        Reading::Unreadable => {
            let path = settings_path().unwrap_or_default();
            let shown = home()
                .and_then(|home| {
                    path.strip_prefix(&home).ok().map(|rest| {
                        let parts: Vec<_> = rest
                            .components()
                            .map(|part| part.as_os_str().to_string_lossy().into_owned())
                            .collect();
                        format!("~/{}", parts.join("/"))
                    })
                })
                .unwrap_or_else(|| path.display().to_string());
            vec![T3CodeOther {
                instance_id: String::new(),
                provider_name: String::new(),
                display_name: shown,
                enabled: true,
                gap: T3CodeGap::Unreadable,
            }]
        }
        Reading::Absent => Vec::new(),
    }
}

/// The outside homes a saved location (or the client's own row) covers, compared by
/// their text only, so the folder T3 Code names is never opened. The location's config
/// is the file its client type names in the folder added, which is the config T3 Code's
/// instance reads.
fn outside_covered(outside: &[Outside]) -> Vec<usize> {
    if outside.is_empty() {
        return Vec::new();
    }
    let saved = locations::saved().unwrap_or_default();
    outside
        .iter()
        .filter(|home| {
            client_config_path_for_type(home.client_type)
                .is_some_and(|main| same_text(&main, &home.config))
                || saved.iter().any(|row| {
                    row.client_type == home.client_type && same_text(&row.config_path, &home.config)
                })
        })
        .map(|home| home.index)
        .collect()
}

/// Instances with a home of their own that no Clients row covers, with why: the folder
/// does not exist yet, or it or its config file is a link or not a plain folder or file
/// ([`walk_below_home`]). A home the page lists anyway (a client's main config, a profile, a launcher or a saved
/// location) is not one. Only homes that passed [`lexically_inside`] get here. A home that
/// passed the walk is compared with the listed rows by where it resolves to; one that did
/// not is compared by its text only, so nothing it names is followed.
fn home_gaps(instances: &[Instance]) -> Vec<T3CodeOther> {
    let Some(home) = home() else {
        return Vec::new();
    };
    let real_home = std::fs::canonicalize(&home).ok();
    // Listing the locations scans launchers; only done when a home needs it.
    let mut listed: Option<Vec<locations::ClientLocation>> = None;
    instances
        .iter()
        .filter_map(|instance| {
            let path = instance.config_path.as_deref()?;
            let walk = walk_below_home(path, &home);
            let gap = match walk {
                // OpenCode reads `opencode.jsonc` when only that exists: a link there
                // makes no row either.
                HomeWalk::Plain if !opencode_jsonc_is_plain(path) => T3CodeGap::LinkedHome,
                HomeWalk::Plain => return None,
                HomeWalk::Missing => T3CodeGap::MissingHome,
                HomeWalk::Link | HomeWalk::NotPlain => T3CodeGap::LinkedHome,
            };
            // The same text below the resolved home, for rows listed by resolved path.
            let below: PathBuf = path.components().skip(home.components().count()).collect();
            let resolved = real_home.as_ref().map(|root| root.join(&below));
            // Only a home that passed the walk may be resolved to compare it.
            let plain = walk == HomeWalk::Plain;
            // OpenCode's row names `opencode.jsonc` when only that exists, as the label
            // ([`annotate_with`]) reads it.
            let file = (plain && instance.provider == Provider::OpenCode)
                .then(|| opencode_file(path.to_path_buf(), true))
                .filter(|file| file != path);
            let names = |other: &Path| {
                same_text(other, path)
                    || resolved.as_deref().is_some_and(|r| same_text(other, r))
                    || (plain && locations::same_path(other, path))
                    || file
                        .as_deref()
                        .is_some_and(|file| locations::same_path(other, file))
            };
            let client_type = instance.provider.client_type();
            if client_config_path_for_type(client_type).is_some_and(|main| names(&main))
                || listed
                    .get_or_insert_with(locations::all)
                    .iter()
                    .any(|row| row.client_type == client_type && names(&row.config_path))
            {
                return None;
            }
            Some(T3CodeOther {
                instance_id: display_name_id(&instance.id),
                provider_name: instance.provider.default_name().into(),
                display_name: instance.display_name.clone(),
                enabled: instance.enabled,
                gap,
            })
        })
        .collect()
}

/// The `opencode.jsonc` beside an OpenCode `opencode.json`, which a location added by
/// hand may name instead when only that file exists.
fn jsonc_twin(config: &Path) -> Option<PathBuf> {
    config
        .file_name()
        .is_some_and(|name| name == "opencode.json")
        .then(|| config.with_extension("jsonc"))
}

/// For an OpenCode `opencode.json` path, whether the `opencode.jsonc` beside it is absent
/// or a plain file reached without a link, since OpenCode reads that one when only it
/// exists. Any other path passes.
pub(super) fn opencode_jsonc_is_plain(config: &Path) -> bool {
    let Some(twin) = jsonc_twin(config) else {
        return true;
    };
    home().is_some_and(|home| {
        walk_below_home(&twin, &home) == HomeWalk::Plain
            || std::fs::canonicalize(&home)
                .ok()
                .is_some_and(|root| walk_below_home(&twin, &root) == HomeWalk::Plain)
    })
}

/// The `mcp_config.json` in the private profile T3 Code gives the Antigravity instance
/// `id`: `<T3 Code data>/userdata/providers/antigravity/<sha256 of id>/config/`.
pub(super) fn antigravity_config_path(home: &Path, id: &str) -> Option<PathBuf> {
    let path = data_dir_for(home)?
        .join("userdata")
        .join("providers")
        .join("antigravity")
        .join(crate::registry::sha256_hex(id))
        .join("config")
        .join("mcp_config.json");
    Some(path)
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
        // Never the user's own Antigravity config: an instance always has its profile.
        Provider::Antigravity => None,
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
            matches!(
                kind,
                "claude-code" | "codex" | "opencode" | "grok" | "pi" | "antigravity"
            ) && hash.len() == 16
                && hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
}

/// A saved T3 Code home's config is an ordinary file in an ordinary folder inside the
/// user's own home, or not written yet, with no link anywhere below home
/// ([`walk_below_home`]). A settings file that names `/etc` or another user's folder
/// therefore never becomes a row Toolport could write to. A folder outside home can
/// still be added by hand as a manual location.
pub(super) fn path_is_safe(config: &Path) -> bool {
    home()
        .and_then(|home| std::fs::canonicalize(home).ok())
        .is_some_and(|root| walk_below_home(config, &root) == HomeWalk::Plain)
}

/// Locations for T3 Code's provider homes (Claude Code, Codex, OpenCode, Grok and Pi)
/// that are not a client's main config and that `known` does not already list. Homes
/// whose folder does not exist yet are left out until it does; [`other_instances`] lists
/// them, and the other homes left out, on the card.
pub(super) fn automatic(known: &[locations::ClientLocation]) -> Vec<locations::ClientLocation> {
    let mut rows: Vec<locations::ClientLocation> = Vec::new();
    // Cursor and Kiro instances use the user's own config, which has its own row.
    let homes: Vec<Instance> = instances()
        .into_iter()
        .filter(|instance| instance.config_path.is_some())
        .collect();
    if homes.is_empty() {
        return rows;
    }
    // Saved locations too, so a folder added by hand and the same folder named by T3
    // Code are one row however each is spelled.
    let saved = locations::saved().unwrap_or_default();
    for instance in homes {
        // The walk first: nothing below resolves the path until it has passed.
        let Some(config_path) = checked_canonical(&instance) else {
            continue;
        };
        let client_type = instance.provider.client_type();
        let main = client_config_path_for_type(client_type);
        if main
            .as_ref()
            .is_some_and(|main| locations::same_path(main, &config_path))
            || known.iter().chain(&saved).chain(rows.iter()).any(|row| {
                row_has_path(row, &config_path)
                    || jsonc_twin(&config_path).is_some_and(|twin| row_has_path(row, &twin))
            })
            || !path_is_safe(&config_path)
            || !opencode_jsonc_is_plain(&config_path)
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

/// Whether a listed row is for `config` (a checked, resolved path). A T3 Code row that
/// no longer passes the walk (its folder became a link, or is gone) is compared by its
/// text only, so nothing it names is followed.
fn row_has_path(row: &locations::ClientLocation, config: &Path) -> bool {
    if locations::is_t3_home(row) && !path_is_safe(&row.config_path) {
        return same_text(&row.config_path, config);
    }
    locations::same_path(&row.config_path, config)
}

/// The config path Toolport's own row for a client type uses, as its definition resolves
/// it (Toolport's `GROK_HOME`, `XDG_CONFIG_HOME`, `OPENCODE_CONFIG_DIR` and
/// `PI_CODING_AGENT_DIR` included). OpenCode's is the `opencode.json` spelling, as T3
/// Code's homes and hand-added locations name it.
fn client_config_path_for_type(client_type: &str) -> Option<PathBuf> {
    match client_type {
        "claude-code" => client_config_path("claude-code"),
        "codex" => codex_path(),
        "grok" => grok_path(),
        "opencode" => opencode_path(),
        "pi" => pi_path(),
        other => client_config_path(other),
    }
}

/// Whether T3 Code still lists the home behind a saved T3 location. A home whose walk
/// passes is compared on disk; any other (gone, or now a link) only by its text, so
/// nothing is followed.
pub(super) fn lists(config: &Path) -> bool {
    let Some(home) = home() else {
        return false;
    };
    let real_home = std::fs::canonicalize(&home).ok();
    instances().iter().any(|instance| {
        let Some(path) = instance.config_path.as_ref() else {
            return false;
        };
        if let Some(checked) = checked_canonical(instance) {
            if path_is_safe(config) {
                return locations::same_path(&checked, config);
            }
            return same_text(&checked, config);
        }
        let below: PathBuf = path.components().skip(home.components().count()).collect();
        same_text(path, config)
            || real_home
                .as_ref()
                .is_some_and(|root| same_text(&root.join(below), config))
    })
}

/// Label each client row with the T3 Code instances that use its config.
pub(super) fn annotate(clients: &mut [DetectedClient]) {
    let reading = current();
    let Reading::Instances(instances, others, outside) = &*reading else {
        return;
    };
    annotate_with(clients, instances);
    annotate_outside(clients, others, outside);
}

/// Label the rows that cover a home outside the user's home: the card leaves those
/// instances out ([`outside_covered`]), so their row says T3 Code uses it and counts
/// them as connected or not. Compared by text only, as the card does, so the folder is
/// never opened.
fn annotate_outside(clients: &mut [DetectedClient], others: &[T3CodeOther], outside: &[Outside]) {
    for home in outside {
        let Some(other) = others.get(home.index) else {
            continue;
        };
        let Some(client) = clients.iter_mut().find(|client| {
            (client.id == home.client_type
                || client.id.starts_with(&format!("{}@", home.client_type)))
                && !client.config_path.is_empty()
                && same_text(Path::new(&client.config_path), &home.config)
        }) else {
            continue;
        };
        if client
            .t3_code
            .iter()
            .any(|known| known.instance_id == other.instance_id)
        {
            continue;
        }
        client.t3_code.push(T3CodeUse {
            instance_id: other.instance_id.clone(),
            display_name: other.display_name.clone(),
            enabled: other.enabled,
        });
    }
}

/// The file OpenCode reads in a folder, chosen as its row's is
/// ([`resolve_opencode_config_path`]): `opencode.jsonc` when only that exists. For a path
/// T3 Code names, called only once the walk has passed for it, and the `.jsonc` is walked
/// too, so a link there is never followed.
fn opencode_file(path: PathBuf, named: bool) -> PathBuf {
    if path.file_name().is_none_or(|name| name != "opencode.json") {
        return path;
    }
    if named
        && !home().is_some_and(|home| {
            walk_below_home(&path.with_extension("jsonc"), &home) == HomeWalk::Plain
        })
    {
        return path;
    }
    resolve_opencode_config_path(path.clone()).unwrap_or(path)
}

pub(super) fn annotate_with(clients: &mut [DetectedClient], instances: &[Instance]) {
    for instance in instances {
        // A home that fails the walk (a link, gone, not plain) labels nothing.
        let Some(path) = checked_path(instance) else {
            continue;
        };
        let mut canonical = checked_canonical(instance);
        let client_type = instance.provider.client_type();
        // OpenCode's row shows `opencode.jsonc` when only that exists.
        let path = if instance.provider == Provider::OpenCode {
            let file = opencode_file(path, instance.config_path.is_some());
            if let (Some(canonical), Some(name)) = (canonical.as_mut(), file.file_name()) {
                canonical.set_file_name(name);
            }
            file
        } else {
            path
        };
        let Some(client) = clients.iter_mut().find(|client| {
            (client.id == client_type || client.id.starts_with(&format!("{client_type}@")))
                && !client.config_path.is_empty()
                && if is_location_id(&client.id) {
                    // A T3 Code row's own path is compared by its text: it may have
                    // become a link since it was saved.
                    canonical.as_ref().is_some_and(|canonical| {
                        same_text(Path::new(&client.config_path), canonical)
                    })
                } else {
                    locations::same_path(Path::new(&client.config_path), &path)
                }
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
#[path = "t3code_antigravity_tests.rs"]
mod antigravity_tests;
#[cfg(test)]
#[path = "t3code_homes_tests.rs"]
mod homes_tests;
#[cfg(test)]
#[path = "t3code_tests.rs"]
mod tests;
