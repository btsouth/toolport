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

/// Move a full log aside, replacing the previous one.
fn rotate_if_full(dir: &Path) {
    let path = log_path(dir);
    if std::fs::metadata(&path).is_ok_and(|meta| meta.len() >= MAX_LOG_BYTES) {
        let _ = std::fs::rename(&path, dir.join(format!("{LOG_FILE}.1")));
    }
}

fn open_for_append(dir: &Path) -> Option<std::fs::File> {
    let _ = std::fs::create_dir_all(dir);
    rotate_if_full(dir);
    crate::registry::open_append_private(&log_path(dir)).ok()
}

/// The file a newly spawned daemon writes its stderr to. Rotated before each
/// start, so the cap holds across runs.
pub fn stderr_file(dir: &Path) -> Option<std::fs::File> {
    open_for_append(dir)
}

/// Append one line stamped with the time and this process id.
pub fn append(dir: &Path, line: &str) {
    let line = crate::secret_redact::redact_log_text(line);
    if let Some(mut file) = open_for_append(dir) {
        let _ = writeln!(file, "{} pid={} {line}", now_ms(), std::process::id());
    }
}

/// The relay threads that carry this process's stderr into `daemon.log`.
static STDERR_RELAY: Mutex<Option<Relay>> = Mutex::new(None);

/// How long an unterminated line waits for its newline before it is written
/// anyway, so a partial write is never held past a crash for long.
const PARTIAL_FLUSH_AFTER: std::time::Duration = std::time::Duration::from_millis(200);
/// Longest unterminated run kept back waiting for a newline.
const MAX_PENDING_BYTES: usize = 64 * 1024;
const RELAY_THREADS: [&str; 2] = ["toolport-stderr-read", "toolport-stderr-redact"];

enum RelayMsg {
    Data(Vec<u8>),
    /// Write everything received so far, partial line included, then ack.
    Sync(std::sync::mpsc::Sender<()>),
}

struct Relay {
    done: std::sync::mpsc::Receiver<()>,
    to_writer: std::sync::mpsc::Sender<RelayMsg>,
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
/// resolved secret values and writes to the original destination, a line at a
/// time. An unterminated line is written after [`PARTIAL_FLUSH_AFTER`] or past
/// [`MAX_PENDING_BYTES`], with a secret cut at its end removed too, and the panic
/// hook, termination signals, console close events and explicit exits drain the
/// relay first. A failed write is dropped, never propagated, so stderr itself
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
    let (to_writer, from_reader) = std::sync::mpsc::channel::<RelayMsg>();
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

/// The relay's output side. The last complete line is held until the next one
/// arrives (or [`PARTIAL_FLUSH_AFTER`] passes), so a secret broken across a line
/// break is matched on both sides of it.
struct RelayOut {
    out: std::fs::File,
    held: Option<String>,
}

impl RelayOut {
    fn write(&mut self, text: &str, cut: bool) {
        // A cut (not at a newline) may split a secret; the fragment pass also
        // removes the piece of one left dangling at either edge.
        let redacted = if cut {
            crate::secret_redact::redact_fragment(text)
        } else {
            crate::secret_redact::redact_text(text)
        };
        let _ = self.out.write_all(redacted.as_bytes());
    }

    /// Pair the held line with `next`, write the held line, return `next` as
    /// adjusted for a secret that crossed between them.
    fn release(&mut self, next: String) -> String {
        let Some(held) = self.held.take() else {
            return next;
        };
        let body = held.trim_end_matches(['\n', '\r']);
        let ending = &held[body.len()..];
        match crate::secret_redact::redact_pair(body, &next) {
            Some((a, b)) => {
                self.write(&format!("{a}{ending}"), false);
                b
            }
            None => {
                self.write(&held, false);
                next
            }
        }
    }

    fn line(&mut self, bytes: &[u8]) {
        let next = self.release(String::from_utf8_lossy(bytes).into_owned());
        self.held = Some(next);
    }

