//! `toolport`: the standalone command line. One setup for every coding agent.
//!
//! Built without the desktop shells or the search model:
//! `cargo build --no-default-features --bin toolport`.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(conduit_lib::cli::run(args));
}
