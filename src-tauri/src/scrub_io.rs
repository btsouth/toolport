//! File access for the secret scrubber that stays inside the data directory.
//!
//! Every file is opened without following a final symlink or reparse point and
//! must be a regular file whose canonical parent is the data directory. Rewrites
//! stream line by line into a temp file that is never more permissive than the
//! original, and same-length masking is journaled so a crash mid-mask is finished
//! at the next start. Callers hold whatever locks the file needs.

use std::borrow::Cow;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Why a file was not touched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    NotFound,
    Symlink,
    Outside,
    NotRegular,
}

const JOURNAL_PREFIX: &str = ".mask-journal-";
const JOURNAL_TMP_SUFFIX: &str = ".tmp";
const JOURNAL_HEADER: &str = "toolport-mask-journal 1";
const JOURNAL_END: &str = "end";

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Open `path` for reading without following a final symlink or reparse point,
/// and confirm the opened file is a regular file whose canonical parent is `dir`
/// (canonicalized).
pub fn open_inside(dir: &Path, path: &Path) -> std::io::Result<Result<File, Skip>> {
    locate(dir, path)?.and_then_io(|loc| open_located(&loc, false))
}

/// Stream `path` line by line through `redact` into a temp file and publish it
/// by rename when something changed. See the module docs for the guarantees.
pub fn rewrite_lines(
    dir: &Path,
    path: &Path,
    redact: &mut dyn FnMut(&str) -> Cow<'_, str>,
    skipped: &mut dyn FnMut(Skip),
    on_multi_link: &mut dyn FnMut() -> std::io::Result<bool>,
) -> std::io::Result<bool> {
    rewrite_stream(
        dir,
        path,
        &mut |line, out| {
            if let Some(line) = line {
                match redact(&String::from_utf8_lossy(line)) {
                    Cow::Borrowed(_) => out.extend_from_slice(line),
                    Cow::Owned(new) => out.extend_from_slice(new.as_bytes()),
                }
            }
        },
        skipped,
        on_multi_link,
    )
}

/// How many directory entries name the open file `file` (hard links).
pub fn link_count(file: &File) -> std::io::Result<u64> {
    Ok(sys::ident(file)?.links)
}

/// Whether a stream's output differs from its input, compared as both are
/// produced (the output may lag behind, holding text back).
#[derive(Default)]
pub struct Diff {
    pending: Vec<u8>,
    changed: bool,
}

impl Diff {
    pub fn input(&mut self, bytes: &[u8]) {
        if !self.changed {
            self.pending.extend_from_slice(bytes);
        }
    }

    pub fn output(&mut self, bytes: &[u8]) {
        if self.changed {
            return;
        }
        if self.pending.starts_with(bytes) {
            self.pending.drain(..bytes.len());
        } else {
            self.changed = true;
            self.pending = Vec::new();
        }
    }

    /// Whether the output already differs from the input so far.
    pub fn diverged(&self) -> bool {
        self.changed
    }

    /// At the end of both: whether they differ.
    pub fn changed(&self) -> bool {
        self.changed || !self.pending.is_empty()
    }
}

/// A stateful line redactor for [`rewrite_stream`].
pub type StreamRedact<'a> = dyn FnMut(Option<&[u8]>, &mut Vec<u8>) + 'a;

/// [`rewrite_lines`] for a stateful redactor: `redact` gets each line (with
/// its `\n`) and appends whatever output is ready, which may lag behind and
/// may span lines, then gets `None` at the end to write the rest.
pub fn rewrite_stream(
    dir: &Path,
    path: &Path,
    redact: &mut StreamRedact<'_>,
    skipped: &mut dyn FnMut(Skip),
    on_multi_link: &mut dyn FnMut() -> std::io::Result<bool>,
) -> std::io::Result<bool> {
    let loc = match locate(dir, path)? {
        Ok(loc) => loc,
        Err(skip) => {
            skipped(skip);
            return Ok(false);
        }
    };
    let source = match open_located(&loc, false)? {
        Ok(file) => file,
        Err(skip) => {
            skipped(skip);
            return Ok(false);
        }
    };
    let original = sys::ident(&source)?;
    if original.links > 1 {
        // A rename would leave the old content alive under the other names.
        drop(source);
        return on_multi_link();
    }

    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp_name = format!(
        ".{}.scrub-{}-{}.tmp",
        loc.name.to_string_lossy(),
        std::process::id(),
        seq
    );
    let tmp_path = loc.dir.join(tmp_name);
    let mut guard = TempGuard {
        path: Some(tmp_path.clone()),
    };
    let tmp = sys::create_temp_like(&tmp_path, &source)?;

    let mut diff = Diff::default();
    {
        let mut reader = BufReader::with_capacity(64 * 1024, &source);
        let mut writer = BufWriter::with_capacity(64 * 1024, &tmp);
        let mut line = Vec::new();
        let mut out = Vec::new();
        loop {
            line.clear();
            let read = reader.read_until(b'\n', &mut line)?;
            out.clear();
            if read == 0 {
                redact(None, &mut out);
            } else {
                diff.input(&line);
                redact(Some(&line), &mut out);
            }
            diff.output(&out);
            writer.write_all(&out)?;
            if read == 0 {
                break;
            }
        }
        writer.flush()?;
    }
    drop(source);
    if !diff.changed() {
        drop(tmp);
        return Ok(false);
    }
    tmp.sync_all()?;
    drop(tmp);

    // The file changed while it was read (or was replaced): keep the newer one.
    let still_same = match open_located(&loc, false)? {
        Ok(now) => sys::ident(&now)? == original,
        Err(_) => false,
    };
    if !still_same {
        return Ok(false);
    }
    std::fs::rename(&tmp_path, loc.target())?;
    guard.path = None;
    sync_dir(&loc.dir)?;
    Ok(true)
}

