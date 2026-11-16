//! The host daemon's own record: panics, stderr, and how the last run ended.
//!
//! The daemon is spawned detached, so before this its stderr went nowhere and a
//! crash left only a stale descriptor. Now `daemon.log` in the data directory
//! takes its stderr and every panic with its location, capped at
//! [`MAX_LOG_BYTES`] with one previous file kept as `daemon.log.1`. Each daemon
//! run also leaves a marker in `daemon-runs/` that only a clean exit removes. The
//! next daemon to start finds a marker whose process is gone, records why in
//! `last-daemon-exit.json` for the app, and shows one line in `toolport_status`.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::child_ledger::ProcessId;

pub const LOG_FILE: &str = "daemon.log";
/// Where the next daemon records an unclean end it found, for the app to read.
pub const LAST_EXIT_FILE: &str = "last-daemon-exit.json";
const RUNS_DIR: &str = "daemon-runs";
/// One megabyte of panics and stderr is weeks of a quiet daemon and enough
/// context for a noisy one; the previous file doubles it.
pub const MAX_LOG_BYTES: u64 = 1024 * 1024;

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

pub fn log_path(dir: &Path) -> PathBuf {
    dir.join(LOG_FILE)
}

/// Move a full log aside, replacing the previous one. The caller holds the
/// `daemon.log` lock; the rename also replaces `daemon.log.1`, so it takes that
/// file's lock too (always after `daemon.log`'s; the scrub holds one at a time),
/// and never renames over a file the scrub is masking.
fn rotate_if_full(dir: &Path) {
    let path = log_path(dir);
    if std::fs::metadata(&path).is_ok_and(|meta| meta.len() >= MAX_LOG_BYTES) {
        let previous = dir.join(format!("{LOG_FILE}.1"));
        if let Ok(_previous) = crate::registry::lock_at(&previous) {
            let _ = std::fs::rename(&path, previous);
        }
    }
}

/// `daemon.log`, rotated if full and opened for append, with its lock: the one
/// the secret scrub holds while it masks the file. `None` when the lock or the
/// file is unavailable; no caller writes without both.
fn open_for_append(dir: &Path) -> Option<(crate::registry::FileLock, std::fs::File)> {
    let _ = std::fs::create_dir_all(dir);
    let lock = crate::registry::lock_at(&log_path(dir)).ok()?;
    rotate_if_full(dir);
    let file = crate::registry::open_append_private(&log_path(dir)).ok()?;
    Some((lock, file))
}

/// The file a newly spawned daemon writes its stderr to. Rotated before each
/// start, so the cap holds across runs. The daemon writes it as its stderr
/// handle, without this lock; the scrub masks it in place for that reason.
pub fn stderr_file(dir: &Path) -> Option<std::fs::File> {
    open_for_append(dir).map(|(_lock, file)| file)
}

/// Append one line stamped with the time and this process id. Redacted under
/// the log's lock, so a secret learned while a scrub masks the file is caught.
pub fn append(dir: &Path, line: &str) {
    if let Some((_lock, mut file)) = open_for_append(dir) {
        let line = crate::secret_redact::redact_log_text(line);
        let _ =
            file.write_all(format!("{} pid={} {line}\n", now_ms(), std::process::id()).as_bytes());
    }
}

/// The relay threads that carry this process's stderr into `daemon.log`.
static STDERR_RELAY: Mutex<Option<Relay>> = Mutex::new(None);

/// A pause long enough for the old timed flush of an unterminated line (kept
/// for tests: text is now written as soon as it cannot be part of a secret).
#[cfg(test)]
const PARTIAL_FLUSH_AFTER: std::time::Duration = std::time::Duration::from_millis(200);
const RELAY_THREADS: [&str; 2] = ["toolport-stderr-read", "toolport-stderr-redact"];

enum RelayMsg {
    Data(Vec<u8>),
    /// Write everything received so far, partial line included, then ack.
    Sync(std::sync::mpsc::Sender<()>),
}

struct Relay {
    done: std::sync::mpsc::Receiver<()>,
    to_writer: std::sync::mpsc::SyncSender<RelayMsg>,
    /// True while the reader holds bytes it has read but not yet handed on.
    busy: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The pipe's read end, to ask how many bytes are still unread.
    read: usize,
    /// The pipe's write end that stands in as this process's stderr handle.
    #[cfg(windows)]
    write: usize,
}

/// Route this process's stderr through the secret redactor before it reaches
/// wherever it pointed (the daemon's `daemon.log`).
///
/// The daemon's stderr is a file the OS writes directly, so every `eprintln!`
/// and panic report, and the stderr of any child that inherits it, would land on
/// disk unredacted. Stderr becomes a pipe (fd 2 on Unix; on Windows both the
/// process's standard error handle, which Rust reads on every write and hands to
/// children, and the C runtime's fd 2). One thread reads it; another replaces
/// resolved secret values and writes to the original destination as one
/// stream ([`RelayOut`]): text is written as soon as it cannot be part of a
/// secret, and only the matcher's live prefix (at most the longest registered
/// form) is held until the stream shows whether the rest follows. The panic
/// hook, termination signals, console close events and explicit exits drain the
/// relay first, which writes a held tail under the end-of-stream rule. A failed write is dropped, never propagated, so stderr itself
/// can never fail.
pub fn install_stderr_redactor() {
    use std::io::Read as _;
    use std::sync::atomic::Ordering;

    let mut guard = STDERR_RELAY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if guard.is_some() {
        return;
    }
    let Some(redirect) = redirect_stderr() else {
        return;
    };
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    // Bounded: when stderr's destination stops reading, the writer blocks, the
    // queue fills, the reader stops reading and the pipe fills, so writers are
    // slowed the way they were before the relay existed.
    let (to_writer, from_reader) = std::sync::mpsc::sync_channel::<RelayMsg>(RELAY_QUEUE_CHUNKS);
    let busy = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let read_raw = redirect.read_raw;
    let (mut reader, original) = (redirect.reader, redirect.original);
    let spawned = std::thread::Builder::new()
        .name(RELAY_THREADS[1].into())
        .spawn(move || relay_writer(from_reader, original, done_tx));
    if spawned.is_err() {
        return;
    }
    let reader_tx = to_writer.clone();
    let reader_busy = std::sync::Arc::clone(&busy);
    let spawned = std::thread::Builder::new()
        .name(RELAY_THREADS[0].into())
        .spawn(move || {
            let mut buf = vec![0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        reader_busy.store(true, Ordering::SeqCst);
                        let sent = reader_tx.send(RelayMsg::Data(buf[..n].to_vec()));
                        reader_busy.store(false, Ordering::SeqCst);
                        if sent.is_err() {
                            break;
                        }
                    }
                }
            }
        });
    if spawned.is_ok() {
        *guard = Some(Relay {
            done: done_rx,
            to_writer,
            busy,
            read: read_raw,
            #[cfg(windows)]
            write: redirect.write,
        });
        drop(guard);
        install_exit_signal_flush();
    }
}

/// The relay's output side: everything read is one [`Stream::lines`] stream,
/// so a secret wrapped over any number of lines or written in any number of
/// chunks is matched whole, and only the matcher's live prefix is ever held.
///
/// [`Stream::lines`]: crate::secret_redact::Stream::lines
struct RelayOut {
    out: std::fs::File,
    stream: crate::secret_redact::Stream,
    /// The bytes of a character not complete yet.
    partial_char: Vec<u8>,
}

impl RelayOut {
    fn new(out: std::fs::File) -> Self {
        Self {
            out,
            stream: crate::secret_redact::Stream::lines(),
            partial_char: Vec::new(),
        }
    }

    fn data(&mut self, bytes: &[u8]) {
        self.partial_char.extend_from_slice(bytes);
        let valid = match std::str::from_utf8(&self.partial_char) {
            Ok(text) => text.len(),
            // An unfinished character at the end waits for its last bytes.
            Err(e) if e.error_len().is_none() => e.valid_up_to(),
            Err(_) => {
                let text = String::from_utf8_lossy(&self.partial_char).into_owned();
                self.partial_char.clear();
                let ready = self.stream.push(&text);
                let _ = self.out.write_all(ready.as_bytes());
                return;
            }
        };
        let text = std::str::from_utf8(&self.partial_char[..valid]).expect("checked above");
        let ready = self.stream.push(text);
        self.partial_char.drain(..valid);
        let _ = self.out.write_all(ready.as_bytes());
    }

    /// The end of what was written so far (a sync or the end of the pipe).
    fn drain(&mut self) {
        if !self.partial_char.is_empty() {
            let text = String::from_utf8_lossy(&self.partial_char).into_owned();
            self.partial_char.clear();
            let ready = self.stream.push(&text);
            let _ = self.out.write_all(ready.as_bytes());
        }
        let rest = self.stream.finish();
        let _ = self.out.write_all(rest.as_bytes());
        let _ = self.out.flush();
    }
}

