// Separate development entry. Never imported by the shipping application.
import { mockIPC } from "@tauri-apps/api/mocks";
import { useState } from "react";
import { TeamsView } from "@/components/TeamsView";
import { createRoot } from "react-dom/client";
import { ClientLogo } from "@/components/ClientLogo";
import { ServerLogo } from "@/components/ServerLogo";
import type {
  AuditEntry,
  PendingApproval,
  Registry,
  SavingsSummary,
  ServerEntry,
} from "@/lib/types";
import "../index.css";

if (!import.meta.env.DEV) throw new Error("Fixtures require the development server");

const servers: ServerEntry[] = ["GitHub", "Linear", "Stripe"].map((name, i) => ({
  id: `fixture-${i}`,
  name,
  enabled: true,
  transport: "stdio",
  command: "fixture-only",
  args: [],
  env: [],
  url: null,
  source: "manual",
}));
const longNames = new URLSearchParams(location.search).has("long-names");
if (longNames) servers[0].name = "A".repeat(70);
const registry: Registry = {
  version: 3,
  servers,
  profiles: [
    { id: "local", name: "Local fixture", enabledServerIds: servers.map((s) => s.id) },
    { id: "work", name: "Work", enabledServerIds: [servers[0].id] },
  ],
  activeProfileId: "local",
  defaultAccessContextId: "local",
  defaultAccessLegacyPolicy: true,
  accessUpgradeNoticeDismissed: false,
  clientScopes: { codex: "" },
};
const memberReviewFixture = new URLSearchParams(location.search).has("teams-review");
if (memberReviewFixture) {
  registry.team = {
    teamId: "fixture-team",
    teamName: "Example team",
    serverUrl: "https://teams.toolport.app",
    role: "member",
    lastVersion: 12,
    memberReview: {
      pending: {
        "server:remote": {
          key: "server:remote",
          title: "Server: Project tools",
          hash: "fixture-remote-hash",
          labels: [
            {
              author: { name: "Alice" },
              at: 1791417600000,
              via: "dashboard",
              approvedBy: { name: "Bob" },
            },
          ],
          fields: [
            {
              field: "url",
              before: "https://old.example/mcp",
              after: "https://new.example/mcp",
            },
            {
              field: "allowedTools",
              before: '["list_projects"]',
              after: '["list_projects", "create_project"]',
            },
          ],
        },
        instructions: {
          key: "instructions",
          title: "Team instructions",
          hash: "fixture-instructions-hash",
          labels: [],
          fields: [
            {
              field: "Content",
              before: "Use the project issue tracker.",
              after:
                "Use the project issue tracker. Include the project ID in every update.",
            },
          ],
        },
        policy: {
          key: "policy",
          title: "Team policy",
          hash: "fixture-policy-hash",
          labels: [],
          fields: [
            { field: "screeningPolicy.minSafetyLevel", before: "strict", after: "ask" },
          ],
        },
        callAuditExport: {
          key: "callAuditExport",
          title: "Call-log export",
          hash: "fixture-export-hash",
          labels: [],
          fields: [{ field: "Content", before: "false", after: "true" }],
        },
      },
    },
  } as NonNullable<Registry["team"]>;
}

const approvalFixture = new URLSearchParams(location.search).has("approvals");
let pendingApproval: PendingApproval[] = approvalFixture
  ? [
      {
        id: "fixture-approval",
        client: "adapter:claude-code",
        clientName: "Claude Code",
        clientLabel: "Claude Code 2.1.0",
        server: "team-slack",
        tool: "delete_issue",
        reason: "destructive",
        arguments: { issue: 42 },
        deadlineMs: Date.now() + 120000,
      },
    ]
  : [];

