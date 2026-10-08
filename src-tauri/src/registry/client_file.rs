//! Client files have external writers. Verify the file displaced by publication,
//! rather than trusting a read of a pathname before rename.
use super::{resolve_atomic_write_dest, TempFileCleanup, ATOMIC_WRITE_SEQ};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

pub(crate) const CHANGED: &str = "Client config revision changed before rename";

#[derive(Clone, PartialEq, Eq)]
struct Identity {
    size: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(any(unix, windows))]
    device: u64,
    #[cfg(any(unix, windows))]
    inode: u64,
    #[cfg(unix)]
    mtime: (i64, i64),
}
impl Identity {
    fn of(meta: &Metadata, _file: &File) -> Result<Self, String> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        #[cfg(windows)]
        let info = {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::Storage::FileSystem::{
                GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
            };
            let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
            if unsafe { GetFileInformationByHandle(_file.as_raw_handle(), &mut info) } == 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            info
        };
        Ok(Self {
            size: meta.len(),
            modified: meta.modified().ok(),
            #[cfg(unix)]
            device: meta.dev(),
            #[cfg(unix)]
            inode: meta.ino(),
            #[cfg(windows)]
            device: u64::from(info.dwVolumeSerialNumber),
            #[cfg(windows)]
            inode: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
            #[cfg(unix)]
            mtime: (meta.mtime(), meta.mtime_nsec()),
        })
    }
}

pub(crate) struct Revision {
    pub(crate) text: Option<String>,
    identity: Option<Identity>,
    permissions: Option<fs::Permissions>,
}

pub(crate) fn read(path: &Path) -> Result<Revision, String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Revision {
                text: None,
                identity: None,
                permissions: None,
            })
        }
        Err(e) => {
            return Err(format!(
                "could not stat/open {} before editing: {e}",
                path.display()
            ))
        }
    };
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.len() > crate::clients::MAX_CONFIG_BYTES {
        return Err(format!(
            "{} is not a regular file within the config size limit",
            path.display()
        ));
    }
    #[cfg(test)]
    hook("read", path);
    let file_identity = Identity::of(&meta, &file)?;
    let mut text = String::new();
    file.take(crate::clients::MAX_CONFIG_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    if text.len() as u64 > crate::clients::MAX_CONFIG_BYTES {
        return Err("Client config exceeds size limit".into());
    }
    Ok(Revision {
        text: Some(text),
        identity: Some(file_identity),
        permissions: Some(meta.permissions()),
    })
}

fn sibling(dest: &Path) -> PathBuf {
    PathBuf::from(format!(
        "{}.{}.{}.conduit-tmp",
        dest.display(),
        std::process::id(),
        ATOMIC_WRITE_SEQ.fetch_add(1, Ordering::Relaxed)
    ))
}

