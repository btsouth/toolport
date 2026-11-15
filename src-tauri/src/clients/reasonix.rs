//! Reasonix keeps MCP servers as `[[plugins]]` tables in its global
//! `config.toml` (`<Reasonix home>/config.toml`).
//!
//! Each table names its server with `name` and uses Claude Code's fields:
//! `type` (`stdio` by default, `http` or `sse`), `command`/`args`/`env` or
//! `url`/`headers`, plus Reasonix's own policy keys. The same file holds the
//! user's whole Reasonix configuration, mostly comments in the template Reasonix
//! writes, so edits go through `toml_edit`: a table Toolport changes keeps its
//! place, a new one is appended, and everything else stays as it was. Every edit
//! is checked by reading the result back.
//!
//! The rest of the client code sees the file through [`normalized`]:
//! `{"mcpServers": {name: table}, "rest": {...}}`.

use serde_json::Value as Json;
use toml_edit::{ArrayOfTables, DocumentMut, InlineTable, Item, Table, Value};

use super::{McpServer, ServerEntry};

pub(super) const KEY: &str = "mcpServers";

const UNTOUCHED: &str = "leaving the Reasonix config untouched";

fn document(text: &str) -> Result<DocumentMut, String> {
    if text.trim().is_empty() {
        return Ok(DocumentMut::new());
    }
    text.parse::<DocumentMut>()
        .map_err(|e| super::diagnostics::offset("TOML", text, e.span()))
}

/// The `[[plugins]]` tables, or `None` when the key holds something else.
fn plugins(doc: &DocumentMut) -> Result<Option<&ArrayOfTables>, String> {
    match doc.get("plugins") {
        None => Ok(None),
        Some(Item::ArrayOfTables(list)) => Ok(Some(list)),
        Some(Item::Value(Value::Array(list))) if list.is_empty() => Ok(None),
        Some(Item::Value(Value::Array(_))) => Err(format!(
            "This Reasonix config lists plugins inline; edit it by hand, {UNTOUCHED}."
        )),
        Some(_) => Err(format!(
            "'plugins' in the Reasonix config must be a list of [[plugins]] tables; {UNTOUCHED}."
        )),
    }
}

fn name_of(table: &Table) -> Option<&str> {
    table.get("name").and_then(Item::as_str)
}

fn to_json(table: &Table) -> Json {
    let mut doc = DocumentMut::new();
    for (key, item) in table.iter() {
        doc.insert(key, item.clone());
    }
    toml::from_str::<toml::Value>(&doc.to_string())
        .ok()
        .and_then(|value| serde_json::to_value(value).ok())
        .unwrap_or(Json::Null)
}

/// A table's fields other than `name`, as the shared code compares them.
fn config_json(table: &Table) -> Json {
    let mut value = to_json(table);
    if let Some(map) = value.as_object_mut() {
        map.remove("name");
    }
    value
}

/// Everything in the file except the plugin tables.
fn rest(text: &str) -> Result<Json, String> {
    if text.trim().is_empty() {
        return Ok(Json::Object(serde_json::Map::new()));
    }
    let mut value: toml::Value =
        toml::from_str(text).map_err(|e| super::diagnostics::offset("TOML", text, e.span()))?;
    if let Some(table) = value.as_table_mut() {
        table.remove("plugins");
    }
    serde_json::to_value(value).map_err(|e| e.to_string())
}

fn servers_map(doc: &DocumentMut) -> Result<serde_json::Map<String, Json>, String> {
    let mut out = serde_json::Map::new();
    for table in plugins(doc)?.into_iter().flat_map(ArrayOfTables::iter) {
        if let Some(name) = name_of(table) {
            let mut config = config_json(table);
            if let Some(previous) = out.get(name) {
                // The last command wins, but earlier disabled-tool policies survive.
                let mut disabled = previous
                    .get("disabled_tools")
                    .and_then(Json::as_array)
                    .cloned()
                    .unwrap_or_default();
                for tool in config
                    .get("disabled_tools")
                    .and_then(Json::as_array)
                    .into_iter()
                    .flatten()
                {
                    if !disabled.contains(tool) {
                        disabled.push(tool.clone());
                    }
                }
                if !disabled.is_empty() {
                    config["disabled_tools"] = Json::Array(disabled);
                }
            }
            out.insert(name.to_string(), config);
        }
    }
    Ok(out)
}

