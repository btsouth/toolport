//! Server-side tool orchestration ("code mode"): run ONE agent-submitted script that
//! calls multiple downstream tools, loops, and branches, and returns a single aggregated
//! value. This is the results-layer twin of lazy discovery: lazy discovery collapses N
//! tool *definitions* to a handful of meta-tools; code mode collapses N tool *calls +
//! results* to one `run_script` round-trip, so the intermediate results never land in the
//! model's context.
//!
//! The engine is [`boa_engine`], a pure-Rust JS interpreter, so this adds no C toolchain
//! or FFI to the build. Because Toolport is single-user and local, the agent is already
//! trusted and the servers already run on the host: the sandbox's job is round-trip and
//! token reduction, NOT a security boundary. It still fails closed on resource limits so a
//! runaway or buggy script can't wedge the gateway.
//!
//! Scripts get:
//! - `toolport.call(name, args)` — synchronous host call (v1)
//! - `toolport.callAsync(name, args)` — returns a Promise; independent calls fan out with
//!   bounded host-side parallelism (v2)
//! - `toolport.callAll([{name, args}, ...])` — `Promise.all` sugar over `callAsync`
//! - `servers.<server>.<tool>(args)` — typed stubs from the scoped catalog (sync); use
//!   `.async(args)` for the Promise form. CamelCase aliases are added when they differ
//!   from the sanitized tool segment (e.g. `create_refund` and `createRefund`).
//! - `toolport.listTools()` / `toolport.listServers()` — catalog introspection
//! - `toolport.fetchResult({cursor, offset?, len?, projection?})` — page a previously
//!   shaped result (cursor handoff / structured projection) without leaving the script
//!
//! Downstream calls from a script still pass scope, HITL, and content-defense gates, but
//! intermediate results are **not** byte-budget shaped: full bodies stay in the sandbox
//! (they never enter model context). The gateway shapes only the script's final aggregate
//! for the client. Limits: call budget, wall-clock deadline, max concurrent host calls,
//! promise-job budget, and boa's loop/recursion caps.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use boa_engine::builtins::promise::{PromiseState, ResolvingFunctions};
use boa_engine::job::{GenericJob, Job, JobExecutor, PromiseJob};
use boa_engine::object::builtins::JsPromise;
use boa_engine::property::Attribute;
use boa_engine::{
    js_string, Context, JsError, JsNativeError, JsValue, NativeFunction, Source,
};
use boa_gc::{Finalize, Trace};
use serde_json::{json, Value};

/// Host binding used for every downstream tool invocation from a script.
///
/// `Send + Sync` so independent `callAsync` work can run on a small thread pool without
/// serializing every host call on the JS thread.
pub type CallBinding = Arc<dyn Fn(&str, Value) -> Value + Send + Sync>;

/// Arguments for [`FetchBinding`] / `toolport.fetchResult`.
#[derive(Debug, Clone)]
pub struct FetchArgs {
    pub cursor: String,
    pub offset: usize,
    pub len: usize,
    pub projection: Option<String>,
}

/// Host binding for paging a shaped result by cursor (same cache as `toolport_fetch_result`).
pub type FetchBinding = Arc<dyn Fn(FetchArgs) -> Value + Send + Sync>;

/// Resource limits for one script run. All are fail-closed: exceeding any of them aborts
/// the script with an error result the agent can read and recover from.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Max number of `toolport.call` / `callAsync` / `fetchResult` invocations.
    /// Bounds fan-out, load, and unbounded result paging (WS2-2).
    pub max_calls: usize,
    /// Wall-clock budget for the whole run, checked at each host call.
    pub wall_clock: Duration,
    /// Max concurrent host tool calls when scripts fan out with `callAsync` / `Promise.all`.
    pub max_parallel: usize,
    /// Max promise/microtask jobs drained during one script (bounds a self-requeuing
    /// `Promise.resolve().then(loop)` chain that would otherwise pin `run_jobs` forever).
    pub max_promise_jobs: usize,
    /// Max total loop iterations across the script (boa runtime limit); bounds a pure-JS
    /// `while(true){}` that never calls a tool.
    pub loop_iteration_limit: u64,
    /// Max recursion depth (boa runtime limit).
    pub recursion_limit: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_calls: 64,
            wall_clock: Duration::from_secs(60),
            max_parallel: 8,
            max_promise_jobs: 100_000,
            loop_iteration_limit: 10_000_000,
            recursion_limit: 400,
        }
    }
}

/// Job executor that drains promise/generic jobs with wall-clock + job-count caps.
///
/// Default boa `SimpleJobExecutor` runs until the queue is empty, so a malicious or buggy
/// script that keeps enqueueing microtasks can pin the gateway thread past the script
/// wall-clock (CodeRabbit #480). This executor checks the deadline between jobs.
struct BoundedJobExecutor {
    promise_jobs: RefCell<VecDeque<PromiseJob>>,
    generic_jobs: RefCell<VecDeque<GenericJob>>,
    /// Async/timeout jobs are rare in code-mode scripts; we refuse them fail-closed rather
    /// than reimplement boa's full async drain (keeps this executor small and deadline-safe).
    rejected_unsupported: Cell<bool>,
    deadline: Instant,
    max_jobs: usize,
    jobs_run: Cell<usize>,
}

impl BoundedJobExecutor {
    fn new(deadline: Instant, max_jobs: usize) -> Self {
        Self {
            promise_jobs: RefCell::new(VecDeque::new()),
            generic_jobs: RefCell::new(VecDeque::new()),
            rejected_unsupported: Cell::new(false),
            deadline,
            max_jobs: max_jobs.max(1),
            jobs_run: Cell::new(0),
        }
    }

    fn clear(&self) {
        self.promise_jobs.borrow_mut().clear();
        self.generic_jobs.borrow_mut().clear();
    }

    fn check_budget(&self) -> Result<(), JsError> {
        if Instant::now() >= self.deadline {
            self.clear();
            return Err(JsError::from_native(JsNativeError::error().with_message(
                "toolport script wall-clock deadline exceeded during promise jobs",
            )));
        }
        if self.jobs_run.get() >= self.max_jobs {
            self.clear();
            return Err(JsError::from_native(JsNativeError::error().with_message(
                format!(
                    "toolport script promise-job budget exceeded ({} jobs)",
                    self.max_jobs
                ),
            )));
        }
        Ok(())
    }
}