fn relay_writer(
    from_reader: std::sync::mpsc::Receiver<RelayMsg>,
    out: std::fs::File,
    done: std::sync::mpsc::Sender<()>,
) {
    let mut out = RelayOut::new(out);
    for msg in from_reader {
        match msg {
            RelayMsg::Data(bytes) => {
                out.data(&bytes);
                let _ = out.out.flush();
            }
            RelayMsg::Sync(ack) => {
                out.drain();
                let _ = ack.send(());
            }
        }
    }
    out.drain();
    let _ = done.send(());
}

/// Write out everything this process has put on stderr so far, partial line
/// included, within `budget`. For the moments just before a process may end
/// without running destructors: a panic, a termination signal, a console
/// close. A no-op on the relay's own threads and when no relay is installed.
pub fn sync_stderr_relay(budget: std::time::Duration) {
    use std::sync::atomic::Ordering;
    if std::thread::current()
        .name()
        .is_some_and(|name| RELAY_THREADS.contains(&name))
    {
        return;
    }
    let Some((to_writer, busy, read)) = STDERR_RELAY.try_lock().ok().and_then(|guard| {
        guard
            .as_ref()
            .map(|r| (r.to_writer.clone(), std::sync::Arc::clone(&r.busy), r.read))
    }) else {
        return;
    };
    let deadline = std::time::Instant::now() + budget;
    // Rust's stderr is unbuffered, so there is nothing to flush, and flushing
    // takes its lock: a thread holding it in a write blocked on a full pipe
    // would hold the sync past its budget. The C runtime's buffered streams
    // are flushed off this thread for the same reason, within the budget.
    #[cfg(windows)]
    {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        if std::thread::Builder::new()
            .name("toolport-crt-flush".into())
            .spawn(move || {
                crt_flush();
                let _ = done_tx.send(());
            })
            .is_ok()
        {
            let _ =
                done_rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()));
        }
    }
    // Wait until the reader has taken every written byte out of the pipe and
    // handed it on; then the sync request queues behind all of it.
    while std::time::Instant::now() < deadline {
        if pipe_unread(read) == 0 && !busy.load(Ordering::SeqCst) {
            std::thread::sleep(SYNC_QUIET);
            if pipe_unread(read) == 0 && !busy.load(Ordering::SeqCst) {
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    // The queue is bounded: a writer stuck on its destination must not hold
    // a sync (a panic, a signal, an exit) past its budget.
    let (ack_tx, ack_rx) = std::sync::mpsc::channel();
    let mut msg = RelayMsg::Sync(ack_tx);
    loop {
        match to_writer.try_send(msg) {
            Ok(()) => {
                let _ = ack_rx
                    .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()));
                return;
            }
            Err(std::sync::mpsc::TrySendError::Full(back))
                if std::time::Instant::now() < deadline =>
            {
                msg = back;
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            Err(_) => return,
        }
    }
}

/// Chunks (each at most one 8 KiB read) the relay queues for its writer.
const RELAY_QUEUE_CHUNKS: usize = 64;

/// How long the pipe must look drained (and the reader idle) before a sync
/// request is queued behind what it read.
#[cfg(not(windows))]
const SYNC_QUIET: std::time::Duration = std::time::Duration::from_millis(2);
#[cfg(windows)]
const SYNC_QUIET: std::time::Duration = std::time::Duration::from_millis(20);

#[cfg(unix)]
fn pipe_unread(read: usize) -> usize {
    let mut n: libc::c_int = 0;
    // SAFETY: FIONREAD on the relay's own pipe fd, which the reader keeps open.
    let ok = unsafe { libc::ioctl(read as libc::c_int, libc::FIONREAD, &mut n) } == 0;
    if ok {
        n.max(0) as usize
    } else {
        0
    }
}

/// Unknown on Windows: `PeekNamedPipe` waits behind the reader's synchronous
/// `ReadFile` on the same pipe, so it cannot be asked. The reader takes bytes
/// as soon as they are written; [`SYNC_QUIET`] gives it the time to.
#[cfg(windows)]
fn pipe_unread(_read: usize) -> usize {
    0
}

#[cfg(not(any(unix, windows)))]
fn pipe_unread(_read: usize) -> usize {
    0
}

/// Termination signals that would otherwise end the process at once drain the
/// relay first, then end it the default way. Only signals still at their
/// default disposition are taken, so a process with its own handlers keeps
/// them.
#[cfg(unix)]
fn install_exit_signal_flush() {
    use std::sync::atomic::{AtomicI32, Ordering};
    static PIPE_W: AtomicI32 = AtomicI32::new(-1);
    extern "C" fn on_signal(sig: libc::c_int) {
        let byte = sig as u8;
        // SAFETY: write(2) is async-signal-safe; the fd stays open for the process.
        unsafe {
            libc::write(
                PIPE_W.load(Ordering::Relaxed),
                (&byte as *const u8).cast(),
                1,
            );
        }
    }
    if PIPE_W.load(Ordering::SeqCst) >= 0 {
        return;
    }
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: plain fd and signal syscalls on fds this function owns.
    unsafe {
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            return;
        }
        libc::fcntl(fds[0], libc::F_SETFD, libc::FD_CLOEXEC);
        libc::fcntl(fds[1], libc::F_SETFD, libc::FD_CLOEXEC);
    }
    PIPE_W.store(fds[1], Ordering::SeqCst);
    let read_fd = fds[0];
    let spawned = std::thread::Builder::new()
        .name("toolport-stderr-signal".into())
        .spawn(move || loop {
            let mut byte = 0u8;
            // SAFETY: reads one byte from this thread's own pipe end.
            let n = unsafe { libc::read(read_fd, (&mut byte as *mut u8).cast(), 1) };
            if n != 1 {
                return;
            }
            sync_stderr_relay(std::time::Duration::from_millis(500));
            // SAFETY: restores the default action and re-raises the same signal.
            unsafe {
                libc::signal(byte as libc::c_int, libc::SIG_DFL);
                libc::raise(byte as libc::c_int);
            }
        });
    if spawned.is_err() {
        return;
    }
    for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        // SAFETY: reads the current action, and installs ours only over the default.
        unsafe {
            let mut old: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(sig, std::ptr::null(), &mut old) != 0
                || old.sa_sigaction != libc::SIG_DFL
            {
                continue;
            }
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as usize;
            action.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(sig, &action, std::ptr::null_mut());
        }
    }
}

/// A console close, Ctrl+C or logoff drains the relay, then lets the next
/// handler (or the default, which ends the process) run.
#[cfg(windows)]
fn install_exit_signal_flush() {
    use windows_sys::core::BOOL;
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    unsafe extern "system" fn on_ctrl(_kind: u32) -> BOOL {
        sync_stderr_relay(std::time::Duration::from_millis(500));
        0
    }
    // SAFETY: registers a handler that only drains the relay.
    unsafe {
        SetConsoleCtrlHandler(Some(on_ctrl), 1);
    }
}

#[cfg(not(any(unix, windows)))]
fn install_exit_signal_flush() {}

struct Redirect {
    reader: std::fs::File,
    original: std::fs::File,
    read_raw: usize,
    #[cfg(windows)]
    write: usize,
}

#[cfg(unix)]
fn redirect_stderr() -> Option<Redirect> {
    use std::os::fd::FromRawFd as _;
    // SAFETY: plain fd syscalls; every fd is checked before use and owned by
    // exactly one `File` afterwards.
    unsafe {
        let mut fds = [0 as libc::c_int; 2];
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            return None;
        }
        let original = libc::dup(2);
        if original < 0 || libc::dup2(fds[1], 2) < 0 {
            libc::close(fds[0]);
            libc::close(fds[1]);
            if original >= 0 {
                libc::close(original);
            }
            return None;
        }
        libc::close(fds[1]);
        // Children get fd 2 (the pipe), never the relay's private ends.
        libc::fcntl(fds[0], libc::F_SETFD, libc::FD_CLOEXEC);
        libc::fcntl(original, libc::F_SETFD, libc::FD_CLOEXEC);
        Some(Redirect {
            reader: std::fs::File::from_raw_fd(fds[0]),
            original: std::fs::File::from_raw_fd(original),
            read_raw: fds[0] as usize,
        })
    }
}

