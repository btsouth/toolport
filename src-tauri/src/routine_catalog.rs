//! Agent-facing Routine Catalog with lexical search and dependency freshness checks.
//!
//! The catalog never exposes executable source. Gateway supplies one narrow dependency
//! resolver adapter for the current caller; ranking and availability stay local here.

use std::cmp::Ordering;

use serde::Serialize;
use serde_json::Value;

use crate::routines::{RoutineDefinition, RoutineRiskClass};

pub const DEFAULT_LIMIT: usize = 8;
pub const MAX_LIMIT: usize = 50;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    Ready,
    Stale,
    Unavailable,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DependencyStatus {
    Available,
    Stale,
    Unavailable,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DependencyPresence {
    Available,
    Unavailable,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FingerprintStatus {
    Current,
    Stale,
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogDependency {
    pub name: String,
    pub status: DependencyPresence,
    pub fingerprint_status: FingerprintStatus,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchSummary {
    pub score: f64,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutineCatalogEntry {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Value,
    pub definition_fingerprint: String,
    pub content_hash: String,
    pub observed_dependencies: Vec<CatalogDependency>,
    pub availability: Availability,
    pub risk_class: RoutineRiskClass,
    pub created_at_ms: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#match: Option<MatchSummary>,
}

pub fn query(
    routines: Vec<RoutineDefinition>,
    query: Option<&str>,
    limit: usize,
    mut resolve: impl FnMut(&str, Option<&str>) -> DependencyStatus,
) -> Vec<RoutineCatalogEntry> {
    let normalized_query = query.map(str::trim).filter(|query| !query.is_empty());
    let mut entries = routines
        .into_iter()
        .filter_map(|routine| {
            let match_summary = normalized_query.map(|query| score(&routine, query));
            if match_summary
                .as_ref()
                .is_some_and(|summary| summary.score <= 0.0)
            {
                return None;
            }
            let observed_dependencies = routine
                .evidence()
                .observed_dependencies()
                .iter()
                .map(|dependency| {
                    let status = resolve(dependency.name(), dependency.tool_fingerprint());
                    let (presence, fingerprint_status) = match status {
                        DependencyStatus::Available => {
                            (DependencyPresence::Available, FingerprintStatus::Current)
                        }
                        DependencyStatus::Stale => {
                            (DependencyPresence::Available, FingerprintStatus::Stale)
                        }
                        DependencyStatus::Unavailable => {
                            (DependencyPresence::Unavailable, FingerprintStatus::Unknown)
                        }
                    };
                    CatalogDependency {
                        name: dependency.name().to_string(),
                        status: presence,
                        fingerprint_status,
                    }
                })
                .collect::<Vec<_>>();
            let availability = if observed_dependencies
                .iter()
                .any(|dependency| dependency.status == DependencyPresence::Unavailable)
            {
                Availability::Unavailable
            } else if observed_dependencies
                .iter()
                .any(|dependency| dependency.fingerprint_status == FingerprintStatus::Stale)
            {
                Availability::Stale
            } else {
                Availability::Ready
            };
            Some(RoutineCatalogEntry {
                id: routine.id().to_string(),
                name: routine.name().to_string(),
                description: routine.description().map(str::to_string),
                input_schema: routine.input_schema().clone(),
                definition_fingerprint: routine.definition_fingerprint().to_string(),
                content_hash: routine.content_hash().to_string(),
                observed_dependencies,
                availability,
                risk_class: routine.evidence().risk_class(),
                created_at_ms: routine.created_at_ms(),
                r#match: match_summary,
            })
        })
        .collect::<Vec<_>>();

    entries.sort_by(|left, right| {
        availability_rank(left.availability)
            .cmp(&availability_rank(right.availability))
            .then_with(|| {
                let left_score = left.r#match.as_ref().map(|item| item.score).unwrap_or(0.0);
                let right_score = right.r#match.as_ref().map(|item| item.score).unwrap_or(0.0);
                right_score
                    .partial_cmp(&left_score)
                    .unwrap_or(Ordering::Equal)
            })
            .then_with(|| right.created_at_ms.cmp(&left.created_at_ms))
            .then_with(|| left.id.cmp(&right.id))
    });
    entries.truncate(limit.clamp(1, MAX_LIMIT));
    entries
}

fn score(routine: &RoutineDefinition, query: &str) -> MatchSummary {
    let query = query.to_lowercase();
    let name = routine.name().to_lowercase();
    let description = routine.description().unwrap_or("").to_lowercase();
    let schema = serde_json::to_string(routine.input_schema())
        .unwrap_or_default()
        .to_lowercase();
    let mut score = 0.0;
    let mut reasons = Vec::new();
    if name.contains(&query) {
        score += 1.0;
        reasons.push("name".to_string());
    }
    if description.contains(&query) {
        score += 0.8;
        reasons.push("description".to_string());
    }
    let terms = query
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .collect::<Vec<_>>();
    if !terms.is_empty() {
        let name_hits = terms.iter().filter(|term| name.contains(**term)).count();
        let description_hits = terms
            .iter()
            .filter(|term| description.contains(**term))
            .count();
        let schema_hits = terms.iter().filter(|term| schema.contains(**term)).count();
        if name_hits > 0 && !reasons.iter().any(|reason| reason == "name") {
            reasons.push("name".to_string());
        }
        if description_hits > 0 && !reasons.iter().any(|reason| reason == "description") {
            reasons.push("description".to_string());
        }
        if schema_hits > 0 {
            reasons.push("input-schema".to_string());
        }
        score +=
            (name_hits as f64 * 0.6 + description_hits as f64 * 0.4 + schema_hits as f64 * 0.2)
                / terms.len() as f64;
    }
    MatchSummary { score, reasons }
}

fn availability_rank(availability: Availability) -> u8 {
    match availability {
        Availability::Ready => 0,
        Availability::Stale => 1,
        Availability::Unavailable => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routines;
    use serde_json::json;

    #[test]
    fn query_ranks_ready_matches_without_source() {
        let routine = routines::new_definition(
            "github-bugs".to_string(),
            Some("Summarize overdue GitHub bugs".to_string()),
            "return input;".to_string(),
            json!({ "type": "object", "properties": { "repo": { "type": "string" } } }),
        )
        .unwrap();
        let entries = query(vec![routine], Some("overdue GitHub"), 8, |_name, _fp| {
            DependencyStatus::Available
        });
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].availability, Availability::Ready);
        assert!(entries[0].r#match.as_ref().unwrap().score > 0.0);
        assert!(serde_json::to_value(&entries[0])
            .unwrap()
            .get("source")
            .is_none());
    }

    #[test]
    fn stale_and_unavailable_dependencies_are_ordered_after_ready() {
        let first = routines::new_definition(
            "first".to_string(),
            None,
            "return input;".to_string(),
            json!({ "type": "object" }),
        )
        .unwrap();
        let second = routines::new_definition(
            "second".to_string(),
            None,
            "return input;".to_string(),
            json!({ "type": "object" }),
        )
        .unwrap();
        let mut calls = 0;
        let entries = query(vec![first, second], None, 8, |_name, _fp| {
            calls += 1;
            if calls == 1 {
                DependencyStatus::Stale
            } else {
                DependencyStatus::Unavailable
            }
        });
        assert_eq!(entries[0].availability, Availability::Stale);
        assert_eq!(entries[1].availability, Availability::Unavailable);
    }
}
