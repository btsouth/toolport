//! Fixed-field, best-effort timing for low-frequency connection work.
//!
//! Off unless a stdio adapter turns it on. The same discovery code also serves the
//! desktop HTTP proxy, which re-runs it for every request and lease renewal.
use std::cell::Cell;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    OnceLock,
};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

static TRACE: OnceLock<u128> = OnceLock::new();
static NEXT_SPAN: AtomicU64 = AtomicU64::new(1);
static ENABLED: AtomicBool = AtomicBool::new(false);

thread_local! {
    static QUIET: Cell<bool> = const { Cell::new(false) };
}

/// Trace this process's connection startup. Only stdio adapters call this.
pub(crate) fn enable() {
    ENABLED.store(true, Ordering::Relaxed);
}

/// Silence spans on this thread until the guard drops, for repeated work such as
/// rediscovering a lost daemon.
pub(crate) fn quiet() -> QuietGuard {
    QuietGuard {
        previous: QUIET.with(|quiet| quiet.replace(true)),
        _not_send: std::marker::PhantomData,
    }
}

pub(crate) struct QuietGuard {
    previous: bool,
    _not_send: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl Drop for QuietGuard {
    fn drop(&mut self) {
        let _ = QUIET.try_with(|quiet| quiet.set(self.previous));
    }
}

fn recording() -> bool {
    if QUIET.try_with(Cell::get).unwrap_or(true) {
        return false;
    }
    #[cfg(test)]
    let enabled = TEST_SINK
        .try_with(|sink| sink.borrow().is_some())
        .unwrap_or(false);
    #[cfg(not(test))]
    let enabled = ENABLED.load(Ordering::Relaxed);
    enabled
}

#[cfg(test)]
thread_local! {
    static TEST_SINK: std::cell::RefCell<Option<std::path::PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Opt in only the current unit-test thread; workers never inherit its sink.
#[cfg(test)]
pub(crate) struct TestSinkGuard {
    previous: Option<std::path::PathBuf>,
    _not_send: std::marker::PhantomData<std::rc::Rc<()>>,
}

#[cfg(test)]
impl TestSinkGuard {
    pub(crate) fn set(dir: &std::path::Path) -> Self {
        let previous = TEST_SINK.with(|sink| sink.replace(Some(dir.join("gateway.log"))));
        Self {
            previous,
            _not_send: std::marker::PhantomData,
        }
    }
}

#[cfg(test)]
impl Drop for TestSinkGuard {
    fn drop(&mut self) {
        let _ = TEST_SINK.try_with(|sink| sink.replace(self.previous.take()));
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Stage {
    AdapterStart,
    PrivateStart,
    Discovery,
    ElectionLock,
    DescriptorLock,
    Spawn,
    Readiness,
    LifetimeOpen,
    TcpConnect,
    FirstInput,
    InitializeHeaders,
    InitializeResponse,
}
impl Stage {
    fn label(self) -> &'static str {
        match self {
            Self::AdapterStart => "adapter_start",
            Self::PrivateStart => "private_start",
            Self::Discovery => "discovery",
            Self::ElectionLock => "election_lock",
            Self::DescriptorLock => "descriptor_lock",
            Self::Spawn => "spawn",
            Self::Readiness => "readiness",
            Self::LifetimeOpen => "lifetime_open",
            Self::TcpConnect => "tcp_connect",
            Self::FirstInput => "first_input",
            Self::InitializeHeaders => "initialize_headers",
            Self::InitializeResponse => "initialize_response",
        }
    }
}

#[derive(Clone, Copy)]
enum Outcome {
    Pending,
    Ok,
    Error,
    Aborted,
}
impl Outcome {
    fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Aborted => "aborted",
        }
    }
}

/// Callers cannot supply text. Persistence retains gatewaylog's best-effort policy.
pub(crate) struct Span {
    stage: Stage,
    id: u64,
    started: Instant,
    finished: bool,
    silent: bool,
}
impl Span {
    pub(crate) fn start(stage: Stage) -> Self {
        let span = Self {
            stage,
            id: NEXT_SPAN.fetch_add(1, Ordering::Relaxed),
            started: Instant::now(),
            finished: false,
            silent: !recording(),
        };
        span.emit(Outcome::Pending);
        span
    }
    pub(crate) fn finish<T, E>(self, result: Result<T, E>) -> Result<T, E> {
        self.end(result.is_ok());
        result
    }
    pub(crate) fn end(mut self, success: bool) {
        self.emit(if success { Outcome::Ok } else { Outcome::Error });
        self.finished = true;
    }
    fn emit(&self, outcome: Outcome) {
        if self.silent {
            return;
        }
        let trace = *TRACE.get_or_init(|| {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            (nanos << 32) | u128::from(std::process::id())
        });
        let line = format_line(
            trace,
            self.id,
            self.stage,
            outcome,
            self.started.elapsed().as_millis(),
            std::process::id(),
        );
        #[cfg(test)]
        if let Some(path) = TEST_SINK
            .try_with(|sink| sink.borrow().clone())
            .ok()
            .flatten()
        {
            crate::gatewaylog::queue_at(&path, &line);
        }
        #[cfg(all(not(test), feature = "test-support"))]
        if let Some(dir) = crate::brand::env_var("TOOLPORT_DATA_DIR", "CONDUIT_DATA_DIR") {
            // Match registry's explicit-directory interpretation without borrowing
            // an unrelated integration test's process-global override.
            crate::gatewaylog::queue_at(&std::path::PathBuf::from(dir).join("gateway.log"), &line);
        }
        #[cfg(not(any(test, feature = "test-support")))]
        crate::gatewaylog::append(&line);
    }
}
impl Drop for Span {
    fn drop(&mut self) {
        if !self.finished {
            self.emit(Outcome::Aborted);
        }
    }
}
fn format_line(
    trace: u128,
    span: u64,
    stage: Stage,
    outcome: Outcome,
    elapsed_ms: u128,
    pid: u32,
) -> String {
    let event = if matches!(outcome, Outcome::Pending) {
        "start"
    } else {
        "end"
    };
    format!("startup: trace={trace} span={span} stage={} event={event} result={} elapsed_ms={elapsed_ms} pid={pid}", stage.label(), outcome.label())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diagnostic_without_a_test_directory_does_not_resolve_one() {
        let _lock = crate::registry::data_dir_test_lock();
        assert!(TEST_SINK.with(|sink| sink.borrow().is_none()));
        let result: Result<(), ()> = Span::start(Stage::Discovery).finish(Err(()));
        assert!(result.is_err());
        drop(Span::start(Stage::FirstInput));
    }

    #[test]
    fn result_and_drop_emit_one_terminal_record() {
        let _lock = crate::registry::data_dir_test_lock();
        let dir = std::env::temp_dir().join(format!(
            "toolport-startup-trace-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let data = crate::registry::DataDirOverride::set(&dir);
        let sink = TestSinkGuard::set(&dir);
        let mut spans = Vec::new();
        for success in [true, false] {
            let span = Span::start(Stage::InitializeResponse);
            spans.push((span.id, if success { "ok" } else { "error" }));
            assert_eq!(
                span.finish(if success { Ok(()) } else { Err(()) }).is_ok(),
                success
            );
        }
        let span = Span::start(Stage::FirstInput);
        spans.push((span.id, "aborted"));
        drop(span);
        // A quiet section writes nothing, and recording resumes after it.
        {
            let _quiet = quiet();
            Span::start(Stage::Discovery).end(true);
            let open_across_quiet = Span::start(Stage::Spawn);
            drop(_quiet);
            open_across_quiet.end(true);
        }
        // An unrelated test thread does not own this process-global override.
        std::thread::spawn(|| {
            Span::start(Stage::Discovery).end(true);
            drop(Span::start(Stage::TcpConnect));
        })
        .join()
        .unwrap();
        drop(sink);
        Span::start(Stage::Discovery).end(true);
        drop(Span::start(Stage::FirstInput));
        assert!(crate::telemetry::flush_for_test(
            std::time::Duration::from_secs(5)
        ));
        let log = std::fs::read_to_string(dir.join("gateway.log")).unwrap();
        assert_eq!(
            log.lines().count(),
            spans.len() * 2,
            "an unrelated thread must not write into this test's log"
        );
        for (id, result) in spans {
            let marker = format!(" span={id} ");
            let records: Vec<_> = log.lines().filter(|line| line.contains(&marker)).collect();
            assert_eq!(records.len(), 2);
            assert!(records[0].contains("event=start result=pending"));
            assert!(records[1].contains(&format!("event=end result={result}")));
        }
        drop(data);
        assert!(crate::telemetry::retire_dir_for_test(
            &dir,
            std::time::Duration::from_secs(5)
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }
    #[test]
    fn fixed_schema_covers_all_outcomes_without_caller_text() {
        for (outcome, label, event) in [
            (Outcome::Pending, "pending", "start"),
            (Outcome::Ok, "ok", "end"),
            (Outcome::Error, "error", "end"),
            (Outcome::Aborted, "aborted", "end"),
        ] {
            assert_eq!(format_line(1, 2, Stage::InitializeResponse, outcome, 3, 4), format!("startup: trace=1 span=2 stage=initialize_response event={event} result={label} elapsed_ms=3 pid=4"));
        }
    }
}
