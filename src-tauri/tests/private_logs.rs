//! Regression guard for append-mode log permissions (SBS-868).
//!
//! Ten log writers opened their file with a bare
//! `OpenOptions::new().create(true).append(true)`, which takes the process umask
//! and lands 0644 under the usual 022. They only became owner-only on their
//! FIRST size-triggered rotation, because rotation goes through
//! `registry::atomic_write`, which sets the mode before writing. So each one was
//! readable by a second OS user from creation until its first rotation.
//!
//! That mattered most for `gateway.log`: it carries the line
//! `[broker] bound 127.0.0.1:<port>; endpoint published at <path>`, which
//! publishes the HITL broker's port. The 0600 on `approval-endpoint.json` was
//! the control meant to keep another OS user out, and this log routed around it.
//!
//! Every production writer now goes through `registry::open_append_private`,
//! which applies the mode at creation AND tightens a file that already exists -
//! `mode()` is an `open(2)` create mask, so without the second half an upgrade
//! would leave every existing 0644 log exactly as it was.

use std::path::{Path, PathBuf};

/// The only files allowed to spell the raw append-open themselves, by path
/// relative to `src-tauri/`: the helper that defines the safe pattern, and a
/// test-only binary that writes a transcript into a temp dir. Matched on the
/// full relative path, not the basename, so a future `foo/registry.rs` is not
/// silently exempt too.
const EXEMPT: [&str; 2] = ["src/registry.rs", "src/bin/mock-mcp-server.rs"];

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// True when `source` opens a file in append mode anywhere.
///
/// Whitespace is stripped first, so `.append ( true )` and a call broken across
/// lines by a formatter are the same match as `.append(true)`. A literal
/// substring search would miss both, which would let the pattern back in through
/// nothing more than a reformat.
fn opens_in_append_mode(source: &str) -> bool {
    let dense: String = source.chars().filter(|c| !c.is_whitespace()).collect();
    dense.contains(".append(true)")
}

#[test]
fn no_production_log_opens_append_mode_directly() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = manifest.join("src");
    let mut files = Vec::new();
    rust_sources(&src, &mut files);
    assert!(
        !files.is_empty(),
        "no sources found under {}",
        src.display()
    );

    let mut offenders = Vec::new();
    for file in files {
        let relative = file
            .strip_prefix(&manifest)
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        if EXEMPT.contains(&relative.as_str()) {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("read {file:?}: {e}"));
        if opens_in_append_mode(&text) {
            offenders.push(relative);
        }
    }
    assert!(
        offenders.is_empty(),
        "these files open an append-mode log directly, so it is created \
         world-readable under the usual umask (SBS-868): {offenders:?}. Use \
         registry::open_append_private instead."
    );
}

/// The scan above is only worth having if it still bites when the call is
/// spelled differently, which is how a guard like this usually dies.
#[test]
fn the_append_scan_survives_reformatting() {
    assert!(opens_in_append_mode(".append(true)"));
    assert!(opens_in_append_mode(". append ( true )"));
    assert!(opens_in_append_mode(
        "std::fs::OpenOptions::new()\n    .create(true)\n    .append(true)\n"
    ));
    assert!(!opens_in_append_mode(".append(false)"));
    assert!(!opens_in_append_mode("appended(true)"));
}

/// The helper's effect on a file it CREATES.
#[cfg(unix)]
#[test]
fn open_append_private_creates_an_owner_only_file() {
    use std::io::Write;

    let dir = temp_dir("create");
    let path = dir.join("gateway.log");

    let mut file = conduit_lib::registry::open_append_private(&path).expect("create log");
    file.write_all(b"[broker] bound 127.0.0.1:54321\n")
        .expect("write");

    assert_eq!(
        mode_of(&path),
        0o600,
        "the log was created {:o}, so a second OS user can read the broker port",
        mode_of(&path)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The half a create-time mode cannot cover, and the one that decides whether
/// SBS-868 is fixed for anyone who already has these files.
///
/// `OpenOptionsExt::mode` is an `open(2)` create mask: POSIX applies it only
/// when the inode is born. Every install upgrading into this change already has
/// `gateway.log`, `inspect.jsonl`, `oauth-debug.log` and the rest sitting at
/// 0644, and `oauth-debug.log` never rotates while `inspect.jsonl` is not opened
/// at all when capture is off, so neither would ever reach `atomic_write`'s 0600
/// rewrite.
///
/// This is also the assertion that cannot pass by accident: it sets 0644
/// explicitly rather than relying on the umask, so removing the tighten fails it
/// on any host, including one hardened to umask 077.
#[cfg(unix)]
#[test]
fn open_append_private_tightens_a_file_that_is_already_world_readable() {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    let dir = temp_dir("tighten");
    let path = dir.join("gateway.log");

    // A log left behind by a build before this change.
    std::fs::write(&path, "[broker] bound 127.0.0.1:54321\n").expect("seed log");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod 0644");
    assert_eq!(mode_of(&path), 0o644, "fixture did not take");

    let mut file = conduit_lib::registry::open_append_private(&path).expect("reopen log");
    file.write_all(b"second line\n").expect("write");

    assert_eq!(
        mode_of(&path),
        0o600,
        "an existing log stayed {:o}: mode() only applies at creation, so the \
         upgrade path fixes nothing without an explicit tighten",
        mode_of(&path)
    );
    // The tighten must not cost the contents.
    let body = std::fs::read_to_string(&path).expect("read back");
    assert!(body.contains("bound 127.0.0.1:54321") && body.contains("second line"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
fn temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("toolport-sbs868-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

#[cfg(unix)]
fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).expect("stat").permissions().mode() & 0o777
}