impl JobExecutor for BoundedJobExecutor {
    fn enqueue_job(self: Rc<Self>, job: Job, _context: &mut Context) {
        match job {
            Job::PromiseJob(p) => self.promise_jobs.borrow_mut().push_back(p),
            Job::GenericJob(g) => self.generic_jobs.borrow_mut().push_back(g),
            // Async/timer (and any future Job variants): code mode does not host timers;
            // refuse rather than hang the drain loop.
            Job::AsyncJob(_) | Job::TimeoutJob(_) | _ => {
                self.rejected_unsupported.set(true);
            }
        }
    }

    fn run_jobs(self: Rc<Self>, context: &mut Context) -> Result<(), JsError> {
        if self.rejected_unsupported.get() {
            self.clear();
            return Err(JsError::from_native(JsNativeError::error().with_message(
                "toolport script used unsupported async/timer jobs inside code mode",
            )));
        }

        loop {
            self.check_budget()?;

            let promise = self.promise_jobs.borrow_mut().pop_front();
            if let Some(job) = promise {
                self.jobs_run.set(self.jobs_run.get() + 1);
                if let Err(err) = job.call(context) {
                    // Match SimpleJobExecutor: drop remaining jobs on first failure.
                    self.clear();
                    return Err(err);
                }
                context.clear_kept_objects();
                continue;
            }

            let generic = self.generic_jobs.borrow_mut().pop_front();
            if let Some(job) = generic {
                self.jobs_run.set(self.jobs_run.get() + 1);
                if let Err(err) = job.call(context) {
                    self.clear();
                    return Err(err);
                }
                context.clear_kept_objects();
                continue;
            }

            break;
        }

        if self.rejected_unsupported.get() {
            self.clear();
            return Err(JsError::from_native(JsNativeError::error().with_message(
                "toolport script used unsupported async/timer jobs inside code mode",
            )));
        }

        Ok(())
    }
}

/// The outcome of running a script.
#[derive(Debug, Clone)]
pub struct ScriptOutcome {
    /// The script's return value as JSON (`null` if it returned nothing). Meaningful only
    /// when `error` is `None`.
    pub value: Value,
    /// How many host tool invocations the script actually made — used to account
    /// round-trips saved (calls - 1) and to report fan-out.
    pub calls: usize,
    /// `Some(message)` if the script threw, hit a limit, or failed to compile. Fail-closed:
    /// the caller surfaces this to the agent as an error result.
    pub error: Option<String>,
}

/// One deferred `callAsync` waiting for host execution + promise settlement.
struct PendingCall {
    name: String,
    args: Value,
    resolvers: ResolvingFunctions,
}

impl Finalize for PendingCall {
    fn finalize(&self) {
        self.resolvers.finalize();
    }
}

// SAFETY: only `resolvers` points into the boa heap; name/args are ordinary Rust data.
unsafe impl Trace for PendingCall {
    unsafe fn trace(&self, tracer: &mut boa_gc::Tracer) {
        // SAFETY: resolvers is a live GC graph while the pending call exists.
        unsafe { self.resolvers.trace(tracer) };
    }
    unsafe fn trace_non_roots(&self) {
        // SAFETY: same as `trace`.
        unsafe { self.resolvers.trace_non_roots() };
    }
    fn run_finalizer(&self) {
        self.finalize();
    }
}

/// Host state shared with the `__toolport_call` / `__toolport_call_async` native functions.
struct HostState {
    call: CallBinding,
    /// Shared with the run so the count survives after the closure is moved into boa.
    calls_made: Rc<Cell<usize>>,
    max_calls: usize,
    max_parallel: usize,
    deadline: Instant,
    /// Queued `callAsync` work; drained with bounded host-side parallelism.
    pending: Rc<RefCell<VecDeque<PendingCall>>>,
}

/// Capture for `__toolport_fetch_result` (no GC refs). Shares the same call
/// budget and wall-clock deadline as `toolport.call` (WS2-2).
struct FetchHost {
    fetch: Option<FetchBinding>,
    calls_made: Rc<Cell<usize>>,
    max_calls: usize,
    deadline: Instant,
}

impl Finalize for FetchHost {}
// SAFETY: only Arc/Option of host closures and Rc counters — no boa heap pointers.
unsafe impl Trace for FetchHost {
    unsafe fn trace(&self, _tracer: &mut boa_gc::Tracer) {}
    unsafe fn trace_non_roots(&self) {}
    fn run_finalizer(&self) {}
}

impl Clone for HostState {
    fn clone(&self) -> Self {
        Self {
            call: Arc::clone(&self.call),
            calls_made: Rc::clone(&self.calls_made),
            max_calls: self.max_calls,
            max_parallel: self.max_parallel,
            deadline: self.deadline,
            pending: Rc::clone(&self.pending),
        }
    }
}

impl Finalize for HostState {
    fn finalize(&self) {
        if let Ok(pending) = self.pending.try_borrow() {
            for item in pending.iter() {
                item.finalize();
            }
        }
    }
}

// SAFETY: only the `ResolvingFunctions` inside `pending` point into the boa heap. The
// `call` Arc, counters, and deadline are ordinary Rust-owned data. While a native call
// holds `pending` mutably, GC is not expected to re-enter; `try_borrow` fails closed by
// skipping the queue for that mark pass (queue entries stay reachable via the promise
// resolvers already rooted as live JS objects).
unsafe impl Trace for HostState {
    unsafe fn trace(&self, tracer: &mut boa_gc::Tracer) {
        if let Ok(pending) = self.pending.try_borrow() {
            for item in pending.iter() {
                // SAFETY: each PendingCall only traces its resolvers.
                unsafe { item.trace(tracer) };
            }
        }
    }
    unsafe fn trace_non_roots(&self) {
        if let Ok(pending) = self.pending.try_borrow() {
            for item in pending.iter() {
                // SAFETY: same as `trace`.
                unsafe { item.trace_non_roots() };
            }
        }
    }
    fn run_finalizer(&self) {
        self.finalize();
    }
}

