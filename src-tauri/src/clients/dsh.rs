//! DeepSeek Harness (`dsh`) keeps MCP servers in Cordis patch files.
//!
//! A patch file is a top-level YAML list of loader rows. A row with `insert` (and
//! no `id`) appends its entries to the root; a row with an `id` overrides fields
//! on an entry an earlier layer added, `disabled: true` included. Each MCP server
//! is an entry whose `name` is the `@deepseek-ai/dsh-mcp-client` plugin and whose
//! `config` carries `serverName`, `transport` and the connection fields.
//!
//! Toolport reads and writes the home layer, `$DSH_HOME/cordis.patch.yml`, which
//! dsh applies over every profile. The file is the user's own and can hold `!!js`
//! expressions, other plugins and comments, so every edit is made on the text: an
//! entry Toolport removes loses only its own lines, and new entries go in one
//! appended `insert` row. Anything the line scan cannot place exactly is refused
//! with the file left untouched.
//!
//! Rows apply in order, as dsh's include plugin applies them: a row with an `id`
//! replaces whole keys (`config`, `disabled`) on an entry inserted before it, and
//! one that matches nothing is skipped with a warning. A server a later row
//! changes is read as dsh sees it and left for the user to edit.
//!
//! The rest of the client code sees the file through [`normalized`], an object
//! whose `mcpServers` map holds each server's `config` by `serverName`.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::Value as Json;
use serde_yaml::Value as Yaml;

use super::{McpServer, ServerEntry};

/// The dsh plugin that bridges one MCP server per entry.
pub(super) const PLUGIN: &str = "@deepseek-ai/dsh-mcp-client";
/// The server map inside [`normalized`].
pub(super) const KEY: &str = "mcpServers";

const UNTOUCHED: &str = "leaving the DeepSeek Harness patch file untouched";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Span {
    start: usize,
    end: usize,
}

struct Entry {
    span: Span,
}

struct Row {
    span: Span,
    value: Yaml,
    /// The row's `insert` entries with their text spans. `None` when the row is
    /// not an insert row, or its list is written in a way the scan cannot split.
    entries: Option<Vec<Entry>>,
}

struct Doc<'a> {
    text: &'a str,
    rows: Vec<Row>,
    /// The whole list was split into rows by line. False for a flow list or an
    /// indentation the scan does not recognize; reads still work, edits do not.
    editable: bool,
    /// A `[]` line standing for an empty list (dsh's template writes one).
    empty_flow: Option<Span>,
}

/// One server as dsh will load it, after the file's later rows.
struct Server {
    name: String,
    id: Option<String>,
    /// The entry's own `disabled`, kept when Toolport rewrites the entry.
    own_disabled: Option<Yaml>,
    disabled: Option<Yaml>,
    config: Yaml,
    /// A later row in this file targets the entry's id.
    overridden: bool,
    /// Moving this entry to the gateway would lose its group's activation context.
    in_group: bool,
    /// Index of the row and the entry within it.
    at: (usize, usize),
}

impl Server {
    fn started(&self) -> bool {
        !js_truthy(self.disabled.as_ref())
    }
}

/// `!!js` is a secondary tag (`tag:yaml.org,2002:js`), which serde_yaml drops,
/// so it is read as this local tag and written back as `!!js`.
const JS_LOCAL: &str = "!toolport-dsh-js";
const JS_TAG: &str = "!!js";

/// Mark `!!js` where it stands as a node's tag.
fn mark_tags(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (i, _) in text.match_indices(JS_TAG) {
        let after = bytes.get(i + JS_TAG.len()).copied();
        let ends = after.is_none_or(|b| b.is_ascii_whitespace() || b",]}".contains(&b));
        let before = text[..i].trim_end_matches(' ');
        let spaced = before.len() < i;
        let starts = match before.as_bytes().last() {
            None | Some(b'\n') => true,
            Some(b'-' | b':' | b'?') => spaced,
            Some(b'[' | b'{' | b',') => true,
            _ => false,
        };
        if ends && starts {
            out.push_str(&text[at..i]);
            out.push_str(JS_LOCAL);
            at = i + JS_TAG.len();
        }
    }
    out.push_str(&text[at..]);
    out
}

fn unmark(text: &str) -> String {
    text.replace(JS_LOCAL, JS_TAG)
}

fn yaml_from<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, String> {
    serde_yaml::from_str(&mark_tags(text)).map_err(|error| super::diagnostics::yaml(&error))
}

