//! Machine-local adapter locations. Deliberately outside Registry and personal sync.
use super::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ClientLocation {
    pub id: String,
    #[serde(default)]
    pub manual: bool,
    pub client_type: String,
    pub name: String,
    pub config_path: PathBuf,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientLocationType {
    pub id: String,
    pub name: String,
    pub config_file: String,
}

/// The folder is the client's config folder, e.g. CODEX_HOME, not a project root.
pub fn client_location_types() -> Vec<ClientLocationType> {
    defs()
        .into_iter()
        .filter(|def| !def.uses_connectors)
        .filter_map(|def| {
            let file = (def.path)()?.file_name()?.to_str()?.to_string();
            Some(ClientLocationType {
                id: def.id.into(),
                name: def.name.into(),
                config_file: file,
            })
        })
        .collect()
}

fn location_name(kind: &str, folder: &str) -> String {
    let folder: String = folder
        .trim_start_matches('.')
        .chars()
        .map(|c| {
            if c.is_alphanumeric()
                || matches!(c, ' ' | '.' | '_' | '-' | '+' | '(' | ')' | '[' | ']')
            {
                c
            } else {
                '-'
            }
        })
        .take(60)
        .collect();
    let mut label = format!("{kind} ({folder})");
    // Bound trusted folder names before they reach the strict telemetry label filter.
    while label.len() > 180 {
        label.pop();
    }
    if !label.ends_with(')') {
        label.push(')');
    }
    label
}

/// The label of a T3 Code home: its folder below home, such as
/// `Claude Code (t3/commandcode/claude)`. T3 Code's own instance name is shown beside it.
pub(super) fn t3_location_name(client_type: &str, config: &Path) -> String {
    let kind = defs()
        .into_iter()
        .find(|def| def.id == client_type)
        .map(|def| def.name)
        .unwrap_or(client_type);
    let folder = config.parent().unwrap_or(config);
    let shown = home()
        .and_then(|root| {
            let root = std::fs::canonicalize(&root).unwrap_or(root);
            let rest = folder.strip_prefix(&root).ok()?;
            let parts: Vec<String> = rest
                .components()
                .map(|part| short_hash(&part.as_os_str().to_string_lossy()))
                .collect();
            Some(format!("~/{}", parts.join("/")))
        })
        .unwrap_or_else(|| folder.display().to_string());
    let shown: String = shown
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || " ._-+()[]/~".contains(c) {
                c
            } else {
                '-'
            }
        })
        .take(80)
        .collect();
    format!("{kind} ({shown})")
}

/// A folder named by a SHA-256 hash, such as T3 Code's Antigravity profiles, by its first
/// eight characters; any other name as it is.
fn short_hash(name: &str) -> String {
    if name.len() == 64 && name.bytes().all(|b| b.is_ascii_hexdigit()) {
        name[..8].to_string()
    } else {
        name.to_string()
    }
}

/// A home found only because T3 Code lists it. Listed again on each refresh while T3 Code
/// does, and saved only once Toolport changes its config.
pub(super) fn is_t3_home(row: &ClientLocation) -> bool {
    !row.manual && super::t3code::is_location_id(&row.id)
}

/// A saved T3 Code home whose folder no longer exists, after T3 Code or the instance was
/// removed. It reads as not installed rather than as an error. Writes still fail
/// [`validate`], so nothing recreates the folder.
pub(super) fn t3_home_folder_is_gone(row: &ClientLocation) -> bool {
    is_t3_home(row)
        && home()
            .and_then(|home| std::fs::canonicalize(home).ok())
            .is_some_and(|root| {
                super::t3code::walk_below_home(&row.config_path, &root)
                    == super::t3code::HomeWalk::Missing
            })
}

/// A saved T3 Code home that T3 Code no longer lists. It stays listed so it can be
/// disconnected, then forgotten.
fn t3_home_is_gone(row: &ClientLocation) -> bool {
    is_t3_home(row) && !super::t3code::lists(&row.config_path)
}

pub fn remove_client_location(id: &str) -> Result<(), String> {
    let _auth = crate::registry_controller::acquire_auth_lock(&format!("client-config:{id}"))?;
    let store = store_path()?;
    let _lock = crate::registry::lock_at(&store)?;
    let mut rows = saved()?;
    if !rows.iter().any(|row| row.id == id && forgettable(row)) {
        return Err(
            "Only manually added or missing automatic client locations can be removed".into(),
        );
    }
    // Forgetting is separate from disconnect and never edits the client's config.
    let client = read_client(&find_def(id).ok_or("Unknown client location")?);
    if client.error.is_some() || client.gateway_installed {
        return Err("Disconnect this client location before removing it".into());
    }
    if let Some(pending) = recovery::list()?
        .into_iter()
        .find(|row| row.client_id == id)
    {
        return Err(crate::i18n::trf("{client} still has saved recovery values. Choose Retry to restore them, or Discard these saved values to remove their copies, before removing this location.", &[("client", &pending.client_name)]));
    }
    rows.retain(|row| row.id != id);
    crate::registry::atomic_write(
        &store,
        &serde_json::to_string_pretty(&rows).map_err(|e| e.to_string())?,
    )?;
    Ok(())
}

fn store_path() -> Result<PathBuf, String> {
    Ok(crate::registry::conduit_dir()
        .ok_or("Could not resolve local data folder")?
        .join("client-locations.json"))
}

pub(super) fn saved() -> Result<Vec<ClientLocation>, String> {
    match std::fs::read(store_path()?) {
        Ok(bytes) => {
            let values: Vec<serde_json::Value> = serde_json::from_slice(&bytes)
                .map_err(|_| "Could not read saved client locations".to_string())?;
            let mut rows = Vec::new();
            for value in values {
                let Ok(mut row) = serde_json::from_value::<ClientLocation>(value) else {
                    eprintln!("toolport: skipping saved client location with invalid fields");
                    continue;
                };
                if !valid_id(&row.id)
                    || !row.config_path.is_absolute()
                    || row.id.split_once('@').map(|(kind, _)| kind)
                        != Some(row.client_type.as_str())
                    || !defs()
                        .iter()
                        .any(|def| def.id == row.client_type && !def.uses_connectors)
                {
                    eprintln!("toolport: skipping saved client location with invalid identity, type or path");
                    continue;
                }
                // Earlier versions did not store the manual flag for path-derived IDs.
                row.manual |= row.id.split_once('@').is_some_and(|(_, key)| {
                    key.len() == 16 && key.bytes().all(|byte| byte.is_ascii_hexdigit())
                });
                let kind = defs()
                    .into_iter()
                    .find(|def| def.id == row.client_type)
                    .unwrap();
                row.name = if super::t3code::is_location_id(&row.id) {
                    t3_location_name(&row.client_type, &row.config_path)
                } else {
                    location_name(kind.name, &folder_label(&row.config_path))
                };
                rows.push(row);
            }
            Ok(rows)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(_) => Err("Could not read saved client locations".into()),
    }
}

pub(super) fn store_error() -> Option<String> {
    saved().err()
}

/// Saving a v1 Claude profile must not narrow its existing gateway ownership.
pub(super) fn legacy_gateway_ownership(id: &str) -> bool {
    if !id.contains('@') {
        return true;
    }
    let Some(row) = lookup(id).filter(|row| row.client_type == "claude-code") else {
        return false;
    };
    let Some(root) = home() else {
        return false;
    };
    if row
        .config_path
        .file_name()
        .is_none_or(|name| name != ".claude.json")
    {
        return false;
    }
    if row.config_path.parent().is_some_and(|folder| {
        folder
            .parent()
            .is_some_and(|parent| same_path(parent, &root))
            && folder
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(legacy_claude_profile_name)
    }) {
        return true;
    }
    // Manual registration canonicalizes folders, so a linked v1 profile is
    // stored at its target. Match the home profile's config, not its saved ID.
    std::fs::read_dir(&root).ok().is_some_and(|entries| {
        entries.filter_map(Result::ok).any(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(legacy_claude_profile_name)
                && same_path(&entry.path().join(".claude.json"), &row.config_path)
        })
    })
}