/// The C runtime keeps its own descriptor table: `fprintf(stderr, ...)` from C
/// code, and any `_write(2, ...)`, use fd 2's handle captured at startup, not the
/// process's standard error handle.
#[cfg(windows)]
mod crt {
    extern "C" {
        pub fn _open_osfhandle(osfhandle: isize, flags: i32) -> i32;
        pub fn _dup2(fd1: i32, fd2: i32) -> i32;
        pub fn _close(fd: i32) -> i32;
        pub fn fflush(stream: *mut std::ffi::c_void) -> i32;
    }
    /// `_O_WRONLY | _O_BINARY`: no CRLF translation of what is relayed.
    pub const WRITE_BINARY: i32 = 0x0001 | 0x8000;
}

#[cfg(windows)]
fn crt_flush() {
    // SAFETY: fflush(NULL) flushes every open C stdio stream.
    unsafe {
        crt::fflush(std::ptr::null_mut());
    }
}

/// Point the C runtime's fd 2 at a duplicate of `handle`. fd 2 owns the
/// duplicate (closing fd 2 closes it); the caller keeps `handle`.
#[cfg(windows)]
fn crt_point_fd2(handle: windows_sys::Win32::Foundation::HANDLE) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    #[cfg(test)]
    if tests::CRT_POINT_FAILS.load(std::sync::atomic::Ordering::SeqCst) {
        return false;
    }
    // SAFETY: duplicates a live handle; `_open_osfhandle` takes ownership of the
    // duplicate, `_dup2` gives fd 2 its own copy, and the temporary fd is closed.
    unsafe {
        let mut dup = std::ptr::null_mut();
        let me = GetCurrentProcess();
        if DuplicateHandle(me, handle, me, &mut dup, 0, 0, DUPLICATE_SAME_ACCESS) == 0 {
            return false;
        }
        let fd = crt::_open_osfhandle(dup as isize, crt::WRITE_BINARY);
        if fd < 0 {
            CloseHandle(dup);
            return false;
        }
        crt_flush();
        let ok = fd == 2 || crt::_dup2(fd, 2) == 0;
        if fd != 2 {
            crt::_close(fd);
        }
        ok
    }
}

#[cfg(windows)]
fn redirect_stderr() -> Option<Redirect> {
    use std::os::windows::io::FromRawHandle as _;
    use windows_sys::Win32::Foundation::{
        CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::System::Console::{GetStdHandle, SetStdHandle, STD_ERROR_HANDLE};
    use windows_sys::Win32::System::Pipes::CreatePipe;
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    // SAFETY: Win32 handle calls; every handle is checked before use and owned by
    // exactly one `File` afterwards. The pipe ends are created non-inheritable:
    // a child that inherits stderr gets its own duplicate from `std::process`.
    unsafe {
        let mut read: HANDLE = std::ptr::null_mut();
        let mut write: HANDLE = std::ptr::null_mut();
        if CreatePipe(&mut read, &mut write, std::ptr::null(), 0) == 0 {
            return None;
        }
        let original = GetStdHandle(STD_ERROR_HANDLE);
        if original.is_null()
            || original == INVALID_HANDLE_VALUE
            || SetStdHandle(STD_ERROR_HANDLE, write) == 0
        {
            CloseHandle(read);
            CloseHandle(write);
            return None;
        }
        // The CRT's fd 2 usually wraps this very handle, and pointing fd 2 at
        // the pipe closes what it wrapped, so the relay writes through its own
        // duplicate.
        let mut kept: HANDLE = std::ptr::null_mut();
        let me = GetCurrentProcess();
        if DuplicateHandle(me, original, me, &mut kept, 0, 0, DUPLICATE_SAME_ACCESS) == 0 {
            SetStdHandle(STD_ERROR_HANDLE, original);
            CloseHandle(read);
            CloseHandle(write);
            return None;
        }
        // C code in this process (and `_write(2, ...)`) writes through the CRT's
        // fd 2, which still names the original file; point it at the pipe too.
        // Not relayed unless both are: native writes would pass in clear.
        if !crt_point_fd2(write) {
            SetStdHandle(STD_ERROR_HANDLE, original);
            CloseHandle(kept);
            CloseHandle(read);
            CloseHandle(write);
            return None;
        }
        Some(Redirect {
            reader: std::fs::File::from_raw_handle(read as _),
            original: std::fs::File::from_raw_handle(kept as _),
            read_raw: read as usize,
            write: write as usize,
        })
    }
}

#[cfg(not(any(unix, windows)))]
fn redirect_stderr() -> Option<Redirect> {
    None
}

/// Point stderr somewhere harmless and close this process's write end of the
/// relay pipe, so the relay reads end-of-file once every writer is done.
#[cfg(unix)]
fn release_stderr(_relay: &Relay) {
    // SAFETY: swaps fd 2 for /dev/null, closing this process's pipe write end.
    unsafe {
        let null = libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC);
        if null >= 0 {
            libc::dup2(null, 2);
            libc::close(null);
        }
    }
}

#[cfg(windows)]
fn release_stderr(relay: &Relay) {
    use std::os::windows::io::IntoRawHandle as _;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Console::{SetStdHandle, STD_ERROR_HANDLE};
    // SAFETY: the NUL handle is leaked on purpose: it stays this process's stderr
    // until exit. The write end was ours alone once stderr no longer names it;
    // the CRT's fd 2 duplicate is replaced (and so closed) by a NUL duplicate.
    unsafe {
        crt_flush();
        if let Ok(null) = std::fs::OpenOptions::new().write(true).open("NUL") {
            let null = null.into_raw_handle();
            crt_point_fd2(null as _);
            SetStdHandle(STD_ERROR_HANDLE, null as _);
        }
        CloseHandle(relay.write as _);
    }
}

#[cfg(not(any(unix, windows)))]
fn release_stderr(_relay: &Relay) {}

/// Drain the stderr relay before an explicit exit, so the last lines (a fatal
/// error, a panic report) are not lost with the process. Bounded: a child still
/// holding the pipe open cannot delay exit past the budget.
pub fn flush_stderr_relay(budget: std::time::Duration) {
    let started = std::time::Instant::now();
    sync_stderr_relay(budget / 2);
    let Some(relay) = STDERR_RELAY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
    else {
        return;
    };
    release_stderr(&relay);
    drop(relay.to_writer);
    let _ = relay
        .done
        .recv_timeout(budget.saturating_sub(started.elapsed()));
}

/// Record every panic, on any thread, with its message and location, then run
/// the previous hook so stderr still gets the usual report.
pub fn install_panic_hook(dir: PathBuf) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|text| text.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "a non-text panic payload".to_string());
        let location = info
            .location()
            .map(|at| format!("{}:{}:{}", at.file(), at.line(), at.column()))
            .unwrap_or_else(|| "an unknown location".to_string());
        let thread = std::thread::current()
            .name()
            .unwrap_or("unnamed")
            .to_string();
        append(
            &dir,
            &format!(
                "panic in thread '{thread}' at {location}: {}",
                message.replace('\n', " ")
            ),
        );
        previous(info);
        // A panic can end the process (abort, or the last thread) before the
        // relay writes the report it just received.
        sync_stderr_relay(std::time::Duration::from_millis(300));
    }));
}

/// How a previous daemon run ended, as `last-daemon-exit.json` stores it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviousExit {
    pub pid: u32,
    pub started_at_ms: u128,
    pub clean_shutdown: bool,
    /// The last panic that run recorded, or why none explains it.
    pub reason: String,
    pub detected_at_ms: u128,
    pub detected_by_pid: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunMarker {
    process: ProcessId,
    started_at_ms: u128,
}

fn marker_path(dir: &Path, pid: u32) -> PathBuf {
    dir.join(RUNS_DIR).join(format!("{pid}.json"))
}

/// The one-line note this daemon shows in `toolport_status`, if the run before
/// it ended badly.
static PREVIOUS_EXIT_NOTE: Mutex<Option<String>> = Mutex::new(None);