    fn partial(&mut self, bytes: &[u8], cut: bool) {
        let next = self.release(String::from_utf8_lossy(bytes).into_owned());
        self.write(&next, cut);
    }

    fn drain(&mut self) {
        if let Some(held) = self.held.take() {
            self.write(&held, false);
        }
        let _ = self.out.flush();
    }

    /// [`Self::drain`] on a timer: a held line whose end may be the start of a
    /// secret broken across the line break stays held until the next line.
    fn drain_settled(&mut self) {
        let open = self.held.as_deref().is_some_and(|held| {
            let body = held.trim_end_matches(['\n', '\r']);
            crate::secret_redact::hold_from(body) < body.len()
        });
        if !open {
            self.drain();
        } else {
            let _ = self.out.flush();
        }
    }

    /// Write the part of an unterminated line that cannot be the start of a
    /// secret, and keep the rest in `pending` for the next chunk.
    fn partial_settled(&mut self, pending: &mut Vec<u8>) {
        let valid = match std::str::from_utf8(pending) {
            Ok(text) => text.len(),
            // An unfinished character at the end waits for its last bytes.
            Err(e) if e.error_len().is_none() => e.valid_up_to(),
            Err(_) => {
                // Not text: no secret can be matched across it, write it all.
                self.partial(pending, true);
                pending.clear();
                return;
            }
        };
        let text = std::str::from_utf8(&pending[..valid]).expect("checked above");
        let cut = crate::secret_redact::hold_from(text);
        if cut == 0 {
            return;
        }
        let rest = pending.split_off(cut);
        let ready = std::mem::replace(pending, rest);
        self.partial(&ready, false);
    }
}

fn relay_writer(
    from_reader: std::sync::mpsc::Receiver<RelayMsg>,
    out: std::fs::File,
    done: std::sync::mpsc::Sender<()>,
) {
    use std::sync::mpsc::RecvTimeoutError;
    let mut pending: Vec<u8> = Vec::new();
    let mut out = RelayOut { out, held: None };
    loop {
        match from_reader.recv_timeout(PARTIAL_FLUSH_AFTER) {
            Ok(RelayMsg::Data(bytes)) => {
                pending.extend_from_slice(&bytes);
                if let Some(last) = pending.iter().rposition(|&b| b == b'\n') {
                    let rest = pending.split_off(last + 1);
                    for line in pending.split_inclusive(|&b| b == b'\n') {
                        out.line(line);
                    }
                    pending = rest;
                }
                if pending.len() > MAX_PENDING_BYTES {
                    out.partial_settled(&mut pending);
                }
            }
            Ok(RelayMsg::Sync(ack)) => {
                if !pending.is_empty() {
                    out.partial(&pending, true);
                    pending.clear();
                }
                out.drain();
                let _ = ack.send(());
            }
            // A pause: write what cannot be the start of a secret, hold the rest
            // until the stream continues or the process ends (a sync).
            Err(RecvTimeoutError::Timeout) => {
                if !pending.is_empty() {
                    out.partial_settled(&mut pending);
                }
                if pending.is_empty() {
                    out.drain_settled();
                } else {
                    let _ = out.out.flush();
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    if !pending.is_empty() {
        out.partial(&pending, false);
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
    let _ = std::io::stderr().flush();
    #[cfg(windows)]
    crt_flush();
    let deadline = std::time::Instant::now() + budget;
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
    let (ack_tx, ack_rx) = std::sync::mpsc::channel();
    if to_writer.send(RelayMsg::Sync(ack_tx)).is_ok() {
        let _ = ack_rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()));
    }
}

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
        crt_point_fd2(write);
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
    if let Some(exit) = &found {
        if let Ok(raw) = serde_json::to_string_pretty(exit) {
            let _ = crate::registry::atomic_write(&dir.join(LAST_EXIT_FILE), &raw);
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

#[cfg(all(test, any(target_os = "linux", target_os = "macos", windows)))]
mod tests {
    use super::*;

    static RUN_TEST_LOCK: Mutex<()> = Mutex::new(());

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
        eprint!("wrapped sk-Pause\n");
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
}
