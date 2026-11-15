//! Parser diagnostics may contain the entire credential-bearing source line.
//! Keep only structured coordinates; never format the parser's Display/Debug.
use super::*;

pub(super) fn syntax(kind: &str, line: usize, column: usize) -> String {
    format!("Could not parse client config. {kind} syntax error in client config at line {line} column {column}; leaving it untouched.")
}
pub(super) fn json(error: &serde_json::Error) -> String {
    syntax("JSON", error.line().max(1), error.column().max(1))
}
pub(super) fn jsonc(error: &jsonc_parser::errors::ParseError) -> String {
    syntax("JSON", error.line_display(), error.column_display())
}
pub(super) fn offset(kind: &str, text: &str, span: Option<std::ops::Range<usize>>) -> String {
    let offset = span.map_or(0, |span| span.start).min(text.len());
    let prefix = &text.as_bytes()[..offset];
    let line = prefix.iter().filter(|&&byte| byte == b'\n').count() + 1;
    let start = prefix
        .iter()
        .rposition(|&byte| byte == b'\n')
        .map_or(0, |i| i + 1);
    let column = String::from_utf8_lossy(&prefix[start..]).chars().count() + 1;
    syntax(kind, line, column)
}
pub(super) fn yaml(error: &serde_yaml::Error) -> String {
    let location = error.location();
    syntax(
        "YAML",
        location.as_ref().map_or(1, |p| p.line()),
        location.as_ref().map_or(1, |p| p.column()),
    )
}
pub(super) fn at(path: &Path, error: String) -> String {
    error.replace("in client config", &format!("in {}", path.display()))
}
