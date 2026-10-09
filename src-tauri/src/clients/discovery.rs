//! Evidence-backed discovery defaults and bounded cold Full catalog waits.
use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryCapabilities {
    pub auto_mode: &'static str,
    pub native_tool_search: Option<bool>,
    pub tools_list_changed: Option<bool>,
    pub cold_full_list_wait_ms: u64,
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
        self.auto_mode
    }
}

pub(super) fn capabilities(id: &str) -> DiscoveryCapabilities {
    // Sources and conservative unknowns for every supported adapter are recorded
    // in docs/client-discovery.md. API hosts are not file-based client adapters.
    match id {
        "claude-code" => DiscoveryCapabilities {
            auto_mode: "lazy",
            native_tool_search: Some(true),
            tools_list_changed: Some(true),
            cold_full_list_wait_ms: 2_000,
            evidence: "https://code.claude.com/docs/en/mcp",
        },
        "codex" => DiscoveryCapabilities {
            auto_mode: "full",
            native_tool_search: Some(true),
            tools_list_changed: Some(false),
            cold_full_list_wait_ms: 8_000,
            evidence: "https://developers.openai.com/codex/config-reference",
        },
        "cursor" => DiscoveryCapabilities {
            auto_mode: "full",
            native_tool_search: Some(true),
            tools_list_changed: Some(false),
            cold_full_list_wait_ms: 5_000,
            evidence: "https://cursor.com/blog/dynamic-context-discovery",
        },
        "opencode" | "gemini-cli" | "cline" | "zed" => DiscoveryCapabilities {
            auto_mode: "lazy",
            native_tool_search: None,
            tools_list_changed: Some(true),
            // Keep the conservative budget: handler registration alone does not
            // establish how quickly every installed version refreshes its catalog.
            cold_full_list_wait_ms: 5_000,
            evidence: "docs/client-conformance.md (source and notification evidence)",
        },
        "anthropic-api" => DiscoveryCapabilities {
            auto_mode: "full",
            native_tool_search: Some(true),
            tools_list_changed: None,
            cold_full_list_wait_ms: 5_000,
            evidence:
                "https://platform.claude.com/docs/en/agents-and-tools/tool-use/tool-search-tool",
        },
        "openai-api" => DiscoveryCapabilities {
            auto_mode: "full",
            native_tool_search: Some(true),
            tools_list_changed: None,
            cold_full_list_wait_ms: 5_000,
            evidence: "https://developers.openai.com/api/docs/guides/tools-tool-search",
        },
        "lm-studio" | "jan" | "anythingllm" => DiscoveryCapabilities {
            auto_mode: "lazy",
            native_tool_search: None,
            tools_list_changed: None,
            cold_full_list_wait_ms: 5_000,
            evidence: "docs/clients.md (local-model clients)",
        },
        _ => DiscoveryCapabilities {
            auto_mode: "lazy",
            native_tool_search: None,
            tools_list_changed: None,
            cold_full_list_wait_ms: 5_000,
            evidence: "docs/clients.md (adapter notes; discovery support unverified)",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn verified_refresh_does_not_imply_native_search_or_a_shorter_startup_budget() {
        for id in ["opencode", "gemini-cli", "cline", "zed"] {
            let caps = capabilities(id);
            assert_eq!(caps.tools_list_changed, Some(true), "{id}");
            assert_eq!(caps.native_tool_search, None, "{id}");
            assert_eq!(caps.auto_mode(), "lazy", "{id}");
            assert_eq!(caps.cold_full_list_wait_ms, 5_000, "{id}");
        }
        assert_eq!(capabilities("codex").tools_list_changed, Some(false));
        assert_eq!(capabilities("cursor").tools_list_changed, None);
    }
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
            "lazy"
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
            "lazy"
        );
    }

    #[test]
    fn adapter_capabilities_do_not_rewrite_registry_choices_or_identity() {
        let mut registry = crate::registry::Registry::default();
        registry.set_client_discovery("claude-code", Some("full"));
        let before = serde_json::to_value(&registry).unwrap();
        for id in ["codex", "client:codex", "adapter:codex"] {
            let caps = super::super::discovery_capabilities(id);
            assert_eq!(caps.cold_full_list_wait_ms, 8_000, "{id}");
            assert_eq!(caps.tools_list_changed, Some(false), "{id}");
        }
        assert_eq!(
            super::super::client_discovery_mode(&registry, "claude-code"),
            "full"
        );
        registry.set_client_discovery("claude-code", None);
        assert_eq!(
            super::super::client_discovery_mode(&registry, "claude-code"),
            "lazy"
        );
        registry.set_client_discovery("claude-code", Some("full"));
        assert_eq!(serde_json::to_value(registry).unwrap(), before);
    }

    #[test]
    fn auto_uses_measured_defaults_with_per_client_cold_budgets() {
        for (id, budget) in [
            ("codex", 8_000),
            ("cursor", 5_000),
            ("anthropic-api", 5_000),
            ("openai-api", 5_000),
        ] {
            assert_eq!(capabilities(id).auto_mode(), "full", "{id}");
            assert_eq!(capabilities(id).cold_full_list_wait_ms, budget, "{id}");
        }
        assert_eq!(capabilities("claude-code").cold_full_list_wait_ms, 2_000);
        for id in [
            "claude-code",
            "opencode",
            "lm-studio",
            "jan",
            "anythingllm",
            "unknown",
        ] {
            assert_eq!(capabilities(id).auto_mode(), "lazy", "{id}");
        }
        assert_eq!(
            DiscoveryCapabilities {
                auto_mode: "lazy",
                native_tool_search: None,
                tools_list_changed: Some(true),
                cold_full_list_wait_ms: 2_000,
                evidence: "fixture"
            }
            .auto_mode(),
            "lazy"
        );
    }
}
