//! Shell parity extraction (tests only; never in a shipped binary).
//!
//! `npm run parity` (scripts/parity/run.mjs) runs [`extract_scenario`] once per
//! scenario file in `parity/scenarios/`, each in its own process under xvfb with
//! `TZ=UTC`. The test:
//!
//! 1. seeds an isolated data directory from the scenario (registry, raw log lines),
//! 2. answers the scenario's IPC calls with the real Rust types and the same
//!    functions the Tauri commands call, and writes them to `<name>.ipc.json`
//!    for the React browser fixture,
//! 3. renders the real GTK page from the same inputs, with the clock pinned to
//!    the scenario's `now_ms`, and
//! 4. walks the page (then its menus) into `<name>.gtk.json`.
//!
//! The React side extracts the same shape from the fixture, and
//! `scripts/parity/diff.mjs` compares the two. Nothing here fails on a
//! difference between the shells; it fails only when the harness itself cannot
//! read a screen (a scenario that does not round-trip, an empty menu, a page
//! that never finishes loading).

use super::*;
use serde_json::{json, Value};

/// One thing a user sees or uses, in reading order.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub(super) struct Node {
    pub(super) region: String,
    pub(super) kind: &'static str,
    pub(super) value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) on: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) items: Option<Vec<String>>,
    /// For text: the label's parent and vertical extent, so labels side by
    /// side on one line in one box are joined, as the React walker does.
    #[serde(skip)]
    line: Option<(usize, f32, f32)>,
}

/// Walks a widget tree in two passes: the page first, then each menu. Opening
/// a menu can re-render the page when it closes (`render_server_list` defers
/// to the popover's `closed` signal), so nothing is opened until every visible
/// widget has been read.
pub(super) struct Walker {
    /// Widgets that start a named region (a server row), besides the page header.
    regions: Vec<(gtk::Widget, String)>,
    pub(super) nodes: Vec<Node>,
    menus: Vec<(usize, gtk::MenuButton)>,
}

impl Walker {
    pub(super) fn new(regions: Vec<(gtk::Widget, String)>) -> Self {
        Self {
            regions,
            nodes: Vec::new(),
            menus: Vec::new(),
        }
    }

    fn push(&mut self, region: &str, kind: &'static str, value: String, on: Option<bool>) {
        self.nodes.push(Node {
            region: region.to_owned(),
            kind,
            value,
            on,
            items: None,
            line: None,
        });
    }

    fn push_text(&mut self, region: &str, label: &gtk::Label) {
        let text = label.text().to_string();
        if text.trim().is_empty() {
            return;
        }
        let line = label.parent().and_then(|parent| {
            let root = label.root()?;
            let bounds = label.compute_bounds(&root)?;
            Some((
                parent.as_ptr() as usize,
                bounds.y(),
                bounds.y() + bounds.height(),
            ))
        });
        let joins = match (self.nodes.last(), line) {
            (Some(last), Some((parent, top, bottom))) => {
                last.region == region
                    && last
                        .line
                        .is_some_and(|(last_parent, last_top, last_bottom)| {
                            last_parent == parent && top < last_bottom && last_top < bottom
                        })
            }
            _ => false,
        };
        if joins {
            let last = self.nodes.last_mut().expect("checked above");
            last.value = format!("{} {}", last.value, text);
            return;
        }
        self.nodes.push(Node {
            region: region.to_owned(),
            kind: "text",
            value: text,
            on: None,
            items: None,
            line,
        });
    }