/// Crash-safe same-length masking of byte ranges in `path`. Writes a journal
/// holding the file name, the file's identity and the ranges (never content),
/// masks each range, fsyncs, then removes the journal.
pub fn mask_ranges(
    dir: &Path,
    path: &Path,
    ranges: &[(u64, u64)],
    mask_for: &dyn Fn(usize) -> Vec<u8>,
) -> std::io::Result<bool> {
    let loc = match locate(dir, path)? {
        Ok(loc) => loc,
        Err(_) => return Ok(false),
    };
    let file = match open_located(&loc, true)? {
        Ok(file) => file,
        Err(_) => return Ok(false),
    };
    let ident = sys::ident(&file)?;
    if ident.links > 1 {
        return Err(multi_link_error(&loc.name.to_string_lossy()));
    }
    let size = ident.size;
    let ranges: Vec<(u64, u64)> = ranges
        .iter()
        .copied()
        .filter(|&(offset, len)| len > 0 && offset.checked_add(len).is_some_and(|e| e <= size))
        .collect();
    if ranges.is_empty() {
        return Ok(false);
    }
    let journal = loc.dir.join(journal_name(&loc.name));
    write_journal(&loc, &journal, ident.id, &ranges)?;
    for &(offset, len) in &ranges {
        let len = usize::try_from(len).map_err(std::io::Error::other)?;
        let mut mask = mask_for(len);
        mask.resize(len, b'*');
        write_at(&file, &mask, offset)?;
    }
    file.sync_all()?;
    drop(file);
    std::fs::remove_file(&journal)?;
    sync_dir(&loc.dir)?;
    Ok(true)
}

/// Finish any masking a crash interrupted: re-apply each leftover journal's
/// ranges with `*` bytes (idempotent) and delete it. Returns how many were
/// completed.
pub fn recover_masks(dir: &Path) -> std::io::Result<usize> {
    let canonical = match std::fs::canonicalize(dir) {
        Ok(path) => path,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let mut completed = 0;
    let mut blocked = None;
    for entry in std::fs::read_dir(&canonical)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(target) = name.strip_prefix(JOURNAL_PREFIX) else {
            continue;
        };
        let journal = canonical.join(name);
        if !entry.file_type()?.is_file() {
            continue;
        }
        // An unpublished journal means masking never started.
        if target.ends_with(JOURNAL_TMP_SUFFIX) {
            remove_if_present(&journal)?;
            continue;
        }
        // Validate the whole journal first so a torn or foreign one changes
        // nothing, then stream its ranges again to apply them. The journal is
        // only removed after the masks are synced, so a crash here is finished
        // by the next start.
        if let Some((file_name, id)) = validate_journal(&canonical, &journal)? {
            if file_name == target {
                match apply_recovered(&canonical, &journal, &file_name, id)? {
                    Some(true) => completed += 1,
                    Some(false) => {}
                    // Another name shares the file: masking it would change that
                    // file too. The journal stays so the cleanup stays unfinished.
                    None => {
                        blocked.get_or_insert_with(|| multi_link_error(&file_name));
                        continue;
                    }
                }
            }
        }
        remove_if_present(&journal)?;
    }
    if completed > 0 {
        sync_dir(&canonical)?;
    }
    blocked.map_or(Ok(completed), Err)
}

/// The error for a log the scrub would not change because it has other hard
/// links (names only).
pub fn multi_link_error(name: &str) -> std::io::Error {
    std::io::Error::other(format!(
        "{name} has other hard links, so the secret scrub left it unchanged; remove the other links to it (or the file) and the cleanup finishes on the next start"
    ))
}

/// Call `f(offset, line_bytes)` for each line of `path` (split on `\n`, which is
/// kept), reading through [`open_inside`] with memory bounded by the longest line.
pub fn for_each_line(
    dir: &Path,
    path: &Path,
    f: &mut dyn FnMut(u64, &[u8]),
) -> std::io::Result<Result<(), Skip>> {
    let file = match open_inside(dir, path)? {
        Ok(file) => file,
        Err(skip) => return Ok(Err(skip)),
    };
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    let mut line = Vec::new();
    let mut offset = 0u64;
    loop {
        line.clear();
        let n = reader.read_until(b'\n', &mut line)?;
        if n == 0 {
            break;
        }
        f(offset, &line);
        offset += n as u64;
    }
    Ok(Ok(()))
}

/// A file name inside a canonical directory.
struct Located {
    dir: PathBuf,
    name: std::ffi::OsString,
}

impl Located {
    fn target(&self) -> PathBuf {
        self.dir.join(&self.name)
    }
}

trait AndThenIo<T> {
    fn and_then_io<U>(
        self,
        f: impl FnOnce(T) -> std::io::Result<Result<U, Skip>>,
    ) -> std::io::Result<Result<U, Skip>>;
}

impl<T> AndThenIo<T> for Result<T, Skip> {
    fn and_then_io<U>(
        self,
        f: impl FnOnce(T) -> std::io::Result<Result<U, Skip>>,
    ) -> std::io::Result<Result<U, Skip>> {
        match self {
            Ok(value) => f(value),
            Err(skip) => Ok(Err(skip)),
        }
    }
}

