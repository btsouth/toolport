//! Persistent, immutable Code Mode routines (issue #625).
//!
//! Routines deliberately live outside `registry.json`: saving executable workflow code must
//! not bump the registry mtime and rebuild every downstream server. The store uses the same
//! owner-only atomic writer and sibling lock as the registry, while retaining its own schema,
//! backup, corruption, and content-integrity rules.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{codemode, registry};

pub const STORE_SCHEMA_VERSION: u32 = 2;
pub const MAX_NAME_CHARS: usize = 128;
pub const MAX_DESCRIPTION_CHARS: usize = 2_048;
pub const MAX_SOURCE_BYTES: usize = 256 * 1024;
pub const MAX_SCHEMA_BYTES: usize = 64 * 1024;
pub const MAX_VALIDATION_ERRORS: usize = 8;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RoutineRiskClass {
    Low,
    Medium,
    High,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ObservedDependency {
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_fingerprint: Option<String>,
}

impl ObservedDependency {
    pub fn new(name: String, tool_fingerprint: Option<String>) -> Result<Self, String> {
        if name.trim().is_empty() {
            return Err("Observed dependency name must not be empty".to_string());
        }
        Ok(Self {
            name,
            tool_fingerprint,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn tool_fingerprint(&self) -> Option<&str> {
        self.tool_fingerprint.as_deref()
    }
}

/// How the promoted source came to be trusted.
///
/// `ImmutableRun` is the original standard: the exact source really executed once,
/// end to end. `SynthesizedFromObservedCalls` covers advisor-built definitions: every
/// downstream call in the evidence really happened (as direct calls), but the glue
/// script around them was generated deterministically and only statically validated,
/// never executed. Approval UI and audit must disclose the difference; it is not a
/// hidden implementation detail.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceProvenance {
    #[default]
    ImmutableRun,
    SynthesizedFromObservedCalls,
}

impl EvidenceProvenance {
    fn is_default(&self) -> bool {
        matches!(self, EvidenceProvenance::ImmutableRun)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PromotionEvidence {
    source_run_id: String,
    executed_at_ms: u128,
    calls: usize,
    observed_dependencies: Vec<ObservedDependency>,
    validation_version: u32,
    risk_class: RoutineRiskClass,
    // Skipped when default so every pre-provenance record keeps its serialized form,
    // and therefore its contentHash, byte-identical. Only synthesized definitions pay
    // the new field into their hash.
    #[serde(default, skip_serializing_if = "EvidenceProvenance::is_default")]
    provenance: EvidenceProvenance,
}

impl PromotionEvidence {
    pub fn new(
        source_run_id: String,
        executed_at_ms: u128,
        calls: usize,
        observed_dependencies: Vec<ObservedDependency>,
        risk_class: RoutineRiskClass,
    ) -> Result<Self, String> {
        if !valid_run_id(&source_run_id) {
            return Err("Promotion evidence has an invalid source run id".to_string());
        }
        if calls == 0 || observed_dependencies.is_empty() {
            return Err("Promotion evidence must contain a real downstream call".to_string());
        }
        let mut names = HashSet::new();
        for dependency in &observed_dependencies {
            if !names.insert(dependency.name.as_str()) {
                return Err(format!(
                    "Promotion evidence repeats dependency {}",
                    dependency.name
                ));
            }
        }
        Ok(Self {
            source_run_id,
            executed_at_ms,
            calls,
            observed_dependencies,
            validation_version: 1,
            risk_class,
            provenance: EvidenceProvenance::ImmutableRun,
        })
    }

    pub fn with_provenance(mut self, provenance: EvidenceProvenance) -> Self {
        self.provenance = provenance;
        self
    }

    pub fn provenance(&self) -> EvidenceProvenance {
        self.provenance
    }

    pub fn source_run_id(&self) -> &str {
        &self.source_run_id
    }

    pub fn executed_at_ms(&self) -> u128 {
        self.executed_at_ms
    }

    pub fn calls(&self) -> usize {
        self.calls
    }

    pub fn observed_dependencies(&self) -> &[ObservedDependency] {
        &self.observed_dependencies
    }

    pub fn risk_class(&self) -> RoutineRiskClass {
        self.risk_class
    }

    fn validate(&self) -> Result<(), String> {
        if self.validation_version != 1 {
            return Err(format!(
                "Unsupported promotion validationVersion {}",
                self.validation_version
            ));
        }
        PromotionEvidence::new(
            self.source_run_id.clone(),
            self.executed_at_ms,
            self.calls,
            self.observed_dependencies.clone(),
            self.risk_class,
        )?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RoutineLimits {
    pub max_calls: usize,
    pub wall_clock_ms: u64,
    pub max_parallel: usize,
    pub max_promise_jobs: usize,
    pub loop_iteration_limit: u64,
    pub recursion_limit: usize,
}

impl Default for RoutineLimits {
    fn default() -> Self {
        Self::from(codemode::Limits::default())
    }
}

impl From<codemode::Limits> for RoutineLimits {
    fn from(value: codemode::Limits) -> Self {
        Self {
            max_calls: value.max_calls,
            wall_clock_ms: value.wall_clock.as_millis().min(u64::MAX as u128) as u64,
            max_parallel: value.max_parallel,
            max_promise_jobs: value.max_promise_jobs,
            loop_iteration_limit: value.loop_iteration_limit,
            recursion_limit: value.recursion_limit,
        }
    }
}

impl RoutineLimits {
    /// Convert persisted limits into executable limits without ever exceeding this build's
    /// hard defaults. A future build can safely lower a cap without old routines bypassing it.
    pub fn effective(&self) -> codemode::Limits {
        let hard = Self::default();
        codemode::Limits {
            max_calls: self.max_calls.min(hard.max_calls),
            wall_clock: Duration::from_millis(self.wall_clock_ms.min(hard.wall_clock_ms)),
            max_parallel: self.max_parallel.min(hard.max_parallel).max(1),
            max_promise_jobs: self.max_promise_jobs.min(hard.max_promise_jobs),
            loop_iteration_limit: self.loop_iteration_limit.min(hard.loop_iteration_limit),
            recursion_limit: self.recursion_limit.min(hard.recursion_limit),
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.max_calls == 0
            || self.wall_clock_ms == 0
            || self.max_parallel == 0
            || self.max_promise_jobs == 0
            || self.loop_iteration_limit == 0
            || self.recursion_limit == 0
        {
            return Err("Routine limits must all be greater than zero".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RoutineDefinition {
    id: String,
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    source: String,
    input_schema: Value,
    limits: RoutineLimits,
    definition_fingerprint: String,
    content_hash: String,
    evidence: PromotionEvidence,
    created_at_ms: u128,
    #[serde(flatten)]
    unknown_fields: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct RoutineStore {
    schema_version: u32,
    #[serde(default)]
    routines: Vec<RoutineDefinition>,
    #[serde(flatten)]
    unknown_fields: BTreeMap<String, Value>,
}

impl Default for RoutineStore {
    fn default() -> Self {
        Self {
            schema_version: STORE_SCHEMA_VERSION,
            routines: Vec::new(),
            unknown_fields: BTreeMap::new(),
        }
    }
}

impl RoutineDefinition {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn input_schema(&self) -> &Value {
        &self.input_schema
    }

    pub fn limits(&self) -> &RoutineLimits {
        &self.limits
    }

    pub fn definition_fingerprint(&self) -> &str {
        &self.definition_fingerprint
    }

    pub fn content_hash(&self) -> &str {
        &self.content_hash
    }

    pub fn evidence(&self) -> &PromotionEvidence {
        &self.evidence
    }

    pub fn created_at_ms(&self) -> u128 {
        self.created_at_ms
    }

    /// Re-check every invariant that makes a definition safe to persist or execute.
    pub fn verify(&self) -> Result<(), String> {
        if !valid_id(&self.id) {
            return Err(format!("Invalid routine id {}", self.id));
        }
        validate_definition_fields(
            &self.name,
            self.description.as_deref(),
            &self.source,
            &self.input_schema,
        )?;
        self.limits.validate()?;
        self.evidence.validate()?;
        reject_credentials_in_serializable("routine metadata", &self.unknown_fields)?;
        let expected_definition =
            definition_fingerprint(&self.source, &self.input_schema, &self.limits)?;
        if self.definition_fingerprint != expected_definition {
            return Err(format!(
                "Routine {} definition fingerprint mismatch",
                self.id
            ));
        }
        let expected = content_hash(self)?;
        if self.content_hash != expected {
            return Err(format!("Routine {} content hash mismatch", self.id));
        }
        Ok(())
    }
}

pub fn routines_path() -> Option<PathBuf> {
    Some(registry::conduit_dir()?.join("routines.json"))
}

fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".bak");
    PathBuf::from(name)
}

pub fn generate_id() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes)
        .map_err(|e| format!("Could not generate a routine id: {e}"))?;
    Ok(format!(
        "routine_{}",
        bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ))
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn valid_id(id: &str) -> bool {
    id.len() == 40
        && id.starts_with("routine_")
        && id[8..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_run_id(id: &str) -> bool {
    id.len() == 36 && id.starts_with("run_") && id[4..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn definition_hash_payload(source: &str, input_schema: &Value, limits: &RoutineLimits) -> Value {
    json!({
        "schemaVersion": STORE_SCHEMA_VERSION,
        "runtime": "toolport-codemode-v1",
        "source": source,
        "inputSchema": input_schema,
        "limits": limits,
    })
}

pub fn definition_fingerprint(
    source: &str,
    input_schema: &Value,
    limits: &RoutineLimits,
) -> Result<String, String> {
    validate_definition_fields("candidate", None, source, input_schema)?;
    limits.validate()?;
    let bytes = serde_json::to_vec(&definition_hash_payload(source, input_schema, limits))
        .map_err(|e| e.to_string())?;
    let digest = Sha256::digest(bytes);
    Ok(format!("sha256:{digest:x}"))
}

fn hash_payload(definition: &RoutineDefinition) -> Value {
    json!({
        "schemaVersion": STORE_SCHEMA_VERSION,
        "name": definition.name,
        "description": definition.description,
        "definitionFingerprint": definition.definition_fingerprint,
        "evidence": definition.evidence,
    })
}

fn content_hash(definition: &RoutineDefinition) -> Result<String, String> {
    let bytes = serde_json::to_vec(&hash_payload(definition)).map_err(|e| e.to_string())?;
    let digest = Sha256::digest(bytes);
    Ok(format!("sha256:{digest:x}"))
}

pub fn new_promoted_definition(
    name: String,
    description: Option<String>,
    source: String,
    input_schema: Value,
    limits: RoutineLimits,
    evidence: PromotionEvidence,
) -> Result<RoutineDefinition, String> {
    validate_definition_fields(&name, description.as_deref(), &source, &input_schema)?;
    limits.validate()?;
    evidence.validate()?;
    let definition_fingerprint = definition_fingerprint(&source, &input_schema, &limits)?;
    let mut definition = RoutineDefinition {
        id: generate_id()?,
        name,
        description,
        source,
        input_schema,
        limits,
        definition_fingerprint,
        content_hash: String::new(),
        evidence,
        created_at_ms: now_ms(),
        unknown_fields: BTreeMap::new(),
    };
    definition.content_hash = content_hash(&definition)?;
    Ok(definition)
}

/// A strong, promotion-available candidate published by a gateway to the desktop app's
/// passive suggestion area. Self-contained: everything needed to persist a definition
/// travels in the message, so approving it does not depend on the publishing gateway
/// process (or its in-memory candidate) still being alive.
///
/// Carries NO observed argument values: synthesized sources and schemas are value-free
/// by construction, and immutable-run sources are the agent-authored script. The input
/// example never rides along - it stays in the session-scoped fetch cursor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RoutineSuggestion {
    pub suggested_name: String,
    pub source: String,
    pub input_schema: Value,
    pub limits: RoutineLimits,
    pub definition_fingerprint: String,
    pub evidence: PromotionEvidence,
    /// Bytes the observed calls put into the model's context; card-display material.
    pub intermediate_bytes: usize,
}

impl RoutineSuggestion {
    /// Fail-closed shape check before a suggestion enters the app's store: the
    /// fingerprint must really describe this source/schema/limits, and the evidence
    /// must be internally valid. Keeps a bad (or tampered) message from parking
    /// persistable-looking material in the UI.
    pub fn validate(&self) -> Result<(), String> {
        let fingerprint = definition_fingerprint(&self.source, &self.input_schema, &self.limits)?;
        if fingerprint != self.definition_fingerprint {
            return Err("suggestion fingerprint does not match its definition".to_string());
        }
        if self.suggested_name.trim().is_empty() {
            return Err("suggestion name must not be empty".to_string());
        }
        self.evidence.validate()
    }
}

/// Test fixture: a definition with fabricated promotion evidence. Absent from
/// production release binaries. `cfg(test)` alone is not enough: the gateway
/// binary's tests link this library without the library's own test cfg, so
/// release-profile binary tests also need `--features test-support`.
#[cfg(any(debug_assertions, test, feature = "test-support"))]
#[doc(hidden)]
pub fn new_definition(
    name: String,
    description: Option<String>,
    source: String,
    input_schema: Value,
) -> Result<RoutineDefinition, String> {
    let dependency = ObservedDependency::new("test__tool".to_string(), Some("test".to_string()))?;
    let evidence = PromotionEvidence::new(
        format!("run_{}", "0".repeat(32)),
        now_ms(),
        1,
        vec![dependency],
        RoutineRiskClass::Low,
    )?;
    new_promoted_definition(
        name,
        description,
        source,
        input_schema,
        RoutineLimits::default(),
        evidence,
    )
}

fn validate_definition_fields(
    name: &str,
    description: Option<&str>,
    source: &str,
    input_schema: &Value,
) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Routine name must not be empty".to_string());
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(format!("Routine name exceeds {MAX_NAME_CHARS} characters"));
    }
    if description.is_some_and(|value| value.chars().count() > MAX_DESCRIPTION_CHARS) {
        return Err(format!(
            "Routine description exceeds {MAX_DESCRIPTION_CHARS} characters"
        ));
    }
    if source.trim().is_empty() {
        return Err("Routine source must not be empty".to_string());
    }
    if source.len() > MAX_SOURCE_BYTES {
        return Err(format!("Routine source exceeds {MAX_SOURCE_BYTES} bytes"));
    }
    validate_input_schema(input_schema)?;
    reject_credentials_in_text("name", name)?;
    if let Some(description) = description {
        reject_credentials_in_text("description", description)?;
    }
    reject_credentials_in_text("source", source)?;
    reject_credentials_in_serializable("inputSchema", input_schema)
}

fn reject_external_refs(value: &Value, path: &str) -> Result<(), String> {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                let child_path = format!("{path}/{key}");
                if key == "$ref" {
                    let reference = child
                        .as_str()
                        .ok_or_else(|| format!("inputSchema {child_path} must be a string"))?;
                    if !reference.starts_with('#') {
                        return Err(format!(
                            "inputSchema {child_path} uses an external reference; only local # fragments are allowed"
                        ));
                    }
                }
                reject_external_refs(child, &child_path)?;
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                reject_external_refs(child, &format!("{path}/{index}"))?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate_input_schema(schema: &Value) -> Result<jsonschema::Validator, String> {
    let object = schema
        .as_object()
        .ok_or_else(|| "inputSchema must be a JSON object".to_string())?;
    if object.get("type").and_then(Value::as_str) != Some("object") {
        return Err("inputSchema root must declare `type: object`".to_string());
    }
    if let Some(declared) = object.get("$schema").and_then(Value::as_str) {
        if !matches!(
            declared,
            "https://json-schema.org/draft/2020-12/schema"
                | "https://json-schema.org/draft/2020-12/schema#"
        ) {
            return Err("inputSchema must use JSON Schema Draft 2020-12".to_string());
        }
    }
    let size = serde_json::to_vec(schema).map_err(|e| e.to_string())?.len();
    if size > MAX_SCHEMA_BYTES {
        return Err(format!("inputSchema exceeds {MAX_SCHEMA_BYTES} bytes"));
    }
    reject_external_refs(schema, "")?;
    jsonschema::draft202012::meta::validate(schema)
        .map_err(|e| format!("inputSchema is not valid Draft 2020-12: {e}"))?;
    jsonschema::draft202012::new(schema)
        .map_err(|e| format!("inputSchema could not be compiled: {e}"))
}

pub fn validate_arguments(schema: &Value, arguments: &Value) -> Result<(), String> {
    let validator = validate_input_schema(schema)?;
    let errors: Vec<String> = validator
        .iter_errors(arguments)
        .take(MAX_VALIDATION_ERRORS)
        .map(|error| {
            format!(
                "instance {} violates schema {}",
                error.instance_path(),
                error.schema_path()
            )
        })
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Routine arguments are invalid: {}",
            errors.join("; ")
        ))
    }
}

/// High-confidence credential patterns that should never be persisted in a routine.
/// This is deliberately narrow: it blocks obvious literal credentials, not legitimate reads
/// such as `input.token`, and is not represented as proof that arbitrary JavaScript is clean.
/// Shared with the routine advisor so a synthesized draft never inlines one either.
pub(crate) fn contains_obvious_credentials(value: &str) -> bool {
    static PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    PATTERN
        .get_or_init(|| {
            regex::Regex::new(
                r#"(?i)(?:gh[pousr]_[a-z0-9]{20,}|sk-[a-z0-9_-]{20,}|AKIA[0-9A-Z]{16}|bearer\s+[a-z0-9._~+/=-]{16,}|(?:api[_-]?key|access[_-]?token|authorization|credential|password|private[_-]?key|secret|token)\s*["']?\s*[:=]\s*(?:\\?["'])[^"'\r\n]{12,}(?:\\?["']))"#,
            )
            .expect("routine credential regex is valid")
        })
        .is_match(value)
}

fn reject_credentials_in_text(field: &str, value: &str) -> Result<(), String> {
    if contains_obvious_credentials(value) {
        Err(format!(
            "Routine {field} contains credential-like literal material and cannot be persisted"
        ))
    } else {
        Ok(())
    }
}

fn reject_credentials_in_serializable(field: &str, value: &impl Serialize) -> Result<(), String> {
    let serialized = serde_json::to_string(value).map_err(|e| e.to_string())?;
    reject_credentials_in_text(field, &serialized)?;

    fn visit(field: &str, value: &Value) -> Result<(), String> {
        match value {
            Value::String(value) => reject_credentials_in_text(field, value),
            Value::Array(values) => {
                for value in values {
                    visit(field, value)?;
                }
                Ok(())
            }
            Value::Object(object) => {
                for (key, value) in object {
                    reject_credentials_in_text(field, key)?;
                    visit(field, value)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    let value = serde_json::to_value(value).map_err(|e| e.to_string())?;
    visit(field, &value)
}

fn validate_store(store: &RoutineStore) -> Result<(), String> {
    if store.schema_version != STORE_SCHEMA_VERSION {
        return Err(format!(
            "Unsupported routines schemaVersion {}; expected {STORE_SCHEMA_VERSION}",
            store.schema_version
        ));
    }
    let mut ids = HashSet::new();
    let mut hashes = HashSet::new();
    let mut definitions = HashSet::new();
    for routine in &store.routines {
        routine.verify()?;
        if !ids.insert(routine.id.as_str()) {
            return Err(format!("Duplicate routine id {}", routine.id));
        }
        if !hashes.insert(routine.content_hash.as_str()) {
            return Err(format!(
                "Duplicate routine content hash {}",
                routine.content_hash
            ));
        }
        if !definitions.insert(routine.definition_fingerprint.as_str()) {
            return Err(format!(
                "Duplicate routine definition fingerprint {}",
                routine.definition_fingerprint
            ));
        }
    }
    reject_credentials_in_serializable("store metadata", &store.unknown_fields)
}

fn parse_store(content: &str) -> Result<RoutineStore, String> {
    if content.trim().is_empty() {
        return Err("routines.json is empty or truncated".to_string());
    }
    let store: RoutineStore =
        serde_json::from_str(content).map_err(|e| format!("Corrupt routines.json: {e}"))?;
    validate_store(&store)?;
    Ok(store)
}

fn recover_from_backup(path: &Path) -> Option<RoutineStore> {
    let backup = backup_path(path);
    let content = std::fs::read_to_string(&backup).ok()?;
    let store = parse_store(&content).ok()?;
    registry::atomic_write(path, &content).ok()?;
    Some(store)
}

fn load_from_inner(path: &Path) -> Result<RoutineStore, String> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RoutineStore::default())
        }
        Err(error) => return Err(format!("Could not read routines.json: {error}")),
    };

    match parse_store(&content) {
        Ok(store) => Ok(store),
        Err(error) if error.starts_with("Unsupported routines schemaVersion") => Err(error),
        Err(error) => recover_from_backup(path)
            .ok_or_else(|| format!("{error}; no valid routines.json backup was available")),
    }
}

fn load_from(path: &Path) -> Result<RoutineStore, String> {
    let lock = registry::lock_at(path)?;
    load_from_locked(path, &lock)
}

fn load_from_locked(path: &Path, _lock: &registry::FileLock) -> Result<RoutineStore, String> {
    load_from_inner(path)
}

fn load() -> Result<RoutineStore, String> {
    let path = routines_path().ok_or("Could not resolve routines path")?;
    load_from(&path)
}

fn save_to(path: &Path, store: &RoutineStore) -> Result<(), String> {
    validate_store(store)?;
    let json = serde_json::to_string_pretty(store).map_err(|e| e.to_string())?;
    if let Ok(existing) = std::fs::read_to_string(path) {
        let current = parse_store(&existing)
            .map_err(|e| format!("Refusing to overwrite an unreadable routines.json: {e}"))?;
        if current == *store {
            return Ok(());
        }
        registry::atomic_write(&backup_path(path), &existing)?;
    }
    registry::atomic_write(path, &json)
}

fn sorted_routines(mut store: RoutineStore) -> Vec<RoutineDefinition> {
    store.routines.sort_by(|left, right| {
        right
            .created_at_ms
            .cmp(&left.created_at_ms)
            .then_with(|| left.id.cmp(&right.id))
    });
    store.routines
}

#[cfg(test)]
fn list_from(path: &Path) -> Result<Vec<RoutineDefinition>, String> {
    load_from(path).map(sorted_routines)
}

pub fn list() -> Result<Vec<RoutineDefinition>, String> {
    load().map(sorted_routines)
}

fn get_from(path: &Path, id: &str) -> Result<Option<RoutineDefinition>, String> {
    Ok(load_from(path)?
        .routines
        .into_iter()
        .find(|routine| routine.id == id))
}

pub fn get(id: &str) -> Result<Option<RoutineDefinition>, String> {
    let path = routines_path().ok_or("Could not resolve routines path")?;
    get_from(&path, id)
}

fn find_by_definition_fingerprint_from(
    path: &Path,
    fingerprint: &str,
) -> Result<Option<RoutineDefinition>, String> {
    Ok(load_from(path)?
        .routines
        .into_iter()
        .find(|routine| routine.definition_fingerprint == fingerprint))
}

pub fn find_by_definition_fingerprint(
    fingerprint: &str,
) -> Result<Option<RoutineDefinition>, String> {
    let path = routines_path().ok_or("Could not resolve routines path")?;
    find_by_definition_fingerprint_from(&path, fingerprint)
}

/// Append an immutable definition under the store lock. Equal content is idempotent and
/// returns the already-persisted definition; reusing an id for different content fails closed.
fn append_immutable_at(
    path: &Path,
    definition: RoutineDefinition,
) -> Result<RoutineDefinition, String> {
    definition.verify()?;
    let lock = registry::lock_at(path)?;
    let mut store = load_from_locked(path, &lock)?;
    if let Some(existing) = store
        .routines
        .iter()
        .find(|routine| routine.definition_fingerprint == definition.definition_fingerprint)
    {
        return Ok(existing.clone());
    }
    if store
        .routines
        .iter()
        .any(|routine| routine.id == definition.id)
    {
        return Err("Generated routine id collided with an existing definition".to_string());
    }
    store.routines.push(definition.clone());
    save_to(path, &store)?;
    Ok(definition)
}

pub fn append_immutable(definition: RoutineDefinition) -> Result<RoutineDefinition, String> {
    let path = routines_path().ok_or("Could not resolve routines path")?;
    append_immutable_at(&path, definition)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let mut random = [0u8; 8];
        getrandom::getrandom(&mut random).unwrap();
        let suffix = random
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        std::env::temp_dir()
            .join(format!("toolport-routines-{name}-{suffix}"))
            .join("routines.json")
    }

    fn definition(name: &str) -> RoutineDefinition {
        new_definition(
            name.to_string(),
            Some("test routine".to_string()),
            format!("// {name}\nreturn input.value;"),
            json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "type": "object",
                "properties": { "value": { "type": "string" } },
                "required": ["value"],
                "additionalProperties": false
            }),
        )
        .unwrap()
    }

    #[test]
    fn missing_file_is_an_empty_store() {
        let path = temp_path("missing");
        assert_eq!(load_from(&path).unwrap(), RoutineStore::default());
    }

    #[test]
    fn save_and_reload_preserves_a_definition() {
        let path = temp_path("roundtrip");
        let mut store = RoutineStore::default();
        store.routines.push(definition("one"));
        save_to(&path, &store).unwrap();
        assert_eq!(load_from(&path).unwrap(), store);
    }

    #[test]
    fn append_serializes_concurrent_style_writes_without_losing_records() {
        let path = temp_path("update");
        append_immutable_at(&path, definition("one")).unwrap();
        append_immutable_at(&path, definition("two")).unwrap();
        assert_eq!(load_from(&path).unwrap().routines.len(), 2);
    }

    #[test]
    fn concurrent_updates_do_not_lose_records() {
        // Asserts no record is LOST, not that all eight writers win the lock inside the
        // production budget. On a loaded runner the 5s default expires, one writer
        // correctly gives up, and this fails for the machine's timing (SBS-895).
        let _lock_budget = crate::registry::LockTimeoutOverride::generous();
        let path = temp_path("concurrent");
        let workers: Vec<_> = (0..8)
            .map(|index| {
                let path = path.clone();
                std::thread::spawn(move || {
                    append_immutable_at(&path, definition(&format!("routine-{index}"))).unwrap();
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }

        let store = load_from(&path).unwrap();
        assert_eq!(store.routines.len(), 8);
        let names: HashSet<&str> = store
            .routines
            .iter()
            .map(|routine| routine.name.as_str())
            .collect();
        assert_eq!(names.len(), 8);
    }

    #[test]
    fn corrupt_primary_recovers_only_from_a_valid_backup() {
        let path = temp_path("backup");
        let first = RoutineStore {
            routines: vec![definition("one")],
            ..RoutineStore::default()
        };
        save_to(&path, &first).unwrap();
        let second = RoutineStore {
            routines: vec![definition("two")],
            ..RoutineStore::default()
        };
        save_to(&path, &second).unwrap();
        registry::atomic_write(&path, "{ broken").unwrap();
        assert_eq!(load_from(&path).unwrap(), first);
    }

    #[test]
    fn empty_and_corrupt_files_without_backup_fail_closed() {
        for (name, content) in [("empty", ""), ("corrupt", "{")] {
            let path = temp_path(name);
            registry::atomic_write(&path, content).unwrap();
            assert!(load_from(&path).is_err());
        }
    }

    #[test]
    fn unknown_store_version_fails_closed_without_backup_recovery() {
        let path = temp_path("version");
        for version in [1, 3] {
            registry::atomic_write(
                &path,
                &format!(r#"{{"schemaVersion":{version},"routines":[]}}"#),
            )
            .unwrap();
            let error = load_from(&path).unwrap_err();
            assert!(error.contains(&format!("Unsupported routines schemaVersion {version}")));
        }
    }

    #[test]
    fn content_hash_tampering_is_rejected() {
        let path = temp_path("tamper");
        let mut def = definition("one");
        def.content_hash = "sha256:tampered".to_string();
        let json = serde_json::to_string(&RoutineStore {
            routines: vec![def],
            ..RoutineStore::default()
        })
        .unwrap();
        registry::atomic_write(&path, &json).unwrap();
        assert!(load_from(&path)
            .unwrap_err()
            .contains("content hash mismatch"));
    }

    #[test]
    fn definition_fingerprint_and_promotion_evidence_tampering_are_rejected() {
        let mut fingerprint = definition("fingerprint");
        fingerprint.definition_fingerprint = "sha256:tampered".to_string();
        assert!(fingerprint
            .verify()
            .unwrap_err()
            .contains("definition fingerprint mismatch"));

        let mut evidence = definition("evidence");
        evidence.evidence.validation_version = 99;
        assert!(evidence
            .verify()
            .unwrap_err()
            .contains("Unsupported promotion validationVersion"));
    }

    #[test]
    fn store_v2_persists_dual_hashes_and_real_run_evidence() {
        let routine = definition("v2-fields");
        let serialized = serde_json::to_value(&RoutineStore {
            routines: vec![routine],
            ..RoutineStore::default()
        })
        .unwrap();
        assert_eq!(serialized["schemaVersion"], STORE_SCHEMA_VERSION);
        let saved = &serialized["routines"][0];
        assert!(saved["definitionFingerprint"]
            .as_str()
            .is_some_and(|value| value.starts_with("sha256:")));
        assert!(saved["contentHash"]
            .as_str()
            .is_some_and(|value| value.starts_with("sha256:")));
        assert!(saved["evidence"]["sourceRunId"]
            .as_str()
            .is_some_and(|value| value.starts_with("run_")));
        assert_eq!(saved["evidence"]["calls"], 1);
        assert_eq!(saved["evidence"]["riskClass"], "low");
    }

    #[test]
    fn schema_validation_is_draft_2020_12_and_local_only() {
        assert!(validate_input_schema(&json!({
            "type": "object",
            "$defs": { "value": { "type": "string" } },
            "properties": { "value": { "$ref": "#/$defs/value" } }
        }))
        .is_ok());
        assert!(validate_input_schema(&json!({
            "type": "object",
            "properties": { "value": { "$ref": "https://example.com/schema" } }
        }))
        .unwrap_err()
        .contains("external reference"));
        assert!(validate_input_schema(&json!({ "type": "string" })).is_err());
        assert!(validate_input_schema(&json!({ "type": "not-a-type" })).is_err());
    }

    #[test]
    fn argument_errors_report_paths_without_values() {
        let schema = json!({
            "type": "object",
            "properties": { "secret": { "type": "integer" } },
            "required": ["secret"],
            "additionalProperties": false
        });
        let error = validate_arguments(&schema, &json!({ "secret": "do-not-echo" })).unwrap_err();
        assert!(error.contains("/secret"));
        assert!(!error.contains("do-not-echo"));
    }

    #[test]
    fn argument_errors_are_bounded_and_enforce_object_rules() {
        let properties = (0..20)
            .map(|index| (format!("field{index}"), json!({ "type": "integer" })))
            .collect::<serde_json::Map<_, _>>();
        let schema = json!({
            "type": "object",
            "properties": properties,
            "additionalProperties": false
        });
        let arguments = (0..20)
            .map(|index| (format!("field{index}"), json!("private-value")))
            .chain(std::iter::once(("unexpected".to_string(), json!(true))))
            .collect::<serde_json::Map<_, _>>();

        let error = validate_arguments(&schema, &Value::Object(arguments)).unwrap_err();
        assert_eq!(error.matches("instance ").count(), MAX_VALIDATION_ERRORS);
        assert!(!error.contains("private-value"));
    }

    #[test]
    fn unknown_fields_round_trip_without_affecting_content_identity() {
        let path = temp_path("unknown-fields");
        let mut routine = definition("one");
        routine
            .unknown_fields
            .insert("futureRoutineField".to_string(), json!({ "enabled": true }));
        let mut store = RoutineStore {
            routines: vec![routine],
            ..RoutineStore::default()
        };
        store
            .unknown_fields
            .insert("futureStoreField".to_string(), json!(7));

        save_to(&path, &store).unwrap();
        assert_eq!(load_from(&path).unwrap(), store);
    }

    #[test]
    fn generated_ids_are_random_and_well_formed() {
        let ids: HashSet<String> = (0..64).map(|_| generate_id().unwrap()).collect();
        assert_eq!(ids.len(), 64);
        assert!(ids.iter().all(|id| valid_id(id)));
    }

    #[test]
    fn credential_scan_blocks_literals_but_allows_input_fields() {
        assert!(contains_obvious_credentials(
            r#"const apiKey = "this-is-a-literal-secret";"#
        ));
        assert!(contains_obvious_credentials(
            "const token = 'ghp_abcdefghijklmnopqrstuvwxyz123456';"
        ));
        assert!(contains_obvious_credentials(
            r#"const token = "abcdefghijklmnopqrstuv";"#
        ));
        assert!(contains_obvious_credentials(
            "headers.Authorization = 'Bearer abcdefghijklmnopqrstuvwxyz012345';"
        ));
        assert!(!contains_obvious_credentials(
            "return toolport.call('api', { token: input.token });"
        ));
    }

    #[test]
    fn definition_rejects_credentials_in_description_and_schema() {
        let description_error = new_definition(
            "description-secret".to_string(),
            Some(r#"password = "this-is-a-literal-secret""#.to_string()),
            "return input;".to_string(),
            json!({ "type": "object" }),
        )
        .unwrap_err();
        assert!(description_error.contains("description contains credential-like"));

        let schema_error = new_definition(
            "schema-secret".to_string(),
            None,
            "return input;".to_string(),
            json!({
                "type": "object",
                "apiKey": "this-is-a-literal-secret"
            }),
        )
        .unwrap_err();
        assert!(schema_error.contains("inputSchema contains credential-like"));
    }

    #[test]
    fn append_is_content_idempotent() {
        let path = temp_path("idempotent");
        let first = definition("same");
        let duplicate = definition("same");
        assert_ne!(first.id, duplicate.id);
        assert_eq!(
            first.definition_fingerprint,
            duplicate.definition_fingerprint
        );

        let saved_first = append_immutable_at(&path, first.clone()).unwrap();
        let saved_duplicate = append_immutable_at(&path, duplicate).unwrap();

        assert_eq!(saved_duplicate.id, saved_first.id);
        assert_eq!(list_from(&path).unwrap(), vec![first]);
    }

    #[test]
    fn persisted_definition_never_contains_validation_arguments() {
        let routine = definition("no-validation-arguments");
        let serialized = serde_json::to_value(&routine).unwrap();
        assert!(serialized.get("validationArguments").is_none());
    }

    #[test]
    fn append_rejects_id_reuse_without_mutating_the_store() {
        let path = temp_path("immutable");
        let first = definition("one");
        append_immutable_at(&path, first.clone()).unwrap();

        let mut colliding = definition("two");
        colliding.id = first.id.clone();
        let error = append_immutable_at(&path, colliding).unwrap_err();
        assert!(error.contains("id collided"));
        assert_eq!(list_from(&path).unwrap(), vec![first]);
    }

    #[test]
    fn get_returns_only_the_requested_immutable_definition() {
        let path = temp_path("get");
        let first = definition("one");
        let second = definition("two");
        append_immutable_at(&path, first.clone()).unwrap();
        append_immutable_at(&path, second).unwrap();

        assert_eq!(get_from(&path, &first.id).unwrap(), Some(first));
        assert_eq!(get_from(&path, "routine_missing").unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn saved_store_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let path = temp_path("permissions");
        save_to(&path, &RoutineStore::default()).unwrap();
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