const auditRows: AuditEntry[] = Array.from({ length: 200 }, (_, i) => ({
  ts: 1_700_000_000_000 - i * 1000,
  server: "GitHub",
  tool: "list_issues",
  ok: true,
  durationMs: 12,
}));
if (approvalFixture) {
  auditRows.splice(
    0,
    auditRows.length,
    {
      ts: Date.now() - 120000,
      server: "team_slack",
      serverId: "team-slack",
      tool: "read_issue",
      ok: true,
      durationMs: 850,
      client: "adapter:claude-code",
      clientName: "Claude Code",
      clientLabel: "Claude Code 2.1",
    },
    ...[
      "approved",
      "denied",
      "no_response",
      "withdrawn",
      "stale_state",
      "unreachable",
    ].map((decision, index) => ({
      ts: Date.now() - 120000,
      server: "team_slack",
      serverId: "team-slack",
      tool: `delete_issue_${index}`,
      kind: "approval",
      decision,
      ok: true,
      heldMs: index === 0 ? 90000 : 1500,
      client: "adapter:claude-code",
      clientName: "Claude Code",
      clientLabel: index === 5 ? "<b>Other client</b> 2.1" : "Claude Code 2.1",
    })),
  );
}
const savingsSummary: SavingsSummary = {
  tokensSaved: 35_000,
  tokenizedLoads: 12,
  catalogTokenDelta: 36_600,
  discoveryTokens: 1_600,
  tokenizer: "cl100k_base",
  discoveryCount: 12,
  discoveryResponseBytes: 6_400,
  legacyEstimatedTokensAvoided: 123_000,
  listLoads: 12,
  peakCatalog: 75,
  sinceTs: 1_700_000_000_000,
  measuredLoads: 12,
  latestCatalogTs: 1_700_000_000_000,
  latestFullToolCount: 75,
  latestExposedToolCount: 4,
  latestFullSurfaceBytes: 15_000,
  latestExposedSurfaceBytes: 1_300,
  fullSurfaceBytes: 180_000,
  exposedSurfaceBytes: 15_600,
  avoidedSurfaceBytes: 164_400,
  estimatedTokensAvoided: 41_100,
  estimateMethod: "utf8_bytes_div_4",
};
const setupFixture = new URLSearchParams(location.search).has("setup");
const setupFailure = new URLSearchParams(location.search).get("setup-failure");
let setupConnected = false;
const setupCatalog = [
  {
    name: "NoteKit",
    description: "Local note tools",
    transport: "stdio",
    command: "fixture-notes",
    args: [],
    envKeys: [],
    url: null,
    source: "curated",
    homepage: null,
    category: "Local tools",
  },
];
const setupItems = ["Notes", "Calendar"].map((name) => ({
  key: name,
  name,
  transport: "stdio",
  command: `fixture-${name.toLowerCase()}`,
  args: [],
  url: null,
  envKeys: name === "Calendar" ? ["PAT"] : [],
  isNew: true,
}));
function fixtureAdd(entry: ServerEntry) {
  const saved = {
    ...entry,
    id: `added-${registry.servers.length}`,
    enabled: !(entry.env?.length || entry.launch?.inputs.length),
  };
  registry.servers.push(saved);
  registry.profiles[0].enabledServerIds.push(saved.id);
  return structuredClone(registry);
}
const calls: Record<string, number> = {};
const missing: string[] = [];
Object.assign(window, { toolportFixture: { calls, missing } });
localStorage.setItem("toolport.onboarded", "1");