/// YAML text with `!!js` tags as dsh writes them.
fn emit(value: &Yaml) -> Result<String, String> {
    serde_yaml::to_string(value)
        .map(|text| unmark(&text))
        .map_err(|e| e.to_string())
}

/// JavaScript truthiness, as dsh reads `disabled`. An expression is decided
/// at run time; it reads as not disabled.
fn js_truthy(value: Option<&Yaml>) -> bool {
    match value {
        None | Some(Yaml::Null | Yaml::Tagged(_)) => false,
        Some(Yaml::Bool(b)) => *b,
        Some(Yaml::Number(n)) => n.as_f64().is_some_and(|n| n != 0.0 && !n.is_nan()),
        Some(Yaml::String(s)) => !s.is_empty(),
        Some(Yaml::Sequence(_) | Yaml::Mapping(_)) => true,
    }
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

/// A line with YAML content, not blank and not only a comment.
fn has_content(line: &str) -> bool {
    let trimmed = line.trim_start_matches('\u{feff}').trim();
    !trimmed.is_empty() && !trimmed.starts_with('#')
}

fn is_item(line: &str, at: usize) -> bool {
    indent(line) == at && {
        let rest = &line[at..];
        rest.starts_with("- ") || rest.trim_end() == "-"
    }
}

/// Byte offsets of each line, the line text included without its newline.
fn lines(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start = 0;
    for piece in text.split_inclusive('\n') {
        // A byte order mark stays in the text but not in the line.
        let skip = if start == 0 && piece.starts_with('\u{feff}') {
            '\u{feff}'.len_utf8()
        } else {
            0
        };
        out.push((start + skip, piece[skip..].trim_end_matches(['\n', '\r'])));
        start += piece.len();
    }
    out
}

fn line_end(text: &str, start: usize, line: &str) -> usize {
    let end = start + line.len();
    let rest = &text[end..];
    if rest.starts_with("\r\n") {
        end + 2
    } else if rest.starts_with('\n') {
        end + 1
    } else {
        end
    }
}

/// Parse a slice holding one block-list item.
fn parse_item(text: &str) -> Option<Yaml> {
    let mut list: Vec<Yaml> = yaml_from(text).ok()?;
    (list.len() == 1).then(|| list.remove(0))
}

/// Split `[from, to)` of `all` into list items at column `at`. Each item runs to
/// its last content line; comments and blanks after it belong to the gap.
fn split_items(text: &str, all: &[(usize, &str)], at: usize) -> Option<Vec<Span>> {
    let mut spans: Vec<Span> = Vec::new();
    let mut open: Option<(usize, usize)> = None;
    for &(start, line) in all {
        if !has_content(line) {
            continue;
        }
        if is_item(line, at) {
            if let Some((s, e)) = open.take() {
                spans.push(Span { start: s, end: e });
            }
            open = Some((start, line_end(text, start, line)));
        } else if indent(line) > at {
            let (_, end) = open.as_mut()?;
            *end = line_end(text, start, line);
        } else {
            return None;
        }
    }
    if let Some((s, e)) = open {
        spans.push(Span { start: s, end: e });
    }
    Some(spans)
}

/// The text column where an item's mapping keys start (`- insert:` -> 2).
fn key_column(line: &str, at: usize) -> usize {
    let after = &line[at + 1..];
    at + 1 + (after.len() - after.trim_start_matches(' ').len())
}

fn insert_entries(text: &str, row: Span, value: &Yaml) -> Option<Vec<Entry>> {
    let expected = value.get("insert")?.as_sequence()?;
    let all = lines(&text[row.start..row.end]);
    let all: Vec<(usize, &str)> = all.into_iter().map(|(s, l)| (s + row.start, l)).collect();
    let first = all.first()?.1;
    let keys = key_column(first, indent(first));
    // The `insert:` key sits on the item line itself or on a sibling key line.
    let key_line = all.iter().enumerate().position(|(i, &(_, line))| {
        (i == 0 || indent(line) == keys)
            && line
                .get(keys..)
                .is_some_and(|rest| rest.starts_with("insert:"))
    })?;
    let (_, line) = all[key_line];
    let rest = line[keys + "insert:".len()..].trim();
    if has_content(rest) {
        // A flow list (`insert: [...]`) or an anchor; not split by line.
        return None;
    }
    let body: Vec<(usize, &str)> = all[key_line + 1..]
        .iter()
        .copied()
        .take_while(|&(_, line)| !has_content(line) || indent(line) > keys || is_item(line, keys))
        .collect();
    let at = body
        .iter()
        .find(|(_, line)| has_content(line))
        .map(|(_, line)| indent(line))?;
    let spans = split_items(text, &body, at)?;
    if spans.len() != expected.len() {
        return None;
    }
    spans
        .into_iter()
        .zip(expected)
        .map(|(span, want)| {
            let got = parse_item(&dedent(&text[span.start..span.end], at))?;
            (&got == want).then_some(Entry { span })
        })
        .collect()
}

/// Strip `by` leading columns from every line that has them.
fn dedent(text: &str, by: usize) -> String {
    text.split_inclusive('\n')
        // Checked as bytes: a shallower comment line can hold a multibyte
        // character across column `by`.
        .map(|line| match line.as_bytes().get(..by) {
            Some(head) if head.iter().all(|&b| b == b' ') => &line[by..],
            _ => line.trim_start_matches(' '),
        })
        .collect()
}

fn root_list(text: &str) -> Result<Vec<Yaml>, String> {
    if !text.lines().any(has_content) {
        return Ok(Vec::new());
    }
    match yaml_from::<Yaml>(text) {
        Ok(Yaml::Sequence(list)) => Ok(list),
        Ok(Yaml::Null) => Ok(Vec::new()),
        Ok(_) => Err(format!(
            "The DeepSeek Harness patch file must be a top-level list; {UNTOUCHED}."
        )),
        Err(e) => Err(format!(
            "Could not parse the DeepSeek Harness patch file ({e}); {UNTOUCHED}."
        )),
    }
}

fn parse(text: &str) -> Result<Doc<'_>, String> {
    let list = root_list(text)?;
    let all = lines(text);
    let first = all.iter().find(|(_, line)| has_content(line));
    let mut doc = Doc {
        text,
        rows: Vec::new(),
        editable: true,
        empty_flow: None,
    };
    let Some(&(start, line)) = first else {
        return Ok(doc);
    };
    if list.is_empty() {
        let bare = line.split(" #").next().unwrap_or(line).trim();
        if bare == "[]" && all.iter().filter(|(_, l)| has_content(l)).count() == 1 {
            doc.empty_flow = Some(Span {
                start,
                end: line_end(text, start, line),
            });
        } else {
            doc.editable = false;
        }
        return Ok(doc);
    }
    let at = indent(line);
    let spans = if is_item(line, at) {
        split_items(text, &all, at)
    } else {
        None
    };
    let spans = match spans {
        Some(spans) if spans.len() == list.len() => spans,
        _ => {
            doc.editable = false;
            doc.rows = list
                .into_iter()
                .map(|value| Row {
                    span: Span { start: 0, end: 0 },
                    value,
                    entries: None,
                })
                .collect();
            return Ok(doc);
        }
    };
    for (span, value) in spans.into_iter().zip(list) {
        let got = parse_item(&dedent(&text[span.start..span.end], at));
        if got.as_ref() != Some(&value) {
            doc.editable = false;
        }
        let entries = insert_entries(text, span, &value);
        doc.rows.push(Row {
            span,
            value,
            entries,
        });
    }
    Ok(doc)
}