fn identity(path: &Path) -> Result<Option<Identity>, String> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    match options.open(path) {
        Ok(file) => Ok(Some(Identity::of(
            &file.metadata().map_err(|e| e.to_string())?,
            &file,
        )?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

fn sync_parent(dest: &Path) {
    if let Some(parent) = dest.parent() {
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
    }
}

// ENOTSUP and EOPNOTSUPP are distinct on macOS, but aliases on Linux.
fn unsupported(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    if error.raw_os_error().is_some_and(|code| {
        code == libc::ENOSYS || code == libc::ENOTSUP || code == libc::EOPNOTSUPP
    }) {
        return true;
    }
    #[cfg(windows)]
    if matches!(error.raw_os_error(), Some(1 | 50 | 120)) {
        return true;
    }
    error.kind() == std::io::ErrorKind::Unsupported
}

// Prefer atomic no-clobber rename when the volume cannot make hard links.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn rename_no_clobber(source: &Path, dest: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let source = std::ffi::CString::new(source.as_os_str().as_bytes())?;
    let dest = std::ffi::CString::new(dest.as_os_str().as_bytes())?;
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            dest.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(target_os = "macos")]
    let result = unsafe { libc::renamex_np(source.as_ptr(), dest.as_ptr(), libc::RENAME_EXCL) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn rename_no_clobber(source: &Path, dest: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::MoveFileExW;
    let wide = |path: &Path| {
        path.as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>()
    };
    // Zero flags omit MOVEFILE_REPLACE_EXISTING.
    if unsafe { MoveFileExW(wide(source).as_ptr(), wide(dest).as_ptr(), 0) } != 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn rename_no_clobber(_: &Path, _: &Path) -> std::io::Result<()> {
    Err(std::io::ErrorKind::Unsupported.into())
}

fn publish_no_clobber(source: &Path, dest: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    let link = if FORCE_NO_HARD_LINKS.with(|slot| slot.get()) {
        Err(std::io::ErrorKind::Unsupported.into())
    } else {
        fs::hard_link(source, dest)
    };
    #[cfg(not(test))]
    let link = fs::hard_link(source, dest);
    match link {
        Ok(()) => return Ok(()),
        Err(error) => {
            // Linux FAT also reports EPERM for unsupported hard links.
            #[cfg(unix)]
            let no_links = error.raw_os_error() == Some(libc::EPERM);
            #[cfg(not(unix))]
            let no_links = false;
            if !unsupported(&error) && !no_links {
                return Err(error);
            }
        }
    }
    #[cfg(test)]
    let rename = if FORCE_NO_RENAME.with(|slot| slot.get()) {
        Err(std::io::ErrorKind::Unsupported.into())
    } else {
        rename_no_clobber(source, dest)
    };
    #[cfg(not(test))]
    let rename = rename_no_clobber(source, dest);
    match rename {
        Ok(()) => return Ok(()),
        Err(error) if unsupported(&error) => {}
        #[cfg(unix)]
        Err(error) if error.raw_os_error() == Some(libc::EINVAL) => {}
        Err(error) => return Err(error),
    }
    // Last resort: reserve the real path without clobbering any native save,
    // retain the source until all bytes and permissions are durable.
    let mut input = File::open(source)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options.open(dest)?;
    std::io::copy(&mut input, &mut output)?;
    output.set_permissions(input.metadata()?.permissions())?;
    output.sync_all()
}

fn cleanup_published(path: &Path) {
    #[cfg(test)]
    let result = if FORCE_CLEANUP_ERROR.with(|slot| slot.get()) {
        Err(std::io::ErrorKind::PermissionDenied.into())
    } else {
        fs::remove_file(path)
    };
    #[cfg(not(test))]
    let result = fs::remove_file(path);
    if let Err(error) = result {
        if error.kind() != std::io::ErrorKind::NotFound {
            eprintln!(
                "toolport: could not clean up published config temp {}: {error}",
                path.display()
            );
        }
    }
}

/// Return false only when the OS or filesystem explicitly lacks exchange.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn exchange(replacement: &Path, dest: &Path, displaced: &Path) -> Result<bool, String> {
    use std::os::unix::ffi::OsStrExt;
    let source =
        std::ffi::CString::new(replacement.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    let target = std::ffi::CString::new(dest.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            target.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    #[cfg(target_os = "macos")]
    let result = unsafe { libc::renamex_np(source.as_ptr(), target.as_ptr(), libc::RENAME_SWAP) };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        if unsupported(&error) || error.raw_os_error() == Some(libc::EINVAL) {
            return Ok(false);
        }
        if error.kind() == std::io::ErrorKind::NotFound {
            return Err(CHANGED.into());
        }
        return Err(error.to_string());
    }
    // On Unix the exchange itself leaves the displaced file at replacement.
    debug_assert_eq!(replacement, displaced);
    Ok(true)
}

#[cfg(windows)]
fn exchange(replacement: &Path, dest: &Path, displaced: &Path) -> Result<bool, String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::ReplaceFileW;
    let wide = |path: &Path| {
        path.as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>()
    };
    let source = wide(replacement);
    let target = wide(dest);
    let backup = wide(displaced);
    if unsafe {
        ReplaceFileW(
            target.as_ptr(),
            source.as_ptr(),
            backup.as_ptr(),
            0,
            std::ptr::null(),
            std::ptr::null(),
        )
    } != 0
    {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    // Partial failures may already have moved the user's file to backup. Never
    // clean that backup up, and recover the pathname without overwriting a save.
    if displaced.exists() {
        let _ = fs::hard_link(displaced, dest);
        return Err(format!(
            "ReplaceFile failed: {error}; displaced config retained at {}",
            displaced.display()
        ));
    }
    match error.raw_os_error() {
        Some(1 | 50 | 120) => Ok(false), // INVALID_FUNCTION, NOT_SUPPORTED, CALL_NOT_IMPLEMENTED
        Some(2 | 3) => Err(CHANGED.into()),
        _ => Err(error.to_string()),
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn exchange(_: &Path, _: &Path, _: &Path) -> Result<bool, String> {
    Ok(false)
}

pub(crate) fn commit(path: &Path, expected: &Revision, output: Option<&str>) -> Result<(), String> {
    let dest = resolve_atomic_write_dest(path)?;
    if output.is_none() {
        return remove(&dest, expected);
    }
    let output = output.unwrap();
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = sibling(&dest);
    let mut cleanup = TempFileCleanup::new(tmp.clone());
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp).map_err(|e| e.to_string())?;
    cleanup.arm();
    file.write_all(output.as_bytes())
        .map_err(|e| e.to_string())?;
    if let Some(permissions) = &expected.permissions {
        file.set_permissions(permissions.clone())
            .map_err(|e| e.to_string())?;
    }
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    if read(&dest)?.text != expected.text {
        return Err(CHANGED.into());
    }
    #[cfg(test)]
    hook("commit", path);
    if expected.text.is_none() {
        // Publishing a first config must never replace one the client just made.
        publish_no_clobber(&tmp, &dest).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                CHANGED.into()
            } else {
                e.to_string()
            }
        })?;
        cleanup.disarm();
        cleanup_published(&tmp);
        sync_parent(&dest);
        return Ok(());
    }
    #[cfg(windows)]
    let displaced = sibling(&dest);
    #[cfg(not(windows))]
    let displaced = tmp.clone();
    #[cfg(test)]
    let use_exchange = !FORCE_FALLBACK.with(|slot| slot.get());
    #[cfg(not(test))]
    let use_exchange = true;
    if use_exchange && exchange(&tmp, &dest, &displaced)? {
        // From here cleanup must never delete unverified displaced bytes.
        cleanup.disarm();
        let matches = fs::symlink_metadata(&displaced).is_ok_and(|meta| meta.is_file())
            && read(&displaced).is_ok_and(|old| old.text == expected.text);
        if !matches {
            #[cfg(windows)]
            let rollback_displaced = sibling(&dest);
            #[cfg(not(windows))]
            let rollback_displaced = displaced.clone();
            if !exchange(&displaced, &dest, &rollback_displaced).map_err(|error| {
                format!(
                    "Could not reverse exchange: {error}; config retained at {}",
                    displaced.display()
                )
            })? {
                return Err(format!(
                    "Could not reverse exchange; config retained at {}",
                    displaced.display()
                ));
            }
            // If a client saved again after our swap, retain that second save
            // rather than deleting it along with our rejected output.
            if read(&rollback_displaced)?.text.as_deref() == Some(output) {
                cleanup_published(&rollback_displaced);
            } else {
                return Err(format!(
                    "Client saved again during recovery; additional config retained at {}",
                    rollback_displaced.display()
                ));
            }
            sync_parent(&dest);
            return Err(CHANGED.into());
        }
        cleanup_published(&displaced);
    } else {
        // No portable compare-and-swap on unsupported filesystems. Check the
        // identity captured from the original open handle immediately before
        // rename. A residual external-writer window remains after this check.
        // Removal also has a crash window between dest -> *.conduit-tmp and
        // tombstone verification/restoration below: the client's bytes remain
        // under that temp name while the real path is missing. Temp names alone
        // cannot distinguish tombstones from pending output, so do not sweep
        // them back automatically.
        if identity(&dest)? != expected.identity {
            return Err(CHANGED.into());
        }
        fs::rename(&tmp, &dest).map_err(|e| e.to_string())?;
        cleanup.disarm();
    }
    sync_parent(&dest);
    Ok(())
}

fn remove(dest: &Path, expected: &Revision) -> Result<(), String> {
    if expected.text.is_none() {
        return if identity(dest)?.is_none() {
            Ok(())
        } else {
            Err(CHANGED.into())
        };
    }
    let tombstone = sibling(dest);
    #[cfg(test)]
    hook("remove", dest);
    fs::rename(dest, &tombstone).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            CHANGED.into()
        } else {
            e.to_string()
        }
    })?;
    // No cleanup guard: unreadable or conflicting bytes must stay recoverable.
    if !read(&tombstone).is_ok_and(|old| old.text == expected.text) {
        // A no-clobber restore also preserves a new save at the original path.
        #[cfg(test)]
        hook("restore", dest);
        match publish_no_clobber(&tombstone, dest) {
            Ok(()) => {
                cleanup_published(&tombstone);
            }
            Err(e) => {
                return Err(format!(
                    "Client config changed during removal: {e}; config retained at {}",
                    tombstone.display()
                ))
            }
        }
        sync_parent(dest);
        return Err(CHANGED.into());
    }
    cleanup_published(&tombstone);
    sync_parent(dest);
    Ok(())
}