/// `{"mcpServers": {name: fields}, "rest": everything else}`.
pub(super) fn normalized(text: &str) -> Result<Json, String> {
    let doc = document(text)?;
    let servers = servers_map(&doc)?;
    let mut root = serde_json::Map::new();
    if !servers.is_empty() {
        root.insert(KEY.into(), Json::Object(servers));
    }
    let rest = rest(text)?;
    if rest.as_object().is_some_and(|map| !map.is_empty()) {
        root.insert("rest".into(), rest);
    }
    Ok(Json::Object(root))
}

fn strings(value: Option<&Json>) -> Vec<String> {
    value
        .and_then(Json::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|item| match item {
                    Json::String(s) => Some(s.clone()),
                    Json::Number(n) => Some(n.to_string()),
                    Json::Bool(b) => Some(b.to_string()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The servers in the file, for import and the client list.
pub(super) fn servers(text: &str) -> Result<Vec<McpServer>, String> {
    let doc = document(text)?;
    Ok(servers_map(&doc)?
        .into_iter()
        .map(|(name, config)| {
            let text = |key: &str| config.get(key).and_then(Json::as_str).map(str::to_string);
            let transport = match text("type").as_deref() {
                None | Some("stdio") | Some("") => "stdio",
                Some("http") | Some("streamable-http") => "http",
                Some("sse") => "sse",
                Some(_) => "unknown",
            };
            let mut env_keys: Vec<String> = ["env", "headers"]
                .iter()
                .filter_map(|key| config.get(*key).and_then(Json::as_object))
                .flat_map(|map| map.keys().cloned())
                .collect();
            env_keys.sort_unstable();
            env_keys.dedup();
            McpServer {
                name,
                transport: transport.into(),
                command: text("command"),
                args: strings(config.get("args")),
                env_keys,
                url: text("url"),
            }
        })
        .collect())
}

/// A Toolport server as a Reasonix plugin table's fields (without `name`).
pub(super) fn config_of(entry: &ServerEntry) -> Result<Json, String> {
    if entry.name.trim().is_empty() {
        return Err(format!("A Reasonix plugin needs a name; {UNTOUCHED}."));
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
    if let Some(command) = &entry.command {
        config.insert("command".into(), Json::String(command.clone()));
        if !entry.args.is_empty() {
            config.insert(
                "args".into(),
                Json::Array(entry.args.iter().cloned().map(Json::String).collect()),
            );
        }
        if !values.is_empty() {
            config.insert("env".into(), Json::Object(values));
        }
    } else if let Some(url) = &entry.url {
        let kind = if entry.transport.eq_ignore_ascii_case("sse") {
            "sse"
        } else {
            "http"
        };
        config.insert("type".into(), kind.into());
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

fn edit_value(value: &Json) -> Option<Value> {
    Some(match value {
        Json::String(s) => Value::from(s.as_str()),
        Json::Bool(b) => Value::from(*b),
        Json::Number(n) => match n.as_i64() {
            Some(i) => Value::from(i),
            None => Value::from(n.as_f64()?),
        },
        Json::Array(list) => {
            let mut array = toml_edit::Array::new();
            for item in list {
                array.push(edit_value(item)?);
            }
            Value::Array(array)
        }
        Json::Object(map) => {
            let mut inline = InlineTable::new();
            for (key, item) in map {
                inline.insert(key, edit_value(item)?);
            }
            Value::InlineTable(inline)
        }
        Json::Null => return None,
    })
}

/// The documented keys first, the way Reasonix's own `mcp add` writes them.
const ORDER: [&str; 7] = ["type", "command", "args", "env", "url", "headers", "load"];

fn new_table(name: &str, config: &Json) -> Result<Table, String> {
    let mut table = Table::new();
    table.insert("name", Item::Value(Value::from(name)));
    fill(&mut table, config)?;
    Ok(table)
}

/// Set `table`'s fields to `config`, keeping the place and comments of keys that
/// stay. `name` is never touched.
fn fill(table: &mut Table, config: &Json) -> Result<(), String> {
    let map = config
        .as_object()
        .ok_or_else(|| format!("A Reasonix plugin must be a table; {UNTOUCHED}."))?;
    let stale: Vec<String> = table
        .iter()
        .map(|(key, _)| key.to_string())
        .filter(|key| key != "name" && !map.contains_key(key))
        .collect();
    for key in stale {
        table.remove(&key);
    }
    let mut keys: Vec<&String> = map.keys().collect();
    keys.sort_by_key(|key| ORDER.iter().position(|k| k == key).unwrap_or(ORDER.len()));
    for key in keys {
        if key == "name" {
            continue;
        }
        let value = &map[key];
        let current = table.get(key).map(|item| {
            let mut one = Table::new();
            one.insert(key, item.clone());
            config_json(&one).get(key).cloned()
        });
        if current.flatten().as_ref() == Some(value) {
            continue;
        }
        let item = edit_value(value)
            .ok_or_else(|| format!("Reasonix can't store a null '{key}'; {UNTOUCHED}."))?;
        table.insert(key, Item::Value(item));
    }
    Ok(())
}

/// Make the file's plugins exactly `desired` (name -> fields). Tables already
/// matching stay byte for byte; a changed one is edited in place.
pub(super) fn sync(
    text: Option<&str>,
    desired: &serde_json::Map<String, Json>,
    raw: &std::collections::BTreeMap<String, String>,
) -> Result<String, String> {
    let text = text.unwrap_or("");
    let mut doc = document(text)?;
    let before_rest = rest(text)?;
    plugins(&doc)?;
    let current = servers_map(&doc)?;
    if current == *desired {
        return Ok(text.to_string());
    }
    let mut names = std::collections::BTreeSet::new();
    let mut duplicates = std::collections::BTreeSet::new();
    for table in plugins(&doc)?.into_iter().flat_map(ArrayOfTables::iter) {
        if let Some(name) = name_of(table) {
            if !names.insert(name.to_string()) {
                duplicates.insert(name.to_string());
            }
        }
    }
    // Preserve every duplicate table for unrelated edits, including Connect.
    // Moving or replacing that name needs its merged policy and raw layout.
    for name in &duplicates {
        if desired.get(name) != current.get(name) {
            return Err(format!(
                "Reasonix has duplicate plugins named '{name}'; resolve that name before moving or replacing it, {UNTOUCHED}."
            ));
        }
    }
    if !matches!(doc.get("plugins"), Some(Item::ArrayOfTables(_))) {
        doc.remove("plugins");
        doc.insert("plugins", Item::ArrayOfTables(ArrayOfTables::new()));
    }
    let list = doc["plugins"]
        .as_array_of_tables_mut()
        .expect("set to an array of tables above");
    let mut seen = std::collections::BTreeSet::new();
    let mut drop = Vec::new();
    for (i, table) in list.iter_mut().enumerate() {
        let Some(name) = name_of(table).map(str::to_string) else {
            continue;
        };
        match desired.get(&name) {
            Some(want) => {
                seen.insert(name.clone());
                if !duplicates.contains(&name) && config_json(table) != *want {
                    fill(table, want)?;
                }
            }
            None => drop.push(i),
        }
    }
    let mut notes = Vec::new();
    for i in drop.into_iter().rev() {
        let table = list.get(i).expect("index from this list");
        let note = split_note(table).0;
        if let (Some(note), Some(at)) = (note, table.position()) {
            notes.push((at, note));
        }
        list.remove(i);
    }
    let mut put_back = Vec::new();
    for (name, config) in desired {
        if seen.contains(name) {
            continue;
        }
        let recorded = raw
            .get(name)
            .filter(|text| definition(text).ok().as_ref() == Some(config))
            .and_then(|text| recorded_tables(text).ok());
        match recorded {
            Some(tables) => {
                let anchor = raw.get(name).and_then(|text| recorded_anchor(text));
                put_back.extend(tables.into_iter().map(|table| (table, anchor.clone())));
            }
            None => list.push(new_table(name, config)?),
        }
    }
    // Latest first, so notes from adjacent removed tables keep their order.
    notes.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
    for (at, note) in notes {
        let mut next = None;
        next_position(doc.as_table(), at, &mut next);
        match next.and_then(|position| table_at(doc.as_table_mut(), position)) {
            Some(table) => {
                let own = table
                    .decor()
                    .prefix()
                    .and_then(|prefix| prefix.as_str())
                    .unwrap_or("")
                    .to_string();
                table.decor_mut().set_prefix(note + &own);
            }
            None => {
                let trailing = doc.trailing().as_str().unwrap_or("").to_string();
                doc.set_trailing(note + &trailing);
            }
        }
    }
    put_back
        .sort_by_key(|(_, anchor)| std::cmp::Reverse(anchor.as_ref().map_or(0, |a| a.position)));
    let note_headers: Vec<_> = headers(&doc).into_iter().map(|(_, key)| key).collect();
    for (table, anchor) in put_back {
        push_recorded(&mut doc, table, anchor.as_ref(), &note_headers);
    }
    if doc
        .get("plugins")
        .and_then(Item::as_array_of_tables)
        .is_some_and(ArrayOfTables::is_empty)
    {
        doc.remove("plugins");
    }
    let records: Vec<_> = raw.values().map(|text| recorded_body(text)).collect();
    let out = preserve_newlines(text, &doc.to_string(), &records);
    verify(&out, &before_rest, desired)?;
    Ok(out)
}

/// The comment lines above a table's header, if any, and the spacing after them.
fn split_note(table: &Table) -> (Option<String>, String) {
    let prefix = table
        .decor()
        .prefix()
        .and_then(|prefix| prefix.as_str())
        .unwrap_or("");
    if !prefix.contains('#') {
        return (None, prefix.to_string());
    }
    let note = prefix.trim_end_matches(['\n', '\r', ' ', '\t']);
    let after = &prefix[note.len()..];
    let after = after
        .strip_prefix("\r\n")
        .or_else(|| after.strip_prefix('\n'))
        .unwrap_or(after);
    (Some(note.to_string() + "\n"), after.to_string())
}

/// The smallest table header position in the document after `after`.
fn next_position(table: &Table, after: usize, best: &mut Option<usize>) {
    let consider = |t: &Table, best: &mut Option<usize>| {
        if let Some(at) = t.position().filter(|&at| at > after) {
            if best.is_none_or(|b| at < b) {
                *best = Some(at);
            }
        }
        next_position(t, after, best);
    };
    for (_, item) in table.iter() {
        match item {
            Item::Table(t) => consider(t, best),
            Item::ArrayOfTables(list) => list.iter().for_each(|t| consider(t, best)),
            _ => {}
        }
    }
}

fn table_at(table: &mut Table, position: usize) -> Option<&mut Table> {
    for (_, item) in table.iter_mut() {
        let tables: Vec<&mut Table> = match item {
            Item::Table(t) => vec![t],
            Item::ArrayOfTables(list) => list.iter_mut().collect(),
            _ => continue,
        };
        for t in tables {
            if t.position() == Some(position) {
                return Some(t);
            }
            if let Some(found) = table_at(t, position) {
                return Some(found);
            }
        }
    }
    None
}

/// Each plugin name's tables as a small document, for the move record: the
/// table and its leading comments, with stable neighboring section anchors.
/// Older records without anchors still append safely.
pub(super) fn raw_entries(text: &str) -> Result<Vec<(String, String, Json)>, String> {
    let doc = document(text)?;
    let effective = servers_map(&doc)?;
    let mut grouped: Vec<(String, Vec<Table>, Json)> = Vec::new();
    for table in plugins(&doc)?.into_iter().flat_map(ArrayOfTables::iter) {
        let Some(name) = name_of(table) else {
            continue;
        };
        let table = table.clone();
        match grouped.iter_mut().find(|(seen, _, _)| seen == name) {
            Some((_, tables, _)) => tables.push(table),
            None => {
                let config = effective[name].clone();
                grouped.push((name.to_string(), vec![table], config));
            }
        }
    }
    Ok(grouped
        .into_iter()
        .map(|(name, tables, config)| {
            let mut one = DocumentMut::new();
            let mut list = ArrayOfTables::new();
            let anchor = anchor_for(&doc, &tables[0]);
            tables.into_iter().for_each(|table| list.push(table));
            one.insert("plugins", Item::ArrayOfTables(list));
            let body = preserve_newlines(text, &one.to_string(), &[]);
            let raw = format!(
                "{ANCHOR_PREFIX}{}\n{body}",
                serde_json::to_string(&anchor).expect("serializable anchors")
            );
            (name, raw, config)
        })
        .collect())
}

/// The tables of an unambiguous recorded document.
fn recorded_tables(raw: &str) -> Result<Vec<Table>, String> {
    let doc = document(recorded_body(raw))?;
    servers_map(&doc)?;
    let tables: Vec<Table> = plugins(&doc)?
        .into_iter()
        .flat_map(ArrayOfTables::iter)
        .cloned()
        .collect();
    if tables.is_empty()
        || name_of(&tables[0]).is_none()
        || tables
            .iter()
            .any(|table| name_of(table) != name_of(&tables[0]))
    {
        return Err("Could not read imported settings".into());
    }
    Ok(tables)
}

/// The effective fields of a recorded name, including merged disabled tools.
pub(super) fn definition(raw: &str) -> Result<Json, String> {
    let tables = recorded_tables(raw)?;
    let doc = document(recorded_body(raw))?;
    Ok(servers_map(&doc)?[name_of(&tables[0]).expect("validated name")].clone())
}

// Metadata stays inside the native raw record, never in the client's config.
const ANCHOR_PREFIX: &str = "# toolport-restore-anchor: ";

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
struct Anchor {
    #[serde(default)]
    position: usize,
    previous: Option<String>,
    next: Option<String>,
}

fn recorded_body(raw: &str) -> &str {
    if recorded_anchor(raw).is_some() {
        raw.split_once('\n').map_or(raw, |(_, body)| body)
    } else {
        raw
    }
}

fn recorded_anchor(raw: &str) -> Option<Anchor> {
    serde_json::from_str(raw.strip_prefix(ANCHOR_PREFIX)?.split_once('\n')?.0).ok()
}

/// Stable section paths and named array entries, including nested settings.
fn headers(doc: &DocumentMut) -> Vec<(usize, String)> {
    fn collect(table: &Table, path: &[String], out: &mut Vec<(usize, String)>) {
        for (key, item) in table.iter() {
            let mut path = path.to_vec();
            path.push(key.to_string());
            let mut add = |t: &Table, path: Vec<String>| {
                if let Some(at) = t.position() {
                    out.push((at, serde_json::to_string(&path).expect("section path")));
                }
                collect(t, &path, out);
            };
            match item {
                Item::Table(t) => add(t, path),
                Item::ArrayOfTables(list) => {
                    for (i, t) in list.iter().enumerate() {
                        let mut path = path.clone();
                        path.push(
                            name_of(t).map_or_else(
                                || format!("index:{i}"),
                                |name| format!("name:{name}"),
                            ),
                        );
                        add(t, path);
                    }
                }
                _ => {}
            }
        }
    }
    let mut out = Vec::new();
    collect(doc.as_table(), &[], &mut out);
    out.sort();
    out
}

fn anchor_for(doc: &DocumentMut, table: &Table) -> Anchor {
    let at = table.position().unwrap_or(0);
    let mut own = Vec::new();
    positions(table, &mut own);
    let headers: Vec<_> = headers(doc)
        .into_iter()
        .filter(|(at, _)| !own.contains(&Some(*at)))
        .collect();
    Anchor {
        position: at,
        previous: headers
            .iter()
            .rev()
            .find(|(p, _)| *p < at)
            .map(|(_, id)| id.clone()),
        next: headers
            .iter()
            .find(|(p, _)| *p > at)
            .map(|(_, id)| id.clone()),
    }
}

/// Line endings are scoped to their owning TOML section, so removing one
/// plugin cannot consume an identical line's ending from an untouched plugin.
fn scoped_lines(text: &str) -> Vec<(String, &str, &str)> {
    fn collect(table: &Table, path: &str, out: &mut Vec<(usize, String)>) {
        for (key, item) in table.iter() {
            let path = format!("{path}/{key}");
            match item {
                Item::Table(t) => {
                    if let Some(span) = t.span() {
                        out.push((span.start, path.clone()));
                    }
                    collect(t, &path, out);
                }
                Item::ArrayOfTables(list) => {
                    for (i, t) in list.iter().enumerate() {
                        let id = format!(
                            "{path}/{}",
                            name_of(t).map_or_else(|| i.to_string(), str::to_string)
                        );
                        if let Some(span) = t.span() {
                            out.push((span.start, id.clone()));
                        }
                        collect(t, &id, out);
                    }
                }
                _ => {}
            }
        }
    }
    let mut sections = Vec::new();
    if let Ok(doc) = toml_edit::ImDocument::parse(text) {
        collect(doc.as_table(), "", &mut sections);
    }
    sections.sort();
    let mut sections = sections.into_iter().peekable();
    let mut section = String::new();
    let mut offset = 0;
    text.split_inclusive('\n')
        .map(|line| {
            while sections.peek().is_some_and(|(at, _)| *at <= offset) {
                section = sections.next().expect("peeked section").1;
            }
            offset += line.len();
            let content = line
                .strip_suffix('\n')
                .unwrap_or(line)
                .strip_suffix('\r')
                .unwrap_or(line.strip_suffix('\n').unwrap_or(line));
            (section.clone(), content, &line[content.len()..])
        })
        .collect()
}

fn preserve_newlines(source: &str, rendered: &str, records: &[&str]) -> String {
    use std::collections::{HashMap, VecDeque};
    let mut endings: HashMap<(String, &str), VecDeque<&str>> = HashMap::new();
    let default = source.find('\n').map_or("\n", |at| {
        if at > 0 && source.as_bytes()[at - 1] == b'\r' {
            "\r\n"
        } else {
            "\n"
        }
    });
    for text in std::iter::once(source).chain(records.iter().copied()) {
        for (section, content, ending) in scoped_lines(text) {
            endings
                .entry((section, content))
                .or_default()
                .push_back(ending);
        }
    }
    let mut out = String::new();
    for (section, content, ending) in scoped_lines(rendered) {
        out.push_str(content);
        out.push_str(
            endings
                .get_mut(&(section, content))
                .and_then(VecDeque::pop_front)
                .unwrap_or(if ending.is_empty() { "" } else { default }),
        );
    }
    out
}

/// Header positions of `table` and the tables inside it, in visit order.
fn positions(table: &Table, out: &mut Vec<Option<usize>>) {
    out.push(table.position());
    for (_, item) in table.iter() {
        match item {
            Item::Table(t) => positions(t, out),
            Item::ArrayOfTables(list) => list.iter().for_each(|t| positions(t, out)),
            _ => {}
        }
    }
}

/// Visit `table` and the tables inside it in the order of [`positions`].
fn each_table(table: &mut Table, f: &mut impl FnMut(&mut Table)) {
    f(table);
    for (_, item) in table.iter_mut() {
        match item {
            Item::Table(t) => each_table(t, f),
            Item::ArrayOfTables(list) => list.iter_mut().for_each(|t| each_table(t, f)),
            _ => {}
        }
    }
}

fn strip_note<'a>(mut prefix: &'a str, note: &str) -> Option<&'a str> {
    let note = note.replace("\r\n", "\n");
    for line in note.split_inclusive('\n') {
        let (head, rest) = prefix.split_once('\n')?;
        if head.trim_end_matches('\r') != line.strip_suffix('\n')? {
            return None;
        }
        prefix = rest;
    }
    Some(prefix)
}

/// Restore beside a surviving neighbor, falling back to after the last plugin
/// or the end of the file. Renumber subtable headers together to retain ownership.
fn push_recorded(
    doc: &mut DocumentMut,
    mut table: Table,
    anchor: Option<&Anchor>,
    note_headers: &[String],
) {
    let note = split_note(&table).0;
    let mut own = Vec::new();
    positions(&table, &mut own);
    // A header without a position sorts with the one before it, as on display.
    let mut last = 0;
    let mut order: Vec<(usize, usize)> = own
        .iter()
        .enumerate()
        .map(|(i, at)| {
            last = at.unwrap_or(last);
            (last, i)
        })
        .collect();
    order.sort();
    let mut rank = vec![0; own.len()];
    for (r, (_, i)) in order.into_iter().enumerate() {
        rank[i] = r + 1;
    }
    let highest = |table: &Table| {
        let mut all = Vec::new();
        positions(table, &mut all);
        all.into_iter().flatten().max()
    };
    let list = doc
        .get("plugins")
        .and_then(Item::as_array_of_tables)
        .filter(|list| !list.is_empty());
    // New tables have no position yet, so a list of only those counts as empty.
    let fallback = match list.and_then(|list| list.iter().filter_map(highest).max()) {
        Some(at) => at,
        None => {
            let mut all = Vec::new();
            for (_, item) in doc.as_table().iter() {
                match item {
                    Item::Table(t) => positions(t, &mut all),
                    Item::ArrayOfTables(l) => l.iter().for_each(|t| positions(t, &mut all)),
                    _ => {}
                }
            }
            all.into_iter().flatten().max().unwrap_or(0)
        }
    };
    // Legacy records stripped leading notes and have no anchors. Keep their
    // original trailing-comment restoration behavior.
    if anchor.is_none() && list.is_none() {
        let trailing = doc.trailing().as_str().unwrap_or("").to_string();
        if trailing.contains('#') {
            let prefix = table
                .decor()
                .prefix()
                .and_then(|p| p.as_str())
                .unwrap_or("");
            let prefix = trailing + prefix;
            table.decor_mut().set_prefix(prefix);
            doc.set_trailing("");
        }
    }
    let anchors = headers(doc);
    let after = anchor
        .and_then(|anchor| {
            anchor
                .next
                .as_ref()
                .and_then(|id| {
                    anchors
                        .iter()
                        .find(|(_, key)| key == id)
                        .map(|(at, _)| at.saturating_sub(1))
                })
                .or_else(|| {
                    anchor
                        .previous
                        .as_ref()
                        .and_then(|id| anchors.iter().find(|(_, key)| key == id).map(|(at, _)| *at))
                })
        })
        .unwrap_or(fallback);
    // sync moved this note to the next header, or to the document trailing text.
    // An identical prefix on an earlier table belongs to that table.
    if let Some(note) = note {
        // A neighbor restored earlier in this batch did not receive the note.
        // Follow the next header that existed before this restore batch instead.
        let next = anchors
            .iter()
            .find(|(at, key)| *at > after && note_headers.contains(key))
            .map(|(at, _)| *at);
        match next.and_then(|at| table_at(doc.as_table_mut(), at)) {
            Some(next) => {
                let prefix = next.decor().prefix().and_then(|p| p.as_str()).unwrap_or("");
                if let Some(rest) = strip_note(prefix, &note).map(str::to_string) {
                    next.decor_mut().set_prefix(rest);
                }
            }
            None => {
                let trailing = doc.trailing().as_str().unwrap_or("");
                if let Some(rest) = strip_note(trailing, &note).map(str::to_string) {
                    doc.set_trailing(rest);
                }
            }
        }
    }
    let room = own.len();
    for (_, item) in doc.as_table_mut().iter_mut() {
        let mut shift = |t: &mut Table| {
            if let Some(at) = t.position().filter(|&at| at > after) {
                t.set_position(at + room);
            }
        };
        match item {
            Item::Table(t) => each_table(t, &mut shift),
            Item::ArrayOfTables(l) => l.iter_mut().for_each(|t| each_table(t, &mut shift)),
            _ => {}
        }
    }
    let mut next = rank.into_iter();
    each_table(&mut table, &mut |t| {
        t.set_position(after + next.next().expect("one rank per table"));
    });
    if !matches!(doc.get("plugins"), Some(Item::ArrayOfTables(_))) {
        doc.remove("plugins");
        doc.insert("plugins", Item::ArrayOfTables(ArrayOfTables::new()));
    }
    doc["plugins"]
        .as_array_of_tables_mut()
        .expect("set to an array of tables above")
        .push(table);
}

/// Put recorded tables back into the file. Names already there stay as they are.
pub(super) fn append_raw(text: Option<&str>, entries: &[String]) -> Result<String, String> {
    let text = text.unwrap_or("");
    let mut doc = document(text)?;
    let before_rest = rest(text)?;
    plugins(&doc)?;
    let mut desired = servers_map(&doc)?;
    let records: Vec<_> = entries.iter().map(|text| recorded_body(text)).collect();
    let note_headers: Vec<_> = headers(&doc).into_iter().map(|(_, key)| key).collect();
    let mut entries: Vec<_> = entries.iter().collect();
    entries.sort_by_key(|raw| std::cmp::Reverse(recorded_anchor(raw).map_or(0, |a| a.position)));
    for raw in entries {
        let tables = recorded_tables(raw)?;
        let name = name_of(&tables[0])
            .expect("checked by recorded_tables")
            .to_string();
        if desired.contains_key(&name) {
            continue;
        }
        desired.insert(name, definition(raw)?);
        let anchor = recorded_anchor(raw);
        for table in tables {
            push_recorded(&mut doc, table, anchor.as_ref(), &note_headers);
        }
    }
    let out = preserve_newlines(text, &doc.to_string(), &records);
    verify(&out, &before_rest, &desired)?;
    Ok(out)
}

fn verify(
    out: &str,
    before_rest: &Json,
    desired: &serde_json::Map<String, Json>,
) -> Result<(), String> {
    let doc = document(out).map_err(|e| {
        format!("Toolport produced an unreadable Reasonix config ({e}); {UNTOUCHED}.")
    })?;
    if servers_map(&doc)? != *desired || rest(out)? != *before_rest {
        return Err(format!(
            "Toolport could not apply this change exactly; {UNTOUCHED}."
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "reasonix_tests.rs"]
mod tests;
