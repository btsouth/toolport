fn main() {
    // `/usr/bin/toolport` is this binary. A command line for the CLI (`toolport scan`,
    // `toolport hook check`, `toolport --version`) runs it before anything touches GTK,
    // the display or a secret; a launch (no arguments, `--hidden`, a link, a GTK option)
    // opens the app.
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if conduit_lib::cli::is_cli_invocation(&args) {
        let args = args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        std::process::exit(conduit_lib::cli::run(
            &conduit_lib::linux_native::cli_host(),
            args,
        ));
    }
    // Before anything can resolve a secret: the shell's stderr (a terminal or
    // the journal) and its children's are redacted from here on.
    conduit_lib::daemon_log::install_stderr_redaction();
    // The relay can only remove values it knows: learn the inline ones now,
    // off the startup path. Vault values are learned when first read for use,
    // so a hidden start never raises a keyring prompt.
    let _ = std::thread::Builder::new()
        .name("toolport-secret-preload".into())
        .spawn(conduit_lib::secret_refs::preload_inline_secrets);
    conduit_lib::linux_native::run();
    conduit_lib::daemon_log::flush_stderr_relay(std::time::Duration::from_millis(500));
}