pub fn previous_exit_note() -> Option<String> {
    PREVIOUS_EXIT_NOTE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// The last panic `pid` recorded in the current or previous log.
fn last_panic_of(dir: &Path, pid: u32) -> Option<String> {
    let needle = format!(" pid={pid} panic ");
    [dir.join(format!("{LOG_FILE}.1")), log_path(dir)]
        .iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .flat_map(|log| {
            log.lines()
                .filter(|line| line.contains(&needle))
                .map(|line| {
                    line.split_once(&needle)
                        .map_or(line, |(_, rest)| rest)
                        .to_string()
                })
                .collect::<Vec<_>>()
        })
        .last()
        .map(|panic| format!("panic {panic}"))
}

/// Start this daemon's run: find runs that ended without a clean exit, record
/// the latest for the app and `toolport_status`, and leave our own marker.
/// Returns the unclean end it found, if any.
pub fn begin_run(dir: &Path) -> Option<PreviousExit> {
    let me = std::process::id();
    let mut found: Option<PreviousExit> = None;
    if let Ok(entries) = std::fs::read_dir(dir.join(RUNS_DIR)) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(marker) = std::fs::read_to_string(&path)
                .ok()
                .and_then(|raw| serde_json::from_str::<RunMarker>(&raw).ok())
            else {
                continue;
            };
            if marker.process.is_alive() {
                continue;
            }
            let _ = std::fs::remove_file(&path);
            if found
                .as_ref()
                .is_some_and(|newer| newer.started_at_ms >= marker.started_at_ms)
            {
                continue;
            }
            let reason = last_panic_of(dir, marker.process.pid).unwrap_or_else(|| {
                "no panic was recorded, so it was most likely killed (for example by a signal, \
                 the OOM killer or a logout)"
                    .to_string()
            });
            found = Some(PreviousExit {
                pid: marker.process.pid,
                started_at_ms: marker.started_at_ms,
                clean_shutdown: false,
                reason,
                detected_at_ms: now_ms(),
                detected_by_pid: me,
            });
        }
    }
    if let Some(exit) = &mut found {
        // The reason is copied from the log, so it is redacted like a log line,
        // under the lock the scrub rewrites this file with: a secret learned
        // while a scrub runs is still caught, and nothing is written unlocked.
        let path = dir.join(LAST_EXIT_FILE);
        let lock = crate::registry::lock_at(&path);
        exit.reason = crate::secret_redact::redact_log_text(&exit.reason).into_owned();
        if let (Ok(raw), Ok(_lock)) = (serde_json::to_string_pretty(&*exit), lock) {
            let _ = crate::registry::atomic_write(&path, &raw);
        }
        append(
            dir,
            &format!(
                "previous daemon pid={} ended without a clean shutdown: {}",
                exit.pid, exit.reason
            ),
        );
        *PREVIOUS_EXIT_NOTE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(format!(
            "The previous host daemon (pid {}) stopped without a clean shutdown: {}. \
             Details are in {LOG_FILE} in the data directory.",
            exit.pid, exit.reason
        ));
    }
    if let Some(process) = ProcessId::of(me) {
        let marker = RunMarker {
            process,
            started_at_ms: now_ms(),
        };
        let path = marker_path(dir, me);
        let _ = std::fs::create_dir_all(dir.join(RUNS_DIR));
        if let Ok(raw) = serde_json::to_string(&marker) {
            let _ = crate::registry::atomic_write(&path, &raw);
        }
    }
    append(dir, &format!("daemon start {}", env!("CARGO_PKG_VERSION")));
    found
}

/// Mark this run as ended cleanly. Call just before a deliberate exit.
pub fn end_run_cleanly(dir: &Path) {
    append(dir, "daemon clean exit");
    let _ = std::fs::remove_file(marker_path(dir, std::process::id()));
}

/// Whether what this process writes to stderr passes the secret redactor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StderrRedaction {
    /// [`install_stderr_redaction`] has not run in this process.
    NotInstalled,
    /// Redaction is wanted, but no secret is registered in this process yet, so
    /// there is nothing to redact: stderr is still the original and no relay
    /// runs. The first registration starts the relay before it returns.
    AwaitingSecret,
    /// Stderr is the relay: this process's writes, and those of every child that
    /// inherits stderr, are redacted before they reach the original destination.
    Relayed,
    /// The process had no stderr to begin with; stderr is the null device.
    NoStderr,
    /// The relay could not start, so stderr is the null device: what is written
    /// there is lost, never written unredacted.
    Silenced { reason: String },
    /// Neither the relay nor the null device could be put in place; stderr still
    /// goes where it did, unredacted.
    Unprotected { reason: String },
}

/// Also the lock the deferred start runs under, so a reader never sees
/// [`StderrRedaction::AwaitingSecret`] once a registration has started the relay.
static STDERR_REDACTION: Mutex<StderrRedaction> = Mutex::new(StderrRedaction::NotInstalled);

/// [`install_stderr_redaction`] ran in this process.
const LAZY_WANTED: u8 = 1;
/// A secret was registered in this process.
const LAZY_SEEN: u8 = 2;
/// The relay was started (or failed and stderr was silenced): nothing left to do.
const LAZY_DONE: u8 = 4;
/// Both halves set their bit with one read-modify-write on this one atomic, so
/// whichever comes second sees the other's bit and starts the relay; a store
/// and a load on two separate flags could each miss the other.
static LAZY_RELAY: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Called by `secret_redact` each time a new secret value is registered. The
/// first one in a process that wants redaction starts the relay and returns
/// only once stderr is the relay (or the null device), so that secret is never
/// written unredacted. After that, and in a process that never wants redaction
/// once it has recorded one secret, every call is one atomic load.
pub fn secret_registered() {
    use std::sync::atomic::Ordering;
    let state = LAZY_RELAY.load(Ordering::Acquire);
    if state & LAZY_DONE != 0 || (state & LAZY_SEEN != 0 && state & LAZY_WANTED == 0) {
        return;
    }
    if LAZY_RELAY.fetch_or(LAZY_SEEN, Ordering::AcqRel) & LAZY_WANTED != 0 {
        // Also when another registration is starting it: this waits for it.
        start_relay();
    }
}

#[cfg(test)]
pub(crate) fn set_stderr_redaction_state_for_test(state: StderrRedaction) {
    *STDERR_REDACTION
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = state;
}

/// What [`install_stderr_redaction`] achieved, for status reporting.
pub fn stderr_redaction_state() -> StderrRedaction {
    STDERR_REDACTION
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Route stderr through the redactor (see [`install_stderr_redactor`]) for every
/// Toolport process: each gateway mode and both desktop shells. Call it first
/// thing, before anything can write a secret to stderr.
///
/// A process with no secret has nothing to redact, so this only records that
/// redaction is wanted ([`StderrRedaction::AwaitingSecret`]); the relay's pipe
/// and threads start when the first secret is registered (at once, if one
/// already was). If the relay cannot start, stderr is pointed at the null device
/// rather than left unredacted, and [`stderr_redaction_state`] says why. Call
/// [`flush_stderr_relay`] before the process ends normally so its last lines are
/// not lost.
pub fn install_stderr_redaction() -> StderrRedaction {
    use std::sync::atomic::Ordering;
    // With no stderr at all there is nothing to relay to, and on Unix any file
    // opened later would take the free fd 2: point it at the null device now.
    if !has_stderr() {
        let mut state = STDERR_REDACTION
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if LAZY_RELAY.fetch_or(LAZY_WANTED | LAZY_DONE, Ordering::AcqRel) & LAZY_DONE == 0 {
            *state = match silence_stderr() {
                Ok(()) => StderrRedaction::NoStderr,
                Err(error) => StderrRedaction::Unprotected {
                    reason: format!(
                        "stderr was closed and the null device could not replace it ({error})"
                    ),
                },
            };
        }
        return state.clone();
    }
    {
        let mut state = STDERR_REDACTION
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *state == StderrRedaction::NotInstalled {
            *state = StderrRedaction::AwaitingSecret;
            // A panic that ends the process (the main thread's, or an abort)
            // must not take its own report with it. Set now rather than when the
            // relay starts: a registration may run inside a panic hook, where
            // the hook cannot be replaced. A no-op until the relay exists.
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                previous(info);
                sync_stderr_relay(std::time::Duration::from_millis(300));
            }));
        }
    }
    if LAZY_RELAY.fetch_or(LAZY_WANTED, Ordering::AcqRel) & LAZY_SEEN != 0 {
        start_relay();
    }
    stderr_redaction_state()
}