pub(super) fn store_display_path() -> String {
    store_path()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub(super) fn valid_id(id: &str) -> bool {
    id.split_once('@')
        .is_some_and(|(kind, key)| !kind.is_empty() && !key.is_empty() && key != "." && key != "..")
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.@%".contains(&c))
}

pub(crate) fn folder_key(folder: &str) -> String {
    folder
        .trim_start_matches('.')
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-_".contains(&byte) {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

pub(super) fn claude_id_for_backup_path(path: &str) -> Option<String> {
    // Recovery records may have been written on another OS. This is a filename
    // check, not a client lookup or a read of the missing config.
    let mut parts = path.rsplit(['/', '\\']);
    if parts.next()? != ".claude.json" {
        return None;
    }
    let folder = parts.next()?;
    claude_profile_name(folder).then(|| format!("claude-code@{}", folder_key(folder)))
}

/// The two ID formats produced by add_client_location and automatic discovery.
pub(super) fn generated_id_type(id: &str) -> Option<&str> {
    let (kind, key) = id.split_once('@')?;
    if key.len() == 16
        && key
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Some(kind);
    }
    if super::t3code::is_location_id(id) {
        return Some(kind);
    }
    if kind == "codex" {
        return codex_relative(key).map(|_| kind);
    }
    if kind != "claude-code" {
        return None;
    }
    let label = decode_key(key)?;
    if label.contains(['/', '\\']) || label.chars().any(char::is_control) {
        return None;
    }
    let folder = format!(".{label}");
    (claude_profile_name(&folder) && folder_key(&folder) == key).then_some(kind)
}

/// The text behind a `%XX`-encoded location key.
fn decode_key(key: &str) -> Option<String> {
    let mut bytes = Vec::new();
    let mut chars = key.as_bytes().iter().copied();
    while let Some(byte) = chars.next() {
        bytes.push(if byte == b'%' {
            u8::from_str_radix(
                std::str::from_utf8(&[chars.next()?, chars.next()?]).ok()?,
                16,
            )
            .ok()?
        } else {
            byte
        });
    }
    String::from_utf8(bytes).ok()
}

/// The folder name a location is labelled with. A Codex home is usually a `.codex`
/// folder inside a folder named for the account, so that outer name is the label.
fn folder_label(config: &Path) -> String {
    let Some(folder) = config.parent() else {
        return String::new();
    };
    let name = folder.file_name().unwrap_or_default().to_string_lossy();
    if name == ".codex" && config.file_name().is_some_and(|file| file == "config.toml") {
        if let Some(outer) = folder.parent().and_then(|outer| outer.file_name()) {
            return outer.to_string_lossy().into_owned();
        }
    }
    name.into_owned()
}

/// The ID key for a Codex home below home, its home-relative path with `/` between
/// folders: `Codex-Work%2F.codex`, `.codex2`.
fn codex_key(relative: &str) -> String {
    relative
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-_.".contains(&byte) {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

/// The home-relative folder of an automatic Codex ID: a `.codex2`-style folder in home,
/// or a `.codex` (or `.codex2`-style) folder one level down, such as `Codex-Work/.codex`.
/// Any other key names a manual location or something that is not Toolport's.
fn codex_relative(key: &str) -> Option<String> {
    let relative = decode_key(key)?;
    let parts: Vec<&str> = relative.split('/').collect();
    let leaf = parts.last()?;
    (codex_key(&relative) == key
        && parts.len() <= 2
        && (codex_sibling_name(leaf) || (parts.len() == 2 && *leaf == ".codex"))
        && parts.iter().all(|part| {
            !part.is_empty()
                && *part != "."
                && *part != ".."
                && !part.contains('\\')
                && !part.chars().any(char::is_control)
        }))
    .then_some(relative)
}

/// [`codex_relative`] for a folder path below `home`.
fn codex_relative_of(home: &Path, folder: &Path) -> Option<String> {
    let parts = folder
        .strip_prefix(home)
        .ok()?
        .components()
        .map(|part| match part {
            std::path::Component::Normal(name) => name.to_str(),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    let relative = parts.join("/");
    codex_relative(&codex_key(&relative))
}

fn codex_folder(home: &Path, relative: &str) -> PathBuf {
    relative
        .split('/')
        .fold(home.to_path_buf(), |path, part| path.join(part))
}

/// `~/.codex2`, `~/.codex-work`: a Codex home beside the default `~/.codex`.
fn codex_sibling_name(name: &str) -> bool {
    profile_folder_name(name, ".codex")
}

/// The Codex client's own config, the default `~/.codex` or the `CODEX_HOME` Toolport
/// was started with. Never also an automatic location.
fn is_main_codex(config: &Path) -> bool {
    codex_path().is_some_and(|main| same_path(&main, config))
}

/// Automatic Codex homes stay ordinary `config.toml` files in ordinary folders at most
/// two levels below home, reached without links. Whether anything points at the folder
/// is checked by the caller.
pub(super) fn codex_automatic_path_is_safe(home: &Path, config: &Path) -> bool {
    let Some(folder) = config.parent() else {
        return false;
    };
    let Some(relative) = codex_relative_of(home, folder) else {
        return false;
    };
    if config.file_name().is_none_or(|name| name != "config.toml") || is_main_codex(config) {
        return false;
    }
    let mut level = home.to_path_buf();
    for part in relative.split('/') {
        level.push(part);
        if !std::fs::symlink_metadata(&level).is_ok_and(|meta| meta.is_dir()) {
            return false;
        }
    }
    if !std::fs::symlink_metadata(config).is_ok_and(|meta| meta.is_file()) {
        return false;
    }
    let (Ok(root), Ok(real)) = (std::fs::canonicalize(home), std::fs::canonicalize(folder)) else {
        return false;
    };
    real == codex_folder(&root, &relative)
}

/// A saved automatic Codex home whose folder was removed.
fn codex_automatic_path_is_missing(root: &Path, row: &ClientLocation) -> bool {
    let Some(folder) = row.config_path.parent() else {
        return false;
    };
    let Some(relative) = codex_relative_of(root, folder) else {
        return false;
    };
    if row
        .config_path
        .file_name()
        .is_none_or(|name| name != "config.toml")
    {
        return false;
    }
    match std::fs::symlink_metadata(folder) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // `Codex-Work/.codex` is only missing if `Codex-Work` is gone too, or is
            // still an ordinary folder in home. A link there could lead a write outside.
            let Some((outer, _)) = relative.split_once('/') else {
                return root.is_dir();
            };
            return match std::fs::symlink_metadata(root.join(outer)) {
                Err(error) => error.kind() == std::io::ErrorKind::NotFound && root.is_dir(),
                Ok(meta) if meta.is_dir() => std::fs::canonicalize(root)
                    .ok()
                    .zip(std::fs::canonicalize(root.join(outer)).ok())
                    .is_some_and(|(root, real)| real == root.join(outer)),
                Ok(_) => false,
            };
        }
        Err(_) => return false,
        Ok(meta) if !meta.is_dir() => return false,
        Ok(_) => {}
    }
    std::fs::symlink_metadata(&row.config_path)
        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        && codex_automatic_path_is_safe_folder(root, folder)
}

/// The folder half of [`codex_automatic_path_is_safe`], for a home whose config is gone.
fn codex_automatic_path_is_safe_folder(root: &Path, folder: &Path) -> bool {
    let Some(relative) = codex_relative_of(root, folder) else {
        return false;
    };
    std::fs::canonicalize(root)
        .ok()
        .zip(std::fs::canonicalize(folder).ok())
        .is_some_and(|(root, real)| real == codex_folder(&root, &relative))
}

/// Codex homes found without being added: `~/.codex2`-style folders beside `~/.codex`,
/// and `.codex` folders one level below home that the user's own launchers set
/// `CODEX_HOME` to, such as `~/Codex-Work/.codex`. Only homes that already have a
/// `config.toml` are listed; anything else is added by hand.
fn automatic_codex(home: &Path, launchers: &[(String, PathBuf)]) -> Vec<ClientLocation> {
    let mut relatives: Vec<String> = std::fs::read_dir(home)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| codex_sibling_name(name))
        .collect();
    let real_home = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    relatives.extend(
        launchers
            .iter()
            .filter_map(|(_, folder)| codex_relative_of(&real_home, folder)),
    );
    relatives.sort();
    relatives.dedup();
    relatives
        .into_iter()
        .filter_map(|relative| {
            let config_path = codex_folder(home, &relative).join("config.toml");
            codex_automatic_path_is_safe(home, &config_path).then(|| ClientLocation {
                id: format!("codex@{}", codex_key(&relative)),
                manual: false,
                client_type: "codex".into(),
                name: location_name("Codex", &folder_label(&config_path)),
                config_path,
            })
        })
        .collect()
}

/// A Codex home found through a launcher, such as `~/Codex-Work/.codex`, rather than
/// beside `~/.codex`. Discovery lists it only while a launcher sets `CODEX_HOME` to it,
/// so it is not saved just for being listed.
pub(super) fn is_launcher_home(row: &ClientLocation) -> bool {
    !row.manual
        && row.client_type == "codex"
        && row
            .id
            .strip_prefix("codex@")
            .and_then(codex_relative)
            .is_some_and(|relative| relative.contains('/'))
}

/// A launcher home that was saved because Toolport changed its config, and that no
/// launcher points at any more. It stays listed so it can be disconnected, then forgotten.
fn launcher_is_gone(row: &ClientLocation) -> bool {
    if !is_launcher_home(row) {
        return false;
    }
    let (Some(root), Some(folder)) = (
        home(),
        row.config_path
            .parent()
            .and_then(|folder| std::fs::canonicalize(folder).ok()),
    ) else {
        return false;
    };
    !super::claude_profiles::launchers_of(
        super::claude_profiles::CODEX,
        &root,
        &super::claude_profiles::user_path_dirs(),
    )
    .iter()
    .any(|(_, dir)| *dir == folder)
}

/// Saved locations Toolport can forget: ones added by hand, automatic ones whose folder
/// is gone, and launcher homes whose launcher is gone.
/// A saved T3 Code home is decided by [`super::t3code::walk_below_home`] alone, before
/// any other disk check, so nothing looks past a folder that became a link. One whose
/// folder exists but has no config yet is kept while T3 Code still lists it.
pub(super) fn forgettable(row: &ClientLocation) -> bool {
    if is_t3_home(row) {
        return t3_home_folder_is_gone(row) || t3_home_is_gone(row);
    }
    row.manual || automatic_path_is_missing(row) || launcher_is_gone(row)
}

/// A T3 Code home that cannot be forgotten because T3 Code still lists it. Both shells
/// show its Remove location control disabled, with why.
pub(super) fn kept_by_t3_code(row: &ClientLocation) -> bool {
    is_t3_home(row) && !forgettable(row)
}

/// Whether a Codex home outside the saved list is one automatic discovery lists.
fn codex_lookup(id: &str, key: &str) -> Option<ClientLocation> {
    let relative = codex_relative(key)?;
    let root = home()?;
    let config_path = codex_folder(&root, &relative).join("config.toml");
    if !codex_automatic_path_is_safe(&root, &config_path) {
        return None;
    }
    if relative.contains('/') {
        let folder = std::fs::canonicalize(config_path.parent()?).ok()?;
        let launchers = super::claude_profiles::launchers_of(
            super::claude_profiles::CODEX,
            &root,
            &super::claude_profiles::user_path_dirs(),
        );
        if !launchers.iter().any(|(_, dir)| *dir == folder) {
            return None;
        }
    }
    Some(ClientLocation {
        id: id.into(),
        manual: false,
        client_type: "codex".into(),
        name: location_name("Codex", &folder_label(&config_path)),
        config_path,
    })
}

/// Automatic locations remain shallow, ordinary files inside home. Manual
/// registration is the explicit opt-in to linked or external config paths.
pub(super) fn automatic_path_is_safe(home: &Path, config: &Path) -> bool {
    let Some(folder) = config.parent() else {
        return false;
    };
    if folder.parent() != Some(home)
        || config.file_name().is_none_or(|name| name != ".claude.json")
        || !folder
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(claude_profile_name)
        || !std::fs::symlink_metadata(folder).is_ok_and(|meta| meta.is_dir())
        || !std::fs::symlink_metadata(config).is_ok_and(|meta| meta.is_file())
    {
        return false;
    }
    let (Ok(root), Ok(folder), Ok(config)) = (
        std::fs::canonicalize(home),
        std::fs::canonicalize(folder),
        std::fs::canonicalize(config),
    ) else {
        return false;
    };
    folder.parent() == Some(root.as_path()) && config.parent() == Some(folder.as_path())
}

pub(super) fn automatic_path_is_missing(row: &ClientLocation) -> bool {
    if row.manual || !matches!(row.client_type.as_str(), "claude-code" | "codex") {
        return false;
    }
    let Some(root) = home() else {
        return false;
    };
    if row.client_type == "codex" {
        return codex_automatic_path_is_missing(&root, row);
    }
    let Some(folder) = row.config_path.parent() else {
        return false;
    };
    if folder.parent() != Some(root.as_path())
        || row
            .config_path
            .file_name()
            .is_none_or(|name| name != ".claude.json")
        || !folder
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(claude_profile_name)
    {
        return false;
    }
    match std::fs::symlink_metadata(folder) {
        Err(error) => return error.kind() == std::io::ErrorKind::NotFound && root.is_dir(),
        Ok(meta) if !meta.is_dir() => return false,
        Ok(_) => {}
    }
    if !std::fs::canonicalize(&root)
        .ok()
        .zip(std::fs::canonicalize(folder).ok())
        .is_some_and(|(root, folder)| folder.parent() == Some(root.as_path()))
    {
        return false;
    }
    std::fs::symlink_metadata(&row.config_path)
        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}

pub(super) fn validate(row: &ClientLocation) -> Result<(), String> {
    // A saved T3 Code home goes through the walk first and only: the older checks below
    // look at the folder with calls that follow a link higher up the path, and would let
    // a write recreate a home whose folder was deleted.
    let ok = if is_t3_home(row) {
        super::t3code::path_is_safe(&row.config_path)
            && super::t3code::opencode_jsonc_is_plain(&row.config_path)
    } else {
        row.manual
            || automatic_path_is_missing(row)
            || (row.client_type == "claude-code"
                && home().is_some_and(|root| automatic_path_is_safe(&root, &row.config_path)))
            || (row.client_type == "codex"
                && home().is_some_and(|root| codex_automatic_path_is_safe(&root, &row.config_path)))
    };
    if ok {
        Ok(())
    } else {
        Err("This automatic client location needs attention. Choose an ordinary config file inside home, or add the location manually to use a linked or external folder.".into())
    }
}

pub(super) fn validate_mutation(client_id: &str, path: &Path) -> Result<(), String> {
    if !client_id.contains('@') {
        // Legacy secondary upkeep shares the main client ID. A saved automatic
        // path must not become writable through that compatibility route.
        if client_id == "claude-code"
            && !client_config_path("claude-code").is_some_and(|main| same_path(&main, path))
        {
            if let Some(row) = saved()?
                .into_iter()
                .find(|row| !row.manual && row.config_path == path)
            {
                validate(&row)?;
            }
        }
        return Ok(());
    }
    let row = lookup(client_id).ok_or("Unknown client location")?;
    validate(&row)?;
    if !row.manual && path != row.config_path {
        // Team instructions/settings in an automatic profile follow the same
        // boundary, while an absent ordinary settings file may be created.
        if path.parent() != row.config_path.parent()
            || std::fs::symlink_metadata(path).is_ok_and(|meta| !meta.is_file())
        {
            return Err(
                "Automatic client settings must be ordinary files in their config folder".into(),
            );
        }
    }
    Ok(())
}

fn automatic(home: &Path) -> Vec<ClientLocation> {
    let mut rows = Vec::new();
    for folder in claude_profile_dirs(home).unwrap_or_default() {
        let config_path = home.join(&folder).join(".claude.json");
        if !automatic_path_is_safe(home, &config_path) {
            continue;
        }
        rows.push(ClientLocation {
            id: format!("claude-code@{}", folder_key(&folder)),
            manual: false,
            client_type: "claude-code".into(),
            name: location_name("Claude Code", &folder),
            config_path,
        });
    }
    rows.extend(automatic_codex(
        home,
        &super::claude_profiles::launchers_of(
            super::claude_profiles::CODEX,
            home,
            &super::claude_profiles::user_path_dirs(),
        ),
    ));
    let t3 = super::t3code::automatic(&rows);
    rows.extend(t3);
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    rows
}

pub(super) fn same_path(a: &Path, b: &Path) -> bool {
    a == b
        || std::fs::canonicalize(a)
            .ok()
            .zip(std::fs::canonicalize(b).ok())
            .is_some_and(|(a, b)| a == b)
        // A config may not exist yet. Its existing canonical parent still
        // identifies the same location, including Windows extended prefixes.
        || (same_file_name(a, b) && a.parent().zip(b.parent()).is_some_and(|(a, b)|
            std::fs::canonicalize(a).ok().zip(std::fs::canonicalize(b).ok()).is_some_and(|(a, b)| a == b)))
}

/// File names compare as the platform does: without regard to case on Windows.
fn same_file_name(a: &Path, b: &Path) -> bool {
    match (a.file_name(), b.file_name()) {
        (Some(a), Some(b)) if cfg!(windows) => {
            a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
        }
        (a, b) => a == b,
    }
}

/// Prefer ordinary Windows paths only when they resolve to the same directory.
/// Device paths and names requiring extended semantics keep their native form.
pub(super) fn display_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    if let Some(text) = path.to_str() {
        let ordinary = if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
            Some(PathBuf::from(format!(r"\\{rest}")))
        } else {
            text.strip_prefix(r"\\?\")
                .filter(|rest| {
                    rest.as_bytes().get(1..3) == Some(b":\\")
                        && rest.as_bytes()[0].is_ascii_alphabetic()
                })
                .map(PathBuf::from)
        };
        if let Some(ordinary) = ordinary.filter(|ordinary| same_path(path, ordinary)) {
            return ordinary;
        }
    }
    path.to_path_buf()
}

