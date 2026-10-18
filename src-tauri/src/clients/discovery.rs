//! Evidence-backed discovery defaults and bounded cold Full catalog waits.
use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryCapabilities {
    pub auto_mode: &'static str,
    pub native_tool_search: Option<bool>,
    pub tools_list_changed: Option<bool>,
    pub cold_full_list_wait_ms: u64,
    /// Most tools this client accepts from one server. Above it the client drops
    /// the server's whole list, so Full answers with the search tools instead.
    pub max_tools: Option<usize>,
    /// Auto answers Full instead of Lazy when the client's scoped catalog has at
    /// most this many tools: below it, listing every tool costs less than searching.
    pub auto_full_max_tools: Option<usize>,
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

    /// Auto's small-catalog Full limit, or `None` when a chosen mode decides instead.
    pub fn auto_full_max_tools_for(self, chosen: Option<&str>) -> Option<usize> {
        let chosen = chosen.is_some_and(|mode| {
            matches!(
                mode.trim().to_ascii_lowercase().as_str(),
                "full" | "lazy" | "grouped"
            )
        });
        if chosen {
            None
        } else {
            self.auto_full_max_tools
        }
    }
}

/// Full gives Claude Code the same list it gets without Toolport, and its own tool
/// search defers it. Lazy wins only for large catalogs: one-call tasks (2026-10-15,
/// 30 tasks per cell) crossed near 130 tools, but multi-step REA tasks (2026-10-18,
/// 10 per cell) had Lazy +33% to +46% at 207 and 375 tools, +3% at 707, -22% at 1,207.
const CLAUDE_CODE_AUTO_FULL_MAX_TOOLS: usize = 800;