/// Start the relay once a secret exists and redaction is wanted, or fall back to
/// the null device. Idempotent; a concurrent caller waits for the first.
///
/// The switch loses and reorders nothing. On Unix, `dup2` replaces fd 2
/// atomically, and a `write(2)` resolves its descriptor once, on entry: each
/// write, even one in progress on another thread during the swap, lands whole in
/// the original file or whole in the pipe. Bytes written before the swap are
/// already in the original file; the relay writes only bytes written after it,
/// through a duplicate of the same open file (same offset), so a thread's later
/// lines always follow its earlier ones. Rust's stderr is unbuffered, so no
/// bytes wait in-process across the switch.
///
/// The swap holds Rust's stderr lock, so no Rust write is running on the
/// original when it happens. The relay shares the original's file offset, and
/// not every system serializes writes that share one (macOS does not): a write
/// still running there could overwrite the relay's first, or be overwritten.
/// On Windows, Rust fetches the standard error handle on every write, and
/// pointing the C runtime's fd 2 at the pipe closes the handle fd 2 wrapped
/// (usually that same handle) and clears the standard handle for a moment; the
/// lock keeps every Rust write wholly before or after it, and the C runtime's
/// own writes take its fd lock, which `_dup2` holds too. The lock is waited
/// for only while a write runs on the original, which never waits on the
/// relay. It is reentrant and always taken before ours, so a registering
/// thread that already holds it proceeds, and two registering threads take the
/// locks in one order. The relay's own threads take neither lock (their sync
/// uses `try_lock`), so they cannot block the start they would wait for.
fn start_relay() {
    use std::sync::atomic::Ordering;
    let _writers = std::io::stderr().lock();
    let mut state = STDERR_REDACTION
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if LAZY_RELAY.load(Ordering::Acquire) & LAZY_DONE != 0 {
        return;
    }
    let relayed = || {
        STDERR_RELAY
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
    };
    if !relayed() {
        install_stderr_redactor();
    }
    *state = if relayed() {
        StderrRedaction::Relayed
    } else {
        let failure = std::io::Error::last_os_error();
        match silence_stderr() {
            Ok(()) => StderrRedaction::Silenced {
                reason: format!("the stderr redaction relay could not start ({failure})"),
            },
            Err(error) => StderrRedaction::Unprotected {
                reason: format!(
                    "the stderr redaction relay could not start ({failure}), and the null \
                     device could not replace stderr ({error})"
                ),
            },
        }
    };
    LAZY_RELAY.fetch_or(LAZY_DONE, Ordering::AcqRel);
}

#[cfg(unix)]
fn has_stderr() -> bool {
    // SAFETY: F_GETFD only reads the descriptor's flags.
    unsafe { libc::fcntl(2, libc::F_GETFD) >= 0 }
}

#[cfg(windows)]
fn has_stderr() -> bool {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE};
    // SAFETY: reads the process's standard error handle.
    let handle = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
    !handle.is_null() && handle != INVALID_HANDLE_VALUE
}

#[cfg(not(any(unix, windows)))]
fn has_stderr() -> bool {
    true
}

/// Point fd 2 at `/dev/null`. Works even with no descriptor to spare: fd 2 is
/// closed first, so the reopen takes its slot.
#[cfg(unix)]
fn silence_stderr() -> std::io::Result<()> {
    // SAFETY: plain fd syscalls on descriptors this function opens, plus fd 2.
    unsafe {
        let mut null = libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC);
        if null < 0 {
            libc::close(2);
            null = libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC);
            if null < 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        if null == 2 {
            // Children inherit stderr: keep fd 2 open across exec.
            libc::fcntl(2, libc::F_SETFD, 0);
            return Ok(());
        }
        let pointed = libc::dup2(null, 2);
        let error = std::io::Error::last_os_error();
        libc::close(null);
        if pointed < 0 {
            return Err(error);
        }
    }
    Ok(())
}

