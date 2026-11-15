//! Replace only parsed scalar/comment spans. Other bytes, keys and user edits stay put.
use super::*;
use std::ops::Range;

// A marker ends after its entire decimal ID. Matching `_1` inside `_10`
// would overwrite a different obligation, including one still unavailable.
pub(super) fn marker_ranges(text: &str, marker: &str) -> Vec<Range<usize>> {
    let prefix = "TOOLPORT_MISSING_CREDENTIAL_";
    text.match_indices(prefix)
        .filter_map(|(start, _)| {
            let digits = text[start + prefix.len()..]
                .bytes()
                .take_while(u8::is_ascii_digit)
                .count();
            let end = start + prefix.len() + digits;
            (digits > 0 && &text[start..end] == marker).then_some(start..end)
        })
        .collect()
}
pub(super) fn contains_marker(text: &str, marker: &str) -> bool {
    !marker_ranges(text, marker).is_empty()
}
pub(super) fn replace_markers(text: &str, values: &[(String, String)]) -> String {
    let mut changes = values
        .iter()
        .flat_map(|(marker, value)| {
            marker_ranges(text, marker)
                .into_iter()
                .map(move |range| (range, value))
        })
        .collect::<Vec<_>>();
    changes.sort_by_key(|(range, _)| range.start);
    changes.dedup_by_key(|(range, _)| range.start);
    let mut result = text.to_string();
    for (range, value) in changes.into_iter().rev() {
        result.replace_range(range, value);
    }
    result
}