pub(super) fn capabilities(id: &str) -> DiscoveryCapabilities {
    // Sources and conservative unknowns for every supported adapter are recorded
    // in docs/client-discovery.md. API hosts are not file-based client adapters.
    match id {
        "claude-code" => DiscoveryCapabilities {
            auto_mode: "lazy",
            native_tool_search: Some(true),
            tools_list_changed: Some(true),
            cold_full_list_wait_ms: 2_000,
            max_tools: None,
            auto_full_max_tools: Some(CLAUDE_CODE_AUTO_FULL_MAX_TOOLS),
            evidence: "https://code.claude.com/docs/en/mcp",
        },
        "codex" => DiscoveryCapabilities {
            auto_mode: "full",
            native_tool_search: Some(true),
            tools_list_changed: Some(false),
            cold_full_list_wait_ms: 8_000,
            // Codex 0.162.1 shows no tools from a server that lists 2,049 or more.
            max_tools: Some(2_048),
            auto_full_max_tools: None,
            evidence: "https://developers.openai.com/codex/config-reference",
        },
        "cursor" => DiscoveryCapabilities {
            auto_mode: "full",
            native_tool_search: Some(true),
            tools_list_changed: Some(false),
            cold_full_list_wait_ms: 5_000,
            max_tools: None,
            auto_full_max_tools: None,
            evidence: "https://cursor.com/blog/dynamic-context-discovery",
        },
        "opencode" | "gemini-cli" | "cline" | "zed" => DiscoveryCapabilities {
            auto_mode: "lazy",
            native_tool_search: None,
            tools_list_changed: Some(true),
            // Keep the conservative budget: handler registration alone does not
            // establish how quickly every installed version refreshes its catalog.
            cold_full_list_wait_ms: 5_000,
            max_tools: None,
            auto_full_max_tools: None,
            evidence: "docs/client-conformance.md (source and notification evidence)",
        },
        // dsh 0.2.0-rc.2 against toolport-gateway 2.2.0 (2026-10-18): it probes
        // `server/discover`, subscribes with `subscriptions/listen`, and lists
        // tools again after every `notifications/tools/list_changed`.
        "deepseek-harness" => DiscoveryCapabilities {
            auto_mode: "lazy",
            native_tool_search: None,
            tools_list_changed: Some(true),
            cold_full_list_wait_ms: 5_000,
            max_tools: None,
            auto_full_max_tools: None,
            evidence: "docs/client-conformance.md (captured startup and refresh)",
        },
        // Reasonix 2.44.0: an MCP server's tools are deferred behind its own
        // use_capability search unless `load = "always"` (config/mcp_load.go).
        // Against toolport-gateway 2.2.0 (2026-10-18) it speaks 2026-07-28 through
        // `server/discover` and never opens `subscriptions/listen`, so the gateway
        // has no channel for list_changed and the catalog stays as first listed.
        "reasonix" => DiscoveryCapabilities {
            auto_mode: "full",
            native_tool_search: Some(true),
            tools_list_changed: Some(false),
            cold_full_list_wait_ms: 5_000,
            max_tools: None,
            auto_full_max_tools: None,
            evidence: "docs/client-conformance.md (captured startup)",
        },
        // Codewhale 0.10.1 against toolport-gateway 2.2.0 (2026-10-18): MCP tools go
        // into a deferred catalogue the model reaches through `tool_search`, and
        // the client has no tools/list_changed handler (crates/tui/src/mcp.rs).
        "codewhale" => DiscoveryCapabilities {
            auto_mode: "full",
            native_tool_search: Some(true),
            tools_list_changed: Some(false),
            cold_full_list_wait_ms: 5_000,
            max_tools: None,
            auto_full_max_tools: None,
            evidence: "docs/client-conformance.md (captured startup)",
        },
        // OpenClaw 2026.10.4 against toolport-gateway 2.2.0 (2026-10-18): embedded
        // runs defer MCP schemas behind its structured Tool Search by default, and
        // the bundle MCP client turns tools/list_changed into a catalog refresh
        // on the next turn (agents/agent-bundle-mcp-runtime.js).
        "openclaw" => DiscoveryCapabilities {
            auto_mode: "full",
            native_tool_search: Some(true),
            tools_list_changed: Some(true),
            cold_full_list_wait_ms: 2_000,
            max_tools: None,
            auto_full_max_tools: None,
            evidence: "docs/client-conformance.md (captured startup)",
        },
        // OpenHands CLI 1.16.0 (SDK 1.21.0) against toolport-gateway 2.2.0
        // (2026-10-18): it speaks 2026-07-28 through `server/discover`, lists tools
        // once and opens no `subscriptions/listen`. Every MCP tool goes into the
        // agent's tool list; the SDK has no tool search and no list_changed handler
        // in the versions the CLI pins.
        // nanobot-ai 0.3.5 lists MCP tools at startup without a list_changed handler.
        "openhands" | "nanobot" => DiscoveryCapabilities {
            auto_mode: "lazy",
            native_tool_search: Some(false),
            tools_list_changed: Some(false),
            cold_full_list_wait_ms: 5_000,
            max_tools: None,
            auto_full_max_tools: None,
            evidence: "docs/client-conformance.md (captured startup)",
        },
        "anthropic-api" => DiscoveryCapabilities {
            auto_mode: "full",
            native_tool_search: Some(true),
            tools_list_changed: None,
            cold_full_list_wait_ms: 5_000,
            max_tools: None,
            auto_full_max_tools: None,
            evidence:
                "https://platform.claude.com/docs/en/agents-and-tools/tool-use/tool-search-tool",
        },
        "openai-api" => DiscoveryCapabilities {
            auto_mode: "full",
            native_tool_search: Some(true),
            tools_list_changed: None,
            cold_full_list_wait_ms: 5_000,
            max_tools: None,
            auto_full_max_tools: None,
            evidence: "https://developers.openai.com/api/docs/guides/tools-tool-search",
        },
        "lm-studio" | "jan" | "anythingllm" => DiscoveryCapabilities {
            auto_mode: "lazy",
            native_tool_search: None,
            tools_list_changed: None,
            cold_full_list_wait_ms: 5_000,
            max_tools: None,
            auto_full_max_tools: None,
            evidence: "docs/clients.md (local-model clients)",
        },
        _ => DiscoveryCapabilities {
            auto_mode: "lazy",
            native_tool_search: None,
            tools_list_changed: None,
            cold_full_list_wait_ms: 5_000,
            max_tools: None,
            auto_full_max_tools: None,
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
        assert_eq!(capabilities("cursor").tools_list_changed, Some(false));
    }
    #[test]
    fn every_definition_carries_capability_evidence_and_preserves_overrides() {
        let mut registry = crate::registry::Registry::default();
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
    fn a_chosen_global_mode_beats_built_in_client_defaults() {
        let mut registry = crate::registry::Registry::default();
        assert_eq!(
            super::super::client_discovery_mode(&registry, "codex"),
            "full"
        );
        assert_eq!(
            super::super::client_discovery_mode(&registry, "claude-code"),
            "lazy"
        );
        registry.set_discovery_mode("lazy");
        assert_eq!(
            super::super::client_discovery_mode(&registry, "codex"),
            "lazy"
        );
        registry.set_discovery_mode("grouped");
        assert_eq!(
            super::super::client_discovery_mode(&registry, "cursor"),
            "grouped"
        );
        // A per-client choice still wins over the global one.
        registry.set_client_discovery("cursor", Some("full"));
        assert_eq!(
            super::super::client_discovery_mode(&registry, "cursor"),
            "full"
        );
        // 1.x "Find tools as needed" turned off meant full for everyone.
        let v1 = crate::registry::Registry {
            lazy_discovery: false,
            ..Default::default()
        };
        assert_eq!(
            super::super::client_discovery_mode(&v1, "claude-code"),
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
            "lazy"
        );
    }

    #[test]
    fn adapter_capabilities_do_not_rewrite_registry_choices_or_identity() {
        let mut registry = crate::registry::Registry::default();
        registry.set_client_discovery("claude-code", Some("full"));
        let before = serde_json::to_value(&registry).unwrap();
        for id in [
            "codex",
            "client:codex",
            "adapter:codex",
            "adapter:client:codex",
        ] {
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
            ("reasonix", 5_000),
            ("codewhale", 5_000),
            ("anthropic-api", 5_000),
            ("openai-api", 5_000),
        ] {
            assert_eq!(capabilities(id).auto_mode(), "full", "{id}");
            assert_eq!(capabilities(id).cold_full_list_wait_ms, budget, "{id}");
        }
        assert_eq!(capabilities("claude-code").cold_full_list_wait_ms, 2_000);
        // OpenClaw re-lists on change, so it keeps the two-second bound.
        assert_eq!(capabilities("openclaw").auto_mode(), "full");
        assert_eq!(capabilities("openclaw").cold_full_list_wait_ms, 2_000);
        for id in [
            "claude-code",
            "opencode",
            "lm-studio",
            "jan",
            "anythingllm",
            "openhands",
            "nanobot",
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
                max_tools: None,
                auto_full_max_tools: None,
                evidence: "fixture"
            }
            .auto_mode(),
            "lazy"
        );
    }
    #[test]
    fn a_chosen_mode_turns_off_the_small_catalog_limit() {
        let claude = capabilities("claude-code");
        for chosen in [None, Some(""), Some("auto"), Some("bogus")] {
            assert_eq!(
                claude.auto_full_max_tools_for(chosen),
                Some(CLAUDE_CODE_AUTO_FULL_MAX_TOOLS),
                "{chosen:?}"
            );
        }
        for chosen in ["full", "lazy", " Grouped "] {
            assert_eq!(
                claude.auto_full_max_tools_for(Some(chosen)),
                None,
                "{chosen}"
            );
        }
        assert_eq!(capabilities("codex").auto_full_max_tools_for(None), None);
    }

    #[test]
    fn claude_code_auto_lists_small_catalogs_in_full() {
        assert_eq!(
            capabilities("claude-code").auto_full_max_tools,
            Some(CLAUDE_CODE_AUTO_FULL_MAX_TOOLS)
        );
        for id in ["opencode", "codex", "cursor", "unknown"] {
            assert_eq!(capabilities(id).auto_full_max_tools, None, "{id}");
        }

        let mut registry = crate::registry::Registry::default();
        for id in ["claude-code", "client:claude-code", "adapter:claude-code"] {
            assert_eq!(
                super::super::auto_full_max_tools(&registry, id),
                Some(CLAUDE_CODE_AUTO_FULL_MAX_TOOLS),
                "{id}"
            );
        }
        assert_eq!(
            super::super::auto_full_max_tools(&registry, "opencode"),
            None
        );
        // An explicit per-client or global choice is never second-guessed.
        registry.set_client_discovery("claude-code", Some("lazy"));
        assert_eq!(
            super::super::auto_full_max_tools(&registry, "adapter:claude-code"),
            None
        );
        // An unrecognized stored value falls back to Auto, as in client_discovery_mode.
        registry
            .client_discovery
            .insert("claude-code".into(), "bogus".into());
        assert_eq!(
            super::super::auto_full_max_tools(&registry, "claude-code"),
            Some(CLAUDE_CODE_AUTO_FULL_MAX_TOOLS)
        );
        registry.set_client_discovery("claude-code", None);
        registry.set_discovery_mode("lazy");
        assert_eq!(
            super::super::auto_full_max_tools(&registry, "claude-code"),
            None
        );
    }
}