    /// Read every mapped widget under `widget`. Mapped means drawn: a hidden
    /// widget, a hidden ancestor, a collapsed expander or revealer and an
    /// inactive stack page are all unmapped.
    pub(super) fn walk(&mut self, widget: &gtk::Widget, region: &str) {
        if !widget.is_mapped() || widget.opacity() == 0.0 {
            return;
        }
        if widget.is::<gtk::Popover>() || widget.is::<gtk::WindowControls>() {
            return;
        }
        let region = if widget.has_css_class("toolport-page-header") {
            "header".to_owned()
        } else {
            self.regions
                .iter()
                .find(|(start, _)| start == widget)
                .map(|(_, name)| name.clone())
                .unwrap_or_else(|| region.to_owned())
        };
        let region = region.as_str();
        if let Some(button) = widget.downcast_ref::<gtk::MenuButton>() {
            let name = control_name(widget);
            self.push(region, "menu", name, None);
            self.menus.push((self.nodes.len() - 1, button.clone()));
            return;
        }
        if let Some(switch) = widget.downcast_ref::<gtk::Switch>() {
            self.push(
                region,
                "switch",
                control_name(widget),
                Some(switch.is_active()),
            );
            return;
        }
        if let Some(toggle) = widget.downcast_ref::<gtk::ToggleButton>() {
            self.push(
                region,
                "toggle",
                control_name(widget),
                Some(toggle.is_active()),
            );
            return;
        }
        if let Some(check) = widget.downcast_ref::<gtk::CheckButton>() {
            self.push(
                region,
                "toggle",
                control_name(widget),
                Some(check.is_active()),
            );
            return;
        }
        if widget.is::<gtk::Button>() {
            self.push(region, "button", control_name(widget), None);
            return;
        }
        if widget.is::<gtk::DropDown>() {
            self.push(region, "dropdown", control_name(widget), None);
            return;
        }
        if let Some(entry) = widget.downcast_ref::<gtk::SearchEntry>() {
            let name = entry
                .placeholder_text()
                .map(|text| text.to_string())
                .unwrap_or_default();
            self.push(region, "search", name, None);
            return;
        }
        if let Some(entry) = widget.downcast_ref::<gtk::Entry>() {
            let name = entry
                .placeholder_text()
                .map(|text| text.to_string())
                .unwrap_or_default();
            self.push(region, "entry", name, None);
            return;
        }
        if let Some(expander) = widget.downcast_ref::<gtk::Expander>() {
            // React draws these as a disclosure button over the section.
            let title = expander
                .label_widget()
                .map(|label| inner_text(&label))
                .or_else(|| expander.label().map(|label| label.to_string()))
                .unwrap_or_default();
            self.push(region, "button", title, None);
            if expander.is_expanded() {
                if let Some(child) = expander.child() {
                    self.walk(&child, region);
                }
            }
            return;
        }
        if let Some(label) = widget.downcast_ref::<gtk::Label>() {
            self.push_text(region, label);
            return;
        }
        let mut child = widget.first_child();
        while let Some(next) = child {
            self.walk(&next, region);
            child = next.next_sibling();
        }
    }

    /// Open each menu found by [`Self::walk`] and read its items. A menu that
    /// is gone (re-rendered), or opens with nothing in it, fails the
    /// extraction: an empty list would compare as "no items" and pass.
    pub(super) fn read_menus(&mut self) -> Result<(), String> {
        for (index, button) in std::mem::take(&mut self.menus) {
            let node = &self.nodes[index];
            let describe = format!("menu {:?} in {}", node.value, node.region);
            if button.root().is_none() || !button.is_mapped() {
                return Err(format!(
                    "{describe} left the page before it was opened; the page re-rendered while it was read"
                ));
            }
            let items = if let Some(model) = button.menu_model() {
                model_items(&model)
            } else {
                let popover = button
                    .popover()
                    .ok_or_else(|| format!("{describe} has no popover"))?;
                button.popup();
                settle(|| popover.is_mapped(), std::time::Duration::from_secs(10))
                    .map_err(|_| format!("{describe} did not open"))?;
                let mut items = Vec::new();
                if let Some(child) = popover.child() {
                    menu_items(&child, &mut items);
                }
                button.popdown();
                settle(|| !popover.is_mapped(), std::time::Duration::from_secs(10))
                    .map_err(|_| format!("{describe} did not close"))?;
                items
            };
            if items.is_empty() {
                return Err(format!("{describe} opened with no items"));
            }
            self.nodes[index].items = Some(items);
        }
        Ok(())
    }
}