/// The JS prelude installed before the user script.
const PRELUDE: &str = r#"
globalThis.toolport = {
    call: function (name, args) {
        var payload = (args === undefined || args === null) ? {} : args;
        return JSON.parse(__toolport_call(String(name), JSON.stringify(payload)));
    },
    callAsync: function (name, args) {
        var payload = (args === undefined || args === null) ? {} : args;
        return __toolport_call_async(String(name), JSON.stringify(payload)).then(function (raw) {
            return JSON.parse(raw);
        });
    },
    callAll: function (items) {
        if (!Array.isArray(items)) {
            return Promise.reject(new TypeError("toolport.callAll expects an array of {name, args}"));
        }
        return Promise.all(items.map(function (item) {
            var name = item && item.name;
            var args = item && item.args;
            return toolport.callAsync(name, args);
        }));
    },
    // Page a shaped result by cursor (same stash as toolport_fetch_result). Prefer
    // projecting / filtering full intermediate tool results in-script instead; this
    // is for cursors handed in via `data` or left from a prior shaped agent turn.
    fetchResult: function (opts) {
        opts = (opts === undefined || opts === null) ? {} : opts;
        return JSON.parse(__toolport_fetch_result(JSON.stringify({
            cursor: opts.cursor,
            offset: opts.offset,
            len: opts.len,
            projection: opts.projection
        })));
    },
    listTools: function () { return []; },
    listServers: function () { return []; },
};
"#;

/// Build the `globalThis.servers` injection from exposed catalog tool names
/// (`server__tool`). Safe to eval after [`PRELUDE`]: stubs call through `toolport.call` /
/// `callAsync`, so gates stay intact. Empty catalog yields an empty `servers` object.
///
/// Property names use the sanitized tool segment; when a camelCase form differs and does
/// not collide with another tool on the same server, both names are registered.
pub fn build_servers_prelude(catalog: &[String]) -> String {
    // server -> (tool_prop -> full exposed name), BTree for stable output in tests.
    let mut by_server: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for exposed in catalog {
        let Some((server, tool)) = split_exposed_name(exposed) else {
            continue;
        };
        if server.is_empty() || tool.is_empty() {
            continue;
        }
        by_server
            .entry(server.to_string())
            .or_default()
            .entry(tool.to_string())
            .or_insert_with(|| exposed.clone());
    }

    let mut out = String::with_capacity(256 + catalog.len() * 80);
    out.push_str("(function () {\n");
    out.push_str("  var servers = Object.create(null);\n");
    out.push_str("  function stub(fullName) {\n");
    out.push_str("    var f = function (args) {\n");
    out.push_str("      return toolport.call(fullName, args === undefined || args === null ? {} : args);\n");
    out.push_str("    };\n");
    out.push_str("    f.async = function (args) {\n");
    out.push_str("      return toolport.callAsync(fullName, args === undefined || args === null ? {} : args);\n");
    out.push_str("    };\n");
    out.push_str("    f.toolName = fullName;\n");
    out.push_str("    return f;\n");
    out.push_str("  }\n");
    out.push_str("  function ensure(server) {\n");
    out.push_str("    if (!servers[server]) servers[server] = Object.create(null);\n");
    out.push_str("    return servers[server];\n");
    out.push_str("  }\n");

    let mut all_tools: Vec<String> = Vec::new();
    for (server, tools) in &by_server {
        let server_lit = js_string_literal(server);
        out.push_str("  {\n");
        out.push_str("    var s = ensure(");
        out.push_str(&server_lit);
        out.push_str(");\n");

        // Track props claimed on this server so camelCase aliases don't clobber peers.
        let claimed: BTreeSet<&str> = tools.keys().map(String::as_str).collect();
        for (tool, exposed) in tools {
            let exposed_lit = js_string_literal(exposed);
            let tool_lit = js_string_literal(tool);
            out.push_str("    s[");
            out.push_str(&tool_lit);
            out.push_str("] = stub(");
            out.push_str(&exposed_lit);
            out.push_str(");\n");
            all_tools.push(exposed.clone());

            let camel = snake_to_camel(tool);
            if camel != *tool && is_js_ident_start(&camel) && !claimed.contains(camel.as_str()) {
                let camel_lit = js_string_literal(&camel);
                out.push_str("    if (s[");
                out.push_str(&camel_lit);
                out.push_str("] === undefined) s[");
                out.push_str(&camel_lit);
                out.push_str("] = s[");
                out.push_str(&tool_lit);
                out.push_str("];\n");
            }
        }
        out.push_str("  }\n");
    }

    out.push_str("  globalThis.servers = servers;\n");
    out.push_str("  var __toolport_catalog = [");
    for (i, name) in all_tools.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&js_string_literal(name));
    }
    out.push_str("];\n");
    out.push_str("  toolport.listTools = function () { return __toolport_catalog.slice(); };\n");
    out.push_str("  toolport.listServers = function () { return Object.keys(servers); };\n");
    out.push_str("})();\n");
    out
}

/// Split an exposed `server__tool` name on the first `__` (matches how the router
/// allocates names). Returns `None` when the separator is missing.
pub fn split_exposed_name(exposed: &str) -> Option<(&str, &str)> {
    exposed.split_once("__")
}

/// True when `name` is a gateway meta-tool that should not appear on `servers.*`.
pub fn is_code_mode_meta_tool(name: &str) -> bool {
    name.starts_with("toolport_") || name.starts_with("help_")
}

fn js_string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

fn snake_to_camel(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut up = false;
    for c in s.chars() {
        if c == '_' {
            up = true;
            continue;
        }
        if up {
            for u in c.to_uppercase() {
                out.push(u);
            }
            up = false;
        } else {
            out.push(c);
        }
    }
    out
}