struct Span {
    range: Range<usize>,
    value: String,
    comment: bool,
}
fn json_spans(text: &str) -> Result<Vec<Span>, String> {
    use jsonc_parser::{ast::Value, common::Ranged};
    fn walk(value: &Value<'_>, out: &mut Vec<Span>) {
        match value {
            Value::StringLit(s) => {
                let r = s.range();
                out.push(Span {
                    range: r.start..r.end,
                    value: s.value.to_string(),
                    comment: false,
                });
            }
            Value::Object(o) => {
                for p in &o.properties {
                    walk(&p.value, out);
                }
            }
            Value::Array(a) => {
                for v in &a.elements {
                    walk(v, out);
                }
            }
            _ => {}
        }
    }
    let parsed = jsonc_parser::parse_to_ast(
        text,
        &jsonc_parser::CollectOptions {
            comments: jsonc_parser::CommentCollectionStrategy::Separate,
            tokens: false,
        },
        &Default::default(),
    )
    .map_err(|e| diagnostics::jsonc(&e))?;
    let mut out = Vec::new();
    if let Some(v) = parsed.value {
        walk(&v, &mut out);
    }
    for comments in parsed.comments.into_iter().flat_map(|m| m.into_values()) {
        for comment in comments.iter() {
            let r = comment.range();
            out.push(Span {
                range: r.start..r.end,
                value: text[r.start..r.end].into(),
                comment: true,
            });
        }
    }
    out.sort_by_key(|s| (s.range.start, s.range.end));
    out.dedup_by_key(|s| (s.range.start, s.range.end));
    Ok(out)
}
fn toml_spans(text: &str) -> Result<Vec<Span>, String> {
    fn val(v: &toml_edit::Value, out: &mut Vec<Span>) {
        match v {
            toml_edit::Value::String(s) => {
                if let Some(range) = v.span() {
                    out.push(Span {
                        range,
                        value: s.value().clone(),
                        comment: false,
                    });
                }
            }
            toml_edit::Value::Array(a) => {
                for v in a.iter() {
                    val(v, out);
                }
            }
            toml_edit::Value::InlineTable(t) => {
                for (_, v) in t.iter() {
                    val(v, out);
                }
            }
            _ => {}
        }
    }
    fn item(i: &toml_edit::Item, out: &mut Vec<Span>) {
        match i {
            toml_edit::Item::Value(v) => val(v, out),
            toml_edit::Item::Table(t) => {
                for (_, v) in t.iter() {
                    item(v, out);
                }
            }
            toml_edit::Item::ArrayOfTables(a) => {
                for t in a.iter() {
                    for (_, v) in t.iter() {
                        item(v, out);
                    }
                }
            }
            _ => {}
        }
    }
    let doc = toml_edit::ImDocument::parse(text)
        .map_err(|e| diagnostics::offset("TOML", text, e.span()))?;
    let mut out = Vec::new();
    item(doc.as_item(), &mut out);
    comments(text, &mut out);
    Ok(out)
}
fn comments(text: &str, out: &mut Vec<Span>) {
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\'' || b[i] == b'"' {
            let q = b[i];
            let triple = b.get(i..i + 3).is_some_and(|s| s == [q, q, q]);
            let n = if triple { 3 } else { 1 };
            i += n;
            while i < b.len() {
                if q == b'"' && b[i] == b'\\' {
                    i = (i + 2).min(b.len());
                    continue;
                }
                if b.get(i..i + n).is_some_and(|s| s.iter().all(|&v| v == q)) {
                    i += n;
                    break;
                }
                i += 1;
            }
        } else if b[i] == b'#' {
            let start = i;
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            if !out
                .iter()
                .any(|s| s.range.start <= start && start < s.range.end)
            {
                out.push(Span {
                    range: start..i,
                    value: text[start..i].into(),
                    comment: true,
                });
            }
        } else {
            i += 1;
        }
    }
}
fn yaml_spans(text: &str) -> Result<Vec<Span>, String> {
    use std::mem::MaybeUninit;
    use unsafe_libyaml as y;
    struct Parser(Box<MaybeUninit<y::yaml_parser_t>>);
    impl Drop for Parser {
        fn drop(&mut self) {
            unsafe {
                y::yaml_parser_delete(self.0.as_mut_ptr());
            }
        }
    }
    let mut raw = Box::<y::yaml_parser_t>::new_uninit();
    // SAFETY: pinned owned parser and borrowed UTF-8 input outlive every event.
    // Read matching union members only, copy all event data before deletion.
    unsafe {
        if y::yaml_parser_initialize(raw.as_mut_ptr()).fail {
            return Err("Could not inspect YAML recovery values".into());
        }
        let mut parser = Parser(raw);
        y::yaml_parser_set_input_string(parser.0.as_mut_ptr(), text.as_ptr(), text.len() as u64);
        // libyaml marks are UTF-8 byte offsets (including multibyte scalars).
        let mut out = Vec::new();
        let mut stack: Vec<Option<bool>> = Vec::new();
        loop {
            let mut e = MaybeUninit::<y::yaml_event_t>::uninit();
            if y::yaml_parser_parse(parser.0.as_mut_ptr(), e.as_mut_ptr()).fail {
                let mark = (&*parser.0.as_ptr()).problem_mark;
                return Err(diagnostics::syntax(
                    "YAML",
                    mark.line as usize + 1,
                    mark.column as usize + 1,
                ));
            }
            let event = e.assume_init_mut();
            let kind = event.type_;
            let node = matches!(
                kind,
                y::YAML_SCALAR_EVENT
                    | y::YAML_ALIAS_EVENT
                    | y::YAML_MAPPING_START_EVENT
                    | y::YAML_SEQUENCE_START_EVENT
            );
            let is_key = if node {
                if let Some(Some(key)) = stack.last_mut() {
                    let was = *key;
                    *key = !*key;
                    was
                } else {
                    false
                }
            } else {
                false
            };
            if kind == y::YAML_SCALAR_EVENT && !is_key {
                let s = event.data.scalar;
                let value =
                    String::from_utf8_lossy(std::slice::from_raw_parts(s.value, s.length as usize))
                        .into_owned();
                let mut start = (event.start_mark.index as usize).min(text.len());
                let end = (event.end_mark.index as usize).min(text.len());
                // Anchors and explicit tags belong to the node, not its value.
                // Keep their exact spelling while replacing only the scalar.
                while start < end && matches!(text.as_bytes()[start], b'&' | b'!') {
                    let fragment = &text[start..end];
                    let Some(stop) = fragment.find(char::is_whitespace) else {
                        break;
                    };
                    start += stop;
                    while start < end && text.as_bytes()[start].is_ascii_whitespace() {
                        start += 1;
                    }
                }
                out.push(Span {
                    range: start..end,
                    value,
                    comment: false,
                });
            }
            if kind == y::YAML_MAPPING_START_EVENT {
                stack.push(Some(true));
            }
            if kind == y::YAML_SEQUENCE_START_EVENT {
                stack.push(None);
            }
            if matches!(kind, y::YAML_MAPPING_END_EVENT | y::YAML_SEQUENCE_END_EVENT) {
                stack.pop();
            }
            y::yaml_event_delete(event);
            if kind == y::YAML_STREAM_END_EVENT {
                break;
            }
        }
        comments(text, &mut out);
        Ok(out)
    }
}
fn spans(format: Format, text: &str) -> Result<Vec<Span>, String> {
    match format {
        Format::TomlMcpServers | Format::TomlReasonixPlugins => toml_spans(text),
        Format::YamlExtensions
        | Format::YamlMcpServers
        | Format::YamlMcpServersList
        | Format::YamlDshPatch => yaml_spans(text),
        _ => json_spans(text),
    }
}
pub(super) fn present(format: Format, text: &str, marker: &str) -> Result<bool, String> {
    Ok(spans(format, text)?
        .iter()
        .any(|s| contains_marker(&s.value, marker)))
}
pub(super) fn patch(
    format: Format,
    text: &str,
    values: &[(String, String)],
) -> Result<String, String> {
    let mut changes = Vec::new();
    for span in spans(format, text)? {
        let value = replace_markers(&span.value, values);
        if value == span.value {
            continue;
        }
        let mut replacement = if span.comment {
            value
        } else if matches!(format, Format::TomlMcpServers | Format::TomlReasonixPlugins) {
            toml_edit::Value::from(value).to_string()
        } else {
            serde_json::to_string(&value).map_err(|e| diagnostics::json(&e))?
        };
        if !span.comment
            && matches!(
                format,
                Format::YamlExtensions
                    | Format::YamlMcpServers
                    | Format::YamlMcpServersList
                    | Format::YamlDshPatch
            )
        {
            let raw = &text[span.range.clone()];
            if raw.ends_with("\r\n") {
                replacement.push_str("\r\n");
            } else if raw.ends_with('\n') {
                replacement.push('\n');
            }
        }
        changes.push((span.range, replacement));
    }
    changes.sort_by_key(|(r, _)| r.start);
    let mut output = text.to_string();
    for (range, value) in changes.into_iter().rev() {
        output.replace_range(range, &value);
    }
    // Validate independently before staging; no parser excerpt can escape.
    let mut expected = mutation::value(format, Some(text))?;
    fn replace(value: &mut serde_json::Value, values: &[(String, String)]) {
        match value {
            serde_json::Value::String(text) => {
                *text = replace_markers(text, values);
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    replace(item, values);
                }
            }
            serde_json::Value::Object(map) => {
                for item in map.values_mut() {
                    replace(item, values);
                }
            }
            _ => {}
        }
    }
    replace(&mut expected, values);
    if mutation::value(format, Some(&output))? != expected {
        return Err(
            "Recovery values could not be verified; config unchanged and recovery retained".into(),
        );
    }
    Ok(output)
}

