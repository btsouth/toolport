//! Compact call failure categories shared by dispatch, guidance and activity.
//! Classify codes and schema fields, never prose supplied by a downstream server.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthTarget {
    Endpoint,
    ServiceCredential,
    Scope,
    OAuthRefresh,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CallFailureKind {
    InvalidInput {
        missing: Vec<String>,
        invalid: Vec<String>,
    },
    NotFound,
    Auth {
        target: AuthTarget,
    },
    Quota,
    Conflict,
    Timeout {
        after_send: bool,
    },
    Unavailable {
        after_send: bool,
    },
    Cancelled,
    Internal,
}

#[derive(Debug, Clone)]
pub struct CallFailure {
    pub kind: CallFailureKind,
    /// Untrusted detail. Must pass through content defense before display.
    pub detail: String,
}

impl CallFailure {
    pub fn new(kind: CallFailureKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}
impl std::fmt::Display for CallFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}
impl From<String> for CallFailure {
    fn from(detail: String) -> Self {
        Self::new(CallFailureKind::Internal, detail)
    }
}

impl CallFailureKind {
    pub fn http_status(status: u16, endpoint: bool) -> Self {
        match status {
            400 | 422 => Self::InvalidInput {
                missing: vec![],
                invalid: vec![],
            },
            401 => Self::Auth {
                target: if endpoint {
                    AuthTarget::Endpoint
                } else {
                    AuthTarget::ServiceCredential
                },
            },
            403 => Self::Auth {
                target: if endpoint {
                    AuthTarget::Endpoint
                } else {
                    AuthTarget::Scope
                },
            },
            404 => Self::NotFound,
            402 | 429 => Self::Quota,
            409 | 412 => Self::Conflict,
            408 | 504 => Self::Timeout { after_send: true },
            500..=599 => Self::Unavailable { after_send: true },
            _ => Self::Internal,
        }
    }

    pub fn rpc(error: &Value) -> Self {
        match error.get("code").and_then(Value::as_i64) {
            Some(-32602) => Self::InvalidInput {
                missing: vec![],
                invalid: vec![],
            },
            Some(-32601) => Self::NotFound,
            Some(400..=599) => Self::http_status(error["code"].as_u64().unwrap() as u16, false),
            _ => Self::Internal,
        }
    }

    /// Only structured status/code data is recognized. Text stays untrusted,
    /// including text that happens to contain JSON or words such as "retry".
    pub fn tool_result(result: &Value) -> Self {
        let Some(data) = result.get("structuredContent") else {
            return Self::Internal;
        };
        if let Some(status) = data
            .get("status")
            .and_then(Value::as_u64)
            .filter(|n| (400..=599).contains(n))
        {
            return Self::http_status(status as u16, false);
        }
        // Known provider error codes. Do not copy messages or actions into guidance.
        let code = data.pointer("/error/code").and_then(Value::as_str);
        match code {
            Some("invalid_api_key" | "authentication_error" | "UNAUTHENTICATED") => Self::Auth {
                target: AuthTarget::ServiceCredential,
            },
            Some("insufficient_scope" | "insufficient_permissions" | "FORBIDDEN") => Self::Auth {
                target: AuthTarget::Scope,
            },
            Some(
                "rate_limit_exceeded"
                | "quota_exceeded"
                | "insufficient_quota"
                | "plan_limit_exceeded",
            ) => Self::Quota,
            Some("resource_missing" | "not_found" | "NOT_FOUND") => Self::NotFound,
            Some("conflict" | "CONFLICT") => Self::Conflict,
            _ => Self::Internal,
        }
    }

    pub fn is_health_failure(&self) -> bool {
        matches!(self, Self::Timeout { .. } | Self::Unavailable { .. })
    }
    pub fn uncertain(&self) -> bool {
        matches!(
            self,
            Self::Timeout { after_send: true } | Self::Unavailable { after_send: true }
        )
    }

    /// Enrich input errors from the published schema, without compiling a new
    /// validator per call. Conditional/ref validation remains the server's job.
    pub fn with_schema(self, schema: &Value, arguments: &Value) -> Self {
        let Self::InvalidInput {
            mut missing,
            mut invalid,
        } = self
        else {
            return self;
        };
        collect_fields(schema, arguments, "", 0, &mut missing, &mut invalid);
        missing.sort();
        missing.dedup();
        missing.truncate(6);
        invalid.sort();
        invalid.dedup();
        invalid.truncate(6);
        let mut budget = 128usize;
        for fields in [&mut missing, &mut invalid] {
            fields.retain(|field| { if field.len() + 2 <= budget { budget -= field.len() + 2; true } else { false } });
        }
        Self::InvalidInput { missing, invalid }
    }

