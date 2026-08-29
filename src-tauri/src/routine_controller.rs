//! Shell-neutral operations for routine suggestions surfaced by the approval broker.

use crate::{approval_broker::ApprovalBroker, audit, routines};

pub fn approve_suggestion(
    broker: &ApprovalBroker,
    fingerprint: &str,
    name: String,
    description: Option<String>,
) -> Result<routines::RoutineDefinition, String> {
    let suggestion = broker
        .suggestion(fingerprint)
        .ok_or_else(|| "no queued suggestion with that fingerprint".to_string())?;
    suggestion.validate()?;
    let started = std::time::Instant::now();
    if let Some(existing) = routines::find_by_definition_fingerprint(fingerprint)? {
        broker.remove_suggestion(fingerprint);
        return Ok(existing);
    }
    let definition = routines::new_promoted_definition(
        name,
        description.filter(|text| !text.trim().is_empty()),
        suggestion.source,
        suggestion.input_schema,
        suggestion.limits,
        suggestion.evidence,
    )?;
    let saved = routines::append_immutable(definition)?;
    audit::record_routine(
        "save",
        saved.id(),
        saved.content_hash(),
        true,
        Some(started.elapsed().as_millis().min(u64::MAX as u128) as u64),
        Some("app_suggestion"),
        None,
    );
    broker.remove_suggestion(fingerprint);
    Ok(saved)
}

pub fn dismiss_suggestion(broker: &ApprovalBroker, fingerprint: &str) {
    broker.dismiss_suggestion(fingerprint);
}