pub(super) fn all() -> Vec<ClientLocation> {
    let known = saved().unwrap_or_else(|error| {
        eprintln!("toolport: {error}");
        Vec::new()
    });
    merge_locations(
        known,
        home().map(|home| automatic(&home)).unwrap_or_default(),
    )
}

fn merge_locations(
    mut known: Vec<ClientLocation>,
    detected: Vec<ClientLocation>,
) -> Vec<ClientLocation> {
    for row in detected {
        if !known
            .iter()
            .any(|saved| saved.id == row.id || saved.config_path == row.config_path)
        {
            known.push(row);
        }
    }
    known
}

fn claude_folder_id(home: &Path, folder: &Path) -> Option<String> {
    let name = folder.file_name()?.to_str()?;
    (folder.parent() == Some(home) && claude_profile_name(name))
        .then(|| format!("claude-code@{}", folder_key(name)))
}

/// A known automatic ID resolves without enumerating home or reading a config.
pub(super) fn lookup(id: &str) -> Option<ClientLocation> {
    if !valid_id(id) {
        return None;
    }
    if let Some(row) = saved().ok()?.into_iter().find(|row| row.id == id) {
        return Some(row);
    }
    if super::t3code::is_location_id(id) {
        // As the page lists it: a T3 Code home a profile or Codex row already covers is
        // not a second way to reach that file.
        return home()
            .map(|home| automatic(&home))
            .unwrap_or_default()
            .into_iter()
            .find(|row| row.id == id);
    }
    if let Some(key) = id.strip_prefix("codex@") {
        return codex_lookup(id, key);
    }
    let key = id.strip_prefix("claude-code@")?;
    let label = decode_key(key)?;
    if label.contains(['/', '\\']) || label.chars().any(char::is_control) {
        return None;
    }
    let folder = format!(".{label}");
    if !claude_profile_name(&folder) || folder_key(&folder) != key {
        return None;
    }
    let root = home()?;
    let path = root.join(&folder);
    if !automatic_path_is_safe(&root, &path.join(".claude.json")) {
        return None;
    }
    Some(ClientLocation {
        id: id.into(),
        manual: false,
        client_type: "claude-code".into(),
        name: location_name("Claude Code", &label),
        config_path: path.join(".claude.json"),
    })
}