thread_local! { static PROGRESS: std::cell::Cell<(usize,usize)>=const{std::cell::Cell::new((0,0))}; }
pub(super) fn reset_progress() {
    PROGRESS.with(|p| p.set((0, 0)));
}
pub(super) fn progress() -> (usize, usize) {
    PROGRESS.with(|p| p.get())
}
pub(super) fn remember_progress(
    format: Format,
    before: Option<&str>,
    after: Option<&str>,
    values: &[backup_storage::RecoveryValue],
) -> Result<(), String> {
    let mut refs = std::collections::BTreeMap::<(String, String), (bool, bool)>::new();
    for v in values {
        let b = before
            .map(|t| present(format, t, &v.marker))
            .transpose()?
            .unwrap_or(false);
        let a = after
            .map(|t| present(format, t, &v.marker))
            .transpose()?
            .unwrap_or(false);
        let pair = refs
            .entry((v.reference.server.clone(), v.reference.key.clone()))
            .or_default();
        pair.0 |= b;
        pair.1 |= a;
    }
    let restored = refs.values().filter(|(b, a)| *b && !*a).count();
    let accounted = refs.values().filter(|(b, a)| !*b && !*a).count();
    PROGRESS.with(|p| p.set((restored, accounted)));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn complete_marker_ids_do_not_match_prefixes_or_cascade_into_saved_values() {
        let m = |n| format!("TOOLPORT_MISSING_CREDENTIAL_{n}");
        let text = serde_json::json!({"one":m(1),"ten":m(10)}).to_string();
        let partial = patch(
            Format::JsonMcpServers,
            &text,
            &[(m(1), "sk-original-one".into())],
        )
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&partial).unwrap();
        assert_eq!(
            parsed["ten"],
            m(10),
            "unavailable value ten must remain pending"
        );
        assert!(!present(Format::JsonMcpServers, &partial, &m(1)).unwrap());
        assert!(present(Format::JsonMcpServers, &partial, &m(10)).unwrap());
        let output = patch(
            Format::JsonMcpServers,
            &text,
            &[(m(1), m(10)), (m(10), "sk-original-ten".into())],
        )
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(
            parsed["one"],
            m(10),
            "never rewrite bytes introduced by a replacement"
        );
        assert_eq!(parsed["ten"], "sk-original-ten");
    }
    #[test]
    fn yaml_recovery_keeps_anchors_tags_and_block_scalar_boundaries() {
        let text="# before\nauth: &token !!str Bearer TOOLPORT_MISSING_CREDENTIAL_1\nalias: *token\nblock: |\n  Bearer TOOLPORT_MISSING_CREDENTIAL_1\nnext: keep\n";
        let out = patch(
            Format::YamlMcpServers,
            text,
            &[(
                "TOOLPORT_MISSING_CREDENTIAL_1".into(),
                "sk-yaml-original".into(),
            )],
        )
        .unwrap();
        assert!(out.contains("&token !!str"));
        assert!(out.contains("alias: *token"));
        assert!(out.contains("# before"));
        assert!(out.contains("next: keep"));
        let value = mutation::value(Format::YamlMcpServers, Some(&out)).unwrap();
        assert_eq!(value["auth"], "Bearer sk-yaml-original");
        assert_eq!(value["alias"], "Bearer sk-yaml-original");
        assert_eq!(value["block"], "Bearer sk-yaml-original\n");
    }
    #[test]
    fn scalar_recovery_preserves_comments_order_escaping_and_embedded_markers() {
        let marker = "TOOLPORT_MISSING_CREDENTIAL_1";
        let secret = "sk-test-quote'\"slash\\newline\nend";
        let cases=[(Format::JsonMcpServers,format!("// {marker}\n{{\"literal\":\"TOOLPORT_MISSING_CREDENTIAL_0\",\"é\":\"keep\",\"auth\":\"Bearer {marker}\"}}\n")),
            (Format::TomlMcpServers,format!("# {marker}\nliteral='TOOLPORT_MISSING_CREDENTIAL_0'\nauth='Bearer {marker}' # keep\n")),
            (Format::YamlMcpServers,format!("# {marker}\nliteral: TOOLPORT_MISSING_CREDENTIAL_0\né: keep\nauth: 'Bearer {marker}' # keep\n"))];
        for (format, text) in cases {
            // A comment cannot safely contain a multiline value. Use its escaped
            // spelling, keeping the comment one line and native scalar exact.
            let value = "sk-test-quote'\"slash\\end";
            let output = patch(format, &text, &[(marker.into(), value.into())]).unwrap();
            assert!(output.contains("TOOLPORT_MISSING_CREDENTIAL_0"));
            assert!(!present(format, &output, marker).unwrap());
            assert!(output.contains("keep"));
            assert_eq!(
                mutation::value(format, Some(&output)).unwrap()["auth"],
                format!("Bearer {value}")
            );
            let clean = text
                .lines()
                .filter(|line| !line.starts_with('#') && !line.starts_with("//"))
                .collect::<Vec<_>>()
                .join("\n");
            let output = patch(format, &clean, &[(marker.into(), secret.into())]).unwrap();
            assert_eq!(
                mutation::value(format, Some(&output)).unwrap()["auth"],
                format!("Bearer {secret}")
            );
        }
    }
}
