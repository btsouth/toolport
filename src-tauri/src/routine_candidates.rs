//! Short-lived evidence for promoting successful Code Mode runs into immutable routines.
//!
//! The registry is intentionally process-local. It keeps exact source and schema only while a
//! promotion is possible, binds every candidate to one caller/session, and exposes promotion
//! through a single-use lease so approval waits cannot race or hold the registry lock.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::routines::{
    self, EvidenceProvenance, ObservedDependency, PromotionEvidence, RoutineLimits,
    RoutineRiskClass,
};

pub const MAX_CANDIDATES: usize = 64;
pub const MAX_CANDIDATE_BYTES: usize = 8 * 1024 * 1024;
pub const CANDIDATE_TTL: Duration = Duration::from_secs(30 * 60);
pub const MAX_PROMPTS_PER_CALLER: usize = 3;
pub const MAX_TRACKED_DEFINITIONS: usize = 1024;
pub const MAX_TRACKED_CALLERS: usize = 256;
const SIGNIFICANT_INTERMEDIATE_BYTES: usize = 32 * 1024;
const SIGNIFICANT_COMPRESSION_RATIO: usize = 4;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Recommendation {
    Strong,
    Suggest,
    Silent,
    DoNotRecommend,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReasonCode {
    MutableDataMode,
    InvalidInputSchema,
    ScriptFailed,
    DownstreamCallFailed,
    NoDownstreamCall,
    CredentialPatternDetected,
    UnresolvedDependency,
    RoutineWritesDisabled,
    CandidateCapacityExceeded,
    HighRiskPromptSuppressed,
    MultiCallOrchestration,
    StableParameterSchema,
    RoundTripsSaved,
    IntermediateResultsCompressed,
    RepeatedDefinition,
    EquivalentRoutineExists,
    SynthesizedFromObservedCalls,
}

#[derive(Debug, Clone)]
pub struct ToolReceipt {
    pub name: String,
    pub ok: bool,
    pub fingerprint: Option<String>,
    pub risk_class: RoutineRiskClass,
    pub result_bytes: usize,
}

#[derive(Debug)]
pub struct CodeRunEvidence {
    pub source: String,
    pub input_schema: Option<Value>,
    pub limits: RoutineLimits,
    pub immutable_input: bool,
    /// For [`EvidenceProvenance::ImmutableRun`], the script really ran to the end.
    /// For synthesized evidence the glue never executed; the caller sets this to the
    /// static validation verdict and `provenance` discloses the difference everywhere.
    pub script_succeeded: bool,
    pub issued_calls: usize,
    pub receipts: Vec<ToolReceipt>,
    pub final_result_bytes: usize,
    pub writes_enabled: bool,
    pub caller: String,
    pub equivalent_routine_exists: bool,
    pub provenance: EvidenceProvenance,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateAssessment {
    pub run_id: String,
    pub source_hash: String,
    pub eligible: bool,
    pub promotion_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promotion_unavailable_reason: Option<ReasonCode>,
    pub recommendation: Recommendation,
    pub observed_tools: Vec<String>,
    pub reason_codes: Vec<ReasonCode>,
    pub risk_class: RoutineRiskClass,
    pub expires_at_ms: Option<u128>,
    pub provenance: EvidenceProvenance,
}

#[derive(Debug, Clone)]
pub struct PromotionDraft {
    pub run_id: String,
    pub source: String,
    pub source_hash: String,
    pub input_schema: Value,
    pub limits: RoutineLimits,
    pub definition_fingerprint: String,
    pub executed_at_ms: u128,
    pub calls: usize,
    pub observed_dependencies: Vec<ObservedDependency>,
    pub risk_class: RoutineRiskClass,
    pub recommendation: Recommendation,
    pub provenance: EvidenceProvenance,
}

impl PromotionDraft {
    pub fn evidence(&self) -> Result<PromotionEvidence, String> {
        Ok(PromotionEvidence::new(
            self.run_id.clone(),
            self.executed_at_ms,
            self.calls,
            self.observed_dependencies.clone(),
            self.risk_class,
        )?
        .with_provenance(self.provenance))
    }

    pub fn validate(&self) -> Result<(), String> {
        if sha256(self.source.as_bytes()) != self.source_hash {
            return Err("Routine candidate source hash changed".to_string());
        }
        let fingerprint =
            routines::definition_fingerprint(&self.source, &self.input_schema, &self.limits)?;
        if fingerprint != self.definition_fingerprint {
            return Err("Routine candidate definition fingerprint changed".to_string());
        }
        self.evidence().map(|_| ())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromotionOutcome {
    Denied,
    Stale,
    Expired,
    Equivalent,
    Persisted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Suppression {
    Prompted,
    Denied,
}

#[derive(Debug, Clone)]
struct Candidate {
    caller: String,
    draft: PromotionDraft,
    expires_at_ms: u128,
    retained_bytes: usize,
    in_flight: bool,
}

#[derive(Debug, Default)]
struct State {
    candidates: HashMap<String, Candidate>,
    order: VecDeque<String>,
    retained_bytes: usize,
    success_counts: HashMap<(String, String), usize>,
    success_order: VecDeque<(String, String)>,
    suppressions: HashMap<(String, String), Suppression>,
    suppression_order: VecDeque<(String, String)>,
    prompt_counts: HashMap<String, usize>,
    prompt_order: VecDeque<String>,
}

/// Keeps a tracking map bounded: before inserting `key` for the first time, evicts the
/// oldest entries until there is room. Every removal path (refunds, `clear_session`, and
/// eviction here) keeps `order` in step with `map`, so the queue front is always the oldest
/// live entry; the in-loop `remove` of an already-absent key is defensive only.
fn reserve_tracked_key<K, V>(map: &mut HashMap<K, V>, order: &mut VecDeque<K>, cap: usize, key: &K)
where
    K: Eq + std::hash::Hash + Clone,
{
    if map.contains_key(key) {
        return;
    }
    while map.len() >= cap {
        let Some(oldest) = order.pop_front() else {
            break;
        };
        map.remove(&oldest);
    }
    order.push_back(key.clone());
}

#[derive(Debug, Clone, Default)]
pub struct CandidateRegistry {
    state: Arc<Mutex<State>>,
}

#[derive(Debug)]
pub struct PromotionLease {
    registry: CandidateRegistry,
    run_id: String,
    caller: String,
    fingerprint: String,
    draft: PromotionDraft,
    expires_at_ms: u128,
    finished: bool,
}

impl PromotionLease {
    pub fn draft(&self) -> &PromotionDraft {
        &self.draft
    }

    pub fn is_expired(&self) -> bool {
        now_ms() >= self.expires_at_ms
    }

    pub fn finish(mut self, outcome: PromotionOutcome) {
        self.registry.finish_promotion_inner(
            &self.run_id,
            &self.caller,
            &self.fingerprint,
            outcome,
        );
        self.finished = true;
    }
}

impl Drop for PromotionLease {
    fn drop(&mut self) {
        if !self.finished {
            self.registry.finish_promotion_inner(
                &self.run_id,
                &self.caller,
                &self.fingerprint,
                PromotionOutcome::Stale,
            );
        }
    }
}

impl CandidateRegistry {
    pub fn assess_run(&self, evidence: CodeRunEvidence) -> CandidateAssessment {
        let run_id = random_id("run_");
        let source_hash = sha256(evidence.source.as_bytes());
        let executed_at_ms = now_ms();
        let mut reason_codes = Vec::new();
        let mut unavailable = None;
        let input_schema = evidence.input_schema.clone();

        let definition_fingerprint = if !evidence.immutable_input {
            reason_codes.push(ReasonCode::MutableDataMode);
            None
        } else if let Some(schema) = input_schema.as_ref() {
            match routines::definition_fingerprint(&evidence.source, schema, &evidence.limits) {
                Ok(fingerprint) => Some(fingerprint),
                Err(error) => {
                    if error.contains("credential-like") {
                        reason_codes.push(ReasonCode::CredentialPatternDetected);
                    } else {
                        reason_codes.push(ReasonCode::InvalidInputSchema);
                    }
                    None
                }
            }
        } else {
            reason_codes.push(ReasonCode::InvalidInputSchema);
            None
        };

        if !evidence.script_succeeded {
            reason_codes.push(ReasonCode::ScriptFailed);
        }
        if evidence.receipts.is_empty() {
            reason_codes.push(ReasonCode::NoDownstreamCall);
        }
        if evidence.receipts.iter().any(|receipt| !receipt.ok) {
            reason_codes.push(ReasonCode::DownstreamCallFailed);
        }
        if evidence
            .receipts
            .iter()
            .any(|receipt| receipt.fingerprint.is_none())
        {
            reason_codes.push(ReasonCode::UnresolvedDependency);
        }

        let eligible = definition_fingerprint.is_some()
            && evidence.script_succeeded
            && !evidence.receipts.is_empty()
            && evidence.receipts.iter().all(|receipt| receipt.ok)
            && evidence
                .receipts
                .iter()
                .all(|receipt| receipt.fingerprint.is_some());

        let risk_class = evidence
            .receipts
            .iter()
            .map(|receipt| receipt.risk_class)
            .max_by_key(|risk| risk_rank(*risk))
            .unwrap_or(RoutineRiskClass::Unknown);

        let observed_tools = unique_tool_names(&evidence.receipts);
        let intermediate_bytes = evidence
            .receipts
            .iter()
            .map(|receipt| receipt.result_bytes)
            .sum::<usize>();
        let compressed = intermediate_bytes >= SIGNIFICANT_INTERMEDIATE_BYTES
            && evidence
                .final_result_bytes
                .saturating_mul(SIGNIFICANT_COMPRESSION_RATIO)
                <= intermediate_bytes;

        let repeated = definition_fingerprint.as_ref().is_some_and(|fingerprint| {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            let state = &mut *state;
            let key = (evidence.caller.clone(), fingerprint.clone());
            reserve_tracked_key(
                &mut state.success_counts,
                &mut state.success_order,
                MAX_TRACKED_DEFINITIONS,
                &key,
            );
            let count = state.success_counts.entry(key).or_default();
            *count += usize::from(eligible);
            *count >= 2
        });

        if matches!(
            evidence.provenance,
            EvidenceProvenance::SynthesizedFromObservedCalls
        ) {
            reason_codes.push(ReasonCode::SynthesizedFromObservedCalls);
        }

        let recommendation = if matches!(
            risk_class,
            RoutineRiskClass::High | RoutineRiskClass::Unknown
        ) {
            Recommendation::DoNotRecommend
        } else if repeated {
            reason_codes.push(ReasonCode::RepeatedDefinition);
            Recommendation::Strong
        } else if evidence.issued_calls >= 4 || compressed {
            if evidence.issued_calls >= 4 {
                reason_codes.push(ReasonCode::MultiCallOrchestration);
                reason_codes.push(ReasonCode::RoundTripsSaved);
            }
            if compressed {
                reason_codes.push(ReasonCode::IntermediateResultsCompressed);
            }
            Recommendation::Suggest
        } else {
            Recommendation::Silent
        };
        if eligible {
            reason_codes.push(ReasonCode::StableParameterSchema);
        }

        let mut promotion_available = eligible && !evidence.equivalent_routine_exists;
        if eligible && evidence.equivalent_routine_exists {
            unavailable = Some(ReasonCode::EquivalentRoutineExists);
        }
        if promotion_available && !evidence.writes_enabled {
            promotion_available = false;
            unavailable = Some(ReasonCode::RoutineWritesDisabled);
        }
        if promotion_available
            && matches!(
                risk_class,
                RoutineRiskClass::High | RoutineRiskClass::Unknown
            )
        {
            promotion_available = false;
            unavailable = Some(ReasonCode::HighRiskPromptSuppressed);
        }

        let mut expires_at_ms = None;
        if promotion_available {
            let schema = input_schema.expect("eligible immutable runs have a schema");
            let fingerprint = definition_fingerprint
                .clone()
                .expect("eligible immutable runs have a fingerprint");
            let dependencies = observed_dependencies(&evidence.receipts);
            let retained_bytes = evidence.source.len()
                + serde_json::to_vec(&schema)
                    .map(|value| value.len())
                    .unwrap_or(0);
            let expires = executed_at_ms + CANDIDATE_TTL.as_millis();
            let candidate = Candidate {
                caller: evidence.caller,
                draft: PromotionDraft {
                    run_id: run_id.clone(),
                    source: evidence.source,
                    source_hash: source_hash.clone(),
                    input_schema: schema,
                    limits: evidence.limits,
                    definition_fingerprint: fingerprint,
                    executed_at_ms,
                    calls: evidence.receipts.len(),
                    observed_dependencies: dependencies,
                    risk_class,
                    recommendation,
                    provenance: evidence.provenance,
                },
                expires_at_ms: expires,
                retained_bytes,
                in_flight: false,
            };
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            prune(&mut state, executed_at_ms);
            if make_room(&mut state, retained_bytes) {
                state.retained_bytes += retained_bytes;
                state.order.push_back(run_id.clone());
                state.candidates.insert(run_id.clone(), candidate);
                expires_at_ms = Some(expires);
            } else {
                promotion_available = false;
                unavailable = Some(ReasonCode::CandidateCapacityExceeded);
            }
        }

        CandidateAssessment {
            run_id,
            source_hash,
            eligible,
            promotion_available,
            promotion_unavailable_reason: unavailable,
            recommendation,
            observed_tools,
            reason_codes,
            risk_class,
            expires_at_ms,
            provenance: evidence.provenance,
        }
    }

    pub fn begin_promotion(&self, run_id: &str, caller: &str) -> Result<PromotionLease, String> {
        let now = now_ms();
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let state = &mut *state;
        prune(state, now);
        let candidate = state
            .candidates
            .get(run_id)
            .ok_or_else(|| "Routine candidate is missing or expired".to_string())?;
        if candidate.caller != caller {
            return Err("Routine candidate belongs to a different caller or session".to_string());
        }
        if candidate.in_flight {
            return Err("Routine candidate promotion is already in progress".to_string());
        }
        let fingerprint = candidate.draft.definition_fingerprint.clone();
        let key = (caller.to_string(), fingerprint.clone());
        if state.suppressions.contains_key(&key) {
            return Err(
                "An equivalent routine promotion was already prompted in this session".to_string(),
            );
        }
        if state.prompt_counts.get(caller).copied().unwrap_or(0) >= MAX_PROMPTS_PER_CALLER {
            return Err(
                "Routine promotion prompt budget was exhausted for this session".to_string(),
            );
        }
        let candidate = state
            .candidates
            .get_mut(run_id)
            .expect("candidate remains present while registry lock is held");
        candidate.in_flight = true;
        let draft = candidate.draft.clone();
        let expires_at_ms = candidate.expires_at_ms;
        reserve_tracked_key(
            &mut state.suppressions,
            &mut state.suppression_order,
            MAX_TRACKED_DEFINITIONS,
            &key,
        );
        state.suppressions.insert(key, Suppression::Prompted);
        let caller_key = caller.to_string();
        reserve_tracked_key(
            &mut state.prompt_counts,
            &mut state.prompt_order,
            MAX_TRACKED_CALLERS,
            &caller_key,
        );
        *state.prompt_counts.entry(caller_key).or_default() += 1;
        Ok(PromotionLease {
            registry: self.clone(),
            run_id: run_id.to_string(),
            caller: caller.to_string(),
            fingerprint,
            draft,
            expires_at_ms,
            finished: false,
        })
    }

    pub fn clear_session(&self, caller: &str) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let run_ids = state
            .candidates
            .iter()
            .filter(|(_, candidate)| candidate.caller == caller)
            .map(|(run_id, _)| run_id.clone())
            .collect::<Vec<_>>();
        for run_id in run_ids {
            remove_candidate(&mut state, &run_id);
        }
        state
            .success_counts
            .retain(|(candidate_caller, _), _| candidate_caller != caller);
        state
            .success_order
            .retain(|(candidate_caller, _)| candidate_caller != caller);
        state
            .suppressions
            .retain(|(candidate_caller, _), _| candidate_caller != caller);
        state
            .suppression_order
            .retain(|(candidate_caller, _)| candidate_caller != caller);
        state.prompt_counts.remove(caller);
        state
            .prompt_order
            .retain(|candidate_caller| candidate_caller != caller);
    }

    fn finish_promotion_inner(
        &self,
        run_id: &str,
        caller: &str,
        fingerprint: &str,
        outcome: PromotionOutcome,
    ) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let state = &mut *state;
        remove_candidate(state, run_id);
        let key = (caller.to_string(), fingerprint.to_string());
        match outcome {
            // The user never made a decision (equivalent short-circuit, internal error, or an
            // unreachable approval broker): refund the fingerprint suppression and the prompt
            // budget so a later run may still ask once.
            PromotionOutcome::Equivalent | PromotionOutcome::Stale => {
                remove_suppression(state, &key);
                if let Some(count) = state.prompt_counts.get_mut(caller) {
                    *count = count.saturating_sub(1);
                }
            }
            // The user approved but the state went stale before the write: allow a fresh run
            // to prompt again, but keep the budget consumed - a prompt was actually shown.
            PromotionOutcome::Expired => {
                remove_suppression(state, &key);
            }
            PromotionOutcome::Denied => {
                state.suppressions.insert(key, Suppression::Denied);
            }
            PromotionOutcome::Persisted => {}
        }
    }
}

fn random_id(prefix: &str) -> String {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).expect("operating system random source is required");
    let suffix = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("{prefix}{suffix}")
}

fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("sha256:{digest:x}")
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn risk_rank(risk: RoutineRiskClass) -> u8 {
    match risk {
        RoutineRiskClass::Low => 0,
        RoutineRiskClass::Medium => 1,
        RoutineRiskClass::High => 2,
        RoutineRiskClass::Unknown => 3,
    }
}

fn unique_tool_names(receipts: &[ToolReceipt]) -> Vec<String> {
    let mut names = Vec::new();
    for receipt in receipts {
        if !names.contains(&receipt.name) {
            names.push(receipt.name.clone());
        }
    }
    names
}

fn observed_dependencies(receipts: &[ToolReceipt]) -> Vec<ObservedDependency> {
    let mut dependencies = Vec::new();
    for receipt in receipts {
        if dependencies
            .iter()
            .any(|dependency: &ObservedDependency| dependency.name() == receipt.name)
        {
            continue;
        }
        if let Ok(dependency) =
            ObservedDependency::new(receipt.name.clone(), receipt.fingerprint.clone())
        {
            dependencies.push(dependency);
        }
    }
    dependencies
}

fn make_room(state: &mut State, incoming: usize) -> bool {
    if incoming > MAX_CANDIDATE_BYTES {
        return false;
    }
    while state.candidates.len() >= MAX_CANDIDATES
        || state.retained_bytes.saturating_add(incoming) > MAX_CANDIDATE_BYTES
    {
        let Some(run_id) = state.order.iter().find_map(|run_id| {
            state
                .candidates
                .get(run_id)
                .is_some_and(|candidate| !candidate.in_flight)
                .then(|| run_id.clone())
        }) else {
            return false;
        };
        remove_candidate(state, &run_id);
    }
    true
}

fn prune(state: &mut State, now: u128) {
    let expired = state
        .candidates
        .iter()
        .filter(|(_, candidate)| candidate.expires_at_ms <= now && !candidate.in_flight)
        .map(|(run_id, _)| run_id.clone())
        .collect::<Vec<_>>();
    for run_id in expired {
        remove_candidate(state, &run_id);
    }
}

fn remove_candidate(state: &mut State, run_id: &str) {
    if let Some(candidate) = state.candidates.remove(run_id) {
        state.retained_bytes = state
            .retained_bytes
            .saturating_sub(candidate.retained_bytes);
    }
    state.order.retain(|candidate_id| candidate_id != run_id);
}

/// Remove a refunded suppression together with its `suppression_order` entry. A stale order
/// entry would let the same key re-enter the queue on a later prompt; once the map reaches
/// capacity, that ghost at the queue front would evict the key's newer, real suppression
/// (even a user's explicit denial) instead of the actual oldest entry.
fn remove_suppression(state: &mut State, key: &(String, String)) {
    state.suppressions.remove(key);
    state.suppression_order.retain(|tracked| tracked != key);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn evidence(caller: &str) -> CodeRunEvidence {
        CodeRunEvidence {
            source: "return input.value;".to_string(),
            input_schema: Some(json!({
                "type": "object",
                "properties": { "value": { "type": "string" } },
                "required": ["value"],
                "additionalProperties": false
            })),
            limits: RoutineLimits::default(),
            immutable_input: true,
            script_succeeded: true,
            issued_calls: 4,
            receipts: vec![ToolReceipt {
                name: "test__read".to_string(),
                ok: true,
                fingerprint: Some("fp".to_string()),
                risk_class: RoutineRiskClass::Low,
                result_bytes: SIGNIFICANT_INTERMEDIATE_BYTES,
            }],
            final_result_bytes: 16,
            writes_enabled: true,
            caller: caller.to_string(),
            equivalent_routine_exists: false,
            provenance: EvidenceProvenance::ImmutableRun,
        }
    }

    #[test]
    fn synthesized_evidence_is_disclosed_end_to_end() {
        let registry = CandidateRegistry::default();
        let mut synthesized = evidence("caller-synth");
        synthesized.provenance = EvidenceProvenance::SynthesizedFromObservedCalls;
        let assessment = registry.assess_run(synthesized);
        assert!(assessment.eligible);
        assert_eq!(
            assessment.provenance,
            EvidenceProvenance::SynthesizedFromObservedCalls
        );
        assert!(assessment
            .reason_codes
            .contains(&ReasonCode::SynthesizedFromObservedCalls));

        // Provenance survives the draft and lands in the persistable evidence, so the
        // approval payload and the store both see it.
        let lease = registry
            .begin_promotion(&assessment.run_id, "caller-synth")
            .unwrap();
        assert_eq!(
            lease.draft().provenance,
            EvidenceProvenance::SynthesizedFromObservedCalls
        );
        let persistable = lease.draft().evidence().unwrap();
        assert_eq!(
            persistable.provenance(),
            EvidenceProvenance::SynthesizedFromObservedCalls
        );
        lease.finish(PromotionOutcome::Persisted);
    }

    #[test]
    fn successful_immutable_run_creates_bound_candidate() {
        let registry = CandidateRegistry::default();
        let assessment = registry.assess_run(evidence("caller-a"));
        assert!(assessment.eligible);
        assert!(assessment.promotion_available);
        assert!(assessment.run_id.starts_with("run_"));
        assert!(registry
            .begin_promotion(&assessment.run_id, "caller-b")
            .unwrap_err()
            .contains("different caller"));
        let lease = registry
            .begin_promotion(&assessment.run_id, "caller-a")
            .unwrap();
        assert_eq!(lease.draft().source, "return input.value;");
        lease.finish(PromotionOutcome::Persisted);
    }

    #[test]
    fn mutable_failed_and_zero_call_runs_are_not_eligible() {
        let registry = CandidateRegistry::default();
        let mut run = evidence("caller");
        run.immutable_input = false;
        run.script_succeeded = false;
        run.receipts.clear();
        let assessment = registry.assess_run(run);
        assert!(!assessment.eligible);
        assert!(!assessment.promotion_available);
        assert!(assessment
            .reason_codes
            .contains(&ReasonCode::MutableDataMode));
        assert!(assessment.reason_codes.contains(&ReasonCode::ScriptFailed));
        assert!(assessment
            .reason_codes
            .contains(&ReasonCode::NoDownstreamCall));
    }

    #[test]
    fn writes_disabled_keeps_eligibility_but_retains_no_candidate() {
        let registry = CandidateRegistry::default();
        let mut run = evidence("caller");
        run.writes_enabled = false;
        let assessment = registry.assess_run(run);
        assert!(assessment.eligible);
        assert!(!assessment.promotion_available);
        assert_eq!(
            assessment.promotion_unavailable_reason,
            Some(ReasonCode::RoutineWritesDisabled)
        );
        assert!(registry
            .begin_promotion(&assessment.run_id, "caller")
            .is_err());
    }

    #[test]
    fn lease_and_fingerprint_suppression_are_atomic() {
        let registry = CandidateRegistry::default();
        let first = registry.assess_run(evidence("caller"));
        let second = registry.assess_run(evidence("caller"));
        let lease = registry.begin_promotion(&first.run_id, "caller").unwrap();
        assert!(registry.begin_promotion(&first.run_id, "caller").is_err());
        assert!(registry.begin_promotion(&second.run_id, "caller").is_err());
        lease.finish(PromotionOutcome::Denied);
    }

    #[test]
    fn high_risk_run_is_assessed_but_not_promotable() {
        let registry = CandidateRegistry::default();
        let mut run = evidence("caller");
        run.receipts[0].risk_class = RoutineRiskClass::High;
        let assessment = registry.assess_run(run);
        assert!(assessment.eligible);
        assert!(!assessment.promotion_available);
        assert_eq!(assessment.recommendation, Recommendation::DoNotRecommend);
    }

    #[test]
    fn equivalent_definition_is_eligible_but_not_retained() {
        let registry = CandidateRegistry::default();
        let mut run = evidence("caller");
        run.equivalent_routine_exists = true;
        let assessment = registry.assess_run(run);
        assert!(assessment.eligible);
        assert!(!assessment.promotion_available);
        assert_eq!(
            assessment.promotion_unavailable_reason,
            Some(ReasonCode::EquivalentRoutineExists)
        );
        assert!(registry
            .begin_promotion(&assessment.run_id, "caller")
            .is_err());
    }

    #[test]
    fn prompt_budget_is_bounded_per_caller() {
        let registry = CandidateRegistry::default();
        for index in 0..MAX_PROMPTS_PER_CALLER {
            let mut run = evidence("caller");
            run.source = format!("// {index}\nreturn input.value;");
            let assessment = registry.assess_run(run);
            registry
                .begin_promotion(&assessment.run_id, "caller")
                .unwrap()
                .finish(PromotionOutcome::Persisted);
        }
        let mut extra = evidence("caller");
        extra.source = "// extra\nreturn input.value;".to_string();
        let assessment = registry.assess_run(extra);
        assert!(registry
            .begin_promotion(&assessment.run_id, "caller")
            .unwrap_err()
            .contains("prompt budget"));
    }

    #[test]
    fn stale_finish_refunds_suppression_and_prompt_budget() {
        let registry = CandidateRegistry::default();
        let first = registry.assess_run(evidence("caller"));
        let lease = registry.begin_promotion(&first.run_id, "caller").unwrap();
        // Broker unreachable / internal error: the user never saw a prompt.
        lease.finish(PromotionOutcome::Stale);
        // A fresh run with the same definition may still prompt once.
        let second = registry.assess_run(evidence("caller"));
        let lease = registry
            .begin_promotion(&second.run_id, "caller")
            .expect("stale finish must not suppress the fingerprint");
        lease.finish(PromotionOutcome::Denied);
        // Budget was refunded once: MAX_PROMPTS_PER_CALLER distinct definitions still fit.
        for index in 0..MAX_PROMPTS_PER_CALLER - 1 {
            let mut run = evidence("caller");
            run.source = format!("// refund {index}\nreturn input.value;");
            let assessment = registry.assess_run(run);
            registry
                .begin_promotion(&assessment.run_id, "caller")
                .unwrap()
                .finish(PromotionOutcome::Persisted);
        }
    }

    #[test]
    fn expired_finish_allows_reprompt_but_keeps_budget() {
        let registry = CandidateRegistry::default();
        let first = registry.assess_run(evidence("caller"));
        let lease = registry.begin_promotion(&first.run_id, "caller").unwrap();
        // The user approved, but the state went stale before the write.
        lease.finish(PromotionOutcome::Expired);
        let second = registry.assess_run(evidence("caller"));
        registry
            .begin_promotion(&second.run_id, "caller")
            .expect("expired finish must not suppress the fingerprint")
            .finish(PromotionOutcome::Persisted);
    }

    #[test]
    fn denied_finish_keeps_fingerprint_suppressed() {
        let registry = CandidateRegistry::default();
        let first = registry.assess_run(evidence("caller"));
        registry
            .begin_promotion(&first.run_id, "caller")
            .unwrap()
            .finish(PromotionOutcome::Denied);
        let second = registry.assess_run(evidence("caller"));
        assert!(registry
            .begin_promotion(&second.run_id, "caller")
            .unwrap_err()
            .contains("already prompted"));
    }

    #[test]
    fn refunds_keep_suppression_order_in_step_with_the_map() {
        let registry = CandidateRegistry::default();
        let order_and_map_len = || {
            let state = registry
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            (state.suppression_order.len(), state.suppressions.len())
        };

        // Every refunding outcome must clear the order entry along with the map entry.
        // A ghost left at the queue front would, at capacity, evict this key's next real
        // suppression (even an explicit denial) instead of the actual oldest entry.
        for outcome in [
            PromotionOutcome::Stale,
            PromotionOutcome::Equivalent,
            PromotionOutcome::Expired,
            PromotionOutcome::Stale,
        ] {
            let assessment = registry.assess_run(evidence("caller"));
            let lease = registry
                .begin_promotion(&assessment.run_id, "caller")
                .unwrap();
            lease.finish(outcome);
            assert_eq!(
                order_and_map_len(),
                (0, 0),
                "refund via {outcome:?} left a ghost order entry"
            );
        }

        // A real denial tracks exactly one entry in both structures.
        let assessment = registry.assess_run(evidence("caller"));
        registry
            .begin_promotion(&assessment.run_id, "caller")
            .unwrap()
            .finish(PromotionOutcome::Denied);
        assert_eq!(order_and_map_len(), (1, 1));
    }

    #[test]
    fn success_tracking_stays_bounded() {
        let registry = CandidateRegistry::default();
        for index in 0..MAX_TRACKED_DEFINITIONS + 8 {
            let mut run = evidence("caller");
            run.source = format!("// bounded {index}\nreturn input.value;");
            run.writes_enabled = false;
            registry.assess_run(run);
        }
        let state = registry
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        assert!(state.success_counts.len() <= MAX_TRACKED_DEFINITIONS);
        assert!(state.success_order.len() <= MAX_TRACKED_DEFINITIONS);
    }
}
