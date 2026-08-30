#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RoutineApprovalSummary {
    pub(super) name: String,
    pub(super) description: Option<String>,
    pub(super) risk: String,
    pub(super) calls: usize,
    pub(super) dependencies: Vec<String>,
}

pub(super) fn routine_approval_summary(
    arguments: &serde_json::Value,
) -> Option<RoutineApprovalSummary> {
    let name = arguments.get("name")?.as_str()?.trim();
    if name.is_empty() {
        return None;
    }
    let evidence = arguments.get("evidence");
    let planned_tools = arguments
        .pointer("/validation/plannedTools")
        .and_then(serde_json::Value::as_array);
    let dependencies = evidence
        .and_then(|value| value.get("observedDependencies"))
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.get("name")?.as_str())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|values| !values.is_empty())
        .or_else(|| {
            planned_tools.map(|values| {
                values
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
        })
        .unwrap_or_default();
    let calls = evidence
        .and_then(|value| value.get("calls"))
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(dependencies.len());
    let risk = arguments
        .get("riskClass")
        .or_else(|| evidence.and_then(|value| value.get("riskClass")))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown")
        .replace('_', " ");
    let description = arguments
        .get("description")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    Some(RoutineApprovalSummary {
        name: name.to_string(),
        description,
        risk,
        calls,
        dependencies,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarizes_promoted_routine_evidence() {
        let summary = routine_approval_summary(&serde_json::json!({
            "name": "release-notes",
            "description": "Draft and publish release notes",
            "riskClass": "high",
            "evidence": {
                "calls": 3,
                "observedDependencies": [
                    {"name": "github/get_release"},
                    {"name": "github/update_release"}
                ]
            }
        }))
        .unwrap();

        assert_eq!(summary.name, "release-notes");
        assert_eq!(summary.risk, "high");
        assert_eq!(summary.calls, 3);
        assert_eq!(
            summary.dependencies,
            ["github/get_release", "github/update_release"]
        );
    }

    #[test]
    fn summarizes_direct_save_validation_when_evidence_is_absent() {
        let summary = routine_approval_summary(&serde_json::json!({
            "name": "triage",
            "validation": {
                "finished": true,
                "plannedTools": ["linear/search", "linear/update"]
            }
        }))
        .unwrap();

        assert_eq!(summary.risk, "unknown");
        assert_eq!(summary.calls, 2);
        assert_eq!(summary.dependencies, ["linear/search", "linear/update"]);
    }

    #[test]
    fn refuses_non_routine_payloads_without_a_name() {
        assert!(routine_approval_summary(&serde_json::json!({"path": "/tmp"})).is_none());
    }
}
