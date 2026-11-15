fn main() {
    // Before anything can resolve a secret: the shell's stderr (a terminal or
    // the journal) and its children's are redacted from here on.
    conduit_lib::daemon_log::install_stderr_redaction();
    conduit_lib::linux_native::run();
    conduit_lib::daemon_log::flush_stderr_relay(std::time::Duration::from_millis(500));
}