/// The visible text inside a widget, joined with spaces. Controls nested in
/// it (a menu button's inner toggle) count only through their labels.
fn inner_text(widget: &gtk::Widget) -> String {
    fn collect(widget: &gtk::Widget, out: &mut Vec<String>) {
        if !widget.is_mapped() || widget.is::<gtk::Popover>() {
            return;
        }
        if let Some(label) = widget.downcast_ref::<gtk::Label>() {
            let text = label.text().to_string();
            if !text.trim().is_empty() {
                out.push(text);
            }
            return;
        }
        let mut child = widget.first_child();
        while let Some(next) = child {
            collect(&next, out);
            child = next.next_sibling();
        }
    }
    let mut out = Vec::new();
    collect(widget, &mut out);
    out.join(" ")
}

/// A control's name as a user meets it: its visible text, else its tooltip
/// (icon-only controls), else a marker the report shows as unnamed.
fn control_name(widget: &gtk::Widget) -> String {
    let text = inner_text(widget);
    if !text.trim().is_empty() {
        return text;
    }
    widget
        .tooltip_text()
        .map(|tip| tip.to_string())
        .filter(|tip| !tip.trim().is_empty())
        .unwrap_or_else(|| "(unnamed)".into())
}

fn menu_items(widget: &gtk::Widget, out: &mut Vec<String>) {
    if !widget.is_mapped() {
        return;
    }
    if widget.is::<gtk::Button>() || widget.is::<gtk::CheckButton>() || widget.is::<gtk::Switch>() {
        out.push(control_name(widget));
        return;
    }
    let mut child = widget.first_child();
    while let Some(next) = child {
        menu_items(&next, out);
        child = next.next_sibling();
    }
}

fn model_items(model: &gtk::gio::MenuModel) -> Vec<String> {
    let mut out = Vec::new();
    for index in 0..model.n_items() {
        if let Some(label) = model
            .item_attribute_value(index, "label", None)
            .and_then(|value| value.get::<String>())
        {
            out.push(label);
        }
        for link in ["section", "submenu"] {
            if let Some(inner) = model.item_link(index, link) {
                out.extend(model_items(&inner));
            }
        }
    }
    out
}

/// Run the main loop until `done`, or fail after `limit`. A periodic source
/// keeps the loop waking while background work (probes, file reads) finishes.
pub(super) fn settle(done: impl Fn() -> bool, limit: std::time::Duration) -> Result<(), ()> {
    let context = gtk::glib::MainContext::default();
    let tick = gtk::glib::timeout_add_local(std::time::Duration::from_millis(20), || {
        gtk::glib::ControlFlow::Continue
    });
    let deadline = std::time::Instant::now() + limit;
    let result = loop {
        while context.pending() {
            context.iteration(false);
        }
        if done() {
            break Ok(());
        }
        if std::time::Instant::now() >= deadline {
            break Err(());
        }
        context.iteration(true);
    };
    tick.remove();
    while context.pending() {
        context.iteration(false);
    }
    result
}

/// Let the window lay out and paint `frames` times, so every new widget has
/// its final allocation before it is read (joining labels on one line
/// compares their positions).
pub(super) fn wait_frames(widget: &gtk::Widget, frames: u32) -> Result<(), String> {
    let clock = widget
        .frame_clock()
        .ok_or("the page has no frame clock; is the window shown?")?;
    let painted = std::rc::Rc::new(std::cell::Cell::new(0u32));
    let counter = painted.clone();
    let handler = clock.connect_after_paint(move |_| counter.set(counter.get() + 1));
    let result = settle(
        || {
            if painted.get() < frames {
                widget.queue_draw();
                widget.queue_resize();
                false
            } else {
                true
            }
        },
        std::time::Duration::from_secs(10),
    );
    clock.disconnect(handler);
    result.map_err(|_| "the window stopped drawing".to_string())
}

