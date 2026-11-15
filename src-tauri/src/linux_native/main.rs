fn main() {
    // Before anything can resolve a secret: the shell's stderr (a terminal or
    // the journal) and its children's are redacted from here on.
    conduit_lib::daemon_log::install_stderr_redaction();
    // The relay can only remove values it knows: learn every configured secret
    // that can be read without a prompt, off the startup path.
    let _ = std::thread::Builder::new()
        .name("toolport-secret-preload".into())
        .spawn(conduit_lib::secret_refs::preload_configured_secrets);
    conduit_lib::linux_native::run();
    conduit_lib::daemon_log::flush_stderr_relay(std::time::Duration::from_millis(500));
}
