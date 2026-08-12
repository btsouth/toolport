//! The always-on gateway log (`gateway.log`).
//!
//! This is what `gather_diagnostics` bundles into a bug report, so it stays on
//! regardless of `TOOLPORT_DEBUG` and holds connection-lifecycle facts only:
//! starts, connect successes, connect failures, and catalogs that came back
//! incomplete.
//!
//! It lives in the library rather than the gateway binary because the code that
//! knows a catalog was truncated is [`crate::downstream`], and a warning only
//! that module can see is worthless if it cannot reach the file a user actually
//! sends us. MCP clients swallow a gateway's stderr, so `eprintln!` alone means
//! a silent truncation is indistinguishable from a healthy connect in any
//! after-the-fact diagnosis - which is exactly how a downstream server served a
//! 3-tool prefix of its 40-tool catalog for days without leaving a trace.

use std::io::Write;
use std::path::Path;

/// Keep the always-on gateway log bounded; trimmed to roughly the back half once
/// it grows past this, so a long-running client can't let it grow without limit.
pub const GATEWAY_LOG_CAP: u64 = 256 * 1024;

/// Append one line to the gateway log. Best-effort: logging must never take down
/// a connection, so every failure here is swallowed.
pub fn append(msg: &str) {
    let Some(path) = crate::registry::gateway_log_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = f.write_all(format!("{msg}\n").as_bytes());
    }
    trim_log_if_large(&path);
}

/// Trim the log to roughly its back half once it exceeds [`GATEWAY_LOG_CAP`],
/// cutting at a line boundary so the survivor never starts mid-record.
pub fn trim_log_if_large(path: &Path) {
    let over = std::fs::metadata(path)
        .map(|m| m.len() > GATEWAY_LOG_CAP)
        .unwrap_or(false);
    if !over {
        return;
    }
    let Ok(data) = std::fs::read(path) else {
        return;
    };
    let keep_from = data.len().saturating_sub((GATEWAY_LOG_CAP / 2) as usize);
    let start = data[keep_from..]
        .iter()
        .position(|&b| b == b'\n')
        .map(|i| keep_from + i + 1)
        .unwrap_or(keep_from);
    let _ = std::fs::write(path, &data[start..]);
}