#[cfg(windows)]
fn silence_stderr() -> std::io::Result<()> {
    use std::os::windows::io::IntoRawHandle as _;
    use windows_sys::Win32::System::Console::{SetStdHandle, STD_ERROR_HANDLE};
    let null = std::fs::OpenOptions::new()
        .write(true)
        .open("NUL")?
        .into_raw_handle();
    // SAFETY: the NUL handle is leaked on purpose: it stays this process's stderr
    // until exit, and the CRT's fd 2 gets its own duplicate.
    unsafe {
        crt_flush();
        if !crt_point_fd2(null as _) {
            return Err(std::io::Error::other(
                "the C runtime's stderr could not be pointed at the null device",
            ));
        }
        if SetStdHandle(STD_ERROR_HANDLE, null as _) == 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn silence_stderr() -> std::io::Result<()> {
    Err(std::io::Error::other("unsupported platform"))
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos", windows)))]
mod tests {
    use super::*;

    static RUN_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Makes [`super::crt_point_fd2`] fail, as a failed handle duplication or
    /// descriptor creation does.
    #[cfg(windows)]
    pub(super) static CRT_POINT_FAILS: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);

    /// Child half of the CRT fd 2 test: it changes the process's stderr.
    #[cfg(windows)]
    #[test]
    fn stderr_crt_point_fails_child() {
        use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE};
        if std::env::var_os("TOOLPORT_STDERR_CRT_FAILS_CHILD").is_none() {
            return;
        }
        CRT_POINT_FAILS.store(true, std::sync::atomic::Ordering::SeqCst);
        // SAFETY: reads the process's standard error handle.
        let before = unsafe { GetStdHandle(STD_ERROR_HANDLE) } as usize;
        let redirected = redirect_stderr().is_some();
        let kept = unsafe { GetStdHandle(STD_ERROR_HANDLE) } as usize == before;
        let silenced = silence_stderr().is_ok();
        println!("redirected={redirected} kept={kept} silenced={silenced}");
    }

    /// Failure mode: when the C runtime's fd 2 could not be pointed at the
    /// relay (or the null device), the failure was ignored: stderr reported
    /// `Relayed` while native writes still reached the original in clear.
    #[cfg(windows)]
    #[test]
    fn stderr_is_not_protected_unless_the_crt_fd_is_redirected_too() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "daemon_log::tests::stderr_crt_point_fails_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("TOOLPORT_STDERR_CRT_FAILS_CHILD", "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{stdout}");
        assert!(
            stdout.contains("redirected=false kept=true silenced=false"),
            "{stdout}"
        );
    }

    /// Child half of `daemon_stderr_is_redacted_before_it_reaches_the_log`: runs in
    /// its own process, because it replaces the process's stderr.
    #[test]
    fn stderr_redactor_child() {
        let Some(secret) = std::env::var_os("TOOLPORT_STDERR_REDACT_CHILD") else {
            return;
        };
        let secret = secret.to_string_lossy().to_string();
        crate::secret_redact::register("relay-server", "RELAY_TOKEN", &secret);
        crate::secret_redact::register("relay-server", "WRAP_TOKEN", "wrapsecretpartone4729318650");
        install_stderr_redactor();
        eprintln!("upstream said: invalid token {secret}");
        // A tool that wraps long values breaks one across lines.
        eprintln!("wrapped wrapsecretpartone\n4729318650 end");
        // A child that inherits fd 2 goes through the relay too.
        #[cfg(unix)]
        let _ = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("echo child echoed {secret} >&2"))
            .status();
        #[cfg(windows)]
        let _ = std::process::Command::new("cmd")
            .args(["/C", &format!("echo child echoed {secret} 1>&2")])
            .status();
        // Native writes to fd 2, bypassing Rust's stderr: libc on Unix, the C
        // runtime's descriptor table on Windows (what C code in the process uses).
        let native = format!("native fd2 wrote {secret}\n");
        #[cfg(unix)]
        // SAFETY: writes a live buffer to fd 2.
        unsafe {
            libc::write(2, native.as_ptr().cast(), native.len());
        }
        #[cfg(windows)]
        {
            extern "C" {
                fn _write(fd: i32, buf: *const std::ffi::c_void, count: u32) -> i32;
            }
            // SAFETY: writes a live buffer to the CRT's fd 2.
            unsafe {
                _write(2, native.as_ptr().cast(), native.len() as u32);
            }
        }
        eprint!("partial line without newline {secret}");
        flush_stderr_relay(std::time::Duration::from_secs(5));
    }

    /// Child half of `stderr_partials_survive_an_abrupt_end`: the process ends
    /// with `abort`, which runs no exit path, so only the timer and the panic
    /// hook can have written what it printed.
    #[test]
    fn stderr_relay_abort_child() {
        let Some(secret) = std::env::var_os("TOOLPORT_STDERR_ABORT_CHILD") else {
            return;
        };
        let secret = secret.to_string_lossy().to_string();
        crate::secret_redact::register("relay-server", "RELAY_TOKEN", &secret);
        install_stderr_redactor();
        let dir = std::env::temp_dir().join(format!("tp-abort-child-{}", std::process::id()));
        install_panic_hook(dir.clone());
        eprint!("unterminated before a pause {secret}");
        std::thread::sleep(std::time::Duration::from_millis(800));
        let _ = std::thread::spawn(move || panic!("worker failed holding {secret}")).join();
        let _ = std::fs::remove_dir_all(&dir);
        std::process::abort();
    }

    /// Child half of `a_secret_split_across_a_pause_is_never_written_in_part`.
    #[test]
    fn stderr_relay_pause_child() {
        if std::env::var_os("TOOLPORT_STDERR_PAUSE_CHILD").is_none() {
            return;
        }
        crate::secret_redact::register("relay-server", "PAUSE_TOKEN", "sk-PauseSecret123");
        install_stderr_redactor();
        let pause = || std::thread::sleep(PARTIAL_FLUSH_AFTER * 3);
        // Split inside a line, across a pause longer than the timed flush.
        eprint!("token=sk-Pa");
        pause();
        eprintln!("useSecret123 end");
        // Split at a line break, across a pause.
        eprintln!("wrapped sk-Pause");
        pause();
        eprintln!("Secret123 tail");
        // A tail that only looked like a start is written once the stream shows
        // it is not one.
        eprint!("ordinary sk-P");
        pause();
        eprintln!("lain words");
        flush_stderr_relay(std::time::Duration::from_secs(5));
    }

    #[test]
    fn a_secret_split_across_a_pause_is_never_written_in_part() {
        let dir = scratch("stderr-pause");
        let log = dir.join("daemon.log");
        let file = std::fs::File::create(&log).unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "daemon_log::tests::stderr_relay_pause_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("TOOLPORT_STDERR_PAUSE_CHILD", "1")
            .stdout(std::process::Stdio::null())
            .stderr(file)
            .status()
            .unwrap();
        assert!(status.success());
        let text = std::fs::read_to_string(&log).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(text.contains("token=[secret:PAUSE_TOKEN] end\n"), "{text}");
        assert!(
            text.contains("wrapped [secret:PAUSE_TOKEN]\n tail\n"),
            "{text}"
        );
        assert!(text.contains("ordinary sk-Plain words\n"), "{text}");
        for piece in ["sk-Pa", "Secret123", "useSecret"] {
            assert!(
                !text.replace("sk-Plain", "").contains(piece),
                "{piece} written: {text}"
            );
        }
    }

    #[test]
    fn stderr_partials_survive_an_abrupt_end() {
        let dir = scratch("stderr-abort");
        let log = dir.join("daemon.log");
        let secret = "abort-canary-3Kp8-SECRET";
        let file = std::fs::File::create(&log).unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "daemon_log::tests::stderr_relay_abort_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("TOOLPORT_STDERR_ABORT_CHILD", secret)
            .stdout(std::process::Stdio::null())
            .stderr(file)
            .status()
            .unwrap();
        assert!(!status.success(), "the child aborts");
        let text = std::fs::read_to_string(&log).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!text.contains(secret), "{text}");
        assert!(
            text.contains("unterminated before a pause [secret:RELAY_TOKEN]"),
            "an unterminated line is written on the timer: {text}"
        );
        assert!(
            text.contains("worker failed holding [secret:RELAY_TOKEN]"),
            "the panic report is drained before the process can end: {text}"
        );
    }

    /// The detached daemon's stderr is a file the OS writes directly; without the
    /// relay an `eprintln!` or an inheriting child would put a secret in daemon.log.
    /// Runs on every CI OS (Linux, macOS and Windows headless core groups).
    #[test]
    fn daemon_stderr_is_redacted_before_it_reaches_the_log() {
        let dir = scratch("stderr-redact");
        let log = dir.join("daemon.log");
        let secret = "relay-canary-7Hq2-SECRET";
        let file = std::fs::File::create(&log).unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "daemon_log::tests::stderr_redactor_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("TOOLPORT_STDERR_REDACT_CHILD", secret)
            .stdout(std::process::Stdio::null())
            .stderr(file)
            .status()
            .unwrap();
        assert!(status.success());
        let text = std::fs::read_to_string(&log).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!text.contains(secret), "{text}");
        assert!(
            text.contains("upstream said: invalid token [secret:RELAY_TOKEN]"),
            "{text}"
        );
        assert!(text.contains("child echoed [secret:RELAY_TOKEN]"), "{text}");
        assert!(
            text.contains("wrapped [secret:WRAP_TOKEN]\n end")
                && !text.contains("wrapsecretpartone")
                && !text.contains("4729318650"),
            "a secret broken across a line break: {text}"
        );
        assert!(
            text.contains("native fd2 wrote [secret:RELAY_TOKEN]"),
            "native fd 2 writes (the CRT's on Windows) are relayed: {text}"
        );
        assert!(
            text.contains("partial line without newline [secret:RELAY_TOKEN]"),
            "the last unterminated line is flushed, redacted: {text}"
        );
    }

    /// Child half of `a_relay_that_cannot_start_silences_stderr`: leaves room for
    /// one new descriptor, and the relay's pipe needs two.
    #[cfg(unix)]
    #[test]
    fn stderr_relay_unavailable_child() {
        let Some(secret) = std::env::var_os("TOOLPORT_STDERR_UNAVAILABLE_CHILD") else {
            return;
        };
        let secret = secret.to_string_lossy().to_string();
        crate::secret_redact::register("relay-server", "RELAY_TOKEN", &secret);
        // SAFETY: F_GETFD probes descriptors; the limit is restored below.
        let (lowest, old) = unsafe {
            let lowest = (0..)
                .find(|fd| libc::fcntl(*fd, libc::F_GETFD) < 0)
                .unwrap();
            let mut old: libc::rlimit = std::mem::zeroed();
            assert_eq!(libc::getrlimit(libc::RLIMIT_NOFILE, &mut old), 0);
            (lowest, old)
        };
        let tight = libc::rlimit {
            rlim_cur: (lowest + 1) as libc::rlim_t,
            rlim_max: old.rlim_max,
        };
        // SAFETY: lowers this child process's own soft descriptor limit.
        unsafe {
            assert_eq!(libc::setrlimit(libc::RLIMIT_NOFILE, &tight), 0);
        }
        let state = install_stderr_redaction();
        // SAFETY: restores the soft limit read above.
        unsafe {
            libc::setrlimit(libc::RLIMIT_NOFILE, &old);
        }
        eprintln!("written after a failed relay start {secret}");
        println!("state={state:?}");
        println!("recorded={:?}", stderr_redaction_state());
    }

    #[cfg(unix)]
    #[test]
    fn a_relay_that_cannot_start_silences_stderr() {
        let dir = scratch("stderr-unavailable");
        let err = dir.join("stderr.txt");
        let secret = "unavailable-canary-9Wd4-SECRET";
        let file = std::fs::File::create(&err).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "daemon_log::tests::stderr_relay_unavailable_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("TOOLPORT_STDERR_UNAVAILABLE_CHILD", secret)
            .stderr(file)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let text = std::fs::read_to_string(&err).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(output.status.success(), "{stdout}");
        assert!(!text.contains(secret), "{text}");
        assert!(
            !text.contains("written after a failed relay start"),
            "stderr points at the null device: {text}"
        );
        assert!(
            stdout
                .contains("state=Silenced { reason: \"the stderr redaction relay could not start"),
            "{stdout}"
        );
        assert!(stdout.contains("recorded=Silenced"), "{stdout}");
    }

    /// What fd 2 is right now: an identity to compare, and whether it is a pipe.
    #[cfg(unix)]
    fn stderr_identity() -> (u64, u64, bool) {
        // SAFETY: fstat fills a zeroed struct for fd 2.
        unsafe {
            let mut st: libc::stat = std::mem::zeroed();
            assert_eq!(libc::fstat(2, &mut st), 0);
            (
                st.st_dev as u64,
                st.st_ino as u64,
                st.st_mode & libc::S_IFMT == libc::S_IFIFO,
            )
        }
    }

    #[cfg(windows)]
    fn stderr_identity() -> (u64, u64, bool) {
        use windows_sys::Win32::Storage::FileSystem::{GetFileType, FILE_TYPE_PIPE};
        use windows_sys::Win32::System::Console::{GetStdHandle, STD_ERROR_HANDLE};
        // SAFETY: reads the standard error handle and asks its type.
        unsafe {
            let handle = GetStdHandle(STD_ERROR_HANDLE);
            (handle as u64, 0, GetFileType(handle) == FILE_TYPE_PIPE)
        }
    }

    /// Whether any relay machinery exists in this process: the relay itself, or
    /// (on Linux, where threads can be listed) one of its threads.
    fn relay_running() -> bool {
        let relay = STDERR_RELAY
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some();
        #[cfg(target_os = "linux")]
        let threads = std::fs::read_dir("/proc/self/task")
            .unwrap()
            .flatten()
            .filter_map(|task| std::fs::read_to_string(task.path().join("comm")).ok())
            .any(|name| name.starts_with("toolport-stderr"));
        #[cfg(not(target_os = "linux"))]
        let threads = false;
        relay || threads
    }

    /// Child half of the lazy relay tests: wants redaction with no secret known,
    /// then learns one while another thread keeps writing.
    #[test]
    fn stderr_lazy_relay_child() {
        let Some(secret) = std::env::var_os("TOOLPORT_STDERR_LAZY_CHILD") else {
            return;
        };
        let secret = secret.to_string_lossy().to_string();
        let before = stderr_identity();
        let state = install_stderr_redaction();
        let after = stderr_identity();
        println!("state={state:?}");
        println!("recorded={:?}", stderr_redaction_state());
        println!("unchanged={} pipe={}", after == before, after.2);
        println!("relay={}", relay_running());
        let warnings = crate::secret_redact::status_notes()
            .into_iter()
            .filter(|note| note.contains("stderr"))
            .count();
        println!("stderr-warnings={warnings}");
        eprintln!("before switch 1");
        eprintln!("before switch 2");
        // One thread writes a numbered stream across the switch; every line must
        // arrive, in order.
        let progress = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = std::sync::Arc::clone(&progress);
        let writer = std::thread::spawn(move || {
            for i in 0..3000 {
                eprintln!("seq {i}");
                seen.store(i + 1, std::sync::atomic::Ordering::SeqCst);
            }
        });
        while progress.load(std::sync::atomic::Ordering::SeqCst) < 500 {
            std::thread::yield_now();
        }
        crate::secret_redact::register("lazy-server", "LAZY_TOKEN", &secret);
        // Registration returned, so the relay is in place: this is redacted.
        eprintln!("after switch {secret}");
        println!(
            "registered relay={} pipe={} recorded={:?}",
            relay_running(),
            stderr_identity().2,
            stderr_redaction_state()
        );
        writer.join().unwrap();
        let native = format!("native after switch {secret}\n");
        #[cfg(unix)]
        // SAFETY: writes a live buffer to fd 2.
        unsafe {
            libc::write(2, native.as_ptr().cast(), native.len());
        }
        #[cfg(windows)]
        {
            extern "C" {
                fn _write(fd: i32, buf: *const std::ffi::c_void, count: u32) -> i32;
            }
            // SAFETY: writes a live buffer to the CRT's fd 2.
            unsafe {
                _write(2, native.as_ptr().cast(), native.len() as u32);
            }
        }
        eprintln!("last line {secret}");
        flush_stderr_relay(std::time::Duration::from_secs(5));
    }

    fn run_lazy_child(label: &str, secret: &str) -> (String, String) {
        let dir = scratch(label);
        let err = dir.join("stderr.txt");
        let file = std::fs::File::create(&err).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "daemon_log::tests::stderr_lazy_relay_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("TOOLPORT_STDERR_LAZY_CHILD", secret)
            .stderr(file)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let text = std::fs::read_to_string(&err).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(output.status.success(), "{stdout}\n{text}");
        (stdout, text)
    }

    /// Child half of the backpressure test: stderr is a pipe nobody reads,
    /// and a thread writes far more than any bound into it.
    #[cfg(target_os = "linux")]
    #[test]
    fn stderr_blocked_destination_child() {
        if std::env::var_os("TOOLPORT_STDERR_BLOCKED_CHILD").is_none() {
            return;
        }
        let rss_kb = || {
            std::fs::read_to_string("/proc/self/status")
                .unwrap()
                .lines()
                .find_map(|l| l.strip_prefix("VmRSS:"))
                .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
                .unwrap()
        };
        let _ = install_stderr_redaction();
        crate::secret_redact::register("blocked-server", "BLOCKED", "blocked-canary-2Wn8-SECRET");
        let before = rss_kb();
        std::thread::spawn(|| {
            let line = "x".repeat(8191);
            for _ in 0..8192 {
                eprintln!("{line}");
            }
        });
        std::thread::sleep(std::time::Duration::from_secs(2));
        if std::env::var_os("TOOLPORT_STDERR_BLOCKED_SYNC").is_some() {
            // The writer above holds Rust's stderr lock in a blocked write.
            let started = std::time::Instant::now();
            sync_stderr_relay(std::time::Duration::from_millis(500));
            println!("sync-ms={}", started.elapsed().as_millis());
        }
        println!("rss-growth-kb={}", rss_kb().saturating_sub(before));
        std::io::Write::flush(&mut std::io::stdout()).unwrap();
        // The writer is blocked on stderr for good: end without draining it.
        std::process::exit(0);
    }

    /// Failure mode: when stderr's destination stops reading (an MCP client
    /// that never drains the pipe), the relay keeps reading into an unbounded
    /// queue, so a noisy process grows without limit instead of being slowed.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_relay_whose_destination_blocks_holds_bounded_memory() {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "daemon_log::tests::stderr_blocked_destination_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("TOOLPORT_STDERR_BLOCKED_CHILD", "1")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = String::new();
        std::io::Read::read_to_string(&mut child.stdout.take().unwrap(), &mut stdout).unwrap();
        let status = child.wait().unwrap();
        assert!(status.success(), "{stdout}");
        let growth: u64 = stdout
            .lines()
            .find_map(|l| l.split_once("rss-growth-kb=").map(|(_, v)| v))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or_else(|| panic!("{stdout}"));
        // 64 MiB was written; the queue holds at most 64 chunks of 8 KiB.
        assert!(growth < 16 * 1024, "grew {growth} KiB");
    }

    /// Failure mode: with stderr's destination stalled and another thread
    /// holding Rust's stderr lock in a blocked write, a sync (a panic, a
    /// signal, an exit) flushed stderr first and so waited on that lock for
    /// good, past its budget, and the signal was never re-raised.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_sync_returns_within_its_budget_while_a_blocked_writer_holds_stderr() {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "daemon_log::tests::stderr_blocked_destination_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("TOOLPORT_STDERR_BLOCKED_CHILD", "1")
            .env("TOOLPORT_STDERR_BLOCKED_SYNC", "1")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut out = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut stdout = String::new();
            let _ = std::io::Read::read_to_string(&mut out, &mut stdout);
            let _ = tx.send(stdout);
        });
        let stdout = rx.recv_timeout(std::time::Duration::from_secs(20));
        let _ = child.kill();
        let _ = child.wait();
        let stdout = stdout.expect("the sync never returned");
        let ms: u64 = stdout
            .lines()
            .find_map(|l| l.split_once("sync-ms=").map(|(_, v)| v))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or_else(|| panic!("{stdout}"));
        assert!(ms < 2000, "the sync took {ms} ms");
    }

    /// Child half of the in-flight-write test: a thread holds Rust's stderr
    /// lock (as a write does while it runs) while another registers a secret.
    #[cfg(unix)]
    #[test]
    fn stderr_inflight_write_child() {
        let Some(secret) = std::env::var_os("TOOLPORT_STDERR_INFLIGHT_CHILD") else {
            return;
        };
        let secret = secret.to_string_lossy().to_string();
        let _ = install_stderr_redaction();
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
        let holder = std::thread::spawn(move || {
            let lock = std::io::stderr().lock();
            held_tx.send(()).unwrap();
            go_rx.recv().unwrap();
            drop(lock);
        });
        held_rx.recv().unwrap();
        let registrant = std::thread::spawn(move || {
            crate::secret_redact::register("inflight-server", "INFLIGHT_TOKEN", &secret);
        });
        std::thread::sleep(std::time::Duration::from_millis(300));
        println!("swapped-while-held={}", stderr_identity().2);
        go_tx.send(()).unwrap();
        holder.join().unwrap();
        registrant.join().unwrap();
        println!("swapped-after={}", stderr_identity().2);
        let line = b"relayed after release\n";
        // SAFETY: writes a live buffer to fd 2.
        unsafe {
            libc::write(2, line.as_ptr().cast(), line.len());
        }
        flush_stderr_relay(std::time::Duration::from_secs(5));
    }

    /// Failure mode: stderr is swapped while a Rust write is still running on
    /// the original. The relay shares its offset, and where writes on it are
    /// not serialized (macOS) one overwrites the other: lines written right
    /// after the switch were lost.
    #[cfg(unix)]
    #[test]
    fn the_relay_waits_for_a_write_running_on_the_original_stderr() {
        let dir = scratch("stderr-inflight");
        let err = dir.join("stderr.txt");
        let file = std::fs::File::create(&err).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "daemon_log::tests::stderr_inflight_write_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(
                "TOOLPORT_STDERR_INFLIGHT_CHILD",
                "inflight-canary-7Qz2-SECRET",
            )
            .stderr(file)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let text = std::fs::read_to_string(&err).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(output.status.success(), "{stdout}\n{text}");
        assert!(stdout.contains("swapped-while-held=false"), "{stdout}");
        assert!(stdout.contains("swapped-after=true"), "{stdout}");
        assert!(text.contains("relayed after release\n"), "{text}");
    }

    /// Failure mode: every process that wants redaction starts the relay (two
    /// threads, a pipe, a signal thread) at once, though most installs never
    /// register a secret and so have nothing to redact.
    #[test]
    fn a_process_without_secrets_keeps_its_stderr_and_starts_no_relay() {
        let (stdout, _) = run_lazy_child("stderr-lazy-idle", "lazy-canary-4Nf7-SECRET");
        assert!(stdout.contains("state=AwaitingSecret"), "{stdout}");
        assert!(stdout.contains("recorded=AwaitingSecret"), "{stdout}");
        assert!(stdout.contains("unchanged=true pipe=false"), "{stdout}");
        assert!(stdout.contains("relay=false"), "{stdout}");
        assert!(stdout.contains("stderr-warnings=0"), "{stdout}");
    }

    /// Failure mode: the relay starts after the first secret is learned, but the
    /// registration returns before stderr is swapped (the secret is written in
    /// the clear), or lines written across the swap are lost or reordered.
    #[test]
    fn the_first_secret_starts_the_relay_without_losing_or_reordering_output() {
        let secret = "lazy-canary-4Nf7-SECRET";
        let (stdout, text) = run_lazy_child("stderr-lazy-switch", secret);
        assert!(
            stdout.contains("registered relay=true pipe=true recorded=Relayed"),
            "{stdout}"
        );
        assert!(!text.contains(secret), "{text}");
        assert!(
            text.contains("after switch [secret:LAZY_TOKEN]\n"),
            "{text}"
        );
        assert!(
            text.contains("native after switch [secret:LAZY_TOKEN]\n"),
            "{text}"
        );
        assert!(text.contains("last line [secret:LAZY_TOKEN]"), "{text}");
        let at = |needle: &str| {
            text.find(needle)
                .unwrap_or_else(|| panic!("{needle}: {text}"))
        };
        assert!(at("before switch 1\n") < at("before switch 2\n"));
        assert!(at("before switch 2\n") < at("seq 0\n"));
        assert!(at("native after switch") < at("last line"));
        let seq: Vec<usize> = text
            .lines()
            .filter_map(|line| line.strip_prefix("seq "))
            .map(|n| n.parse().unwrap())
            .collect();
        assert_eq!(seq, (0..3000).collect::<Vec<_>>(), "lost or reordered");
    }

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "toolport-daemon-log-{label}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_dead_run_without_a_clean_exit_is_reported_with_its_panic() {
        let _run = RUN_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = scratch("unclean");
        let me = ProcessId::of(std::process::id()).unwrap();
        // A run whose pid is now someone else: same pid, another start time.
        let dead = ProcessId {
            pid: me.pid + 1_000_000,
            start: 1,
        };
        std::fs::create_dir_all(dir.join(RUNS_DIR)).unwrap();
        std::fs::write(
            marker_path(&dir, dead.pid),
            serde_json::to_string(&RunMarker {
                process: dead,
                started_at_ms: 5,
            })
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            log_path(&dir),
            format!(
                "1 pid={} panic in thread 'router' at src/router.rs:1:2: boom\n",
                dead.pid
            ),
        )
        .unwrap();

        let exit = begin_run(&dir).expect("the unclean run is found");
        assert_eq!(exit.pid, dead.pid);
        assert!(!exit.clean_shutdown);
        assert!(
            exit.reason.contains("src/router.rs:1:2: boom"),
            "{}",
            exit.reason
        );
        let stored: PreviousExit =
            serde_json::from_str(&std::fs::read_to_string(dir.join(LAST_EXIT_FILE)).unwrap())
                .unwrap();
        assert_eq!(stored, exit);
        assert!(previous_exit_note().unwrap().contains("boom"));
        assert!(!marker_path(&dir, dead.pid).exists());
        assert!(marker_path(&dir, me.pid).exists());

        // A clean exit leaves nothing for the next start to report.
        end_run_cleanly(&dir);
        assert!(!marker_path(&dir, me.pid).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_reused_own_pid_is_reported_and_a_live_run_is_preserved() {
        let _run = RUN_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = scratch("reused-pid");
        let me = ProcessId::of(std::process::id()).unwrap();
        std::fs::create_dir_all(dir.join(RUNS_DIR)).unwrap();
        let path = marker_path(&dir, me.pid);
        std::fs::write(
            &path,
            serde_json::to_string(&RunMarker {
                process: ProcessId {
                    pid: me.pid,
                    start: me.start + 1,
                },
                started_at_ms: 5,
            })
            .unwrap(),
        )
        .unwrap();
        let exit = begin_run(&dir).expect("a stale start time is a different run");
        assert_eq!(exit.pid, me.pid);
        assert_eq!(exit.started_at_ms, 5);
        let current: RunMarker =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(current.process, me);
        assert!(
            begin_run(&dir).is_none(),
            "the live identity is not an exit"
        );
        assert!(path.exists());
        end_run_cleanly(&dir);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_log_rotates_and_keeps_one_previous_file() {
        let dir = scratch("rotate");
        std::fs::write(log_path(&dir), vec![b'x'; MAX_LOG_BYTES as usize]).unwrap();
        append(&dir, "after the cap");
        let current = std::fs::read_to_string(log_path(&dir)).unwrap();
        assert!(current.contains("after the cap"));
        assert!(current.len() < 1024);
        assert_eq!(
            std::fs::metadata(dir.join(format!("{LOG_FILE}.1")))
                .unwrap()
                .len(),
            MAX_LOG_BYTES
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Failure mode: `append` redacts and writes without the log's lock, so a
    /// line written while a scrub masks `daemon.log` keeps a secret the scrub
    /// just learned.
    #[test]
    fn append_waits_for_the_scrub_lock_and_redacts_inside_it() {
        let _data_lock = crate::registry::data_dir_test_lock();
        let dir = scratch("scrub-append");
        let _data = crate::registry::DataDirOverride::set(&dir);
        let target = dir.clone();
        let text = crate::secret_redact::append_during_scrub_for_test(
            &log_path(&dir),
            ("daemon-log-lock", "TOKEN", "daemon-late-token-R7wQ4zNc"),
            move || append(&target, "relay saw daemon-late-token-R7wQ4zNc"),
        );
        let _ = std::fs::remove_dir_all(&dir);
        assert!(text.contains("relay saw "), "{text}");
        assert!(!text.contains("daemon-late-token-R7wQ4zNc"), "{text}");
    }

    /// Failure mode: rotation renames `daemon.log` over `daemon.log.1` while a
    /// scrub holds `daemon.log.1`, so the scrub masks a file that is then
    /// replaced by one it never saw.
    #[test]
    fn rotation_waits_for_both_log_locks() {
        let dir = scratch("rotate-lock");
        let previous = dir.join(format!("{LOG_FILE}.1"));
        std::fs::write(&previous, "OLD PREVIOUS\n").unwrap();
        std::fs::write(log_path(&dir), vec![b'x'; MAX_LOG_BYTES as usize]).unwrap();
        let held = crate::registry::lock_at(&previous).unwrap();
        let target = dir.clone();
        let writer = std::thread::spawn(move || append(&target, "after the cap"));
        std::thread::sleep(std::time::Duration::from_millis(400));
        let during = std::fs::read_to_string(&previous).unwrap();
        drop(held);
        writer.join().unwrap();
        let after = std::fs::metadata(&previous).unwrap().len();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            during == "OLD PREVIOUS\n",
            "rotated under a held lock ({} bytes)",
            during.len()
        );
        assert_eq!(after, MAX_LOG_BYTES, "rotated once the lock freed");
    }

    /// Failure mode: `last-daemon-exit.json` copies a panic line from the log and
    /// is written without the scrub's lock or redaction, so a secret in that
    /// line (learned while the file is scrubbed) stays in the clear.
    #[test]
    fn last_exit_record_waits_for_the_scrub_lock_and_is_redacted() {
        let _run = RUN_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _data_lock = crate::registry::data_dir_test_lock();
        let dir = scratch("exit-lock");
        let _data = crate::registry::DataDirOverride::set(&dir);
        let me = ProcessId::of(std::process::id()).unwrap();
        let dead = ProcessId {
            pid: me.pid + 1_000_000,
            start: 1,
        };
        std::fs::create_dir_all(dir.join(RUNS_DIR)).unwrap();
        std::fs::write(
            marker_path(&dir, dead.pid),
            serde_json::to_string(&RunMarker {
                process: dead,
                started_at_ms: 5,
            })
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            log_path(&dir),
            format!(
                "1 pid={} panic in thread 'router' at src/router.rs:1:2: bad exit-late-key-M2xP8vKs\n",
                dead.pid
            ),
        )
        .unwrap();
        let target = dir.clone();
        let text = crate::secret_redact::append_during_scrub_for_test(
            &dir.join(LAST_EXIT_FILE),
            ("daemon-exit-lock", "KEY", "exit-late-key-M2xP8vKs"),
            move || {
                begin_run(&target).expect("the unclean run is found");
            },
        );
        end_run_cleanly(&dir);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(text.contains("src/router.rs:1:2: bad "), "{text}");
        assert!(!text.contains("exit-late-key-M2xP8vKs"), "{text}");
    }
}