/// Every key the scenario wrote must survive a trip through the Rust type with
/// the same value. Fields the type adds with defaults are fine; a key the type
/// drops or a value it changes means the scenario is not something the backend
/// can send, so the comparison would test a state that never happens.
pub(super) fn round_trip_loss(input: &Value, output: &Value, path: &str) -> Vec<String> {
    match (input, output) {
        (Value::Object(want), Value::Object(have)) => want
            .iter()
            .flat_map(|(key, value)| {
                let at = format!("{path}.{key}");
                match have.get(key) {
                    Some(got) => round_trip_loss(value, got, &at),
                    None if value.is_null() => Vec::new(),
                    None => vec![format!("{at} was dropped")],
                }
            })
            .collect(),
        (Value::Array(want), Value::Array(have)) if want.len() == have.len() => want
            .iter()
            .zip(have)
            .enumerate()
            .flat_map(|(index, (value, got))| {
                round_trip_loss(value, got, &format!("{path}[{index}]"))
            })
            .collect(),
        _ if input == output => Vec::new(),
        _ => vec![format!("{path} changed from {input} to {output}")],
    }
}

fn round_trip<T: serde::de::DeserializeOwned + serde::Serialize>(
    input: &Value,
    what: &str,
) -> Result<(T, Value), String> {
    let typed: T = serde_json::from_value(input.clone())
        .map_err(|error| format!("{what} does not deserialize: {error}"))?;
    let output = serde_json::to_value(&typed).map_err(|error| error.to_string())?;
    let loss = round_trip_loss(input, &output, what);
    if loss.is_empty() {
        Ok((typed, output))
    } else {
        Err(format!(
            "{what} does not round-trip through the Rust type:\n  {}",
            loss.join("\n  ")
        ))
    }
}

/// Raw lines for the data directory: listed lines plus generated ones, oldest first.
fn seed_lines(seed: &Value) -> Vec<Value> {
    let mut lines = seed["lines"].as_array().cloned().unwrap_or_default();
    if let Some(generate) = seed.get("generate") {
        let count = generate["count"].as_u64().unwrap_or(0);
        let start = generate["start_ts"].as_u64().unwrap_or(0);
        let step = generate["step_ms"].as_u64().unwrap_or(1000);
        let cycle = generate["cycle"].as_array().cloned().unwrap_or_default();
        for index in 0..count {
            if cycle.is_empty() {
                break;
            }
            let mut line = cycle[index as usize % cycle.len()].clone();
            if let Some(object) = line.as_object_mut() {
                object.insert("ts".into(), json!(start + index * step));
                // `{n}` in a string field numbers the line, so rows stay distinct.
                for value in object.values_mut() {
                    if let Some(text) = value.as_str() {
                        if text.contains("{n}") {
                            *value = json!(text.replace("{n}", &index.to_string()));
                        }
                    }
                }
            }
            lines.push(line);
        }
    }
    lines.sort_by_key(|line| line["ts"].as_u64().unwrap_or(0));
    lines
}

