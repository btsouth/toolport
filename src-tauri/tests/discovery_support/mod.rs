//! Explicit discovery choices for transport fixtures whose assertions inspect
//! the full catalog. Unknown synthetic client IDs correctly default to lazy.

pub fn select_full(dir: &std::path::Path, client_id: &str) {
    let path = dir.join("registry.json");
    let _lock = conduit_lib::registry::lock_at(&path).expect("lock fixture registry");
    let mut registry: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).expect("read fixture registry"))
            .expect("parse fixture registry");
    if !registry["clientDiscovery"].is_object() {
        registry["clientDiscovery"] = serde_json::json!({});
    }
    registry["clientDiscovery"][client_id] = serde_json::json!("full");
    conduit_lib::registry::atomic_write(
        &path,
        &serde_json::to_string_pretty(&registry).expect("serialize fixture registry"),
    )
    .expect("save fixture discovery choice");
}
