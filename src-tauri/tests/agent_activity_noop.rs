//! 2.0 removed the agent activity sensor, but kept the `--toolport-hook` subcommand
//! as a silent no-op so hook entries an earlier release installed into an AI client's
//! settings cannot error or block the client. This drives the real gateway binary to
//! prove the flag exits 0 and prints nothing.

use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn toolport_hook_flag_exits_zero_and_prints_nothing() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_toolport-gateway"))
        .args(["--toolport-hook", "tool"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn toolport-gateway");

    // A realistic payload, the shape a PostToolUse hook receives. The no-op drains it
    // so the write cannot fail or block.
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(br#"{"session_id":"s1","tool_name":"Bash","tool_input":{"command":"echo hi"}}"#)
        .expect("write hook payload");

    let out = child.wait_with_output().expect("wait for gateway");
    assert!(
        out.status.success(),
        "the hook path must exit 0: {:?}",
        out.status
    );
    assert!(
        out.stdout.is_empty(),
        "the hook path must print nothing to stdout: {:?}",
        out.stdout
    );
    assert!(
        out.stderr.is_empty(),
        "the hook path must print nothing to stderr: {:?}",
        out.stderr
    );
}