fn seed_data_dir(dir: &std::path::Path, seeds: &Value) -> Result<(), String> {
    let Some(files) = seeds["files"].as_object() else {
        return Ok(());
    };
    for (name, seed) in files {
        if name.contains('/') || name.contains("..") {
            return Err(format!("seed file {name:?} must be a plain file name"));
        }
        let path = dir.join(name);
        if seed["unreadable"].as_bool() == Some(true) {
            // A directory where the log should be: every read fails.
            std::fs::create_dir_all(&path).map_err(|error| error.to_string())?;
            continue;
        }
        let mut text = String::new();
        for line in seed_lines(seed) {
            text.push_str(&line.to_string());
            text.push('\n');
        }
        std::fs::write(&path, text).map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// The answer the Tauri command gives, through the function it calls.
/// Commands not listed here are struct inputs (`get_registry`, `probe_servers`)
/// or not covered; a scenario naming one fails.
fn materialize(command: &str, args: &Value) -> Result<Result<Value, String>, String> {
    let limit = || {
        args["limit"]
            .as_u64()
            .map(|limit| limit as usize)
            .ok_or_else(|| format!("{command} needs a numeric limit"))
    };
    let io = |result: std::io::Result<Value>, prefix: &str| {
        result.map_err(|error| format!("{prefix}: {error}"))
    };
    let list = |result: std::io::Result<Vec<Value>>, prefix: &str| {
        result
            .map(Value::Array)
            .map_err(|error| format!("{prefix}: {error}"))
    };
    Ok(match command {
        "get_audit_log" => list(
            crate::audit::read_activity(limit()?),
            "Couldn't read the activity log",
        ),
        "audit_stats" => io(crate::audit::stats(), "Couldn't read the activity log"),
        "savings_summary" => io(
            crate::savings::try_summary(),
            "Couldn't read catalog telemetry",
        ),
        "get_security_events" => list(
            crate::integrity::review_events(limit()?),
            "Couldn't read security events",
        ),
        "activity_safety" => serde_json::to_value(crate::integrity::activity_safety())
            .map_err(|error| error.to_string()),
        "get_search_traces" => list(
            crate::searchtrace::read_recent(limit()?),
            "Couldn't read search traces",
        ),
        "get_inspect_log" => list(
            crate::inspect::read_recent(limit()?),
            "Couldn't read the inspector log",
        ),
        "list_tool_identities" => crate::registry::load().and_then(|registry| {
            crate::integrity::tool_identities(&registry.servers, &registry.profiles)
                .map(|identities| serde_json::to_value(identities).unwrap_or(Value::Null))
        }),
        "activity_health" => Ok(crate::observability_controller::activity_health()),
        "activity_revision" => {
            crate::observability_controller::activity_revision().map(Value::from)
        }
        "release_activity_revision" => Ok(Value::Null),
        other => {
            return Err(format!(
                "parity cannot answer {other}; add it to parity.rs materialize"
            ))
        }
    })
}

fn answer(command: &str, args: Value, result: Result<Value, String>) -> Value {
    match result {
        Ok(value) => json!({ "cmd": command, "args": args, "result": value }),
        Err(error) => json!({ "cmd": command, "args": args, "error": error }),
    }
}

fn new_window(
    app: &adw::Application,
    content: &impl IsA<gtk::Widget>,
    scenario: &Value,
) -> adw::ApplicationWindow {
    // The React page gets the same width minus its sidebar (240 px).
    let width = scenario["viewport"]["width"].as_i64().unwrap_or(1240) as i32 - 240;
    let height = scenario["viewport"]["height"].as_i64().unwrap_or(900) as i32;
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .default_width(width)
        .default_height(height)
        .content(content)
        .build();
    window.present();
    window
}

fn test_app(id: &str) -> adw::Application {
    adw::init().unwrap();
    let app = adw::Application::builder()
        .application_id(id)
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.register(gtk::gio::Cancellable::NONE).unwrap();
    app
}

/// Renders the Servers page from `get_registry` and `probe_servers`.
fn servers_screen(
    scenario: &Value,
    registry: Option<Result<crate::registry::Registry, String>>,
    probes: Vec<crate::server_runtime::ProbeResult>,
) -> Result<Vec<Node>, String> {
    let app = test_app("com.tsout.Toolport.ParityServers");
    let (content, page, _) = build_content(&app, crate::approval_broker::start_native());
    let window = new_window(&app, &content, scenario);
    settle(|| content.is_mapped(), std::time::Duration::from_secs(10))
        .map_err(|_| "the Servers page was never drawn".to_string())?;
    match registry {
        Some(Ok(registry)) => {
            let snapshot = state::RegistrySnapshot::from_registry(registry);
            // Seed the health cache as if each probe just finished, so the
            // page's own probe pass finds nothing to start.
            let enabled = snapshot
                .servers
                .iter()
                .filter(|server| server.enabled)
                .map(|server| (server.id.as_str(), server.probe_fingerprint))
                .collect::<Vec<_>>();
            let now = std::time::Instant::now();
            let tickets =
                page.health
                    .borrow_mut()
                    .plan(&enabled, health::ProbeReason::ConfigChanged, now);
            for ticket in tickets {
                let probe = probes
                    .iter()
                    .find(|probe| probe.server_id == ticket.server_id)
                    .cloned()
                    .ok_or_else(|| {
                        format!(
                            "probe_servers has no result for enabled server {:?}",
                            ticket.server_id
                        )
                    })?;
                page.health.borrow_mut().complete(&ticket, probe, now);
            }
            page.render(state::RegistryState::Ready(snapshot));
            for probe in &probes {
                page.apply_probe(&probe.server_id, probe);
            }
            page.update_posture();
        }
        Some(Err(cause)) => page.render(state::RegistryState::Unavailable(cause)),
        None => page.render(state::RegistryState::FirstRun),
    }
    wait_frames(content.upcast_ref(), 3)?;
    let regions = page
        .rows
        .borrow()
        .iter()
        .map(|(server, card)| {
            (
                card.clone().upcast::<gtk::Widget>(),
                format!("row:{}", server.id),
            )
        })
        .collect();
    let mut walker = Walker::new(regions);
    walker.walk(content.upcast_ref(), "body");
    walker.read_menus()?;
    window.destroy();
    Ok(walker.nodes)
}

/// Renders the Activity page from the seeded data directory through its own loader.
fn activity_screen(scenario: &Value) -> Result<Vec<Node>, String> {
    let app = test_app("com.tsout.Toolport.ParityActivity");
    let page = ActivityPage::new(&app);
    let window = new_window(&app, &page.root, scenario);
    settle(|| page.root.is_mapped(), std::time::Duration::from_secs(10))
        .map_err(|_| "the Activity page was never drawn".to_string())?;
    page.refresh();
    settle(
        || !page.loading.get() && page.refresh_button.is_sensitive(),
        std::time::Duration::from_secs(30),
    )
    .map_err(|_| "the Activity page did not finish loading in 30 s".to_string())?;
    expand_sections(page.root.upcast_ref(), scenario);
    settle(|| !page.loading.get(), std::time::Duration::from_secs(30))
        .map_err(|_| "the Activity page did not settle after opening its sections".to_string())?;
    wait_frames(page.root.upcast_ref(), 3)?;
    let mut walker = Walker::new(Vec::new());
    walker.walk(page.root.upcast_ref(), "body");
    walker.read_menus()?;
    window.destroy();
    Ok(walker.nodes)
}

/// Open the collapsed sections the scenario names (`expand`), where this
/// shell has them, so their content is compared too.
fn expand_sections(root: &gtk::Widget, scenario: &Value) {
    let names = scenario["expand"]
        .as_array()
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    fn visit(widget: &gtk::Widget, names: &[String]) {
        if let Some(expander) = widget.downcast_ref::<gtk::Expander>() {
            let title = expander
                .label_widget()
                .map(|label| inner_text(&label))
                .unwrap_or_default();
            if names.iter().any(|name| title.starts_with(name.as_str())) {
                expander.set_expanded(true);
            }
        }
        let mut child = widget.first_child();
        while let Some(next) = child {
            visit(&next, names);
            child = next.next_sibling();
        }
    }
    if !names.is_empty() {
        visit(root, &names);
    }
}

fn run_scenario(path: &std::path::Path, out: &std::path::Path) -> Result<(), String> {
    let name = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or("scenario file name")?
        .to_owned();
    let scenario: Value = serde_json::from_str(
        &std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?,
    )
    .map_err(|error| format!("{}: {error}", path.display()))?;
    let screen = scenario["screen"]
        .as_str()
        .ok_or("scenario needs a screen")?;
    let now_ms = scenario["now_ms"].as_u64().ok_or("scenario needs now_ms")?;
    super::TEST_NOW_MS.store(now_ms, std::sync::atomic::Ordering::Relaxed);

    let data = crate::registry::DataDirTestEnv::new("parity");
    let mut answers = Vec::new();

    // Struct inputs: the scenario holds the IPC answer itself.
    let registry = match scenario["ipc"].get("get_registry") {
        None => None,
        Some(input) if input.get("error").is_some() && input.get("servers").is_none() => {
            let cause = input["error"].as_str().unwrap_or_default().to_owned();
            answers.push(answer("get_registry", json!("*"), Err(cause.clone())));
            Some(Err(cause))
        }
        Some(input) => {
            let (mut registry, _) = round_trip::<crate::registry::Registry>(input, "get_registry")?;
            crate::registry::save(&registry)?;
            // What the command adds before answering.
            crate::personal_sync::attach_status(&mut registry);
            let sent = serde_json::to_value(&registry).map_err(|error| error.to_string())?;
            answers.push(answer("get_registry", json!("*"), Ok(sent)));
            Some(Ok(registry))
        }
    };
    let probes = match scenario["ipc"].get("probe_servers") {
        None => Vec::new(),
        Some(input) => {
            let (probes, sent) =
                round_trip::<Vec<crate::server_runtime::ProbeResult>>(input, "probe_servers")?;
            answers.push(answer("probe_servers", json!("*"), Ok(sent)));
            probes
        }
    };

    seed_data_dir(&data.dir, &scenario["seeds"])?;
    // Derived answers: never hand-written, always read back through Rust.
    for call in scenario["calls"].as_array().cloned().unwrap_or_default() {
        let command = call[0]
            .as_str()
            .ok_or("calls entries are [command, args]")?;
        let args = call.get(1).cloned().unwrap_or_else(|| json!({}));
        let result = materialize(command, &args)?;
        answers.push(answer(command, args, result));
    }
    let ipc = json!({ "scenario": name, "screen": screen, "now_ms": now_ms, "answers": answers });
    std::fs::write(
        out.join(format!("{name}.ipc.json")),
        serde_json::to_string_pretty(&ipc).unwrap(),
    )
    .map_err(|error| error.to_string())?;

    let nodes = match screen {
        "servers" => servers_screen(&scenario, registry, probes)?,
        "activity" => activity_screen(&scenario)?,
        other => return Err(format!("unknown screen {other:?}")),
    };
    if nodes.is_empty() {
        return Err("the GTK page showed nothing".into());
    }
    std::fs::write(
        out.join(format!("{name}.gtk.json")),
        serde_json::to_string_pretty(&nodes).unwrap(),
    )
    .map_err(|error| error.to_string())?;
    drop(data);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One scenario per process: `PARITY_SCENARIO` is the scenario file,
    /// `PARITY_OUT` the directory for its `ipc.json` and `gtk.json`.
    #[test]
    #[ignore = "run by npm run parity under xvfb"]
    fn extract_scenario() {
        let scenario = std::env::var("PARITY_SCENARIO").expect("PARITY_SCENARIO");
        let out = std::env::var("PARITY_OUT").expect("PARITY_OUT");
        if let Err(error) =
            run_scenario(std::path::Path::new(&scenario), std::path::Path::new(&out))
        {
            panic!("parity extraction failed for {scenario}: {error}");
        }
    }

    /// The extractor's own checks: hidden widgets stay out, menu items are
    /// really read, and an empty menu fails instead of comparing as empty.
    #[test]
    #[ignore = "run by npm run parity under xvfb"]
    fn extractor_self_check() {
        let app = test_app("com.tsout.Toolport.ParitySelfCheck");
        let root = gtk::Box::new(gtk::Orientation::Vertical, 4);
        root.append(&gtk::Label::new(Some("Shown text")));
        let one_line = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        one_line.append(&gtk::Label::new(Some("Name")));
        one_line.append(&gtk::Label::new(Some("· 2m ago")));
        root.append(&one_line);
        let two_lines = gtk::Box::new(gtk::Orientation::Vertical, 0);
        two_lines.append(&gtk::Label::new(Some("Line one")));
        two_lines.append(&gtk::Label::new(Some("Line two")));
        root.append(&two_lines);
        let hidden = gtk::Label::new(Some("Hidden text"));
        hidden.set_visible(false);
        root.append(&hidden);
        let hidden_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
        hidden_box.append(&gtk::Label::new(Some("Text in a hidden box")));
        hidden_box.set_visible(false);
        root.append(&hidden_box);
        let collapsed = gtk::Expander::new(Some("Collapsed section"));
        collapsed.set_child(Some(&gtk::Label::new(Some("Text in a collapsed section"))));
        root.append(&collapsed);
        let menu = gtk::MenuButton::builder()
            .tooltip_text("Full menu")
            .icon_name("open-menu-symbolic")
            .build();
        let popover = toolport_menu_popover();
        let items = gtk::Box::new(gtk::Orientation::Vertical, 4);
        items.append(&toolport_menu_button("First item"));
        let hidden_item = toolport_menu_button("Hidden item");
        hidden_item.set_visible(false);
        items.append(&hidden_item);
        items.append(&toolport_menu_button("Second item"));
        popover.set_child(Some(&items));
        menu.set_popover(Some(&popover));
        root.append(&menu);
        let window = new_window(&app, &root, &json!({}));
        settle(|| root.is_mapped(), std::time::Duration::from_secs(10)).unwrap();
        wait_frames(root.upcast_ref(), 3).unwrap();

        let mut walker = Walker::new(Vec::new());
        walker.walk(root.upcast_ref(), "body");
        walker.read_menus().unwrap();
        let values = walker
            .nodes
            .iter()
            .map(|node| node.value.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            values,
            [
                "Shown text",
                "Name · 2m ago",
                "Line one",
                "Line two",
                "Collapsed section",
                "Full menu"
            ]
        );
        assert_eq!(
            walker.nodes[5].items.as_deref(),
            Some(&["First item".to_string(), "Second item".to_string()][..])
        );

        let empty = gtk::MenuButton::builder()
            .tooltip_text("Empty menu")
            .icon_name("open-menu-symbolic")
            .build();
        let empty_popover = toolport_menu_popover();
        empty_popover.set_child(Some(&gtk::Box::new(gtk::Orientation::Vertical, 0)));
        empty.set_popover(Some(&empty_popover));
        root.append(&empty);
        settle(|| empty.is_mapped(), std::time::Duration::from_secs(10)).unwrap();
        wait_frames(root.upcast_ref(), 3).unwrap();
        let mut walker = Walker::new(Vec::new());
        walker.walk(root.upcast_ref(), "body");
        let error = walker
            .read_menus()
            .expect_err("an empty menu must fail the extraction, not compare as empty");
        assert!(
            error.contains("\"Empty menu\" in body opened with no items"),
            "{error}"
        );
        window.destroy();
        println!("parity self-check passed");
    }

    #[test]
    fn round_trip_flags_dropped_and_changed_keys_but_not_added_defaults() {
        let input = json!({"a": 1, "b": {"c": "x", "gone": true}, "n": null, "list": [{"k": 1}]});
        let output = json!({"a": 1, "b": {"c": "y", "extra": 0}, "list": [{"k": 1, "d": false}], "added": []});
        assert_eq!(
            round_trip_loss(&input, &output, "in"),
            [
                "in.b.c changed from \"x\" to \"y\"",
                "in.b.gone was dropped"
            ]
        );
        assert!(round_trip_loss(
            &input,
            &json!({"a": 1, "b": {"c": "x", "gone": true}, "list": [{"k": 1}]}),
            "in"
        )
        .is_empty());
    }

    #[test]
    fn generated_seed_lines_are_numbered_and_oldest_first() {
        let seed = json!({
            "lines": [{"ts": 5, "tool": "listed"}],
            "generate": {"count": 3, "start_ts": 10, "step_ms": 2, "cycle": [{"tool": "a_{n}"}, {"tool": "b"}]}
        });
        let lines = seed_lines(&seed);
        let tools = lines
            .iter()
            .map(|line| line["tool"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(tools, ["listed", "a_0", "b", "a_2"]);
        assert_eq!(lines[3]["ts"], 14);
    }
}