fn str_of<'a>(value: &'a Yaml, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Yaml::as_str)
}

fn is_mcp_entry(entry: &Yaml) -> bool {
    str_of(entry, "name") == Some(PLUGIN)
        && entry
            .get("config")
            .and_then(|config| str_of(config, "serverName"))
            .is_some()
}

/// Every `id` the file uses, on entries at any depth and on override rows.
fn ids(value: &Yaml, out: &mut BTreeSet<String>) {
    match value {
        Yaml::Mapping(map) => {
            if let Some(Yaml::String(id)) = map.get("id") {
                out.insert(id.clone());
            }
            map.values().for_each(|v| ids(v, out));
        }
        Yaml::Sequence(list) => list.iter().for_each(|v| ids(v, out)),
        Yaml::Tagged(tagged) => ids(&tagged.value, out),
        _ => {}
    }
}

impl Doc<'_> {
    /// MCP servers after applying the file's rows in order.
    fn servers(&self) -> Vec<Server> {
        struct Node {
            value: Yaml,
            parent: Option<usize>,
            at: Option<(usize, usize)>,
            live: bool,
            overridden: bool,
        }
        fn insert(
            value: &Yaml,
            parent: Option<usize>,
            at: Option<(usize, usize)>,
            indexed: bool,
            nodes: &mut Vec<Node>,
            by_id: &mut HashMap<String, usize>,
        ) {
            let index = nodes.len();
            nodes.push(Node {
                value: value.clone(),
                parent,
                at,
                live: true,
                overridden: false,
            });
            if let Some(id) = str_of(value, "id").filter(|_| indexed) {
                by_id.insert(id.to_string(), index);
            }
            // Keep the tree even before a later row enables a group. The loader
            // indexes children only when buildMap visits an inserted group.
            for child in value
                .get("config")
                .and_then(Yaml::as_sequence)
                .into_iter()
                .flatten()
            {
                insert(
                    child,
                    Some(index),
                    None,
                    indexed && js_truthy(value.get("group")),
                    nodes,
                    by_id,
                );
            }
        }
        fn started(nodes: &[Node], index: usize) -> bool {
            let node = &nodes[index];
            node.live
                && !js_truthy(node.value.get("disabled"))
                && node
                    .parent
                    .is_none_or(|p| js_truthy(nodes[p].value.get("group")) && started(nodes, p))
        }
        let mut nodes: Vec<Node> = Vec::new();
        let mut by_id: HashMap<String, usize> = HashMap::new();
        for (r, row) in self.rows.iter().enumerate() {
            if let Some(list) = row.value.get("insert").and_then(Yaml::as_sequence) {
                let parent = if row
                    .value
                    .get("id")
                    .is_some_and(|id| matches!(id, Yaml::Tagged(_)) || js_truthy(Some(id)))
                {
                    let Some(&target) = str_of(&row.value, "id").and_then(|id| by_id.get(id))
                    else {
                        // Profile-dependent inserts are left in place, never invented as root servers.
                        continue;
                    };
                    if !js_truthy(nodes[target].value.get("group")) {
                        continue;
                    }
                    if !nodes[target]
                        .value
                        .get("config")
                        .is_some_and(Yaml::is_sequence)
                    {
                        nodes[target].value["config"] = Yaml::Sequence(Vec::new());
                    }
                    Some(target)
                } else {
                    None
                };
                for (e, entry) in list.iter().enumerate() {
                    insert(entry, parent, Some((r, e)), true, &mut nodes, &mut by_id);
                }
                continue;
            }
            let Some(&i) = str_of(&row.value, "id").and_then(|id| by_id.get(id)) else {
                continue;
            };
            if str_of(&row.value, "name")
                .is_some_and(|name| Some(name) != str_of(&nodes[i].value, "name"))
            {
                continue;
            }
            nodes[i].overridden = true;
            for (key, value) in row.value.as_mapping().into_iter().flatten() {
                if key.as_str().is_some_and(|key| key != "id" && key != "name") {
                    if let Some(map) = nodes[i].value.as_mapping_mut() {
                        map.insert(key.clone(), value.clone());
                    }
                }
            }
            if row.value.get("config").is_some() {
                // Replacing a group config disconnects its old children from the tree.
                for node in &mut nodes {
                    if node.parent == Some(i) {
                        node.live = false;
                    }
                }
                let children = nodes[i]
                    .value
                    .get("config")
                    .and_then(Yaml::as_sequence)
                    .cloned()
                    .unwrap_or_default();
                for child in children {
                    // applyEntryPatches does not rebuild its id map for overrides.
                    insert(&child, Some(i), None, false, &mut nodes, &mut by_id);
                }
            }
        }
        nodes
            .iter()
            .enumerate()
            .filter(|(i, node)| is_mcp_entry(&node.value) && started(&nodes, *i))
            .map(|(_, node)| Server {
                name: str_of(&node.value["config"], "serverName")
                    .unwrap_or_default()
                    .to_string(),
                id: str_of(&node.value, "id").map(str::to_string),
                own_disabled: node.value.get("disabled").cloned(),
                disabled: node.value.get("disabled").cloned(),
                config: node.value["config"].clone(),
                overridden: node.overridden || node.at.is_none(),
                in_group: node.parent.is_some(),
                at: node.at.unwrap_or((usize::MAX, usize::MAX)),
            })
            .collect()
    }

    /// Servers dsh will start.
    fn active(&self) -> Vec<Server> {
        self.servers()
            .into_iter()
            .filter(|server| server.started() && !server.name.is_empty())
            .collect()
    }

    /// Every row and entry except the servers dsh starts, for change detection
    /// outside them. A disabled server stays here, untouched by every edit.
    fn other(&self) -> Json {
        let started: BTreeSet<(usize, usize)> =
            self.active().into_iter().map(|server| server.at).collect();
        let mut rows = Vec::new();
        for (r, row) in self.rows.iter().enumerate() {
            let mut value = row.value.clone();
            if let Some(list) = value.get_mut("insert").and_then(Yaml::as_sequence_mut) {
                let mut e = 0;
                list.retain(|_| {
                    e += 1;
                    !started.contains(&(r, e - 1))
                });
                // A row that only carried servers says nothing else.
                let only_insert = row.value.as_mapping().is_some_and(|map| map.len() == 1);
                if list.is_empty() && only_insert {
                    continue;
                }
            }
            rows.push(to_json(&value));
        }
        Json::Array(rows)
    }

    fn ids(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for row in &self.rows {
            ids(&row.value, &mut out);
        }
        out
    }
}