fn is_js_ident_start(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '$' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

/// Run `script` with `data` available as a global `data` object, giving it `toolport.call`
/// / `callAsync` / `callAll` / `fetchResult` bound to host closures, plus `servers.*`
/// stubs from `catalog`. Returns one aggregated [`ScriptOutcome`]; intermediate call
/// results never surface to the model.
///
/// Scripts are wrapped in an async IIFE so top-level `await` and returned Promises work.
/// Independent `callAsync` work is flushed with up to [`Limits::max_parallel`] host calls
/// in flight at once.
///
/// `fetch` wires `toolport.fetchResult` (cursor paging / projection). Pass `None` to
/// leave fetchResult as a fail-closed stub (tests that only exercise tool calls).
///
/// `catalog` is the list of exposed tool names (`server__tool`) the script may stub;
/// pass the client-scoped cache (meta tools filtered). Empty is fine: string-form
/// `toolport.call` still works.
///
/// `call` must be `'static` (the gateway builds it from `Arc`-cloned handles). It is
/// invoked once per host tool call; whatever it returns becomes that call's JS result.
pub fn run_script(
    script: &str,
    data: Value,
    call: CallBinding,
    fetch: Option<FetchBinding>,
    limits: Limits,
    catalog: &[String],
) -> ScriptOutcome {
    let calls_made = Rc::new(Cell::new(0usize));
    let pending = Rc::new(RefCell::new(VecDeque::new()));
    let deadline = Instant::now() + limits.wall_clock;
    let executor = Rc::new(BoundedJobExecutor::new(deadline, limits.max_promise_jobs));
    let mut context = match Context::builder().job_executor(executor).build() {
        Ok(ctx) => ctx,
        Err(e) => {
            return ScriptOutcome {
                value: json!(null),
                calls: 0,
                error: Some(format!("toolport code mode: failed to create JS context: {e}")),
            };
        }
    };

    // Pure-JS runaway guards (a script that loops/recurses forever without calling a tool).
    context
        .runtime_limits_mut()
        .set_loop_iteration_limit(limits.loop_iteration_limit);
    context
        .runtime_limits_mut()
        .set_recursion_limit(limits.recursion_limit);

    let state = HostState {
        call,
        calls_made: calls_made.clone(),
        max_calls: limits.max_calls,
        max_parallel: limits.max_parallel.max(1),
        deadline,
        pending,
    };

    let sync_native = NativeFunction::from_copy_closure_with_captures(
        |_this: &JsValue, args: &[JsValue], state: &HostState, _ctx: &mut Context| {
            reserve_call_slot(state)?;
            let (name, parsed) = parse_call_args(args);
            state.calls_made.set(state.calls_made.get() + 1);
            let result = (state.call)(&name, parsed);
            let result_str = serde_json::to_string(&result).unwrap_or_else(|_| "null".to_string());
            Ok(JsValue::from(js_string!(result_str)))
        },
        state.clone(),
    );

    let async_native = NativeFunction::from_copy_closure_with_captures(
        |_this: &JsValue, args: &[JsValue], state: &HostState, ctx: &mut Context| {
            reserve_call_slot(state)?;
            let (name, parsed) = parse_call_args(args);
            state.calls_made.set(state.calls_made.get() + 1);
            let (promise, resolvers) = JsPromise::new_pending(ctx);
            state.pending.borrow_mut().push_back(PendingCall {
                name,
                args: parsed,
                resolvers,
            });
            Ok(promise.into())
        },
        state.clone(),
    );

    if let Err(e) = context.register_global_callable(js_string!("__toolport_call"), 2, sync_native)
    {
        return fail(calls_made.get(), e);
    }
    if let Err(e) =
        context.register_global_callable(js_string!("__toolport_call_async"), 2, async_native)
    {
        return fail(calls_made.get(), e);
    }

    // Fetch binding: pages shaped results by cursor. Absent binding fails closed.
    // Count against the same call budget / wall-clock as toolport.call (WS2-2).
    let fetch_host = FetchHost {
        fetch,
        calls_made: calls_made.clone(),
        max_calls: limits.max_calls,
        deadline,
    };
    let fetch_native = NativeFunction::from_copy_closure_with_captures(
        |_this: &JsValue, args: &[JsValue], host: &FetchHost, _ctx: &mut Context| {
            // Availability first: an exhausted budget must not mask the real reason
            // a script cannot page results here.
            let Some(fetch) = host.fetch.as_ref() else {
                return Err(JsError::from_native(JsNativeError::error().with_message(
                    "toolport.fetchResult is unavailable in this context",
                )));
            };
            // Budget/deadline next, still before argument parsing, so a script that
            // loops on invalid specs cannot spin past its wall clock unchecked.
            reserve_budget(host.calls_made.get(), host.max_calls, host.deadline)?;
            let raw = args
                .first()
                .and_then(JsValue::as_string)
                .map(|s| s.to_std_string_escaped())
                .unwrap_or_else(|| "{}".to_string());
            let spec: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
            let cursor = spec
                .get("cursor")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if cursor.is_empty() {
                return Err(JsError::from_native(JsNativeError::error().with_message(
                    "toolport.fetchResult requires a non-empty cursor",
                )));
            }
            let offset = spec
                .get("offset")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;
            let len = spec.get("len").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let projection = spec
                .get("projection")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            host.calls_made.set(host.calls_made.get() + 1);
            let result = (fetch)(FetchArgs {
                cursor,
                offset,
                len,
                projection,
            });
            let result_str = serde_json::to_string(&result).unwrap_or_else(|_| "null".to_string());
            Ok(JsValue::from(js_string!(result_str)))
        },
        fetch_host,
    );
    if let Err(e) =
        context.register_global_callable(js_string!("__toolport_fetch_result"), 1, fetch_native)
    {
        return fail(calls_made.get(), e);
    }

    // Inject `data` as a global before the prelude/script run.
    match JsValue::from_json(&data, &mut context) {
        Ok(v) => {
            if let Err(e) =
                context.register_global_property(js_string!("data"), v, Attribute::all())
            {
                return fail(calls_made.get(), e);
            }
        }
        Err(e) => return fail(calls_made.get(), e),
    }

    if let Err(e) = context.eval(Source::from_bytes(PRELUDE)) {
        return fail(calls_made.get(), e);
    }

    // Typed `servers.*` surface from the scoped catalog (after toolport bindings exist).
    let servers_js = build_servers_prelude(catalog);
    if let Err(e) = context.eval(Source::from_bytes(servers_js.as_bytes())) {
        return fail(calls_made.get(), e);
    }

    // Async IIFE: top-level `return` and `await` both work; the result is always a Promise.
    let wrapped = format!("(async function () {{\n{script}\n}})()");
    let top = match context.eval(Source::from_bytes(wrapped.as_bytes())) {
        Ok(v) => v,
        Err(e) => return fail(calls_made.get(), e),
    };

    match drive_to_completion(&mut context, &state, top) {
        Ok(value) => ScriptOutcome {
            value,
            calls: calls_made.get(),
            error: None,
        },
        Err(e) => fail(calls_made.get(), e),
    }
}

fn reserve_call_slot(state: &HostState) -> Result<(), JsError> {
    reserve_budget(state.calls_made.get(), state.max_calls, state.deadline)
}

/// Shared budget check for `toolport.call`, `callAsync`, and `fetchResult` (WS2-2).
fn reserve_budget(calls_made: usize, max_calls: usize, deadline: Instant) -> Result<(), JsError> {
    if calls_made >= max_calls {
        return Err(JsError::from_native(JsNativeError::error().with_message(
            format!("toolport.call budget exceeded ({max_calls} calls)"),
        )));
    }
    if Instant::now() >= deadline {
        return Err(JsError::from_native(
            JsNativeError::error().with_message("toolport script wall-clock deadline exceeded"),
        ));
    }
    Ok(())
}

fn parse_call_args(args: &[JsValue]) -> (String, Value) {
    let name = args
        .first()
        .and_then(JsValue::as_string)
        .map(|s| s.to_std_string_escaped())
        .unwrap_or_default();
    let args_json = args
        .get(1)
        .and_then(JsValue::as_string)
        .map(|s| s.to_std_string_escaped())
        .unwrap_or_else(|| "{}".to_string());
    let parsed: Value = serde_json::from_str(&args_json).unwrap_or(Value::Null);
    (name, parsed)
}

/// Flush pending host work and promise jobs until the top-level async IIFE settles.
fn drive_to_completion(
    context: &mut Context,
    state: &HostState,
    top: JsValue,
) -> Result<Value, JsError> {
    // Bound the drive loop so a pure-JS pending Promise can't spin forever.
    const MAX_DRIVE_ITERS: usize = 1_000_000;

    for _ in 0..MAX_DRIVE_ITERS {
        flush_pending_host_calls(context, state)?;

        if let Err(e) = context.run_jobs() {
            return Err(e);
        }

        // Host work may have been enqueued from a microtask after run_jobs.
        if !state.pending.borrow().is_empty() {
            continue;
        }

        let Some(obj) = top.as_object() else {
            return Ok(top.to_json(context).ok().flatten().unwrap_or(Value::Null));
        };
        let promise = match JsPromise::from_object(obj.clone()) {
            Ok(p) => p,
            Err(_) => {
                return Ok(top.to_json(context).ok().flatten().unwrap_or(Value::Null));
            }
        };

        match promise.state() {
            PromiseState::Pending => {
                // No host work left and jobs drained: the script is stuck on a Promise
                // that nothing will settle.
                return Err(JsError::from_native(JsNativeError::error().with_message(
                    "toolport script hung on an unresolved Promise (no pending tool calls)",
                )));
            }
            PromiseState::Fulfilled(value) => {
                return Ok(value.to_json(context).ok().flatten().unwrap_or(Value::Null));
            }
            PromiseState::Rejected(reason) => {
                let msg = reason
                    .to_string(context)
                    .map(|s| s.to_std_string_escaped())
                    .unwrap_or_else(|_| reason.display().to_string());
                return Err(JsError::from_native(
                    JsNativeError::error().with_message(format!("Uncaught {msg}")),
                ));
            }
        }
    }

    Err(JsError::from_native(JsNativeError::error().with_message(
        "toolport script exceeded internal async drive budget",
    )))
}

/// Take queued `callAsync` work, run it with bounded host-side parallelism, resolve each
/// Promise with the JSON-stringified tool result (same wire shape as sync `call`).
fn flush_pending_host_calls(context: &mut Context, state: &HostState) -> Result<(), JsError> {
    loop {
        if Instant::now() >= state.deadline {
            return Err(JsError::from_native(
                JsNativeError::error().with_message("toolport script wall-clock deadline exceeded"),
            ));
        }

        let batch: Vec<PendingCall> = {
            let mut pending = state.pending.borrow_mut();
            if pending.is_empty() {
                return Ok(());
            }
            let take = pending.len().min(state.max_parallel);
            pending.drain(..take).collect()
        };

        let names_args: Vec<(String, Value)> = batch
            .iter()
            .map(|p| (p.name.clone(), p.args.clone()))
            .collect();
        let results = run_calls_parallel(&state.call, names_args);

        for (pending_call, result) in batch.into_iter().zip(results.into_iter()) {
            let result_str =
                serde_json::to_string(&result).unwrap_or_else(|_| "null".to_string());
            let js_result = JsValue::from(js_string!(result_str));
            pending_call.resolvers.resolve.call(
                &JsValue::undefined(),
                &[js_result],
                context,
            )?;
        }

        // More may still be queued; keep draining this wave before returning to the
        // outer drive loop (which also runs promise jobs).
        if state.pending.borrow().is_empty() {
            return Ok(());
        }
    }
}

/// Run independent host tool calls, using a short-lived thread scope when more than one
/// is ready so wall-clock tracks the slowest call in the batch rather than the sum.
fn run_calls_parallel(call: &CallBinding, items: Vec<(String, Value)>) -> Vec<Value> {
    if items.is_empty() {
        return Vec::new();
    }
    if items.len() == 1 {
        let (name, args) = &items[0];
        return vec![call(name, args.clone())];
    }

    let n = items.len();
    let mut results: Vec<Option<Value>> = (0..n).map(|_| None).collect();
    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(n);
        for (i, (name, args)) in items.into_iter().enumerate() {
            let call = call;
            handles.push(scope.spawn(move || (i, call(&name, args))));
        }
        for handle in handles {
            match handle.join() {
                Ok((i, value)) => results[i] = Some(value),
                Err(_) => {
                    // Worker panicked; leave a slot error in place so order is preserved.
                    // Index is unknown if join payload was lost — fill first empty slot.
                    if let Some(slot) = results.iter_mut().find(|s| s.is_none()) {
                        *slot = Some(json!({
                            "content": [{ "type": "text", "text": "Toolport: callAsync worker panicked" }],
                            "isError": true
                        }));
                    }
                }
            }
        }
    });
    results
        .into_iter()
        .map(|v| {
            v.unwrap_or_else(|| {
                json!({
                    "content": [{ "type": "text", "text": "Toolport: callAsync worker missing result" }],
                    "isError": true
                })
            })
        })
        .collect()
}