mockIPC(
  (command, payload) => {
    const args: Record<string, unknown> =
      payload &&
      !Array.isArray(payload) &&
      !(payload instanceof ArrayBuffer) &&
      !(payload instanceof Uint8Array)
        ? payload
        : {};
    calls[command] = (calls[command] ?? 0) + 1;
    switch (command) {
      case "preview_client_setup":
        return {
          configPath: "/fixture/codex.toml",
          backupDir: "/fixture/Toolport/backups/codex",
          revision: "fixture-review",
          items: setupItems,
        };
      case "migrate_client":
        if (setupFailure)
          throw new Error(
            setupFailure === "credential"
              ? "Calendar needs credentials. Open Credentials and retry. Client config unchanged."
              : "Notes could not start. Check its command and retry. Client config unchanged.",
          );
        setupConnected = true;
        return {
          registry: structuredClone(registry),
          imported: 1,
          servers: (args.selected as string[]).map((name) => ({
            name,
            toolCount: 3,
            credentialState: "none",
          })),
          moved: args.selected,
          tools: [{ name: "notes__read" }],
          outcome: {
            path: "/fixture/codex.toml",
            backup: "/fixture/Toolport/backups/codex/previous.toml",
          },
        };
      case "popular_catalog":
      case "search_catalog":
        return setupCatalog;
      case "list_stacks":
        return [
          {
            id: "local-notes",
            name: "Local notes",
            description: "Notes and Calendar",
            servers: [
              ...setupCatalog,
              { ...setupCatalog[0], name: "Calendar", envKeys: ["PAT"] },
            ],
          },
        ];
      case "add_server":
        return fixtureAdd(args.entry as ServerEntry);
      case "set_secret":
      case "set_launch_secret":
        return structuredClone(registry);
      case "parse_server_snippet": {
        const parsed = JSON.parse(String(args.text));
        return Object.entries(parsed.mcpServers).map(([name, value]) => {
          const s = value as {
            command: string;
            args?: string[];
            env?: Record<string, string>;
          };
          return {
            name,
            transport: "stdio",
            command: s.command,
            args: s.args ?? [],
            url: null,
            env: Object.entries(s.env ?? {}).map(([key, value]) => ({ key, value })),
          };
        });
      }
      case "add_snippet_servers": {
        const parsed = JSON.parse(String(args.text));
        Object.entries(parsed.mcpServers).forEach(([name, value], i) => {
          if (!(args.selected as string[]).includes(String(i))) return;
          const s = value as {
            command: string;
            args?: string[];
            env?: Record<string, string>;
          };
          fixtureAdd({
            id: "",
            name,
            transport: "stdio",
            command: s.command,
            args: s.args ?? [],
            env: Object.keys(s.env ?? {}).map((key) => ({
              key,
              value: null,
              secret: true,
            })),
            url: null,
            source: "manual",
          });
        });
        return structuredClone(registry);
      }
      case "team_instructions_status":
        return null;
      case "team_review": {
        const team = registry.team as NonNullable<Registry["team"]> & {
          memberReview: { pending: Record<string, unknown> };
        };
        delete team.memberReview.pending[String(args.key)];
        return structuredClone(registry);
      }

      case "dismiss_access_upgrade_notice":
        registry.accessUpgradeNoticeDismissed = true;
        return registry;
      case "set_default_access":
        registry.defaultAccessProfileId = args.profile as string | null;
        return registry;
      case "set_access_server": {
        const profile = registry.profiles.find((p) => p.id === args.profileId)!;
        profile.enabledServerIds = profile.enabledServerIds.filter(
          (id) => id !== args.serverId,
        );
        if (args.included) profile.enabledServerIds.push(args.serverId as string);
        return registry;
      }
      case "create_profile":
        registry.profiles.push({
          id: String(args.name).toLowerCase(),
          name: String(args.name),
          enabledServerIds: [],
        });
        return registry;
      case "delete_profile":
        registry.profiles = registry.profiles.filter((p) => p.id !== args.id);
        return registry;
      case "install_gateway":
        registry.clientScopes = {
          ...registry.clientScopes,
          [String(args.clientId)]: String(args.profile || ""),
        };
        return registry;
      case "set_server_enabled": {
        const server = registry.servers.find((server) => server.id === args.serverId);
        if (server) server.enabled = args.enabled as boolean;
        return structuredClone(registry);
      }
      case "plugin:process|exit":
        return null;
      case "export_config":
        return JSON.stringify({ servers });
      case "set_client_discovery":
        registry.clientDiscovery ??= {};
        if (args.mode)
          registry.clientDiscovery[String(args.clientId)] = String(args.mode);
        else delete registry.clientDiscovery[String(args.clientId)];
        return { ...registry, clientDiscovery: { ...registry.clientDiscovery } };
      case "get_registry":
        return registry;
      case "detect_clients":
        return [
          {
            id: "codex",
            name: "Codex",
            usesConnectors: false,
            configPath: "/fixture/codex.toml",
            configExists: true,
            appPresent: true,
            servers: setupFixture ? setupItems : [],
            pluginServers: [],
            gatewayInstalled: !setupFixture || setupConnected,
            entryState: !setupFixture || setupConnected ? "managed" : "absent",
            discovery: {
              nativeToolSearch: true,
              toolsListChanged: null,
              evidence: "fixture",
            },
            error: null,
          },
        ];
      case "probe_servers":
        return servers.map((s) => ({
          serverId: s.id,
          ok: true,
          toolCount: 25,
          error: null,
        }));
      case "main_window_visible":
        return true;
      case "take_pending_tray_approvals":
        return false;
      case "take_pending_shared":
      case "take_registry_recovery_notice":
      case "team_pair_state":
      case "plugin:updater|check":
        return null;
      case "savings_summary":
        return savingsSummary;
      case "plugin:app|version":
        return "1.18.0-fixture";
      case "get_audit_log":
        return auditRows;
      case "audit_stats":
        return { total: 200, errors: 0, errorRate: 0, servers: [] };
      case "list_server_tools":
        return [
          {
            name: longNames ? "t".repeat(70) : "get_issue",
            toolportQuarantine: "clear",
            description: "Read an issue by number.",
            annotations: { readOnlyHint: true },
            inputSchema: {
              type: "object",
              required: ["number"],
              properties: { number: { type: "integer", description: "Issue number" } },
            },
          },
          {
            name: "delete_issue",
            toolportQuarantine: "clear",
            description: "Delete an issue permanently.",
            annotations: { destructiveHint: true },
            inputSchema: { type: "object", properties: {} },
          },
        ];
      case "list_server_resources":
      case "list_server_prompts":
        return [];
      case "call_tool":
        return {
          content: [{ type: "text", text: `Fixture result: ${JSON.stringify(payload)}` }],
          isError: false,
        };
      case "is_launch_at_login_enabled":
      case "plugin:autostart|is_enabled":
        return false;
      case "http_bridge_status":
        return { running: false, port: null, url: null, token: null };
      case "stop_stale_gateways":
        return { killed: [], failed: [], needsRestart: [] };
      case "list_pending_approvals":
        return pendingApproval;
      case "decide_approval":
        pendingApproval = [];
        return null;
      case "clients_needing_restart":
      case "list_allowed_tools":
      case "list_quarantined":
      case "get_security_events":
      case "get_search_traces":
      case "get_inspect_log":
      case "list_tool_identities":
        return [];
      default:
        missing.push(command);
        throw new Error(`Unimplemented fixture command: ${command}`);
    }
  },
  { shouldMockEvents: true },
);