    pub fn identifier_failure(&self) -> bool {
        match self {
            Self::NotFound => true,
            Self::InvalidInput { missing, invalid } => missing.iter().chain(invalid).any(|field| {
                let field = field
                    .rsplit('.')
                    .next()
                    .unwrap_or(field)
                    .to_ascii_lowercase();
                field == "id"
                    || field.ends_with("_id")
                    || field.ends_with("id")
                    || field.ends_with("_slug")
            }),
            _ => false,
        }
    }

    /// Fixed text plus bounded schema field names. No server prose is an instruction.
    pub fn guidance(&self, read_only: bool) -> String {
        let text = match self {
            Self::InvalidInput { missing, invalid } => {
                let mut text = "Check tool input.".to_string();
                if !missing.is_empty() {
                    text.push_str(&format!(" Missing: {}.", missing.join(", ")));
                }
                if !invalid.is_empty() {
                    text.push_str(&format!(" Invalid: {}.", invalid.join(", ")));
                }
                return text;
            }
            Self::NotFound => "Resource or tool not found. Check its identifier.",
            Self::Auth {
                target: AuthTarget::Endpoint,
            } => "Toolport cannot authenticate the MCP endpoint. Check its connection auth.",
            Self::Auth {
                target: AuthTarget::ServiceCredential,
            } => "The service rejected its API credential. Check the service key.",
            Self::Auth {
                target: AuthTarget::Scope,
            } => "The service denied access. Check credential scopes and permissions.",
            Self::Auth {
                target: AuthTarget::OAuthRefresh,
            } => "MCP OAuth refresh failed. Reconnect in Toolport.",
            Self::Quota => "Quota or rate limit reached. Check limits before retrying.",
            Self::Conflict => "State conflict. Check current state before retrying.",
            Self::Timeout { after_send: false } => {
                "Timed out before send. Retry when the endpoint is reachable."
            }
            Self::Timeout { after_send: true } if !read_only => {
                "Timed out after send; may have completed, check before retrying."
            }
            Self::Timeout { .. } => "Timed out waiting for the endpoint. Retry the read later.",
            Self::Unavailable { after_send: true } if !read_only => {
                "Endpoint connection failed after send; may have completed, check before retrying."
            }
            Self::Unavailable { .. } => {
                "Toolport cannot reach the MCP endpoint. Check its connection."
            }
            Self::Cancelled => "Call cancelled. Check state before repeating a write.",
            Self::Internal => "Call failed. Check the error details.",
        };
        text.to_string()
    }
}

fn collect_fields(
    schema: &Value,
    args: &Value,
    prefix: &str,
    depth: usize,
    missing: &mut Vec<String>,
    invalid: &mut Vec<String>,
) {
    if depth > 4 || missing.len() + invalid.len() >= 12 {
        return;
    }
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return;
    };
    for (name, child) in properties {
        // Never let schema text inject instructions or exhaust the error budget.
        if name.is_empty()
            || name.len() > 40
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.-'".contains(&b))
        {
            continue;
        }
        let path = format!("{prefix}{name}");
        if path.len() > 48 { continue; }
        let value = args.get(name);
        if value.is_none()
            && schema
                .get("required")
                .and_then(Value::as_array)
                .is_some_and(|r| r.iter().any(|v| v.as_str() == Some(name)))
        {
            missing.push(path);
        } else if let Some(value) = value {
            let wrong_type = match child.get("type").and_then(Value::as_str) {
                Some("string") => !value.is_string(),
                Some("object") => !value.is_object(),
                Some("array") => !value.is_array(),
                Some("boolean") => !value.is_boolean(),
                Some("integer") => !(value.is_i64() || value.is_u64()),
                Some("number") => !value.is_number(),
                Some("null") => !value.is_null(),
                _ => false,
            };
            let wrong_enum = child
                .get("enum")
                .and_then(Value::as_array)
                .is_some_and(|values| !values.contains(value));
            if wrong_type || wrong_enum {
                invalid.push(path);
            } else if value.is_object() {
                collect_fields(
                    child,
                    value,
                    &format!("{path}."),
                    depth + 1,
                    missing,
                    invalid,
                );
            }
        }
        if missing.len() + invalid.len() >= 12 {
            break;
        }
    }
}