pub(super) fn remember(row: &ClientLocation) -> Result<(), String> {
    let store = store_path()?;
    let _lock = crate::registry::lock_at(&store)?;
    let mut rows = saved()?;
    if let Some(existing) = rows.iter_mut().find(|saved| saved.id == row.id) {
        if !row.manual || existing.manual {
            return Ok(());
        }
        existing.manual = true;
    } else {
        rows.push(row.clone());
    }
    crate::registry::atomic_write(
        &store,
        &serde_json::to_string_pretty(&rows).map_err(|e| e.to_string())?,
    )?;
    Ok(())
}

pub fn add_client_location(client_type: &str, folder: &str) -> Result<String, String> {
    let kind = client_location_types()
        .into_iter()
        .find(|kind| kind.id == client_type)
        .ok_or("Choose a supported client type")?;
    let raw = PathBuf::from(folder.trim());
    let folder = if folder.trim() == "~" {
        home().ok_or("Could not resolve home")?
    } else if let Some(relative) = folder.trim().strip_prefix("~/") {
        home().ok_or("Could not resolve home")?.join(relative)
    } else {
        raw
    };
    if !folder.is_absolute() {
        return Err("Choose an absolute client config folder".into());
    }
    let folder = display_path(
        &std::fs::canonicalize(&folder).map_err(|_| "Choose an existing client config folder")?,
    );
    if !folder.is_dir() {
        return Err("Choose a client config folder, not a file".into());
    }
    let path = folder.join(&kind.config_file);
    if let Some(def) = defs().into_iter().find(|def| {
        def.id == client_type && (def.path)().is_some_and(|existing| same_path(&existing, &path))
    }) {
        return Ok(def.id.into());
    }
    if let Some(row) = all()
        .into_iter()
        .find(|row| same_path(&row.config_path, &path))
    {
        if row.client_type != client_type {
            return Err("This config file is already registered for another client type".into());
        }
        let mut row = row;
        row.manual = true;
        remember(&row)?;
        return Ok(row.id);
    }
    let store = store_path()?;
    let _lock = crate::registry::lock_at(&store)?;
    let mut rows = saved()?;
    if let Some(row) = rows
        .iter_mut()
        .find(|row| same_path(&row.config_path, &path))
    {
        if row.client_type != client_type {
            return Err("This config file is already registered for another client type".into());
        }
        row.manual = true;
        let id = row.id.clone();
        crate::registry::atomic_write(
            &store,
            &serde_json::to_string_pretty(&rows).map_err(|e| e.to_string())?,
        )?;
        return Ok(id);
    }
    use sha2::{Digest, Sha256};
    let key = format!("{:x}", Sha256::digest(path.to_string_lossy().as_bytes()));
    let automatic_id = if client_type == "claude-code" {
        home()
            .and_then(|root| std::fs::canonicalize(root).ok())
            .and_then(|root| claude_folder_id(&display_path(&root), &folder))
    } else {
        None
    };
    let id = automatic_id.unwrap_or_else(|| format!("{client_type}@{}", &key[..16]));
    let label = folder_label(&path);
    rows.push(ClientLocation {
        id: id.clone(),
        manual: true,
        client_type: client_type.into(),
        name: location_name(&kind.name, &label),
        config_path: path,
    });
    crate::registry::atomic_write(
        &store,
        &serde_json::to_string_pretty(&rows).map_err(|e| e.to_string())?,
    )?;
    Ok(id)
}

