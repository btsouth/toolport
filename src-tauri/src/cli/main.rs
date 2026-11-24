//! `toolport`: the standalone command line. One setup for every coding agent.
//!
//! Built without the desktop shells or the search model:
//! `cargo build --no-default-features --features cli --bin toolport`.

/// Identifies this binary. The npm launcher only runs a `toolport` found on PATH when it
/// contains this, so it never starts the Linux desktop app's `toolport`, which opens its
/// window when run without arguments. Defined here, not in the library, so the desktop
/// shell that links the CLI never carries it.
const BINARY_MARKER: &str = "toolport-cli-binary-v1";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let host = conduit_lib::cli::Host {
        binary: BINARY_MARKER,
        version_detail: None,
        help_footer: None,
    };
    std::process::exit(conduit_lib::cli::run(&host, args));
}