/// YAML to JSON for comparisons and the shared client code. A tagged value (a
/// `!!js` expression) becomes `{"!tag": tag, "value": inner}`, so it compares by
/// its source and can be written back as the same tag.
pub(super) fn to_json(value: &Yaml) -> Json {
    match value {
        Yaml::Null => Json::Null,
        Yaml::Bool(b) => Json::Bool(*b),
        Yaml::Number(n) => serde_json::to_value(n).unwrap_or(Json::Null),
        Yaml::String(s) => Json::String(unmark(s)),
        Yaml::Sequence(list) => Json::Array(list.iter().map(to_json).collect()),
        Yaml::Mapping(map) => Json::Object(
            map.iter()
                .map(|(k, v)| {
                    let key = match k {
                        Yaml::String(s) => unmark(s),
                        other => emit(other).unwrap_or_default().trim().to_string(),
                    };
                    (key, to_json(v))
                })
                .collect(),
        ),
        Yaml::Tagged(tagged) => serde_json::json!({
            "!tag": unmark(&tagged.tag.to_string()),
            "value": to_json(&tagged.value),
        }),
    }
}

fn to_yaml(value: &Json) -> Yaml {
    match value {
        Json::Null => Yaml::Null,
        Json::Bool(b) => Yaml::Bool(*b),
        Json::Number(n) => serde_yaml::to_value(n).unwrap_or(Yaml::Null),
        Json::String(s) => Yaml::String(s.clone()),
        Json::Array(list) => Yaml::Sequence(list.iter().map(to_yaml).collect()),
        Json::Object(map) => {
            if let (Some(Json::String(tag)), Some(inner), 2) =
                (map.get("!tag"), map.get("value"), map.len())
            {
                let tag = if tag == JS_TAG { JS_LOCAL } else { tag };
                return Yaml::Tagged(Box::new(serde_yaml::value::TaggedValue {
                    tag: serde_yaml::value::Tag::new(tag.trim_start_matches('!')),
                    value: to_yaml(inner),
                }));
            }
            Yaml::Mapping(
                map.iter()
                    .map(|(k, v)| (Yaml::String(k.clone()), to_yaml(v)))
                    .collect(),
            )
        }
    }
}