/// Read-only benchmark helper, excluded from shipped builds.
#[cfg(any(test, feature = "test-support"))]
pub fn measure_scan(home: &Path, iterations: u32) -> (usize, f64, f64) {
    let start = std::time::Instant::now();
    for _ in 0..iterations {
        let dirs: Vec<String> = std::fs::read_dir(home)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.path().is_dir())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        let _ = dirs
            .iter()
            .filter(|name| *name == ".claude" || name.starts_with(".claude-"))
            .filter(|name| home.join(name).join(".claude.json").is_file())
            .count();
    }
    let before = start.elapsed().as_secs_f64() * 1000.0 / iterations as f64;
    let start = std::time::Instant::now();
    for _ in 0..iterations {
        let _ = automatic(home);
    }
    let after = start.elapsed().as_secs_f64() * 1000.0 / iterations as f64;
    (std::fs::read_dir(home).unwrap().count(), before, after)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempFolder(PathBuf);
    impl TempFolder {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "toolport-location-{}-{}-{}",
                std::process::id(),
                epoch_millis(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempFolder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn codex_home(home: &Path, relative: &str) -> PathBuf {
        let folder = codex_folder(home, relative);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("config.toml"), "").unwrap();
        folder
    }

    #[test]
    fn codex_homes_come_from_launchers_and_sibling_folders() {
        let root = TempFolder::new();
        let home = std::fs::canonicalize(root.path()).unwrap();
        let work = codex_home(&home, "Codex-Work/.codex");
        let second = codex_home(&home, "Codex-Second/.codex");
        codex_home(&home, ".codex2");
        codex_home(&home, ".codex-work");
        // Backups, folders nothing points at, and homes without a config stay out.
        codex_home(&home, ".codex-bak");
        codex_home(&home, "Codex-Unused/.codex");
        // A launcher home not named .codex is added by hand, not found.
        let plain = codex_home(&home, "codex-plain");
        let empty = codex_folder(&home, "Codex-Empty/.codex");
        std::fs::create_dir_all(&empty).unwrap();
        // Deeper than two levels, outside home, or reached through a link: not automatic.
        let deep = codex_home(&home, "a/b/.codex");
        let outside = TempFolder::new();
        let outside_home = codex_home(outside.path(), ".codex");
        #[cfg(unix)]
        let linked = {
            std::os::unix::fs::symlink(outside.path(), home.join("Codex-Link")).unwrap();
            home.join("Codex-Link").join(".codex")
        };
        let mut launchers = vec![
            ("codex-work".to_string(), work.clone()),
            ("codex-second".to_string(), second.clone()),
            ("codex-empty".to_string(), empty),
            ("codex-plain".to_string(), plain),
            ("codex-deep".to_string(), deep),
            ("codex-outside".to_string(), outside_home),
        ];
        #[cfg(unix)]
        launchers.push(("codex-link".to_string(), linked));
        let rows = automatic_codex(&home, &launchers);
        let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "codex@.codex-work",
                "codex@.codex2",
                "codex@Codex-Second%2F.codex",
                "codex@Codex-Work%2F.codex",
            ]
        );
        let work_row = rows
            .iter()
            .find(|row| row.id.ends_with("Work%2F.codex"))
            .unwrap();
        assert_eq!(work_row.name, "Codex (Codex-Work)");
        assert_eq!(work_row.config_path, work.join("config.toml"));
        assert!(!work_row.manual);
        for row in &rows {
            assert_eq!(generated_id_type(&row.id), Some("codex"));
            assert!(codex_automatic_path_is_safe(&home, &row.config_path));
        }
    }

    #[test]
    fn codex_ids_only_name_shallow_folders_below_home() {
        assert_eq!(
            codex_relative("Codex-Work%2F.codex").as_deref(),
            Some("Codex-Work/.codex")
        );
        assert_eq!(codex_relative(".codex2").as_deref(), Some(".codex2"));
        for key in [
            "a%2Fb%2F.codex",
            "..%2F.codex",
            "%2E%2E",
            "%2Fetc",
            "Codex-Work/.codex",
            "a%5C.codex",
            "x%0A",
            "personal",
            "Codex-Work",
            ".codex",
            "Codex-Work%2Fcodex",
        ] {
            assert_eq!(codex_relative(key), None, "{key}");
        }
        assert_eq!(generated_id_type("codex@..%2F.codex"), None);
        // A backup folder named like a manual location is not an automatic one.
        assert_eq!(generated_id_type("codex@personal"), None);
    }

    #[test]
    fn codex_lookup_needs_a_launcher_for_folders_that_are_not_siblings() {
        let _data = crate::registry::DataDirTestEnv::new("codex-location-lookup");
        let root = TempFolder::new();
        let home = std::fs::canonicalize(root.path()).unwrap();
        let _home = TestHome::set(&home);
        codex_home(&home, ".codex2");
        codex_home(&home, "Codex-Work/.codex");
        assert!(lookup("codex@.codex2").is_some());
        assert!(lookup("codex@Codex-Work%2F.codex").is_none());
        std::fs::create_dir_all(home.join(".local/bin")).unwrap();
        std::fs::write(
            home.join(".local/bin/codex-work"),
            "#!/bin/bash\nexport CODEX_HOME=\"$HOME/Codex-Work/.codex\"\nexec codex \"$@\"\n",
        )
        .unwrap();
        let row = lookup("codex@Codex-Work%2F.codex").unwrap();
        assert_eq!(row.client_type, "codex");
        assert_eq!(row.name, "Codex (Codex-Work)");
        assert!(validate(&row).is_ok());

        // A removed home can be forgotten; a replaced link cannot be written.
        std::fs::remove_dir_all(home.join("Codex-Work")).unwrap();
        assert!(automatic_path_is_missing(&row));
        assert!(validate(&row).is_ok());
        #[cfg(unix)]
        {
            let outside = TempFolder::new();
            codex_home(outside.path(), ".codex");
            std::os::unix::fs::symlink(outside.path(), home.join("Codex-Work")).unwrap();
            assert!(!automatic_path_is_missing(&row));
            assert!(validate(&row).is_err());
            assert!(lookup("codex@Codex-Work%2F.codex").is_none());
        }
    }

    #[test]
    fn launcher_homes_are_listed_while_their_launcher_exists() {
        let _data = crate::registry::DataDirTestEnv::new("codex-launcher-home");
        let root = TempFolder::new();
        let home = std::fs::canonicalize(root.path()).unwrap();
        let _home = TestHome::set(&home);
        codex_home(&home, ".codex2");
        codex_home(&home, "Codex-Work/.codex");
        codex_home(&home, "Codex-Becca/.codex");
        std::fs::create_dir_all(home.join(".local/bin")).unwrap();
        for name in ["work", "becca"] {
            std::fs::write(
                home.join(format!(".local/bin/codex-{name}")),
                format!(
                    "#!/bin/bash\nexport CODEX_HOME=\"$HOME/Codex-{}/.codex\"\nexec codex \"$@\"\n",
                    if name == "work" { "Work" } else { "Becca" }
                ),
            )
            .unwrap();
        }
        let ids = |rows: &[super::super::DetectedClient]| -> Vec<String> {
            rows.iter()
                .filter(|row| row.id.starts_with("codex@"))
                .map(|row| row.id.clone())
                .collect()
        };
        let rows = super::super::detect_clients_for_display_in(
            super::super::machine_defs(true),
            crate::registry::load().unwrap(),
            None,
        )
        .unwrap();
        assert_eq!(
            ids(&rows),
            [
                "codex@.codex2",
                "codex@Codex-Becca%2F.codex",
                "codex@Codex-Work%2F.codex"
            ]
        );
        // Listing saves the sibling folder but not the launcher homes.
        let saved_ids: Vec<_> = saved().unwrap().into_iter().map(|row| row.id).collect();
        assert_eq!(saved_ids, ["codex@.codex2"]);
        assert!(rows.iter().all(|row| !row.location_removable));

        // Changing a launcher home's config saves it, as it does for any location.
        let work = lookup("codex@Codex-Work%2F.codex").unwrap();
        assert!(is_launcher_home(&work));
        assert!(!is_launcher_home(&lookup("codex@.codex2").unwrap()));
        remember(&work).unwrap();

        // Without its launcher, an unsaved home is no longer listed or writable, and a
        // saved one stays listed so it can be disconnected and forgotten.
        std::fs::remove_file(home.join(".local/bin/codex-becca")).unwrap();
        std::fs::remove_file(home.join(".local/bin/codex-work")).unwrap();
        assert!(lookup("codex@Codex-Becca%2F.codex").is_none());
        let rows = super::super::detect_clients_for_display_in(
            super::super::machine_defs(true),
            crate::registry::load().unwrap(),
            None,
        )
        .unwrap();
        assert_eq!(ids(&rows), ["codex@.codex2", "codex@Codex-Work%2F.codex"]);
        let work_row = rows
            .iter()
            .find(|row| row.id == "codex@Codex-Work%2F.codex")
            .unwrap();
        assert!(work_row.location_removable);
        remove_client_location("codex@Codex-Work%2F.codex").unwrap();
        assert!(lookup("codex@Codex-Work%2F.codex").is_none());
        assert!(home.join("Codex-Work/.codex/config.toml").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn a_missing_launcher_home_behind_a_linked_folder_is_not_missing() {
        let _data = crate::registry::DataDirTestEnv::new("codex-missing-link");
        let root = TempFolder::new();
        let home = std::fs::canonicalize(root.path()).unwrap();
        let _home = TestHome::set(&home);
        let row = ClientLocation {
            id: "codex@Codex-Work%2F.codex".into(),
            manual: false,
            client_type: "codex".into(),
            name: "Codex (Codex-Work)".into(),
            config_path: home.join("Codex-Work/.codex/config.toml"),
        };
        // Both folders gone, or the outer one still an ordinary folder: missing.
        assert!(automatic_path_is_missing(&row));
        std::fs::create_dir(home.join("Codex-Work")).unwrap();
        assert!(automatic_path_is_missing(&row));
        std::fs::remove_dir(home.join("Codex-Work")).unwrap();
        // The outer folder replaced by a link to a place without `.codex`: not missing,
        // so nothing is written through it.
        let outside = TempFolder::new();
        std::os::unix::fs::symlink(outside.path(), home.join("Codex-Work")).unwrap();
        assert!(!automatic_path_is_missing(&row));
        assert!(validate(&row).is_err());
        assert!(!outside.path().join(".codex").exists());
    }

    #[test]
    fn invalid_saved_rows_do_not_discard_valid_locations() {
        let _data = crate::registry::DataDirTestEnv::new("location-invalid-row");
        let folder = TempFolder::new();
        let id = add_client_location("codex", folder.path().to_str().unwrap()).unwrap();
        let store = store_path().unwrap();
        let mut rows: Vec<serde_json::Value> =
            serde_json::from_slice(&std::fs::read(&store).unwrap()).unwrap();
        rows.push(serde_json::json!({"id":"bad"}));
        rows.push(serde_json::json!({"id":"codex@bad", "clientType":"codex", "name":"bad", "configPath":"relative"}));
        std::fs::write(&store, serde_json::to_vec(&rows).unwrap()).unwrap();
        let rows = saved().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, id);
        assert!(find_def(&id).is_some());
    }

    #[test]
    fn removing_manual_locations_preserves_config_and_requires_disconnect() {
        let _data = crate::registry::DataDirTestEnv::new("location-remove");
        let folder = TempFolder::new();
        let path = folder.path().join(".claude.json");
        let text = r#"{"mcpServers":{"toolport":{"command":"/usr/bin/toolport-gateway"}}}"#;
        std::fs::write(&path, text).unwrap();
        let id = add_client_location("claude-code", folder.path().to_str().unwrap()).unwrap();
        assert!(read_client(&find_def(&id).unwrap()).location_removable);
        assert!(remove_client_location(&id).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        std::fs::write(&path, "{}").unwrap();
        remove_client_location(&id).unwrap();
        assert!(saved().unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "{}");
    }

    #[test]
    fn backup_folders_are_never_automatic_clients() {
        let root = TempFolder::new();
        for name in [
            ".claude.bak",
            ".claudesync",
            ".claude-backup-2026",
            ".claude-work.bak",
            ".claude-old",
            ".claude2",
            ".claude-work",
        ] {
            std::fs::create_dir(root.path().join(name)).unwrap();
            std::fs::write(root.path().join(name).join(".claude.json"), "{}").unwrap();
        }
        let rows = automatic(root.path());
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().any(|row| row.id == "claude-code@claude2"));
    }

    #[test]
    fn folder_labels_and_encoded_ids_survive_the_audit_recording_path() {
        let _data = crate::registry::DataDirTestEnv::new("location-audit-record");
        let folder = TempFolder::new();
        for label in [
            "claude2".to_string(),
            "claude-work@office".to_string(),
            format!("claude-{}", "x".repeat(210)),
        ] {
            let path = folder.path().join(&label);
            std::fs::create_dir(&path).unwrap();
            let id = format!("claude-code@{}", folder_key(&label));
            remember(&ClientLocation {
                id: id.clone(),
                manual: true,
                client_type: "claude-code".into(),
                name: location_name("Claude Code", &label),
                config_path: path.join(".claude.json"),
            })
            .unwrap();
            let principal = format!("adapter:{id}");
            crate::audit::record_internal("search", 1, false, Some(&principal), true);
            let stored = crate::audit::read_all().unwrap();
            assert_eq!(stored[0]["client"], principal);
            let rows = crate::audit::read_activity(1).unwrap();
            let row = &rows[0];
            assert_eq!(row["client"], principal);
            assert_ne!(row["clientName"], "[private]");
            assert!(row["clientName"]
                .as_str()
                .unwrap()
                .starts_with("Claude Code ("));
        }
        assert_eq!(
            crate::session_observability::display_label("reported@label").as_deref(),
            Some("[private]")
        );
    }

    #[test]
    fn location_disconnect_preserves_a_foreign_entry_using_the_gateway_binary() {
        let _data = crate::registry::DataDirTestEnv::new("location-restore-foreign");
        let folder = TempFolder::new();
        let path = folder.path().join(".claude.json");
        let original = serde_json::json!({"mcpServers":{
            "toolport":{"command":"/usr/bin/toolport-gateway", "env":{"TOOLPORT_CLIENT_ID":"claude-code"}},
            "foreign":{"command":"/usr/bin/toolport-gateway", "args":["--http","9999"]},
            "notes":{"command":"notes"}}});
        std::fs::write(&path, original.to_string()).unwrap();
        let id = add_client_location("claude-code", folder.path().to_str().unwrap()).unwrap();
        let path = resolved_definition_path(&find_def(&id).unwrap()).unwrap();
        moved::record_selected(&id, Format::JsonMcpServers, &path, &["notes".into()]).unwrap();
        let mut moved_config = original.clone();
        moved_config["mcpServers"]
            .as_object_mut()
            .unwrap()
            .remove("notes");
        std::fs::write(&path, moved_config.to_string()).unwrap();
        let entry: ServerEntry = serde_json::from_value(serde_json::json!({"id":"toolport", "name":"toolport", "transport":"stdio", "command":"/usr/bin/toolport-gateway", "env":[{"key":"TOOLPORT_CLIENT_ID", "value":id, "secret":false}]})).unwrap();
        install_or_remove(&id, Some(&entry)).unwrap();
        uninstall_gateway(&id).unwrap();
        let after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(
            after["mcpServers"]["foreign"],
            original["mcpServers"]["foreign"]
        );
        assert!(after["mcpServers"].get("toolport").is_none());
        assert_eq!(
            after["mcpServers"]["notes"],
            original["mcpServers"]["notes"]
        );
    }

    #[test]
    fn detection_is_shallow_name_filtered_and_does_not_parse_configs() {
        let root = TempFolder::new();
        for folder in [
            ".claude",
            ".claude2",
            ".claude3",
            ".claude-work",
            ".claude-empty",
            "unrelated/.claude4",
        ] {
            std::fs::create_dir_all(root.path().join(folder)).unwrap();
            if folder != ".claude-empty" {
                std::fs::write(root.path().join(folder).join(".claude.json"), "not JSON").unwrap();
            }
        }
        let rows = automatic(root.path());
        assert_eq!(rows.len(), 4);
        assert!(rows
            .iter()
            .any(|row| row.id == "claude-code@claude2" && row.name == "Claude Code (claude2)"));
        assert_ne!(folder_key(".claude+a"), folder_key(".claude-a"));
    }

    #[cfg(unix)]
    #[test]
    fn saved_automatic_file_link_never_reads_or_writes_outside() {
        saved_automatic_link_never_reads_or_writes_outside(false);
    }

    #[cfg(unix)]
    #[test]
    fn saved_automatic_folder_link_never_reads_or_writes_outside() {
        saved_automatic_link_never_reads_or_writes_outside(true);
    }

    #[cfg(unix)]
    fn saved_automatic_link_never_reads_or_writes_outside(directory_link: bool) {
        {
            let data = crate::registry::DataDirTestEnv::new("location-saved-link-r2");
            let root = TempFolder::new();
            let outside = TempFolder::new();
            let _home = TestHome::set(root.path());
            let folder = root.path().join(".claude2");
            std::fs::create_dir(&folder).unwrap();
            let path = folder.join(".claude.json");
            std::fs::write(&path, "{}").unwrap();
            let gateway = data.dir.join("toolport-gateway");
            std::fs::write(&gateway, "fixture").unwrap();
            let def = || {
                let location = all()
                    .into_iter()
                    .find(|row| row.id == "claude-code@claude2")
                    .unwrap();
                let mut def = defs()
                    .into_iter()
                    .find(|def| def.id == "claude-code")
                    .unwrap();
                def.location = Some(location);
                def
            };
            let rows = detect_clients_for_display_in(
                vec![def()],
                crate::registry::Registry::default(),
                Some(gateway.clone()),
            )
            .unwrap();
            assert!(rows[0].error.is_none());
            assert!(!saved().unwrap()[0].manual);
            let stale_def = def();
            let original = r#"{"mcpServers":{"conduit":{"command":"/old/conduit-gateway","env":{"CONDUIT_CLIENT_ID":"claude-code"}}}}"#;
            let external = outside.path().join(".claude.json");
            std::fs::write(&external, original).unwrap();
            std::fs::remove_file(&path).unwrap();
            if directory_link {
                std::fs::remove_dir(&folder).unwrap();
                std::os::unix::fs::symlink(outside.path(), &folder).unwrap();
            } else {
                std::os::unix::fs::symlink(&external, &path).unwrap();
            }
            // A definition captured before the replacement must also be safe.
            let read = read_client(&stale_def);
            assert!(read.error.is_some(), "directory_link={directory_link}");
            assert!(read.servers.is_empty());
            assert!(
                client_rules_target("claude-code@claude2", crate::instructions::Scope::Team)
                    .is_none()
            );
            let rows = detect_clients_for_display_in(
                vec![def()],
                crate::registry::Registry::default(),
                Some(gateway),
            )
            .unwrap();
            assert!(rows[0].error.is_some());
            assert_eq!(rows[0].config_path, path.to_string_lossy());
            let id = "claude-code@claude2";
            assert!(install_gateway(id, None).is_err());
            assert!(uninstall_gateway(id).is_err());
            assert!(claude_code_config_paths()
                .iter()
                .all(|candidate| candidate != &path));
            let mut outcome = RepointOutcome::default();
            repoint_other_claude_configs("/new/toolport-gateway", &mut outcome);
            assert!(outcome.extra_claude_repointed.is_empty());
            assert_eq!(std::fs::read_to_string(&external).unwrap(), original);
            // Registering the linked folder is the explicit opt-in to that target.
            let manual = add_client_location("claude-code", folder.to_str().unwrap()).unwrap();
            assert!(find_def(&manual).unwrap().location.unwrap().manual);
            let entry: ServerEntry = serde_json::from_value(serde_json::json!({"id":"toolport", "name":"toolport", "transport":"stdio", "command":"/new/toolport-gateway"})).unwrap();
            install_or_remove(&manual, Some(&entry)).unwrap();
            assert!(std::fs::read_to_string(&external)
                .unwrap()
                .contains("/new/toolport-gateway"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn automatic_location_link_swap_before_commit_is_refused() {
        for directory_link in [false, true] {
            let _data = crate::registry::DataDirTestEnv::new("location-link-swap-r2");
            let root = TempFolder::new();
            let outside = TempFolder::new();
            let _home = TestHome::set(root.path());
            let folder = root.path().join(".claude2");
            std::fs::create_dir(&folder).unwrap();
            let path = folder.join(".claude.json");
            std::fs::write(&path, "{}").unwrap();
            remember(&automatic(root.path()).remove(0)).unwrap();
            let external = outside.path().join(".claude.json");
            // Matching contents also exercise the boundary independently of
            // optimistic content conflicts.
            std::fs::write(&external, "{}").unwrap();
            let target = external.clone();
            mutation::BEFORE_COMMIT.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move |path| {
                    std::fs::remove_file(path).unwrap();
                    if directory_link {
                        std::fs::remove_dir(path.parent().unwrap()).unwrap();
                        std::os::unix::fs::symlink(
                            target.parent().unwrap(),
                            path.parent().unwrap(),
                        )
                        .unwrap();
                    } else {
                        std::os::unix::fs::symlink(&target, path).unwrap();
                    }
                }));
            });
            let entry: ServerEntry = serde_json::from_value(serde_json::json!({"id":"toolport", "name":"toolport", "transport":"stdio", "command":"toolport-gateway"})).unwrap();
            let error = install_or_remove("claude-code@claude2", Some(&entry)).unwrap_err();
            assert!(error.contains("needs attention"), "{error}");
            assert_eq!(std::fs::read_to_string(external).unwrap(), "{}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn discovery_does_not_follow_symlinks_outside_home() {
        let home = TempFolder::new();
        let outside = TempFolder::new();
        std::fs::write(outside.path().join(".claude.json"), "{}").unwrap();
        std::os::unix::fs::symlink(outside.path(), home.path().join(".claude-outside")).unwrap();
        std::fs::create_dir(home.path().join(".claude-linked-file")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join(".claude.json"),
            home.path().join(".claude-linked-file/.claude.json"),
        )
        .unwrap();
        assert!(automatic(home.path()).is_empty());
    }

    #[test]
    fn manual_locations_are_stable_deduplicated_and_separate_from_registry() {
        let _data = crate::registry::DataDirTestEnv::new("client-locations");
        let folder = TempFolder::new();
        let id = add_client_location("codex", folder.path().to_str().unwrap()).unwrap();
        assert_eq!(
            add_client_location("codex", folder.path().to_str().unwrap()).unwrap(),
            id
        );
        assert_eq!(saved().unwrap().len(), 1);
        let def = find_def(&id).unwrap();
        assert_eq!(
            resolved_definition_path(&def).unwrap(),
            display_path(&std::fs::canonicalize(folder.path()).unwrap()).join("config.toml")
        );
        assert_eq!(
            discovery_capabilities(&id).auto_mode,
            discovery_capabilities("codex").auto_mode
        );
        assert!(!crate::registry::resolved_path().unwrap().exists());
        assert!(add_client_location("codex", "relative").is_err());
        assert!(!valid_id("codex@../escape"));
        let claude_id =
            add_client_location("claude-code", folder.path().to_str().unwrap()).unwrap();
        let claude = read_client(&find_def(&claude_id).unwrap());
        assert!(claude.app_present);
        assert!(!claude.config_exists);
    }

    #[test]
    fn environment_selected_location_has_one_identity_for_upkeep() {
        let base = defs().into_iter().find(|def| def.id == "codex").unwrap();
        let mut location = defs().into_iter().find(|def| def.id == "codex").unwrap();
        location.location = Some(ClientLocation {
            id: "codex@selected".into(),
            manual: true,
            client_type: "codex".into(),
            name: "Codex (selected)".into(),
            config_path: resolved_definition_path(&base).unwrap(),
        });
        let rows = unique_config_definitions(vec![base, location]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].client_id(), "codex");
    }

    #[test]
    fn a_manual_claude_folder_keeps_its_identity_when_the_config_appears() {
        let root = TempFolder::new();
        let folder = root.path().join(".claude4");
        std::fs::create_dir(&folder).unwrap();
        let id = claude_folder_id(root.path(), &folder).unwrap();
        assert_eq!(id, "claude-code@claude4");
        assert!(automatic(root.path()).is_empty());
        let saved = ClientLocation {
            id: id.clone(),
            manual: true,
            client_type: "claude-code".into(),
            name: "Claude Code (claude4)".into(),
            config_path: folder.join(".claude.json"),
        };
        std::fs::write(&saved.config_path, "{}").unwrap();
        let rows = merge_locations(vec![saved.clone()], automatic(root.path()));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, id);
        // A previously registered ID also wins over newly discovered metadata.
        let old_id = ClientLocation {
            id: "claude-code@registered".into(),
            ..saved
        };
        let rows = merge_locations(vec![old_id], automatic(root.path()));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "claude-code@registered");
        assert!(claude_folder_id(root.path(), &folder.join("nested")).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn an_environment_path_alias_does_not_duplicate_a_registered_location() {
        let data = crate::registry::DataDirTestEnv::new("location-path-alias");
        let folder = data.dir.join("real");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("config.toml"), "").unwrap();
        std::os::unix::fs::symlink(&folder, data.dir.join("alias")).unwrap();
        fn alias_path() -> Option<PathBuf> {
            Some(crate::registry::conduit_dir()?.join("alias/config.toml"))
        }
        let mut base = defs().into_iter().find(|def| def.id == "codex").unwrap();
        base.path = alias_path;
        let id = add_client_location("codex", folder.to_str().unwrap()).unwrap();
        let location = find_def(&id).unwrap();
        let rows = unique_config_definitions(vec![base, location]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].client_id(), "codex");
    }

    #[test]
    fn existing_manual_gateway_is_adopted_without_duplicate_entries() {
        let _data = crate::registry::DataDirTestEnv::new("client-location-adoption");
        let folder = TempFolder::new();
        let path = folder.path().join(".claude.json");
        std::fs::write(&path, r#"{"mcpServers":{"toolport":{"command":"/usr/bin/toolport-gateway","env":{"TOOLPORT_CLIENT_ID":"claude-code"}},"other":{"command":"other"}}}"#).unwrap();
        let id = add_client_location("claude-code", folder.path().to_str().unwrap()).unwrap();
        let def = find_def(&id).unwrap();
        let detected = read_client(&def);
        assert_eq!(
            resolve_entry_state(&detected.servers, None),
            GatewayEntryState::Managed
        );
        let mut entry: ServerEntry = serde_json::from_value(serde_json::json!({"id":"toolport", "name":"toolport", "transport":"stdio", "env":[{"key":"TOOLPORT_CLIENT_ID", "value":id, "secret":false}]})).unwrap();
        entry.command = Some("/usr/bin/toolport-gateway".into());
        install_or_remove(&id, Some(&entry)).unwrap();
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["mcpServers"].as_object().unwrap().len(), 2);
        assert_eq!(
            value["mcpServers"]["toolport"]["env"]["TOOLPORT_CLIENT_ID"],
            id
        );
        assert!(backup_dir(&id).unwrap().is_dir());
        assert_eq!(known_adapter_name(&id).unwrap(), detected.name);
    }

    #[test]
    fn long_ascii_profile_connects_with_stable_id_and_recoverable_backups() {
        long_profile_connects_with_stable_id_and_recoverable_backups(format!(
            ".claude-{}",
            "x".repeat(237)
        ));
    }

    #[test]
    fn long_unicode_profile_connects_with_stable_id_and_recoverable_backups() {
        long_profile_connects_with_stable_id_and_recoverable_backups(format!(
            ".claude-{}",
            "é".repeat(100)
        ));
    }

    fn long_profile_connects_with_stable_id_and_recoverable_backups(folder: String) {
        let _data = crate::registry::DataDirTestEnv::new("location-long-backup-r2");
        let root = TempFolder::new();
        let _home = TestHome::set(root.path());
        let mut dirs = Vec::new();
        {
            let profile = root.path().join(&folder);
            std::fs::create_dir(&profile).unwrap();
            let path = profile.join(".claude.json");
            let original = r#"{"mcpServers":{"notes":{"command":"notes"}},"session":42}"#;
            std::fs::write(&path, original).unwrap();
            let location = automatic(root.path())
                .into_iter()
                .find(|row| row.config_path == path)
                .unwrap();
            assert_eq!(location.id, format!("claude-code@{}", folder_key(&folder)));
            assert!(location.id.len() > 255);
            remember(&location).unwrap();
            let entry: ServerEntry = serde_json::from_value(serde_json::json!({"id":"toolport", "name":"toolport", "transport":"stdio", "command":"toolport-gateway"})).unwrap();
            let write = install_or_remove(&location.id, Some(&entry)).unwrap();
            assert!(Path::new(write.backup.as_ref().unwrap()).is_file());
            let dir = backup_dir(&location.id).unwrap();
            assert!(dir.file_name().unwrap().as_encoded_bytes().len() <= 255);
            assert!(restore::record_path(
                &location.id,
                &resolved_definition_path(&find_def(&location.id).unwrap()).unwrap()
            )
            .unwrap()
            .is_file());
            assert!(!dirs.contains(&dir));
            dirs.push(dir);
            uninstall_gateway(&location.id).unwrap();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
            assert_eq!(lookup(&location.id).unwrap().id, location.id);
        }
    }

    #[test]
    fn existing_long_location_backups_remain_findable() {
        let data = crate::registry::DataDirTestEnv::new("location-legacy-backup-r2");
        let id = format!("claude-code@claude-{}", "x".repeat(210));
        let legacy = data.dir.join("backups").join(&id);
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("old-backup.json"), "original").unwrap();
        assert_eq!(backup_dir(&id).unwrap(), legacy);
        assert_eq!(
            std::fs::read_to_string(backup_dir(&id).unwrap().join("old-backup.json")).unwrap(),
            "original"
        );
    }

    #[test]
    fn access_and_caller_names_remain_independent_per_location() {
        let _data = crate::registry::DataDirTestEnv::new("location-access");
        let a = TempFolder::new();
        let b = TempFolder::new();
        let first = add_client_location("codex", a.path().to_str().unwrap()).unwrap();
        let second = add_client_location("codex", b.path().to_str().unwrap()).unwrap();
        assert_ne!(first, second);
        let mut reg = crate::registry::Registry::default();
        for id in ["first-access", "second-access"] {
            reg.profiles
                .push(serde_json::from_value(serde_json::json!({"id":id,"name":id})).unwrap());
        }
        reg.client_scopes
            .insert(first.clone(), "first-access".into());
        reg.client_scopes
            .insert(second.clone(), "second-access".into());
        assert_eq!(
            resolve_launch_profile(&reg, Some(&first), &None),
            Some("first-access".into())
        );
        assert_eq!(
            resolve_launch_profile(&reg, Some(&second), &None),
            Some("second-access".into())
        );
        let entries = vec![
            serde_json::json!({"client":format!("adapter:{first}")}),
            serde_json::json!({"client":format!("adapter:{second}")}),
        ];
        let names = CallerNames::for_entries(&entries);
        assert_ne!(
            names.trusted_name(entries[0]["client"].as_str(), None),
            names.trusted_name(entries[1]["client"].as_str(), None)
        );
        assert_ne!(backup_dir(&first), backup_dir(&second));
    }

    #[test]
    fn later_background_adoption_keeps_existing_access_and_location_override() {
        let _data = crate::registry::DataDirTestEnv::new("later-location-access");
        let mut registry = crate::registry::Registry::default();
        for id in ["work", "personal"] {
            registry
                .profiles
                .push(serde_json::from_value(serde_json::json!({"id": id, "name": id})).unwrap());
        }
        registry
            .client_scopes
            .insert("claude-code".into(), "work".into());
        crate::registry::save(&registry).unwrap();
        let text = r#"{"mcpServers":{"other":{"env":{"TOOLPORT_CLIENT_ID":"unrelated"}},"toolport":{"command":"toolport-gateway","env":{"TOOLPORT_CLIENT_ID":"claude-code"}}}}"#;
        inherit_location_access(
            "claude-code@claude2",
            Format::JsonMcpServers,
            "toolport",
            text,
        )
        .unwrap();
        let mut registry = crate::registry::load().unwrap();
        assert_eq!(
            registry.client_scopes.get("claude-code@claude2"),
            Some(&"work".into())
        );
        registry
            .client_scopes
            .insert("claude-code@claude2".into(), "personal".into());
        crate::registry::save(&registry).unwrap();
        inherit_location_access(
            "claude-code@claude2",
            Format::JsonMcpServers,
            "toolport",
            text,
        )
        .unwrap();
        assert_eq!(
            crate::registry::load()
                .unwrap()
                .client_scopes
                .get("claude-code@claude2"),
            Some(&"personal".into())
        );
    }

    #[test]
    fn identity_only_adoption_preserves_existing_gateway_fields_and_restore_record() {
        let _data = crate::registry::DataDirTestEnv::new("location-identity-adoption");
        let folder = TempFolder::new();
        let path = folder.path().join(".claude.json");
        let raw = serde_json::json!({"mcpServers":{"toolport":{"command":"/usr/bin/toolport-gateway","args":["--private"],"env":{"TOOLPORT_CLIENT_ID":"claude-code","TOOLPORT_PROFILE":"work","KEEP":"value"},"unknown":42}}});
        std::fs::write(&path, raw.to_string()).unwrap();
        let id = add_client_location("claude-code", folder.path().to_str().unwrap()).unwrap();
        mutation::run(&id, &path, Format::JsonMcpServers, || {
            backfill_gateway_identity(&find_def(&id).unwrap(), &path, "toolport", &id)
        })
        .unwrap();
        let mut after: serde_json::Value =
            serde_json::from_str(&read_config_file(&path).unwrap()).unwrap();
        assert_eq!(
            after["mcpServers"]["toolport"]["env"][crate::brand::CLIENT_ID],
            id
        );
        after["mcpServers"]["toolport"]["env"][crate::brand::CLIENT_ID] =
            serde_json::json!("claude-code");
        after["mcpServers"]["toolport"]["env"]
            .as_object_mut()
            .unwrap()
            .remove(crate::brand::ATTRIBUTION_ID);
        assert_eq!(after, raw);
        assert!(restore::recorded_paths()
            .iter()
            .any(|(record_id, recorded, _)| record_id == &id && recorded == &path));
        let outcome = uninstall_gateway(&id).unwrap();
        assert!(outcome.warnings.is_empty());
        assert!(!read_client(&find_def(&id).unwrap()).gateway_installed);
        finish_uninstall(&id, &outcome).unwrap();
    }

    #[test]
    #[ignore = "scan benchmark; run explicitly with --ignored --nocapture"]
    fn scan_timings() {
        let root = TempFolder::new();
        for i in 0..10_000 {
            std::fs::write(root.path().join(format!("unrelated-{i}")), "").unwrap();
        }
        for i in 0..20 {
            let dir = root.path().join(format!(".claude-profile{i}"));
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(dir.join(".claude.json"), "{}").unwrap();
        }
        fn measure(home: &Path) {
            let (entries, before, after) = measure_scan(home, 20);
            println!("entries={entries} before_ms={before:.3} after_ms={after:.3}");
        }
        measure(root.path());
        // Only directory metadata; no config contents or writes in real home.
        if let Some(real_home) = home() {
            measure(&real_home);
        }
    }
}