/// Resolve `path` to (canonical dir, file name) and reject anything whose
/// canonical parent is not the canonical `dir`. The final component is never
/// resolved, so a symlink there is caught by the no-follow open.
fn locate(dir: &Path, path: &Path) -> std::io::Result<Result<Located, Skip>> {
    let Some(name) = path.file_name() else {
        return Ok(Err(Skip::Outside));
    };
    let name_str = name.to_string_lossy();
    if name_str.contains(['/', '\\', ':']) {
        return Ok(Err(Skip::Outside));
    }
    let canonical_dir = match std::fs::canonicalize(dir) {
        Ok(path) => path,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Err(Skip::NotFound)),
        Err(e) => return Err(e),
    };
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let canonical_parent = match std::fs::canonicalize(parent) {
        Ok(path) => path,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Err(Skip::Outside)),
        Err(e) => return Err(e),
    };
    if !same_path(&canonical_parent, &canonical_dir) {
        return Ok(Err(Skip::Outside));
    }
    Ok(Ok(Located {
        dir: canonical_dir,
        name: name.to_os_string(),
    }))
}

fn same_path(a: &Path, b: &Path) -> bool {
    #[cfg(windows)]
    {
        a.as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&b.as_os_str().to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}

fn open_located(loc: &Located, write: bool) -> std::io::Result<Result<File, Skip>> {
    let file = match sys::open_nofollow(loc, write)? {
        Ok(file) => file,
        Err(skip) => return Ok(Err(skip)),
    };
    let meta = file.metadata()?;
    if sys::is_reparse(&meta) {
        return Ok(Err(Skip::Symlink));
    }
    if !meta.file_type().is_file() {
        return Ok(Err(Skip::NotRegular));
    }
    if !sys::parent_is(&file, &loc.dir)? {
        return Ok(Err(Skip::Outside));
    }
    Ok(Ok(file))
}

/// Removes the temp file unless it was published.
struct TempGuard {
    path: Option<PathBuf>,
}

impl Drop for TempGuard {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn remove_if_present(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// A pending mask journal for `path`, as a crash after publishing it leaves.
#[cfg(test)]
pub(crate) fn journal_for_test(dir: &Path, path: &Path, ranges: &[(u64, u64)]) {
    let loc = locate(dir, path).unwrap().unwrap();
    let ident = sys::ident(&File::open(path).unwrap()).unwrap();
    let journal = loc.dir.join(journal_name(&loc.name));
    write_journal(&loc, &journal, ident.id, ranges).unwrap();
}

/// Create an empty marker file at `path`, owner-only, never through a link:
/// an existing name (a file, or a link planted there) is left as it is.
pub fn create_marker(path: &Path) -> std::io::Result<()> {
    match sys::create_private(path) {
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        other => other.map(drop),
    }
}

fn journal_name(file_name: &std::ffi::OsStr) -> String {
    format!("{JOURNAL_PREFIX}{}", file_name.to_string_lossy())
}

fn write_journal(
    loc: &Located,
    journal: &Path,
    id: (u64, u64),
    ranges: &[(u64, u64)],
) -> std::io::Result<()> {
    let tmp = PathBuf::from(format!("{}{JOURNAL_TMP_SUFFIX}", journal.display()));
    remove_if_present(&tmp)?;
    let mut guard = TempGuard {
        path: Some(tmp.clone()),
    };
    {
        let mut out = BufWriter::with_capacity(64 * 1024, sys::create_private(&tmp)?);
        write!(
            out,
            "{JOURNAL_HEADER}\n{}\n{} {}\n",
            loc.name.to_string_lossy(),
            id.0,
            id.1
        )?;
        for (offset, len) in ranges {
            writeln!(out, "{offset} {len}")?;
        }
        writeln!(out, "{JOURNAL_END}")?;
        out.into_inner().map_err(|e| e.into_error())?.sync_all()?;
    }
    std::fs::rename(&tmp, journal)?;
    guard.path = None;
    sync_dir(&loc.dir)
}

/// Longest journal line accepted; real lines are a file name or two numbers.
const JOURNAL_LINE_MAX: u64 = 64 * 1024;

/// A journal's file name and the identity of the file it masks.
type JournalHeader = (String, (u64, u64));

enum JournalEntry {
    Range(u64, u64),
    End,
}

/// Streams a journal line by line so recovery memory does not grow with it.
struct JournalReader {
    reader: BufReader<File>,
    line: Vec<u8>,
}

impl JournalReader {
    /// Open a journal and parse its header (file name and identity). A torn or
    /// foreign header yields `None`.
    fn open(dir: &Path, journal: &Path) -> std::io::Result<Option<(Self, JournalHeader)>> {
        let file = match open_inside(dir, journal)? {
            Ok(file) => file,
            Err(_) => return Ok(None),
        };
        let mut reader = JournalReader {
            reader: BufReader::with_capacity(64 * 1024, file),
            line: Vec::new(),
        };
        if reader.next_line()? != Some(JOURNAL_HEADER) {
            return Ok(None);
        }
        let Some(name) = reader.next_line()?.map(str::to_string) else {
            return Ok(None);
        };
        let Some(id) = reader.next_line()?.and_then(parse_pair) else {
            return Ok(None);
        };
        Ok(Some((reader, (name, id))))
    }

    /// The next entry, or `None` when the journal is torn or malformed.
    fn next_entry(&mut self) -> std::io::Result<Option<JournalEntry>> {
        Ok(match self.next_line()? {
            Some(JOURNAL_END) => Some(JournalEntry::End),
            Some(line) => parse_pair(line).map(|(a, b)| JournalEntry::Range(a, b)),
            None => None,
        })
    }

    /// The next line without its line ending; `None` at end of file or for a
    /// line that is overlong or not UTF-8.
    fn next_line(&mut self) -> std::io::Result<Option<&str>> {
        self.line.clear();
        let n = std::io::Read::take(&mut self.reader, JOURNAL_LINE_MAX)
            .read_until(b'\n', &mut self.line)?;
        if n == 0 {
            return Ok(None);
        }
        let mut line = self.line.as_slice();
        if let Some(rest) = line.strip_suffix(b"\n") {
            line = rest.strip_suffix(b"\r").unwrap_or(rest);
        } else if n as u64 == JOURNAL_LINE_MAX {
            return Ok(None);
        }
        Ok(std::str::from_utf8(line).ok())
    }
}

fn parse_pair(line: &str) -> Option<(u64, u64)> {
    let (a, b) = line.split_once(' ')?;
    Some((a.parse().ok()?, b.parse().ok()?))
}

/// Check a whole journal parses through its end marker without holding its
/// ranges; a torn or foreign one yields `None`.
fn validate_journal(dir: &Path, journal: &Path) -> std::io::Result<Option<JournalHeader>> {
    let Some((mut reader, (name, id))) = JournalReader::open(dir, journal)? else {
        return Ok(None);
    };
    loop {
        match reader.next_entry()? {
            Some(JournalEntry::Range(..)) => {}
            Some(JournalEntry::End) => return Ok(Some((name, id))),
            None => return Ok(None),
        }
    }
}

/// Re-apply a validated journal's ranges to `file_name` with `*` bytes,
/// streaming them from the journal. Nothing is written unless the file still
/// has the journaled identity; `None` (nothing written) when it has other hard
/// links.
fn apply_recovered(
    dir: &Path,
    journal: &Path,
    file_name: &str,
    id: (u64, u64),
) -> std::io::Result<Option<bool>> {
    let loc = match locate(dir, &dir.join(file_name))? {
        Ok(loc) => loc,
        Err(_) => return Ok(Some(false)),
    };
    let file = match open_located(&loc, true)? {
        Ok(file) => file,
        Err(_) => return Ok(Some(false)),
    };
    let ident = sys::ident(&file)?;
    // A different file now has this name (rotated or replaced): leave it alone.
    if ident.id != id {
        return Ok(Some(false));
    }
    if ident.links > 1 {
        return Ok(None);
    }
    let Some((mut reader, (name, journal_id))) = JournalReader::open(dir, journal)? else {
        return Ok(Some(false));
    };
    if name != file_name || journal_id != id {
        return Ok(Some(false));
    }
    let mut mask = Vec::new();
    let complete = loop {
        match reader.next_entry()? {
            Some(JournalEntry::Range(offset, len)) => {
                let Some(end) = offset.checked_add(len) else {
                    continue;
                };
                if len == 0 || end > ident.size {
                    continue;
                }
                let len = usize::try_from(len).map_err(std::io::Error::other)?;
                if mask.len() < len {
                    mask.resize(len, b'*');
                }
                write_at(&file, &mask[..len], offset)?;
            }
            Some(JournalEntry::End) => break true,
            None => break false,
        }
    };
    file.sync_all()?;
    Ok(Some(complete))
}

#[cfg(unix)]
fn write_at(file: &File, bytes: &[u8], offset: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(file, bytes, offset)
}

#[cfg(windows)]
fn write_at(file: &File, mut bytes: &[u8], mut offset: u64) -> std::io::Result<()> {
    while !bytes.is_empty() {
        let n = std::os::windows::fs::FileExt::seek_write(file, bytes, offset)?;
        if n == 0 {
            return Err(std::io::ErrorKind::WriteZero.into());
        }
        bytes = &bytes[n..];
        offset += n as u64;
    }
    Ok(())
}

#[cfg(unix)]
fn sync_dir(dir: &Path) -> std::io::Result<()> {
    File::open(dir)?.sync_all()
}

/// Windows cannot flush a directory handle opened by std; NTFS journals the
/// rename itself.
#[cfg(windows)]
fn sync_dir(_dir: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Identity and state of an open file, compared to detect concurrent change.
#[derive(Debug, PartialEq, Eq)]
struct Ident {
    /// (device, inode) on Unix; (volume serial, file index) on Windows.
    id: (u64, u64),
    size: u64,
    links: u64,
    /// Modification and status-change times in nanoseconds where available.
    times: (i128, i128),
}

#[cfg(unix)]
mod sys {
    use super::{Ident, Located, Skip};
    use std::fs::File;
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    use std::path::Path;

    pub(super) fn open_nofollow(loc: &Located, write: bool) -> std::io::Result<Result<File, Skip>> {
        let dir = match std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(&loc.dir)
        {
            Ok(dir) => dir,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Err(Skip::NotFound)),
            Err(e) => return Err(e),
        };
        let name = std::ffi::CString::new(loc.name.as_bytes())
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
        let access = if write { libc::O_RDWR } else { libc::O_RDONLY };
        // O_NONBLOCK keeps a FIFO from blocking the open; it has no effect on
        // regular files, which are the only ones kept.
        let flags = access | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NOCTTY | libc::O_NONBLOCK;
        use std::os::fd::{AsRawFd as _, FromRawFd as _};
        // SAFETY: `dir` is an open directory descriptor and `name` a valid
        // NUL-terminated string, both alive for the call. openat does not retain
        // either pointer.
        let fd = unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            let err = std::io::Error::last_os_error();
            return match err.raw_os_error() {
                Some(libc::ENOENT) => Ok(Err(Skip::NotFound)),
                // Linux and macOS report ELOOP for a final symlink under
                // O_NOFOLLOW; FreeBSD reports EMLINK.
                Some(libc::ELOOP) | Some(libc::EMLINK) => Ok(Err(Skip::Symlink)),
                Some(libc::EISDIR) | Some(libc::ENXIO) | Some(libc::ENODEV) => {
                    Ok(Err(Skip::NotRegular))
                }
                _ => Err(err),
            };
        }
        // SAFETY: openat just returned this descriptor and nothing else owns it.
        Ok(Ok(unsafe { File::from_raw_fd(fd) }))
    }

    pub(super) fn is_reparse(_meta: &std::fs::Metadata) -> bool {
        false
    }

    /// The descriptor was opened relative to the canonical dir's descriptor,
    /// so its parent is that directory by construction.
    pub(super) fn parent_is(_file: &File, _dir: &Path) -> std::io::Result<bool> {
        Ok(true)
    }

    pub(super) fn ident(file: &File) -> std::io::Result<Ident> {
        let m = file.metadata()?;
        let ns = |s: i64, n: i64| i128::from(s) * 1_000_000_000 + i128::from(n);
        Ok(Ident {
            id: (m.dev(), m.ino()),
            size: m.len(),
            links: m.nlink(),
            times: (ns(m.mtime(), m.mtime_nsec()), ns(m.ctime(), m.ctime_nsec())),
        })
    }

    /// Create the temp file 0600, give it the original's owner when allowed,
    /// then the original's permission bits (minus group bits when the group
    /// could not be matched), all before any content is written.
    pub(super) fn create_temp_like(path: &Path, original: &File) -> std::io::Result<File> {
        let meta = original.metadata()?;
        let tmp = create_private(path)?;
        use std::os::fd::AsRawFd as _;
        // SAFETY: `tmp` is an open descriptor for the duration of the call.
        // Failure (EPERM when not root) is expected and handled below.
        let _ = unsafe { libc::fchown(tmp.as_raw_fd(), meta.uid(), meta.gid()) };
        let now = tmp.metadata()?;
        let mut mode = meta.mode() & 0o777;
        if now.gid() != meta.gid() {
            mode &= !0o070;
        }
        // If the owner could not be matched the temp belongs to us, and we
        // could already read the original, so the owner bits widen nothing.
        tmp.set_permissions(std::os::unix::fs::PermissionsExt::from_mode(mode))?;
        Ok(tmp)
    }

    pub(super) fn create_private(path: &Path) -> std::io::Result<File> {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
    }
}

#[cfg(windows)]
mod sys {
    use super::{Ident, Located, Skip};
    use std::fs::File;
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
    use std::os::windows::io::AsRawHandle as _;
    use std::path::{Path, PathBuf};
    use windows_sys::Win32::Foundation::{LocalFree, GENERIC_WRITE};
    use windows_sys::Win32::Security::Authorization::{
        GetSecurityInfo, SetSecurityInfo, SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::{
        GetSecurityDescriptorControl, ACL, DACL_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SE_DACL_PROTECTED,
        UNPROTECTED_DACL_SECURITY_INFORMATION,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, GetFinalPathNameByHandleW, BY_HANDLE_FILE_INFORMATION,
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, FILE_NAME_NORMALIZED,
        READ_CONTROL, VOLUME_NAME_DOS, WRITE_DAC,
    };

    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_WRITE: u32 = 0x2;
    const FILE_SHARE_DELETE: u32 = 0x4;

    pub(super) fn open_nofollow(loc: &Located, write: bool) -> std::io::Result<Result<File, Skip>> {
        let target = loc.target();
        let mut options = std::fs::OpenOptions::new();
        options
            .read(true)
            .write(write)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            // A live writer holds daemon.log with the default sharing; share all
            // three so neither side blocks the other.
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE);
        match options.open(&target) {
            Ok(file) => Ok(Ok(file)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Err(Skip::NotFound)),
            Err(e) => match std::fs::symlink_metadata(&target) {
                Ok(meta) if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 => {
                    Ok(Err(Skip::Symlink))
                }
                Ok(meta) if !meta.is_file() => Ok(Err(Skip::NotRegular)),
                _ => Err(e),
            },
        }
    }

    pub(super) fn is_reparse(meta: &std::fs::Metadata) -> bool {
        meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }

    pub(super) fn final_path(file: &File) -> std::io::Result<PathBuf> {
        use std::os::windows::ffi::OsStringExt as _;
        let mut buf = vec![0u16; 512];
        loop {
            // SAFETY: the handle is open for the call and `buf` is writable for
            // `buf.len()` u16s. The call writes at most that many.
            let n = unsafe {
                GetFinalPathNameByHandleW(
                    file.as_raw_handle(),
                    buf.as_mut_ptr(),
                    buf.len() as u32,
                    FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
                )
            } as usize;
            if n == 0 {
                return Err(std::io::Error::last_os_error());
            }
            if n < buf.len() {
                buf.truncate(n);
                return Ok(PathBuf::from(std::ffi::OsString::from_wide(&buf)));
            }
            // Too small: `n` is the required size including the terminator.
            buf.resize(n + 1, 0);
        }
    }

    pub(super) fn parent_is(file: &File, dir: &Path) -> std::io::Result<bool> {
        let path = final_path(file)?;
        Ok(path.parent().is_some_and(|p| super::same_path(p, dir)))
    }

    pub(super) fn ident(file: &File) -> std::io::Result<Ident> {
        // SAFETY: zeroed is a valid value for this plain C struct.
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: the handle is open and `info` is a valid out pointer.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let join = |hi: u32, lo: u32| (u64::from(hi) << 32) | u64::from(lo);
        let write = join(
            info.ftLastWriteTime.dwHighDateTime,
            info.ftLastWriteTime.dwLowDateTime,
        );
        Ok(Ident {
            id: (
                u64::from(info.dwVolumeSerialNumber),
                join(info.nFileIndexHigh, info.nFileIndexLow),
            ),
            size: join(info.nFileSizeHigh, info.nFileSizeLow),
            links: u64::from(info.nNumberOfLinks),
            times: (i128::from(write), 0),
        })
    }

    /// Create the temp file, copy the original's DACL (and whether it is
    /// protected from inheritance) onto it, and its read-only flag, all before
    /// any content is written.
    pub(super) fn create_temp_like(path: &Path, original: &File) -> std::io::Result<File> {
        let tmp = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .access_mode(GENERIC_WRITE | WRITE_DAC | READ_CONTROL)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        copy_dacl(original, &tmp)?;
        Ok(tmp)
    }

    fn copy_dacl(from: &File, to: &File) -> std::io::Result<()> {
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `from` is open with read access (which includes READ_CONTROL);
        // the out pointers are valid. On success `sd` owns the buffer `dacl`
        // points into and is freed with LocalFree below.
        let err = unsafe {
            GetSecurityInfo(
                from.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut dacl,
                std::ptr::null_mut(),
                &mut sd,
            )
        };
        if err != 0 {
            return Err(std::io::Error::from_raw_os_error(err as i32));
        }
        let mut control = 0u16;
        let mut revision = 0u32;
        // SAFETY: `sd` is the descriptor GetSecurityInfo returned.
        let ok = unsafe { GetSecurityDescriptorControl(sd, &mut control, &mut revision) };
        let protection = if ok != 0 && control & SE_DACL_PROTECTED != 0 {
            PROTECTED_DACL_SECURITY_INFORMATION
        } else {
            UNPROTECTED_DACL_SECURITY_INFORMATION
        };
        // SAFETY: `to` was opened with WRITE_DAC; `dacl` stays valid until `sd`
        // is freed after the call.
        let err = unsafe {
            SetSecurityInfo(
                to.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | protection,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                dacl,
                std::ptr::null(),
            )
        };
        // SAFETY: `sd` was allocated by GetSecurityInfo and is freed once.
        unsafe { LocalFree(sd) };
        if err != 0 {
            return Err(std::io::Error::from_raw_os_error(err as i32));
        }
        Ok(())
    }

    pub(super) fn create_private(path: &Path) -> std::io::Result<File> {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "toolport-scrub-io-{tag}-{}-{seq}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            TempDir(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn redact_secret(line: &str) -> Cow<'_, str> {
        if line.contains("SECRET") {
            Cow::Owned(line.replace("SECRET", "[redacted]"))
        } else {
            Cow::Borrowed(line)
        }
    }

    /// Pins a closure to the higher-ranked signature `rewrite_lines` takes.
    fn higher_ranked<F: FnMut(&str) -> Cow<'_, str>>(f: F) -> F {
        f
    }

    fn rewrite(dir: &Path, path: &Path) -> (std::io::Result<bool>, Vec<Skip>, usize) {
        let mut skips = Vec::new();
        let mut multi = 0;
        let result = rewrite_lines(
            dir,
            path,
            &mut redact_secret,
            &mut |s| skips.push(s),
            &mut || {
                multi += 1;
                Ok(false)
            },
        );
        (result, skips, multi)
    }

    fn leftovers(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with('.'))
            .collect()
    }

    #[cfg(unix)]
    fn try_symlink(src: &Path, dst: &Path) -> bool {
        std::os::unix::fs::symlink(src, dst).is_ok()
    }

    #[cfg(windows)]
    fn try_symlink(src: &Path, dst: &Path) -> bool {
        std::os::windows::fs::symlink_file(src, dst).is_ok()
    }

    #[test]
    fn rewrites_and_redacts_lines() {
        let dir = TempDir::new("basic");
        let path = dir.0.join("a.log");
        std::fs::write(&path, b"ok\nhas SECRET here\n\xff bad SECRET\ntail").unwrap();
        let (result, skips, multi) = rewrite(&dir.0, &path);
        assert!(result.unwrap());
        assert!(skips.is_empty());
        assert_eq!(multi, 0);
        let out = std::fs::read(&path).unwrap();
        assert_eq!(
            out,
            "ok\nhas [redacted] here\n\u{fffd} bad [redacted]\ntail".as_bytes()
        );
        assert!(leftovers(&dir.0).is_empty());
    }

    #[test]
    fn unchanged_lines_keep_original_bytes() {
        let dir = TempDir::new("bytes");
        let path = dir.0.join("a.log");
        std::fs::write(&path, b"\xff\xfe raw\nSECRET\n").unwrap();
        assert!(rewrite(&dir.0, &path).0.unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), b"\xff\xfe raw\n[redacted]\n");
    }

    #[test]
    fn symlink_to_outside_file_is_skipped() {
        let dir = TempDir::new("link");
        let outside = TempDir::new("link-outside");
        let target = outside.0.join("victim.log");
        std::fs::write(&target, "SECRET\n").unwrap();
        let link = dir.0.join("a.log");
        if !try_symlink(&target, &link) {
            eprintln!("symlinks not permitted; skipping");
            return;
        }
        let (result, skips, _) = rewrite(&dir.0, &link);
        assert!(!result.unwrap());
        assert_eq!(skips, vec![Skip::Symlink]);
        assert!(matches!(
            open_inside(&dir.0, &link).unwrap(),
            Err(Skip::Symlink)
        ));
        assert!(!mask_ranges(&dir.0, &link, &[(0, 3)], &|n| vec![b'#'; n]).unwrap());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "SECRET\n");
    }

    #[test]
    fn file_outside_dir_is_rejected() {
        let dir = TempDir::new("outside");
        let other = TempDir::new("outside-other");
        let path = other.0.join("a.log");
        std::fs::write(&path, "SECRET\n").unwrap();
        let (result, skips, _) = rewrite(&dir.0, &path);
        assert!(!result.unwrap());
        assert_eq!(skips, vec![Skip::Outside]);
        let dotted = dir
            .0
            .join("..")
            .join(other.0.file_name().unwrap())
            .join("a.log");
        assert!(matches!(
            open_inside(&dir.0, &dotted).unwrap(),
            Err(Skip::Outside)
        ));
        assert!(matches!(
            for_each_line(&dir.0, &path, &mut |_, _| {}).unwrap(),
            Err(Skip::Outside)
        ));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "SECRET\n");
    }

    #[test]
    fn missing_and_directory_are_skipped() {
        let dir = TempDir::new("kinds");
        let missing = dir.0.join("gone.log");
        assert_eq!(rewrite(&dir.0, &missing).1, vec![Skip::NotFound]);
        let sub = dir.0.join("sub");
        std::fs::create_dir(&sub).unwrap();
        assert_eq!(rewrite(&dir.0, &sub).1, vec![Skip::NotRegular]);
    }

    #[cfg(unix)]
    #[test]
    fn mode_is_preserved() {
        use std::os::unix::fs::PermissionsExt as _;
        for mode in [0o640, 0o600] {
            let dir = TempDir::new("mode");
            let path = dir.0.join("a.log");
            std::fs::write(&path, "SECRET\n").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            assert!(rewrite(&dir.0, &path).0.unwrap());
            let now = std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777;
            assert_eq!(now, mode);
        }
    }

    #[test]
    fn unchanged_file_is_not_rewritten() {
        let dir = TempDir::new("same");
        let path = dir.0.join("a.log");
        std::fs::write(&path, "clean\nlines\n").unwrap();
        let before = sys::ident(&File::open(&path).unwrap()).unwrap();
        assert!(!rewrite(&dir.0, &path).0.unwrap());
        let after = sys::ident(&File::open(&path).unwrap()).unwrap();
        assert_eq!(before, after);
        assert!(leftovers(&dir.0).is_empty());
    }

    #[test]
    fn file_changed_meanwhile_is_not_published() {
        let dir = TempDir::new("race");
        let path = dir.0.join("a.log");
        std::fs::write(&path, "SECRET\n").unwrap();
        let appender = path.clone();
        // Append once: the streaming reader sees the appended line too, so
        // appending on every call would never reach the end of the file.
        let mut appended = false;
        let mut redact = higher_ranked(|line: &str| {
            if !appended {
                appended = true;
                let mut f = crate::registry::open_append_private(&appender).unwrap();
                f.write_all(b"appended\n").unwrap();
            }
            Cow::Owned(line.replace("SECRET", "x"))
        });
        let result = rewrite_lines(&dir.0, &path, &mut redact, &mut |_| {}, &mut || Ok(true));
        assert!(!result.unwrap());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("SECRET\nappended\n"));
        assert!(leftovers(&dir.0).is_empty());
    }

    #[test]
    fn multi_link_file_goes_to_on_multi_link() {
        let dir = TempDir::new("links");
        let path = dir.0.join("a.log");
        let other = dir.0.join("b.log");
        std::fs::write(&path, "SECRET\n").unwrap();
        std::fs::hard_link(&path, &other).unwrap();
        let (result, _, multi) = rewrite(&dir.0, &path);
        assert!(!result.unwrap());
        assert_eq!(multi, 1);
        // No rename happened: both names are still the same file.
        let a = sys::ident(&File::open(&path).unwrap()).unwrap();
        let b = sys::ident(&File::open(&other).unwrap()).unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(std::fs::read_to_string(&other).unwrap(), "SECRET\n");
        assert!(leftovers(&dir.0).is_empty());
    }

    #[test]
    fn mask_ranges_masks_and_removes_journal() {
        let dir = TempDir::new("mask");
        let path = dir.0.join("daemon.log");
        std::fs::write(&path, "key=SECRET1 and SECRET2\n").unwrap();
        let masked = mask_ranges(&dir.0, &path, &[(4, 7), (16, 7)], &|n| {
            let mut m = b"[r]".to_vec();
            m.truncate(n);
            m
        })
        .unwrap();
        assert!(masked);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "key=[r]**** and [r]****\n"
        );
        assert!(leftovers(&dir.0).is_empty());
        // Ranges past the end are ignored rather than extending the file.
        assert!(!mask_ranges(&dir.0, &path, &[(20, 100)], &|n| vec![b'#'; n]).unwrap());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 24);
    }

    #[test]
    fn leftover_journal_is_completed() {
        let dir = TempDir::new("recover");
        let path = dir.0.join("daemon.log");
        std::fs::write(&path, "key=SECRET1 tail\n").unwrap();
        let loc = locate(&dir.0, &path).unwrap().unwrap();
        let ident = sys::ident(&File::open(&path).unwrap()).unwrap();
        let journal = loc.dir.join(journal_name(&loc.name));
        write_journal(&loc, &journal, ident.id, &[(4, 7)]).unwrap();
        // A torn journal that never got published, and one for a missing file.
        std::fs::write(dir.0.join(".mask-journal-x.log.tmp"), "partial").unwrap();
        std::fs::write(
            dir.0.join(".mask-journal-gone.log"),
            format!("{JOURNAL_HEADER}\ngone.log\n1 2\n0 3\n{JOURNAL_END}\n"),
        )
        .unwrap();
        assert_eq!(recover_masks(&dir.0).unwrap(), 1);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "key=******* tail\n"
        );
        assert!(leftovers(&dir.0).is_empty());
        assert_eq!(recover_masks(&dir.0).unwrap(), 0);
    }

    #[test]
    fn recovery_never_masks_through_a_hard_link() {
        let dir = TempDir::new("recover-link");
        let outside = TempDir::new("recover-link-outside");
        let path = dir.0.join("daemon.log");
        let other = outside.0.join("notes.txt");
        std::fs::write(&path, "key=SECRET1 tail\n").unwrap();
        let loc = locate(&dir.0, &path).unwrap().unwrap();
        let ident = sys::ident(&File::open(&path).unwrap()).unwrap();
        let journal = loc.dir.join(journal_name(&loc.name));
        write_journal(&loc, &journal, ident.id, &[(4, 7)]).unwrap();
        std::fs::hard_link(&path, &other).unwrap();
        let err = recover_masks(&dir.0).unwrap_err();
        assert!(err.to_string().contains("hard links"), "{err}");
        assert_eq!(
            std::fs::read_to_string(&other).unwrap(),
            "key=SECRET1 tail\n"
        );
        // The journal stays, so the cleanup finishes once the link is gone.
        assert!(journal.exists());
        std::fs::remove_file(&other).unwrap();
        assert_eq!(recover_masks(&dir.0).unwrap(), 1);
        assert!(!journal.exists());
    }

    #[test]
    fn journal_for_replaced_file_is_dropped() {
        let dir = TempDir::new("recover-replaced");
        let path = dir.0.join("daemon.log");
        std::fs::write(&path, "abcdef\n").unwrap();
        std::fs::write(
            dir.0.join(".mask-journal-daemon.log"),
            format!("{JOURNAL_HEADER}\ndaemon.log\n0 0\n0 3\n{JOURNAL_END}\n"),
        )
        .unwrap();
        assert_eq!(recover_masks(&dir.0).unwrap(), 0);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "abcdef\n");
        assert!(leftovers(&dir.0).is_empty());
    }

    #[test]
    fn journal_over_16_mib_is_recovered() {
        let dir = TempDir::new("recover-large");
        let path = dir.0.join("daemon.log");
        let size = 4 * 1024 * 1024u64;
        std::fs::write(&path, vec![b'a'; size as usize]).unwrap();
        let loc = locate(&dir.0, &path).unwrap().unwrap();
        let ident = sys::ident(&File::open(&path).unwrap()).unwrap();
        let journal = loc.dir.join(journal_name(&loc.name));
        let ranges: Vec<(u64, u64)> = (0..size / 2).map(|i| (i * 2, 1)).collect();
        write_journal(&loc, &journal, ident.id, &ranges).unwrap();
        assert!(std::fs::metadata(&journal).unwrap().len() > 16 * 1024 * 1024);
        assert_eq!(recover_masks(&dir.0).unwrap(), 1);
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len() as u64, size);
        assert!(bytes
            .iter()
            .enumerate()
            .all(|(i, &b)| b == if i % 2 == 0 { b'*' } else { b'a' }));
        assert!(leftovers(&dir.0).is_empty());
    }

    #[test]
    fn large_file_streams_correctly() {
        let dir = TempDir::new("large");
        let path = dir.0.join("big.log");
        let line = "x".repeat(99) + "\n";
        let mut expected = Vec::with_capacity(21_000_000);
        {
            let mut w = BufWriter::new(File::create(&path).unwrap());
            for i in 0..200_000u32 {
                if i % 1000 == 7 {
                    w.write_all(b"token SECRET end\n").unwrap();
                    expected.extend_from_slice(b"token [redacted] end\n");
                } else {
                    w.write_all(line.as_bytes()).unwrap();
                    expected.extend_from_slice(line.as_bytes());
                }
            }
        }
        assert!(std::fs::metadata(&path).unwrap().len() > 19_000_000);
        let mut lines = 0usize;
        let mut last_offset = 0u64;
        for_each_line(&dir.0, &path, &mut |offset, bytes| {
            assert!(bytes.len() <= 100);
            lines += 1;
            last_offset = offset;
        })
        .unwrap()
        .unwrap();
        assert_eq!(lines, 200_000);
        assert_eq!(last_offset + 100, std::fs::metadata(&path).unwrap().len());
        assert!(rewrite(&dir.0, &path).0.unwrap());
        assert!(std::fs::read(&path).unwrap() == expected);
    }
}