if (new URLSearchParams(location.search).has("logos")) {
  const paths = Object.keys(import.meta.glob("../assets/client-logos/*.svg"));
  const aliases: Record<string, string> = {
    claude: "claude-desktop",
    devin: "devin-cli",
  };
  const logoServers = [
    ...servers,
    ...["Trello", "RevenueCat", "Redis", "Postman"].map((name) => ({
      id: name,
      name,
      transport: "http",
    })),
  ];
  const clients = paths.map((p) => p.split("/").pop()!.replace(".svg", ""));
  createRoot(document.getElementById("root")!).render(
    <main>
      {[false, true].map((dark) => (
        <section key={String(dark)} className={dark ? "dark" : ""}>
          <div className="bg-background text-foreground p-6">
            <h1>{dark ? "Dark" : "Light"} logo fixture</h1>
            <div className="grid grid-cols-8 gap-4 mt-4">
              {clients.map((id) => (
                <div key={id} className="flex flex-col items-center gap-2 text-xs">
                  <ClientLogo id={aliases[id] ?? id} name={id} size={32} />
                  {id}
                </div>
              ))}
              {logoServers.map((s) => (
                <div key={s.id} className="flex flex-col items-center gap-2 text-xs">
                  <ServerLogo name={s.name} transport={s.transport} size={32} />
                  {s.name}
                </div>
              ))}
            </div>
          </div>
        </section>
      ))}
    </main>,
  );
} else if (memberReviewFixture) {
  function ReviewFixture() {
    const [current, setCurrent] = useState(registry);
    return (
      <main className="max-w-4xl mx-auto p-6">
        <TeamsView registry={current} onRegistryChange={setCurrent} />
      </main>
    );
  }
  createRoot(document.getElementById("root")!).render(<ReviewFixture />);
} else {
  await import("../main");
}