#[cfg(test)]
thread_local! {
    pub(crate) static HOOK: std::cell::RefCell<Option<(&'static str, Box<dyn FnOnce(&Path)>)>> = const { std::cell::RefCell::new(None) };
    pub(crate) static FORCE_NO_HARD_LINKS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    pub(crate) static FORCE_NO_RENAME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    pub(crate) static FORCE_CLEANUP_ERROR: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    pub(crate) static FORCE_FALLBACK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
#[cfg(test)]
fn hook(point: &str, path: &Path) {
    let callback = HOOK.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.as_ref().is_some_and(|(at, _)| *at == point) {
            slot.take().map(|(_, callback)| callback)
        } else {
            None
        }
    });
    if let Some(callback) = callback {
        callback(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "toolport-client-commit-{}-{}",
            std::process::id(),
            ATOMIC_WRITE_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        fs::write(&path, "original").unwrap();
        (dir, path)
    }
    pub(crate) fn replace(path: &Path, text: &str) {
        let tmp = sibling(path);
        fs::write(&tmp, text).unwrap();
        #[cfg(windows)]
        fs::remove_file(path).unwrap();
        fs::rename(tmp, path).unwrap();
    }
    struct ForcedUnsupported;
    impl ForcedUnsupported {
        fn new(no_rename: bool) -> Self {
            FORCE_NO_HARD_LINKS.with(|slot| slot.set(true));
            FORCE_NO_RENAME.with(|slot| slot.set(no_rename));
            Self
        }
    }
    impl Drop for ForcedUnsupported {
        fn drop(&mut self) {
            FORCE_NO_HARD_LINKS.with(|slot| slot.set(false));
            FORCE_NO_RENAME.with(|slot| slot.set(false));
            FORCE_CLEANUP_ERROR.with(|slot| slot.set(false));
        }
    }
    #[cfg(unix)]
    #[test]
    fn enotsup_is_an_unsupported_operation() {
        assert!(unsupported(&std::io::Error::from_raw_os_error(
            libc::ENOTSUP
        )));
        assert!(unsupported(&std::io::Error::from_raw_os_error(
            libc::EOPNOTSUPP
        )));
        assert!(!unsupported(&std::io::Error::from_raw_os_error(
            libc::EACCES
        )));
        #[cfg(target_os = "macos")]
        assert_eq!(libc::ENOTSUP, 45);
    }
    #[test]
    fn first_publication_without_hard_links_creates_the_real_path() {
        for no_rename in [false, true] {
            let _forced = ForcedUnsupported::new(no_rename);
            let (dir, path) = fixture();
            fs::remove_file(&path).unwrap();
            let revision = read(&path).unwrap();
            commit(&path, &revision, Some("toolport edit")).unwrap();
            assert_eq!(fs::read_to_string(&path).unwrap(), "toolport edit");
            assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
            fs::remove_dir_all(dir).unwrap();
        }
    }
    #[test]
    fn first_publication_without_hard_links_preserves_a_new_native_save() {
        for no_rename in [false, true] {
            let _forced = ForcedUnsupported::new(no_rename);
            let (dir, path) = fixture();
            fs::remove_file(&path).unwrap();
            let revision = read(&path).unwrap();
            HOOK.with(|slot| {
                *slot.borrow_mut() = Some((
                    "commit",
                    Box::new(|path| fs::write(path, "client save").unwrap()),
                ))
            });
            assert_eq!(
                commit(&path, &revision, Some("toolport edit")).unwrap_err(),
                CHANGED
            );
            assert_eq!(fs::read_to_string(&path).unwrap(), "client save");
            fs::remove_dir_all(dir).unwrap();
        }
    }
    #[test]
    fn removal_without_hard_links_restores_even_non_utf8_native_bytes() {
        for no_rename in [false, true] {
            let _forced = ForcedUnsupported::new(no_rename);
            let (dir, path) = fixture();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
            }
            let revision = read(&path).unwrap();
            HOOK.with(|slot| {
                *slot.borrow_mut() =
                    Some(("remove", Box::new(|path| fs::write(path, [0xff]).unwrap())))
            });
            assert_eq!(commit(&path, &revision, None).unwrap_err(), CHANGED);
            assert_eq!(fs::read(&path).unwrap(), [0xff]);
            assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                    0o640
                );
            }
            fs::remove_dir_all(dir).unwrap();
        }
    }
    #[test]
    fn removal_without_hard_links_keeps_both_native_saves() {
        for no_rename in [false, true] {
            let _forced = ForcedUnsupported::new(no_rename);
            let (dir, path) = fixture();
            let revision = read(&path).unwrap();
            HOOK.with(|slot| {
                *slot.borrow_mut() = Some((
                    "remove",
                    Box::new(|path| {
                        fs::write(path, "first save").unwrap();
                        HOOK.with(|slot| {
                            *slot.borrow_mut() = Some((
                                "restore",
                                Box::new(|path| fs::write(path, "second save").unwrap()),
                            ))
                        });
                    }),
                ))
            });
            assert!(commit(&path, &revision, None)
                .unwrap_err()
                .contains("config retained at"));
            assert_eq!(fs::read_to_string(&path).unwrap(), "second save");
            let tombstone = fs::read_dir(&dir)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .find(|entry| entry != &path)
                .unwrap();
            assert_eq!(fs::read_to_string(tombstone).unwrap(), "first save");
            fs::remove_dir_all(dir).unwrap();
        }
    }
    #[test]
    fn cleanup_failure_does_not_fail_a_published_write_or_removal() {
        let _forced = ForcedUnsupported::new(false);
        for output in [Some("toolport edit"), None] {
            let (dir, path) = fixture();
            let revision = read(&path).unwrap();
            FORCE_CLEANUP_ERROR.with(|slot| slot.set(true));
            commit(&path, &revision, output).unwrap();
            assert_eq!(read(&path).unwrap().text.as_deref(), output);
            assert!(fs::read_dir(&dir)
                .unwrap()
                .any(|entry| entry.unwrap().path() != path));
            fs::remove_dir_all(dir).unwrap();
        }
    }
    #[test]
    fn cleanup_failure_does_not_change_a_recovered_conflict() {
        let _forced = ForcedUnsupported::new(true);
        let (dir, path) = fixture();
        let revision = read(&path).unwrap();
        HOOK.with(|slot| {
            *slot.borrow_mut() = Some((
                "remove",
                Box::new(|path| fs::write(path, "client save").unwrap()),
            ))
        });
        FORCE_CLEANUP_ERROR.with(|slot| slot.set(true));
        assert_eq!(commit(&path, &revision, None).unwrap_err(), CHANGED);
        assert_eq!(fs::read_to_string(&path).unwrap(), "client save");
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn replacement_during_read_does_not_turn_old_handle_into_current_revision() {
        let (dir, path) = fixture();
        HOOK.with(|slot| {
            *slot.borrow_mut() = Some(("read", Box::new(|path| replace(path, "client save"))))
        });
        let revision = read(&path).unwrap();
        assert_eq!(revision.text.as_deref(), Some("original"));
        assert_eq!(
            commit(&path, &revision, Some("toolport edit")).unwrap_err(),
            CHANGED
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "client save");
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn exchange_verifies_displaced_bytes_after_final_check() {
        let (dir, path) = fixture();
        let revision = read(&path).unwrap();
        HOOK.with(|slot| {
            *slot.borrow_mut() = Some(("commit", Box::new(|path| replace(path, "client save"))))
        });
        assert_eq!(
            commit(&path, &revision, Some("toolport edit")).unwrap_err(),
            CHANGED
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "client save");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn replacement_during_final_read_is_detected_by_exchange() {
        let (dir, path) = fixture();
        let revision = read(&path).unwrap();
        HOOK.with(|slot| {
            *slot.borrow_mut() = Some(("read", Box::new(|path| replace(path, "client save"))))
        });
        assert_eq!(
            commit(&path, &revision, Some("toolport edit")).unwrap_err(),
            CHANGED
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "client save");
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn interrupted_native_utf8_save_is_restored_after_exchange() {
        let (dir, path) = fixture();
        let revision = read(&path).unwrap();
        HOOK.with(|slot| {
            *slot.borrow_mut() = Some(("commit", Box::new(|path| fs::write(path, [0xff]).unwrap())))
        });
        assert_eq!(
            commit(&path, &revision, Some("toolport edit")).unwrap_err(),
            CHANGED
        );
        assert_eq!(fs::read(&path).unwrap(), [0xff]);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn fallback_rechecks_identity_after_final_read() {
        let (dir, path) = fixture();
        let revision = read(&path).unwrap();
        HOOK.with(|slot| {
            *slot.borrow_mut() = Some(("commit", Box::new(|path| replace(path, "client save"))))
        });
        FORCE_FALLBACK.with(|slot| slot.set(true));
        let result = commit(&path, &revision, Some("toolport edit"));
        FORCE_FALLBACK.with(|slot| slot.set(false));
        assert_eq!(result.unwrap_err(), CHANGED);
        assert_eq!(fs::read_to_string(&path).unwrap(), "client save");
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn removal_verifies_tombstone_and_restores_client_save() {
        let (dir, path) = fixture();
        let revision = read(&path).unwrap();
        HOOK.with(|slot| {
            *slot.borrow_mut() = Some(("remove", Box::new(|path| replace(path, "client save"))))
        });
        assert_eq!(commit(&path, &revision, None).unwrap_err(), CHANGED);
        assert_eq!(fs::read_to_string(&path).unwrap(), "client save");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn first_publication_never_clobbers_new_client_file() {
        let (dir, path) = fixture();
        fs::remove_file(&path).unwrap();
        let revision = read(&path).unwrap();
        HOOK.with(|slot| {
            *slot.borrow_mut() = Some((
                "commit",
                Box::new(|path| fs::write(path, "client save").unwrap()),
            ))
        });
        assert_eq!(
            commit(&path, &revision, Some("toolport edit")).unwrap_err(),
            CHANGED
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "client save");
        fs::remove_dir_all(dir).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn client_write_preserves_mode_while_data_write_stays_private() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, path) = fixture();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let revision = read(&path).unwrap();
        commit(&path, &revision, Some("toolport edit")).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o644
        );
        let data = dir.join("snapshot");
        super::super::atomic_write(&data, "private").unwrap();
        assert_eq!(
            fs::metadata(data).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