/// The file as the shared client code sees it: `{"mcpServers": {name: config},
/// "rows": [...]}`. Disabled servers are left out, as dsh does not start them.
pub(super) fn normalized(text: &str) -> Result<Json, String> {
    let doc = parse(text)?;
    let servers = current(&doc);
    let mut root = serde_json::Map::new();
    if !servers.is_empty() {
        root.insert(KEY.into(), Json::Object(servers));
    }
    let other = doc.other();
    if other.as_array().is_some_and(|rows| !rows.is_empty()) {
        root.insert("rows".into(), other);
    }
    Ok(Json::Object(root))
}

fn strings(value: Option<&Yaml>) -> Vec<String> {
    value
        .and_then(Yaml::as_sequence)
        .map(|list| {
            list.iter()
                .filter_map(|item| match item {
                    Yaml::String(s) => Some(unmark(s)),
                    Yaml::Number(n) => Some(n.to_string()),
                    Yaml::Bool(b) => Some(b.to_string()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn keys(value: Option<&Yaml>) -> Vec<String> {
    let mut out: Vec<String> = value
        .and_then(Yaml::as_mapping)
        .map(|map| {
            map.keys()
                .filter_map(|k| k.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    out.sort_unstable();
    out.dedup();
    out
}

/// The servers dsh will start, for import and the client list.
pub(super) fn servers(text: &str) -> Result<Vec<McpServer>, String> {
    let doc = parse(text)?;
    Ok(doc
        .active()
        .into_iter()
        .map(|server| {
            let config = &server.config;
            let transport = match str_of(config, "transport") {
                Some("stdio") => "stdio",
                Some("streamable-http") => "http",
                _ => "unknown",
            };
            let mut env_keys = keys(config.get("env"));
            env_keys.extend(keys(config.get("headers")));
            env_keys.sort_unstable();
            env_keys.dedup();
            McpServer {
                name: server.name,
                transport: transport.into(),
                command: str_of(config, "command").map(unmark),
                args: strings(config.get("args")),
                env_keys,
                url: str_of(config, "url").map(unmark),
            }
        })
        .collect())
}

/// dsh accepts `[A-Za-z0-9_-]{1,32}` as a server name.
fn valid_name(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// A Toolport server as a dsh `config` mapping.
pub(super) fn config_of(entry: &ServerEntry) -> Result<Json, String> {
    if !valid_name(&entry.name) {
        return Err(format!(
            "DeepSeek Harness server names use only letters, digits, '-' and '_' (at most 32); rename '{}' first. {UNTOUCHED}.",
            entry.name
        ));
    }
    let values: serde_json::Map<String, Json> = entry
        .env
        .iter()
        .filter_map(|e| {
            e.value
                .as_ref()
                .map(|v| (e.key.clone(), Json::String(v.clone())))
        })
        .collect();
    let mut config = serde_json::Map::new();
    config.insert("serverName".into(), Json::String(entry.name.clone()));
    if let Some(command) = &entry.command {
        config.insert("transport".into(), "stdio".into());
        config.insert("command".into(), Json::String(command.clone()));
        config.insert(
            "args".into(),
            Json::Array(entry.args.iter().cloned().map(Json::String).collect()),
        );
        if !values.is_empty() {
            config.insert("env".into(), Json::Object(values));
        }
        if let Some(cwd) = &entry.cwd {
            config.insert("cwd".into(), Json::String(cwd.clone()));
        }
    } else if let Some(url) = &entry.url {
        if entry.transport.eq_ignore_ascii_case("sse") {
            return Err(format!(
                "DeepSeek Harness connects to stdio and Streamable HTTP servers, not SSE ('{}'). {UNTOUCHED}.",
                entry.name
            ));
        }
        config.insert("transport".into(), "streamable-http".into());
        config.insert("url".into(), Json::String(url.clone()));
        if !values.is_empty() {
            config.insert("headers".into(), Json::Object(values));
        }
    } else {
        return Err(format!(
            "'{}' has no command or URL; {UNTOUCHED}.",
            entry.name
        ));
    }
    Ok(Json::Object(config))
}

/// Raw entry text for the move record: the entry mapping, dedented, so it goes
/// back verbatim (comments and `!!js` included) when Disconnect restores it.
pub(super) fn raw_entries(text: &str) -> Result<Vec<(String, String, Json)>, String> {
    let doc = parse(text)?;
    let mut out = Vec::new();
    // Only servers dsh starts; a disabled entry stays where it is. One a later
    // row changes is not moved, since Toolport doesn't edit it.
    for server in doc.active() {
        if server.overridden || server.in_group {
            continue;
        }
        let (r, e) = server.at;
        let Some(entry) = doc.rows[r].entries.as_ref().and_then(|list| list.get(e)) else {
            // Not split by line: keep the parsed value as YAML instead.
            let value = &doc.rows[r].value["insert"][e];
            out.push((server.name, emit(value)?, to_json(&server.config)));
            continue;
        };
        let item = &doc.text[entry.span.start..entry.span.end];
        out.push((server.name, item_body(item), to_json(&server.config)));
    }
    Ok(out)
}

/// A list item's mapping, dedented: "    - id: x\n      name: y" -> "id: x\nname: y".
fn item_body(item: &str) -> String {
    let item = dedent(item, indent(item));
    if let Some((first, rest)) = item.split_once('\n') {
        if first.trim_end() == "-" {
            let at = rest
                .lines()
                .find(|line| has_content(line))
                .map_or(0, indent);
            return dedent(rest, at);
        }
    }
    let keys = key_column(&item, 0);
    let body = format!("{}{}", " ".repeat(keys), &item[keys..]);
    dedent(&body, keys)
}

/// The fields of one recorded raw entry.
pub(super) fn definition(raw: &str) -> Result<Json, String> {
    let doc: Yaml = yaml_from(raw).map_err(|_| "Could not read imported settings")?;
    Ok(to_json(&doc["config"]))
}

/// Render entry mappings as list items whose dash sits at `column`.
fn items(entries: &[String], column: usize) -> String {
    let mut out = String::new();
    for entry in entries {
        for (i, line) in entry.lines().enumerate() {
            if line.is_empty() {
                out.push('\n');
                continue;
            }
            out.push_str(&" ".repeat(column));
            out.push_str(if i == 0 { "- " } else { "  " });
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn entry_text(id: &str, disabled: Option<&Yaml>, config: &Json) -> Result<String, String> {
    let mut map = serde_yaml::Mapping::new();
    map.insert("id".into(), id.into());
    map.insert("name".into(), PLUGIN.into());
    if let Some(disabled) = disabled {
        map.insert("disabled".into(), disabled.clone());
    }
    map.insert("config".into(), ordered(config));
    emit(&Yaml::Mapping(map))
}

/// A config mapping with dsh's documented keys first, as its guides write them.
fn ordered(config: &Json) -> Yaml {
    const ORDER: [&str; 8] = [
        "serverName",
        "transport",
        "command",
        "args",
        "cwd",
        "env",
        "url",
        "headers",
    ];
    let Json::Object(map) = config else {
        return to_yaml(config);
    };
    let mut out = serde_yaml::Mapping::new();
    for key in ORDER {
        if let Some(value) = map.get(key) {
            out.insert(key.into(), to_yaml(value));
        }
    }
    for (key, value) in map {
        if !ORDER.contains(&key.as_str()) {
            out.insert(key.as_str().into(), to_yaml(value));
        }
    }
    Yaml::Mapping(out)
}

/// Remove `drop` entries, rewrite `replace` entries where they stand and
/// append `add` entry texts as one row.
fn rewrite(
    doc: &Doc,
    drop: &BTreeSet<(usize, usize)>,
    replace: &BTreeMap<(usize, usize), String>,
    add: &[String],
) -> Result<String, String> {
    let refuse = || {
        format!("This DeepSeek Harness patch file is laid out in a way Toolport can't edit safely; edit it by hand, {UNTOUCHED}.")
    };
    if !doc.editable {
        return Err(refuse());
    }
    let text = doc.text;
    let crlf = text.contains("\r\n");
    // Match a CRLF file; lines Toolport didn't write keep their endings.
    let ending = |added: String| {
        if crlf {
            added.replace('\n', "\r\n")
        } else {
            added
        }
    };
    let mut cuts: Vec<(Span, String)> = Vec::new();
    for (r, row) in doc.rows.iter().enumerate() {
        let gone: Vec<usize> = drop
            .iter()
            .filter(|(dr, _)| *dr == r)
            .map(|(_, e)| *e)
            .collect();
        let changed: Vec<(usize, &String)> = replace
            .iter()
            .filter(|((cr, _), _)| *cr == r)
            .map(|((_, e), entry)| (*e, entry))
            .collect();
        if gone.is_empty() && changed.is_empty() {
            continue;
        }
        let entries = row.entries.as_ref().ok_or_else(refuse)?;
        let only_insert = row
            .value
            .as_mapping()
            .is_some_and(|map| map.len() == 1 && map.contains_key("insert"));
        if gone.len() == entries.len() && only_insert {
            cuts.push((row.span, String::new()));
            continue;
        } else if gone.len() == entries.len() {
            // The row has other keys (a group insert); leave it to the user.
            return Err(refuse());
        }
        cuts.extend(gone.iter().map(|&e| (entries[e].span, String::new())));
        for (e, entry) in changed {
            let span = entries[e].span;
            let column = indent(&text[span.start..]);
            cuts.push((span, ending(items(std::slice::from_ref(entry), column))));
        }
    }
    if let Some(span) = doc.empty_flow.filter(|_| !add.is_empty()) {
        cuts.push((span, String::new()));
    }
    cuts.sort_by_key(|(span, _)| span.start);
    let mut out = String::with_capacity(text.len() + 256);
    let mut at = 0;
    for (span, with) in cuts {
        out.push_str(&text[at..span.start]);
        out.push_str(&with);
        at = span.end;
    }
    out.push_str(&text[at..]);
    if !add.is_empty() {
        let mut addition = String::new();
        if !out.is_empty() && !out.ends_with('\n') {
            addition.push('\n');
        }
        let blank = out.ends_with("\n\n") || out.ends_with("\r\n\r\n");
        if out.lines().any(has_content) && !blank {
            addition.push('\n');
        }
        addition.push_str("- insert:\n");
        addition.push_str(&items(add, 4));
        out.push_str(&ending(addition));
    }
    if !out.lines().any(has_content) {
        // dsh refuses a file that is not a list; keep its own empty form.
        if !out.is_empty() && !out.ends_with('\n') {
            out.push_str(&ending("\n".into()));
        }
        out.push_str(&ending("[]\n".into()));
    }
    Ok(out)
}

/// Make the file's MCP servers exactly `desired` (`serverName` -> `config`).
/// Entries already matching stay byte for byte; changed ones are rewritten
/// where they stand with their id. A server to add whose recorded raw entry in
/// `raw` still matches goes back verbatim, its id and comments included.
pub(super) fn sync(
    text: Option<&str>,
    desired: &serde_json::Map<String, Json>,
    raw: &BTreeMap<String, String>,
) -> Result<String, String> {
    let text = text.unwrap_or("");
    let doc = parse(text)?;
    let before_other = doc.other();
    let active = doc.active();
    let mut names = BTreeSet::new();
    if let Some(twice) = active.iter().find(|s| !names.insert(s.name.as_str())) {
        if &current(&doc) == desired {
            return Ok(text.to_string());
        }
        return Err(format!(
            "Two DeepSeek Harness servers are named '{}'; remove one by hand, {UNTOUCHED}.",
            twice.name
        ));
    }
    let locked = |name: &str| {
        format!("DeepSeek Harness stores '{name}' inside a nested entry or changes it in a later row; edit that server by hand, {UNTOUCHED}.")
    };
    let mut taken = doc.ids();
    let fresh = |name: &str, taken: &mut BTreeSet<String>| {
        let mut id = format!("mcp-{name}");
        let mut n = 2;
        while taken.contains(&id) {
            id = format!("mcp-{name}-{n}");
            n += 1;
        }
        taken.insert(id.clone());
        id
    };
    let mut drop = BTreeSet::new();
    let mut replace = BTreeMap::new();
    for server in &active {
        match desired.get(&server.name) {
            Some(want) if &to_json(&server.config) == want => {}
            _ if server.overridden => return Err(locked(&server.name)),
            Some(want) => {
                let id = match &server.id {
                    Some(id) => id.clone(),
                    None => fresh(&server.name, &mut taken),
                };
                replace.insert(
                    server.at,
                    entry_text(&id, server.own_disabled.as_ref(), want)?,
                );
            }
            None if server.in_group => return Err(format!(
                "Moving '{}' out of its DeepSeek Harness group would lose its activation context; keep that server in the client, {UNTOUCHED}.", server.name
            )),
            None => {
                drop.insert(server.at);
            }
        }
    }
    let mut add = Vec::new();
    for (name, config) in desired {
        if names.contains(name.as_str()) {
            continue;
        }
        let recorded = raw.get(name).filter(|text| {
            let id = yaml_from::<Yaml>(text)
                .ok()
                .and_then(|value| str_of(&value, "id").map(str::to_string));
            definition(text).ok().as_ref() == Some(config) && id.is_some_and(|id| taken.insert(id))
        });
        match recorded {
            Some(text) => add.push(text.clone()),
            None => add.push(entry_text(&fresh(name, &mut taken), None, config)?),
        }
    }
    if drop.is_empty() && replace.is_empty() && add.is_empty() {
        return Ok(text.to_string());
    }
    let out = rewrite(&doc, &drop, &replace, &add)?;
    verify(&out, &before_other, desired)?;
    Ok(out)
}

fn current(doc: &Doc) -> serde_json::Map<String, Json> {
    doc.active()
        .into_iter()
        .map(|server| (server.name, to_json(&server.config)))
        .collect()
}

/// Append raw entry mappings (from the move record) as one row.
pub(super) fn append_raw(text: Option<&str>, entries: &[String]) -> Result<String, String> {
    let text = text.unwrap_or("");
    let doc = parse(text)?;
    let before = normalized(text)?;
    let out = rewrite(&doc, &BTreeSet::new(), &BTreeMap::new(), entries)?;
    let after = normalized(&out)?;
    if after.get("rows") != before.get("rows") {
        return Err(format!(
            "Restoring would change other DeepSeek Harness settings; {UNTOUCHED}."
        ));
    }
    Ok(out)
}

fn verify(
    out: &str,
    before_other: &Json,
    desired: &serde_json::Map<String, Json>,
) -> Result<(), String> {
    let doc = parse(out)
        .map_err(|e| format!("Toolport produced an unreadable patch file ({e}); {UNTOUCHED}."))?;
    if &current(&doc) != desired || &doc.other() != before_other {
        return Err(format!(
            "Toolport could not apply this change exactly; {UNTOUCHED}."
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "dsh_tests.rs"]
mod tests;