/// Build a fail-closed outcome from a boa error, rendering it to a readable message.
/// Uses the error's `Display` rather than `to_opaque` on purpose: boa's uncatchable
/// runtime-limit errors (loop/recursion caps) panic when converted to an opaque JS value,
/// and `Display` yields a usable message (`Uncaught Error: ...`, `RuntimeLimit: ...`) for
/// every error kind.
fn fail(calls: usize, err: JsError) -> ScriptOutcome {
    ScriptOutcome {
        value: json!(null),
        calls,
        error: Some(err.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::thread;
    use std::time::Duration as StdDuration;

    /// A call binding that records the calls it saw and echoes a canned reply per tool.
    fn recording_call(
        log: Arc<Mutex<Vec<(String, Value)>>>,
    ) -> CallBinding {
        Arc::new(move |name: &str, args: Value| {
            log.lock().unwrap().push((name.to_string(), args.clone()));
            json!({ "echo": name, "args": args })
        })
    }

    fn run(script: &str, data: Value, call: CallBinding, limits: Limits) -> ScriptOutcome {
        run_script(script, data, call, None, limits, &[])
    }

    #[test]
    fn runs_a_plain_script_and_returns_its_value() {
        let call = Arc::new(|_: &str, _: Value| Value::Null);
        let out = run("return 1 + 2;", json!({}), call, Limits::default());
        assert_eq!(out.error, None, "unexpected error: {:?}", out.error);
        assert_eq!(out.value, json!(3));
        assert_eq!(out.calls, 0);
    }

    #[test]
    fn toolport_call_reaches_the_binding_and_data_is_injected() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let call = recording_call(log.clone());
        let script = r#"
            var out = [];
            for (var i = 0; i < data.ids.length; i++) {
                var r = toolport.call("lookup", { id: data.ids[i] });
                out.push(r.args.id);
            }
            return out;
        "#;
        let out = run(script, json!({ "ids": [10, 20, 30] }), call, Limits::default());
        assert_eq!(out.error, None, "unexpected error: {:?}", out.error);
        assert_eq!(out.value, json!([10, 20, 30]));
        assert_eq!(out.calls, 3);
        let log = log.lock().unwrap();
        assert_eq!(log.len(), 3);
        assert_eq!(log[0].0, "lookup");
        assert_eq!(log[1].1, json!({ "id": 20 }));
    }

    #[test]
    fn call_budget_is_enforced() {
        let call = Arc::new(|_: &str, _: Value| json!({}));
        let limits = Limits {
            max_calls: 2,
            ..Limits::default()
        };
        let out = run(
            "for (var i = 0; i < 10; i++) { toolport.call('t', {}); } return 'done';",
            json!({}),
            call,
            limits,
        );
        // Fail-closed: it made exactly the budgeted calls, then errored instead of finishing.
        assert_eq!(out.calls, 2);
        assert_ne!(out.error, None);
        assert!(out.error.unwrap().contains("budget"));
    }

    #[test]
    fn loop_limit_stops_pure_js_runaway() {
        let call = Arc::new(|_: &str, _: Value| Value::Null);
        let limits = Limits {
            loop_iteration_limit: 1000,
            ..Limits::default()
        };
        let out = run("while (true) {} return 1;", json!({}), call, limits);
        assert_ne!(out.error, None, "an infinite loop must be stopped");
        assert_eq!(out.calls, 0);
    }

    #[test]
    fn a_thrown_error_is_reported_not_panicked() {
        let call = Arc::new(|_: &str, _: Value| Value::Null);
        let out = run("throw new Error('boom');", json!({}), call, Limits::default());
        assert!(out.error.unwrap().contains("boom"));
    }

    #[test]
    fn syntax_error_fails_closed() {
        let call = Arc::new(|_: &str, _: Value| Value::Null);
        let out = run("this is not valid )(", json!({}), call, Limits::default());
        assert_ne!(out.error, None);
        assert_eq!(out.calls, 0);
    }

    #[test]
    fn call_async_with_await_returns_result() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let call = recording_call(log.clone());
        let script = r#"
            const a = await toolport.callAsync("lookup", { id: 7 });
            return a.args.id;
        "#;
        let out = run(script, json!({}), call, Limits::default());
        assert_eq!(out.error, None, "unexpected error: {:?}", out.error);
        assert_eq!(out.value, json!(7));
        assert_eq!(out.calls, 1);
    }

    #[test]
    fn promise_all_fans_out_and_preserves_order() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let call = recording_call(log.clone());
        let script = r#"
            const results = await Promise.all([
                toolport.callAsync("a", { n: 1 }),
                toolport.callAsync("b", { n: 2 }),
                toolport.callAsync("c", { n: 3 }),
            ]);
            return results.map(function (r) { return r.echo + ":" + r.args.n; });
        "#;
        let out = run(script, json!({}), call, Limits::default());
        assert_eq!(out.error, None, "unexpected error: {:?}", out.error);
        assert_eq!(out.value, json!(["a:1", "b:2", "c:3"]));
        assert_eq!(out.calls, 3);
        let names: Vec<String> = log.lock().unwrap().iter().map(|(n, _)| n.clone()).collect();
        assert_eq!(names.len(), 3);
        assert!(names.contains(&"a".to_string()));
        assert!(names.contains(&"b".to_string()));
        assert!(names.contains(&"c".to_string()));
    }

    #[test]
    fn call_all_sugar_matches_promise_all() {
        let call = Arc::new(|name: &str, args: Value| {
            json!({ "echo": name, "args": args })
        });
        let script = r#"
            const results = await toolport.callAll([
                { name: "x", args: { i: 1 } },
                { name: "y", args: { i: 2 } },
            ]);
            return results.map(function (r) { return r.echo + r.args.i; }).join(",");
        "#;
        let out = run(script, json!({}), call, Limits::default());
        assert_eq!(out.error, None, "unexpected error: {:?}", out.error);
        assert_eq!(out.value, json!("x1,y2"));
        assert_eq!(out.calls, 2);
    }

    #[test]
    fn call_async_budget_is_enforced() {
        let call = Arc::new(|_: &str, _: Value| json!({}));
        let limits = Limits {
            max_calls: 2,
            ..Limits::default()
        };
        let out = run(
            r#"
                await Promise.all([
                    toolport.callAsync("a", {}),
                    toolport.callAsync("b", {}),
                    toolport.callAsync("c", {}),
                ]);
                return "done";
            "#,
            json!({}),
            call,
            limits,
        );
        assert_eq!(out.calls, 2);
        assert_ne!(out.error, None);
        assert!(out.error.unwrap().contains("budget"));
    }

    #[test]
    fn parallel_host_calls_overlap_in_wall_clock() {
        // Each call sleeps ~80ms. Three sequential would be ~240ms; with max_parallel >= 3
        // the batch should finish near one sleep (+ overhead).
        let call: CallBinding = Arc::new(|name: &str, _: Value| {
            thread::sleep(StdDuration::from_millis(80));
            json!({ "echo": name })
        });
        let limits = Limits {
            max_parallel: 8,
            wall_clock: StdDuration::from_secs(10),
            ..Limits::default()
        };
        let started = Instant::now();
        let out = run(
            r#"
                return await Promise.all([
                    toolport.callAsync("a", {}),
                    toolport.callAsync("b", {}),
                    toolport.callAsync("c", {}),
                ]);
            "#,
            json!({}),
            call,
            limits,
        );
        let elapsed = started.elapsed();
        assert_eq!(out.error, None, "unexpected error: {:?}", out.error);
        assert_eq!(out.calls, 3);
        // Three 80ms sequential would be ~240ms; allow headroom for scheduler noise.
        assert!(
            elapsed < StdDuration::from_millis(220),
            "expected overlapping host calls, took {elapsed:?}"
        );
    }

    #[test]
    fn max_parallel_caps_concurrent_host_work() {
        let in_flight = Arc::new(Mutex::new(0usize));
        let peak = Arc::new(Mutex::new(0usize));
        let in_flight2 = in_flight.clone();
        let peak2 = peak.clone();
        let call: CallBinding = Arc::new(move |_name: &str, _: Value| {
            {
                let mut n = in_flight2.lock().unwrap();
                *n += 1;
                let mut p = peak2.lock().unwrap();
                if *n > *p {
                    *p = *n;
                }
            }
            thread::sleep(StdDuration::from_millis(30));
            {
                let mut n = in_flight2.lock().unwrap();
                *n -= 1;
            }
            json!({ "ok": true })
        });
        let limits = Limits {
            max_parallel: 2,
            wall_clock: StdDuration::from_secs(10),
            ..Limits::default()
        };
        let out = run(
            r#"
                return await Promise.all([
                    toolport.callAsync("a", {}),
                    toolport.callAsync("b", {}),
                    toolport.callAsync("c", {}),
                    toolport.callAsync("d", {}),
                ]);
            "#,
            json!({}),
            call,
            limits,
        );
        assert_eq!(out.error, None, "unexpected error: {:?}", out.error);
        assert_eq!(out.calls, 4);
        let peak = *peak.lock().unwrap();
        assert!(peak <= 2, "peak concurrency {peak} exceeded max_parallel 2");
        assert!(peak >= 2, "expected to reach max_parallel, peak={peak}");
    }

    #[test]
    fn servers_stub_calls_through_with_snake_and_camel() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let call = recording_call(log.clone());
        let catalog = vec![
            "stripe__create_refund".to_string(),
            "github__create_pull_request".to_string(),
        ];
        let script = r#"
            const a = servers.stripe.create_refund({ id: "ch_1" });
            const b = servers.stripe.createRefund({ id: "ch_2" });
            const c = servers.github.createPullRequest({ title: "t" });
            return {
                tools: toolport.listTools().sort(),
                servers: toolport.listServers().sort(),
                names: [a.echo, b.echo, c.echo],
                ids: [a.args.id, b.args.id],
                title: c.args.title,
            };
        "#;
        let out = run_script(script, json!({}), call, None, Limits::default(), &catalog);
        assert_eq!(out.error, None, "unexpected error: {:?}", out.error);
        assert_eq!(out.calls, 3);
        assert_eq!(
            out.value["names"],
            json!([
                "stripe__create_refund",
                "stripe__create_refund",
                "github__create_pull_request"
            ])
        );
        assert_eq!(out.value["ids"], json!(["ch_1", "ch_2"]));
        assert_eq!(out.value["title"], json!("t"));
        assert_eq!(
            out.value["tools"],
            json!(["github__create_pull_request", "stripe__create_refund"])
        );
        assert_eq!(out.value["servers"], json!(["github", "stripe"]));
        let log = log.lock().unwrap();
        assert_eq!(log[0].0, "stripe__create_refund");
        assert_eq!(log[2].0, "github__create_pull_request");
    }

    #[test]
    fn servers_stub_async_fans_out() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let call = recording_call(log.clone());
        let catalog = vec![
            "stripe__create_refund".to_string(),
            "resend__send_email".to_string(),
        ];
        let script = r#"
            const results = await Promise.all([
                servers.stripe.createRefund.async({ id: 1 }),
                servers.resend.send_email.async({ to: "a@b.c" }),
            ]);
            return results.map(function (r) { return r.echo; }).sort();
        "#;
        let out = run_script(script, json!({}), call, None, Limits::default(), &catalog);
        assert_eq!(out.error, None, "unexpected error: {:?}", out.error);
        assert_eq!(out.calls, 2);
        assert_eq!(
            out.value,
            json!(["resend__send_email", "stripe__create_refund"])
        );
    }

    #[test]
    fn empty_catalog_still_exposes_empty_servers() {
        let call = Arc::new(|_: &str, _: Value| Value::Null);
        let out = run(
            "return { tools: toolport.listTools(), servers: toolport.listServers(), has: typeof servers };",
            json!({}),
            call,
            Limits::default(),
        );
        assert_eq!(out.error, None, "unexpected error: {:?}", out.error);
        assert_eq!(out.value["tools"], json!([]));
        assert_eq!(out.value["servers"], json!([]));
        assert_eq!(out.value["has"], json!("object"));
    }

    #[test]
    fn js_string_literal_escapes_quotes_and_newlines() {
        assert_eq!(js_string_literal("a'b\\c\nd"), "'a\\'b\\\\c\\nd'");
    }

    #[test]
    fn snake_to_camel_basic() {
        assert_eq!(snake_to_camel("create_refund"), "createRefund");
        assert_eq!(snake_to_camel("alreadyCamel"), "alreadyCamel");
        assert_eq!(snake_to_camel("a_b_c"), "aBC");
    }

    #[test]
    fn split_exposed_name_uses_first_separator() {
        assert_eq!(
            split_exposed_name("stripe__create_refund"),
            Some(("stripe", "create_refund"))
        );
        assert_eq!(
            split_exposed_name("s__tool__extra"),
            Some(("s", "tool__extra"))
        );
        assert_eq!(split_exposed_name("nosep"), None);
    }

    #[test]
    fn camel_alias_does_not_clobber_sibling_tool() {
        // If both create_refund and createRefund exist as distinct tools, keep both.
        let log = Arc::new(Mutex::new(Vec::new()));
        let call = recording_call(log.clone());
        let catalog = vec![
            "s__create_refund".to_string(),
            "s__createRefund".to_string(),
        ];
        let script = r#"
            return {
                a: servers.s.create_refund({}).echo,
                b: servers.s.createRefund({}).echo,
            };
        "#;
        let out = run_script(script, json!({}), call, None, Limits::default(), &catalog);
        assert_eq!(out.error, None, "unexpected error: {:?}", out.error);
        assert_eq!(out.value["a"], json!("s__create_refund"));
        assert_eq!(out.value["b"], json!("s__createRefund"));
    }

    #[test]
    fn fetch_result_binding_pages_shaped_cursor() {
        // Simulate a shaped stash entry via the fetch binding (gateway wires shaping::fetch_result).
        let full = "abcdefghijklmnopqrstuvwxyz";
        let fetch: FetchBinding = Arc::new(move |args: FetchArgs| {
            assert_eq!(args.cursor, "r1");
            let start = args.offset.min(full.len());
            let end = if args.len == 0 {
                full.len()
            } else {
                start.saturating_add(args.len).min(full.len())
            };
            json!({
                "content": [{ "type": "text", "text": &full[start..end] }],
                "isError": false
            })
        });
        let call = Arc::new(|_: &str, _: Value| Value::Null);
        let out = run_script(
            r#"
                const a = toolport.fetchResult({ cursor: "r1", offset: 0, len: 5 });
                const b = toolport.fetchResult({ cursor: "r1", offset: 5, len: 5 });
                return { a: a.content[0].text, b: b.content[0].text };
            "#,
            json!({}),
            call,
            Some(fetch),
            Limits::default(),
            &[],
        );
        assert_eq!(out.error, None, "unexpected error: {:?}", out.error);
        assert_eq!(out.value["a"], json!("abcde"));
        assert_eq!(out.value["b"], json!("fghij"));
    }

    #[test]
    fn fetch_result_unavailable_without_binding() {
        let call = Arc::new(|_: &str, _: Value| Value::Null);
        let out = run(
            r#"return toolport.fetchResult({ cursor: "r1" });"#,
            json!({}),
            call,
            Limits::default(),
        );
        assert_ne!(out.error, None);
        assert!(out.error.unwrap().contains("unavailable"));
    }

    /// An exhausted budget must not mask the real reason a script cannot page
    /// results: with no binding AND no budget left, the error still says
    /// "unavailable", not "budget exceeded".
    #[test]
    fn fetch_result_unavailable_wins_over_an_exhausted_budget() {
        let call = Arc::new(|_: &str, _: Value| Value::Null);
        let out = run(
            r#"return toolport.fetchResult({ cursor: "r1" });"#,
            json!({}),
            call,
            Limits {
                max_calls: 0,
                ..Limits::default()
            },
        );
        let err = out.error.expect("no binding must still be an error");
        assert!(
            err.contains("unavailable"),
            "availability must be reported ahead of the budget: {err}"
        );
        assert!(!err.contains("budget"), "misleading budget error: {err}");
    }

    /// The deadline check still runs before argument parsing, so a script looping on
    /// invalid specs against a live binding cannot spin past its wall clock.
    #[test]
    fn fetch_result_charges_the_budget_before_validating_the_spec() {
        let fetch: FetchBinding = Arc::new(|_: FetchArgs| {
            json!({ "content": [{ "type": "text", "text": "x" }], "isError": false })
        });
        let call = Arc::new(|_: &str, _: Value| Value::Null);
        let out = run_script(
            // Empty cursor is invalid; the budget must be refused first.
            r#"return toolport.fetchResult({ cursor: "" });"#,
            json!({}),
            call,
            Some(fetch),
            Limits {
                max_calls: 0,
                ..Limits::default()
            },
            &[],
        );
        let err = out.error.expect("exhausted budget must be an error");
        assert!(
            err.contains("budget"),
            "budget must be checked before spec validation: {err}"
        );
    }

    /// WS2-2: fetchResult must share the call budget with toolport.call.
    #[test]
    fn fetch_result_counts_against_call_budget() {
        let fetch: FetchBinding = Arc::new(|_: FetchArgs| {
            json!({ "content": [{ "type": "text", "text": "x" }], "isError": false })
        });
        let call = Arc::new(|_: &str, _: Value| Value::Null);
        let limits = Limits {
            max_calls: 2,
            ..Limits::default()
        };
        let out = run_script(
            r#"
                toolport.fetchResult({ cursor: "r1" });
                toolport.fetchResult({ cursor: "r1" });
                toolport.fetchResult({ cursor: "r1" });
                return 1;
            "#,
            json!({}),
            call,
            Some(fetch),
            limits,
            &[],
        );
        assert_ne!(out.error, None, "third fetchResult must hit budget");
        assert!(
            out.error.as_ref().unwrap().contains("budget"),
            "got: {:?}",
            out.error
        );
        assert_eq!(out.calls, 2);
    }

    #[test]
    fn self_requeuing_promise_jobs_hit_budget() {
        // CodeRabbit #480: unbounded Promise microtask chains must not pin run_jobs forever.
        let call = Arc::new(|_: &str, _: Value| Value::Null);
        let limits = Limits {
            max_promise_jobs: 50,
            wall_clock: StdDuration::from_secs(5),
            ..Limits::default()
        };
        let out = run(
            r#"
                let n = 0;
                function loop() {
                    n += 1;
                    return Promise.resolve().then(loop);
                }
                loop();
                return new Promise(function () {}); // never settles
            "#,
            json!({}),
            call,
            limits,
        );
        assert_ne!(out.error, None, "expected promise-job budget error");
        let err = out.error.unwrap();
        assert!(
            err.contains("promise-job budget") || err.contains("wall-clock"),
            "unexpected error: {err}"
        );
    }
}
