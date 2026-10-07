//! Evidence-backed discovery defaults. Unknown capabilities stay lazy: a cold
//! catalog returns in two seconds and late tools require list_changed refreshes.
use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryCapabilities {
    pub native_tool_search: Option<bool>,
    pub tools_list_changed: Option<bool>,
    pub evidence: &'static str,
}

impl DiscoveryCapabilities {
    pub fn resolve_mode(self, mode: Option<&str>) -> &'static str {
        match mode.map(str::trim) {
            Some(mode) if mode.eq_ignore_ascii_case("full") => "full",
            Some(mode) if mode.eq_ignore_ascii_case("lazy") => "lazy",
            Some(mode) if mode.eq_ignore_ascii_case("grouped") => "grouped",
            _ => self.auto_mode(),
        }
    }

    pub fn auto_mode(self) -> &'static str {
        if self.native_tool_search == Some(true) && self.tools_list_changed == Some(true) {
            "full"
        } else {
            "lazy"
        }
    }
}

pub(super) fn capabilities(id: &str) -> DiscoveryCapabilities {
    // Sources and conservative unknowns for every supported adapter are recorded
    // in docs/client-discovery.md. API hosts are not file-based client adapters.
    match id {
        "claude-code" => DiscoveryCapabilities {
            native_tool_search: Some(true),
            tools_list_changed: Some(true),
            evidence: "https://code.claude.com/docs/en/mcp",
        },
        "codex" => DiscoveryCapabilities {
            native_tool_search: Some(true),
            tools_list_changed: None,
            evidence: "https://developers.openai.com/codex/config-reference",
        },
        "cursor" => DiscoveryCapabilities {
            native_tool_search: Some(true),
            tools_list_changed: None,
            evidence: "https://cursor.com/blog/dynamic-context-discovery",
        },
        "anthropic-api" => DiscoveryCapabilities {
            native_tool_search: Some(true),
            tools_list_changed: None,
            evidence:
                "https://platform.claude.com/docs/en/agents-and-tools/tool-use/tool-search-tool",
        },
        "openai-api" => DiscoveryCapabilities {
            native_tool_search: Some(true),
            tools_list_changed: None,
            evidence: "https://developers.openai.com/api/docs/guides/tools-tool-search",
        },
        "lm-studio" | "jan" | "anythingllm" => DiscoveryCapabilities {
            native_tool_search: None,
            tools_list_changed: None,
            evidence: "docs/clients.md (local-model clients)",
        },
        _ => DiscoveryCapabilities {
            native_tool_search: None,
            tools_list_changed: None,
            evidence: "docs/clients.md (adapter notes; discovery support unverified)",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_definition_carries_capability_evidence_and_preserves_overrides() {
        let mut registry = crate::registry::Registry::default();
        registry.discovery_mode = Some("full".into());
        for definition in super::super::defs() {
            assert!(
                !definition.discovery.evidence.is_empty(),
                "{}",
                definition.id
            );
            assert_eq!(
                super::super::client_discovery_mode(&registry, definition.id),
                definition.discovery.auto_mode()
            );
            for mode in ["full", "lazy", "grouped"] {
                registry.set_client_discovery(definition.id, Some(mode));
                let bytes = serde_json::to_vec(&registry).unwrap();
                let restored = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(
                    super::super::client_discovery_mode(&restored, definition.id),
                    mode
                );
                assert_eq!(restored.version, 3);
            }
            registry.set_client_discovery(definition.id, None);
            assert_eq!(
                super::super::client_discovery_mode(&registry, definition.id),
                definition.discovery.auto_mode()
            );
        }
        assert_eq!(
            super::super::discovery_capabilities("client:claude-code").auto_mode(),
            "full"
        );
    }

    #[test]
    fn http_adapter_identity_and_legacy_spelling_keep_explicit_choices() {
        let mut registry = crate::registry::Registry::default();
        registry
            .client_discovery
            .insert("claude-code".into(), " GROUPED ".into());
        assert_eq!(
            super::super::client_discovery_mode(&registry, "claude-code"),
            "grouped"
        );
        assert_eq!(
            super::super::client_discovery_mode(&registry, "client:claude-code"),
            "grouped"
        );
        assert_eq!(registry.client_discovery["claude-code"], " GROUPED ");
        registry.set_client_discovery("client:claude-code", Some("lazy"));
        assert_eq!(
            super::super::client_discovery_mode(&registry, "client:claude-code"),
            "lazy"
        );
        registry.set_client_discovery("client:claude-code", None);
        registry.set_client_discovery("claude-code", None);
        assert_eq!(
            super::super::client_discovery_mode(&registry, "client:claude-code"),
            "full"
        );
    }

    #[test]
    fn auto_requires_search_and_late_catalog_refresh() {
        assert_eq!(capabilities("claude-code").auto_mode(), "full");
        for id in [
            "codex",
            "cursor",
            "anthropic-api",
            "openai-api",
            "lm-studio",
            "jan",
            "anythingllm",
            "unknown",
        ] {
            assert_eq!(capabilities(id).auto_mode(), "lazy", "{id}");
        }
        assert_eq!(
            DiscoveryCapabilities {
                native_tool_search: None,
                tools_list_changed: Some(true),
                evidence: "fixture"
            }
            .auto_mode(),
            "lazy"
        );
    }
}
