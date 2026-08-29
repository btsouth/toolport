//! Shell-neutral local observability mutations.

pub fn clear_activity_logs() -> Result<(), String> {
    let mut failed = Vec::new();
    if crate::audit::try_clear().is_err() {
        failed.push("audit log");
    }
    if crate::searchtrace::try_clear().is_err() {
        failed.push("search traces");
    }
    if crate::inspect::try_clear().is_err() {
        failed.push("inspector captures");
    }
    if crate::savings::try_clear().is_err() {
        failed.push("savings");
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!("Couldn't clear: {}", failed.join(", ")))
    }
}
