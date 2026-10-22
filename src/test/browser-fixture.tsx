import { initializeLanguage, isLanguageChoice, LanguageProvider } from "@/lib/i18n";
import { LocalizationControls } from "./localization-controls";
import { securityFixture } from "./security-fixture";
import { applySkillState, skillsFixture } from "./skills-fixture";
const fixtureLocations: import("@/lib/types").DetectedClient[] = [];
// Separate development entry. Never imported by the shipping application.
import { mockIPC } from "@tauri-apps/api/mocks";
import { useState } from "react";
import { PageHeader } from "@/components/PageHeader";
import { TeamsView } from "@/components/TeamsView";
import { createRoot } from "react-dom/client";
import { ClientLogo } from "@/components/ClientLogo";
import { ServerLogo } from "@/components/ServerLogo";
import type {
  AuditEntry,
  ClientSession,
  ClientActivity,
  PendingApproval,
  Registry,
  SavingsSummary,
  ServerEntry,
} from "@/lib/types";
import "../index.css";

if (!import.meta.env.DEV) throw new Error("Fixtures require the development server");

// Capture install origin before fixture startup creates any data. Session
// storage stands in for the backend's machine-local settings and install marker.
const fixtureInstallOrigin =
  sessionStorage.getItem("toolport-fixture-install-origin") ??
  (new URLSearchParams(location.search).has("language-upgrader") ? "existing" : "fresh");
sessionStorage.setItem("toolport-fixture-install-origin", fixtureInstallOrigin);
if (sessionStorage.getItem("toolport-fixture-language") === null) {
  sessionStorage.setItem(
    "toolport-fixture-language",
    fixtureInstallOrigin === "existing" ? "en" : "system",
  );
  sessionStorage.setItem(
    "toolport-fixture-language-suggestion",
    String(fixtureInstallOrigin === "existing"),
  );
}

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
if (new URLSearchParams(location.search).has("tool-changes")) {
  servers.push(
    ...[
      ["cloudflare-full-api", "Cloudflare (Full API)"],
      ["revenuecat", "RevenueCat"],
    ].map(([id, name]) => ({ ...servers[0], id, name, enabled: false })),
  );
}
const authGuidance = new URLSearchParams(location.search).has("auth-guidance");
if (authGuidance) {
  servers[0] = {
    ...servers[0],
    name: "Remote project tools",
    transport: "http",
    command: null,
    url: "https://example.invalid/mcp",
  };
  servers[1] = {
    ...servers[1],
    name: "Service API tools",
    env: [{ key: "API_KEY", value: "", secret: true }],
  };
}
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
const pagesTruth = new URLSearchParams(location.search).has("pages-truth");
// Several Claude Code profiles plus a copy of the default, and several Codex homes, for
// the grouped Clients page.
const claudeProfiles = new URLSearchParams(location.search).has("claude-profiles");
const fixtureClient = (
  id: string,
  name: string,
  overrides: Record<string, unknown> = {},
): Record<string, unknown> => ({
  id,
  name,
  usesConnectors: false,
  configPath: `/fixture/${id}.json`,
  configExists: true,
  appPresent: true,
  servers: [],
  pluginServers: [],
  gatewayInstalled: false,
  entryState: "absent",
  error: null,
  ...overrides,
});
const detailStates = new URLSearchParams(location.search).has("detail-states");
const detailClients = [
  fixtureClient("vscode", "VS Code", {
    gatewayInstalled: true,
    entryState: "customized",
  }),
  fixtureClient("zed", "Zed", {
    error: "Could not read this client config. Check its syntax and file permissions.",
  }),
  fixtureClient("goose", "Goose", { appPresent: false, configExists: false }),
  fixtureClient("opencode", "OpenCode", {
    servers: [
      {
        name: "Calendar",
        transport: "stdio",
        command: "calendar-mcp",
        args: [],
        envKeys: [],
        url: null,
      },
    ],
  }),
];
const claudeProfileClients = [
  fixtureClient("claude-code", "Claude Code", {
    configPath: "/fixture/.claude.json",
    gatewayInstalled: true,
    entryState: "managed",
    claudeProfile: {
      command: "claude",
      account: "me@work.example",
      folder: "~/.claude.json",
      lastUsedMs: Date.now() - 120_000,
      sameAs: null,
    },
  }),
  fixtureClient("claude-code@claude2", "Claude Code (claude2)", {
    configPath: "/fixture/.claude2/.claude.json",
    claudeProfile: {
      command: "claude2",
      account: "me@home.example",
      folder: "~/.claude2",
      lastUsedMs: Date.now() - 3 * 3_600_000,
      sameAs: null,
    },
  }),
  fixtureClient("claude-code@claude-api", "Claude Code (claude-api)", {
    configPath: "/fixture/.claude-api/.claude.json",
    claudeProfile: {
      command: null,
      account: null,
      folder: "~/.claude-api",
      lastUsedMs: Date.now() - 5 * 86_400_000,
      sameAs: null,
    },
  }),
  fixtureClient("claude-code@claude", "Claude Code (claude)", {
    configPath: "/fixture/.claude/.claude.json",
    claudeProfile: {
      command: null,
      account: "me@work.example",
      folder: "~/.claude",
      lastUsedMs: Date.now() - 60_000,
      sameAs: "claude-code",
    },
  }),
  fixtureClient("codex", "Codex", {
    configPath: "/fixture/.codex/config.toml",
    gatewayInstalled: true,
    entryState: "managed",
    claudeProfile: {
      command: "codex",
      account: null,
      folder: "~/.codex",
      lastUsedMs: Date.now() - 600_000,
      sameAs: null,
    },
  }),
  fixtureClient("codex@Codex-Work%2F.codex", "Codex (Codex-Work)", {
    configPath: "/fixture/Codex-Work/.codex/config.toml",
    claudeProfile: {
      command: "codex-work",
      account: null,
      folder: "~/Codex-Work/.codex",
      lastUsedMs: Date.now() - 2 * 86_400_000,
      sameAs: null,
    },
  }),
  fixtureClient("codex@Codex-Second%2F.codex", "Codex (Codex-Second)", {
    configPath: "/fixture/Codex-Second/.codex/config.toml",
    claudeProfile: {
      command: "codex-second",
      account: null,
      folder: "~/Codex-Second/.codex",
      lastUsedMs: Date.now() - 16 * 86_400_000,
      sameAs: null,
    },
  }),
  fixtureClient("cursor", "Cursor", { gatewayInstalled: true, entryState: "managed" }),
  fixtureClient("gemini-cli", "Gemini CLI"),
  fixtureClient("opencode", "OpenCode"),
  fixtureClient("zed", "Zed"),
];
if (pagesTruth) {
  registry.safetyLevel = "off";
  registry.team = {
    teamId: "fixture-team",
    teamName: "Example team",
    role: "member",
    serverUrl: "https://teams.example.invalid",
    lastVersion: 12,
  };
  servers[0] = {
    ...servers[0],
    name: "Team Linear",
    transport: "http",
    command: null,
    url: "https://linear.example.invalid/mcp",
    source: "team:fixture-team",
  };
  servers[1] = {
    ...servers[1],
    name: "Project tools",
    transport: "http",
    command: null,
    url: "https://projects.example.invalid/mcp",
  };
  servers[2] = { ...servers[2], name: "Local tools", command: "/bin/false" };
}
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

const driftFixture = new URLSearchParams(location.search).has("tool-changes");
const profileDriftFixture = new URLSearchParams(location.search).has("profile-drift");
const driftEvents = profileDriftFixture
  ? ["local", "work"].map((profile) => ({
      ...securityFixture()[0],
      // Reloads reread the same persisted occurrence, just like the security log.
      ts: 1_790_000_000_000,
      profile,
      new_fp: "v2:profile-fixture",
      blocked: true,
      blocked_profiles: [profile],
    }))
  : driftFixture
    ? securityFixture()
    : [];
const personalFixture = new URLSearchParams(location.search).get("personal-sync");
if (personalFixture && personalFixture !== "setup") {
  registry.team = {
    serverUrl: "https://teams.toolport.app",
    teamId: "personal-fixture",
    role: "admin",
    lastVersion: 3,
    accountStatus: {
      personalSync: true,
      plan: personalFixture === "trial" ? "pro" : "free",
      trialActive: personalFixture === "trial",
      trialEndsAt: Date.now() + 7 * 86400000,
      freeSyncGraceEndsAt: personalFixture === "grace" ? Date.now() + 2 * 86400000 : null,
      deviceId: "fixture",
      canReceiveConfig: personalFixture !== "blocked",
      reason:
        personalFixture === "blocked"
          ? "Choose this device in Your account to resume sync."
          : null,
    },
    personalSyncState: {
      lastSyncedAt: Date.now(),
      error: personalFixture === "blocked" ? "Sync is paused on this device" : null,
    },
  };
}
const approvalFixture = new URLSearchParams(location.search).has("approvals");
const sessionFixture = new URLSearchParams(location.search).has("sessions");
const callerFixture = new URLSearchParams(location.search).has("caller-names");
const sessionRows: ClientSession[] = sessionFixture
  ? [
      {
        sessionId: "f3-fixture-session",
        clientName: "Unknown app (via Cursor)",
        clientLabel: "kt 1",
        clientType: "unknown",
        gatewayVersion: "2.0.0-preview.3",
        phase: "checkpoint",
        reason: "tools_list",
        transport: "stdio",
        toolsListCount: 3,
        listChangedCount: 1,
        firstCatalogSize: 4,
        firstCatalogRevision: 1,
        catalogRevision: 2,
        contentChanged: true,
      },
    ]
  : [];
let pendingApproval: PendingApproval[] = approvalFixture
  ? [
      {
        id: "fixture-approval",
        client: "adapter:claude-code",
        clientName: sessionFixture ? "Unknown app (via Cursor)" : "Claude Code",
        clientLabel: sessionFixture ? "kt 1" : "Claude Code 2.1.0",
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
      clientName: sessionFixture ? "Unknown app (via Cursor)" : "Claude Code",
      clientLabel: sessionFixture ? "kt 1" : "Claude Code 2.1",
      dispatchMs: sessionFixture ? 3 : undefined,
      cold: sessionFixture ? false : undefined,
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
if (callerFixture) {
  const identities = [
    { client: "adapter:inbox", clientName: "inbox" },
    { clientName: "Unknown client", clientLabel: "inbox" },
    {},
    { clientName: "[private]" },
  ];
  auditRows.splice(
    0,
    auditRows.length,
    ...identities.map((identity, index) => ({
      ts: Date.now() - 1000,
      server: "GitHub",
      tool: `list_issues_${index}`,
      ok: true,
      durationMs: 12,
      ...identity,
    })),
  );
  sessionRows.splice(
    0,
    sessionRows.length,
    ...identities.map<ClientSession>((identity, index) => ({
      sessionId: `caller-fixture-${index}`,
      clientType: "unknown",
      gatewayVersion: "2.0.0-preview.6",
      phase: "close",
      reason: "client_disconnect",
      transport: "stdio",
      toolsListCount: 1,
      listChangedCount: 0,
      firstCatalogSize: 4,
      firstCatalogRevision: 1,
      catalogRevision: 1,
      contentChanged: false,
      clientName: "Unrecorded client",
      ...identity,
    })),
  );
  pendingApproval = identities.slice(0, 3).map((identity, index) => ({
    id: `caller-approval-${index}`,
    server: "team-slack",
    tool: `delete_issue_${index}`,
    reason: "destructive",
    arguments: { issue: 42 },
    deadlineMs: Date.now() + 120000,
    client: null,
    ...identity,
  }));
}
const dogfoodFixture = new URLSearchParams(location.search).has("dogfood");
if (dogfoodFixture) {
  sessionRows.splice(
    0,
    sessionRows.length,
    ...Array.from({ length: 5 }, (_, i): ClientSession => ({
      sessionId: `codex-${i}`,
      client: "adapter:codex",
      clientName: "Codex",
      clientType: "codex",
      gatewayVersion: "2.0.0-preview.6",
      phase: "close",
      reason: "client_disconnect",
      transport: "stdio",
      toolsListCount: 0,
      listChangedCount: 0,
      firstCatalogSize: 1711,
      firstCatalogRevision: 1,
      catalogRevision: 1,
      contentChanged: false,
    })),
  );
  auditRows.splice(
    0,
    auditRows.length,
    ...["search", "describe", "status", "fetch"].map((tool, i) => ({
      ts: Date.now() - (i + 1) * 60000,
      kind: "internal",
      server: "toolport",
      tool,
      ok: true,
      client: "adapter:codex",
      clientName: "Codex",
      durationMs: 5,
      cold: false,
      dispatchMs: 2,
    })),
    {
      ts: Date.now() - 300000,
      server: "GitHub",
      tool: "list_issues",
      ok: true,
      client: "adapter:codex",
      clientName: "Codex",
      durationMs: 12,
      piiReplaced: 1,
      cold: false,
      dispatchMs: 3,
    },
  );
}

const savingsSummary: SavingsSummary = {
  tokensSaved: 35_000,
  fullTokens: 40_000,
  exposedTokens: 3_400,
  historicalListLoads: 5_884,
  historicalMeasuredLoads: 3_027,
  previousReportedTokensSaved: 195_640_064,
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
const connectedImportFixture = new URLSearchParams(location.search).has(
  "connected-import",
);
const setupFixture = new URLSearchParams(location.search).has("setup");
const setupFailure = new URLSearchParams(location.search).get("setup-failure");
const setupVerifying = new URLSearchParams(location.search).has("setup-verifying");
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
  credentials:
    name === "Calendar"
      ? [
          {
            key: "PAT",
            secret: true,
            present: setupFailure !== "credential" && setupFailure !== "optional",
            required: setupFailure !== "optional",
          },
        ]
      : [],
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
Object.assign(window, { toolportFixture: { calls, missing, driftEvents } });
if (connectedImportFixture) {
  servers.splice(0);
  localStorage.removeItem("toolport.onboarded");
} else localStorage.setItem("toolport.onboarded", "1");

let skills = skillsFixture();
// ?many-agents: a busy machine with several profiles per client, to check the grid
// stacks under the lists before its columns crowd out the skill names.
if (new URLSearchParams(location.search).has("many-agents")) {
  const base = skills.agents;
  const copies = [
    ["claude-code@claude2", "Claude Code (claude2)"],
    ["claude-code@claude3", "Claude Code (claude3)"],
    ["codex@codex-becca", "Codex (codex-becca)"],
    ["codex@codex-second", "Codex (codex-second)"],
    ["codex@codex-work", "Codex (codex-work)"],
    ["github-copilot-cli", "Copilot CLI"],
    ["gemini-cli", "Gemini CLI"],
    ["kiro", "Kiro"],
    ["amp", "Amp"],
    ["goose", "Goose"],
    ["hermes", "Hermes"],
  ].map(([id, name]) => ({ ...base[id.startsWith("codex") ? 1 : 0], id, name }));
  skills.agents = [
    { ...base[0], name: "Claude Code (claude)" },
    { ...base[1], name: "Codex (codex)" },
    ...base.slice(2),
    ...copies,
  ];
  for (const skill of skills.skills) {
    const state = skill.states["claude-code"];
    if (state) for (const { id } of copies) skill.states[id] = state;
  }
}
if (new URLSearchParams(location.search).has("rules-link")) {
  skills.issues.push({
    id: "rules-missing",
    level: "info",
    title: "OpenCode gets no global rules",
    detail:
      "Your global rules files reach other clients but not these. Project files such as AGENTS.md still apply inside a project.",
    agents: ["opencode"],
    skills: [],
    skillNames: [],
    fixLabel: "Link to working-agreements.md",
    fixNote:
      "Toolport makes each file below a link to ~/.agents/working-agreements.md, so these clients read the same rules. It doesn't change any file that already exists. To undo, delete the link.",
    fixPaths: ["~/.config/opencode/AGENTS.md"],
    fingerprint: "rules-missing:00000004",
    dismissed: false,
  });
}
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
    const state = new URLSearchParams(location.search).get("visual-state");
    const stateCommands = [
      "get_registry",
      "detect_clients",
      "get_audit_log",
      "audit_stats",
      "get_search_traces",
      "get_security_events",
    ];
    if (stateCommands.includes(command)) {
      if (state === "loading") return new Promise(() => {});
      if (state === "error" && command !== "detect_clients")
        throw new Error("Synthetic gateway read failure");
      if (state === "empty") {
        if (command === "get_registry") return { ...registry, servers: [] };
        if (command === "audit_stats")
          return { total: 0, errors: 0, errorRate: 0, servers: [] };
        return [];
      }
    }
    switch (command) {
      case "preview_client_setup":
        return {
          configPath: "/fixture/codex.toml",
          backupDir: "/fixture/Toolport/backups/codex",
          revision: "fixture-review",
          items: setupItems,
        };
      case "migrate_client":
        if (setupVerifying) return new Promise(() => {});
        if (setupFailure && setupFailure !== "optional")
          throw new Error(
            setupFailure === "vault"
              ? "Keychain unavailable. Unlock it and retry importing. Client config unchanged."
              : setupFailure === "credential"
                ? "Calendar needs credentials. Open Credentials and retry. Client config unchanged."
                : "Notes could not start. Check its command and retry. Client config unchanged.",
          );
        setupConnected = true;
        fixtureAdd({
          id: "calendar",
          name: "Calendar",
          transport: "stdio",
          command: "fixture-calendar",
          args: [],
          env: [{ key: "PAT", value: null, secret: true }],
          url: null,
          source: "imported:codex",
        });
        return {
          registry: structuredClone(registry),
          imported: 1,
          servers: (args.selected as string[]).map((name) => ({
            name,
            toolCount: 3,
            credentialState: name === "Calendar" ? "stored" : "none",
          })),
          moved: args.selected,
          tools: [{ name: "toolport_search_tools" }, { name: "toolport_call_tool" }],
          outcome: {
            path: "/fixture/codex.toml",
            backup: "/fixture/Toolport/backups/codex/previous.toml",
          },
        };
      case "popular_catalog":
      case "search_catalog":
        return setupCatalog;
      case "add_server":
        return fixtureAdd(args.entry as ServerEntry);
      case "test_secret_reference":
        return null;
      case "set_secret_reference": {
        const server = registry.servers.find((s) => s.id === args.serverId);
        if (server) {
          let entry = server.env.find((e) => e.key === args.key);
          if (!entry) {
            entry = { key: String(args.key), value: null, secret: true };
            server.env.push(entry);
          }
          entry.source = { ref: String(args.reference) };
          entry.value = null;
        }
        return structuredClone(registry);
      }
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
        return {
          registry: structuredClone(registry),
          servers: (args.selected as string[]).map((i) => ({
            name: Object.keys(parsed.mcpServers)[Number(i)],
            status: "added",
            missing: [],
          })),
        };
      }
      case "team_instructions_status":
        return null;
      case "skills_inventory":
        return skills;
      case "set_skill_state": {
        const { agent, key, state } = args as {
          agent: string;
          key: string;
          state: "on" | "off" | "name-only";
        };
        skills = applySkillState(skills, agent, key, state);
        return skills;
      }
      case "fix_skill_issue": {
        const { issue } = args as { issue: string };
        skills = { ...skills, issues: skills.issues.filter((i) => i.id !== issue) };
        return skills;
      }
      case "open_skill_path":
      case "open_skill_path_with":
        return null;
      case "skill_open_apps":
        return [
          {
            id: "org.gnome.TextEditor.desktop",
            name: "Text Editor",
            default: true,
            chosen: false,
          },
          { id: "dev.zed.Zed.desktop", name: "Zed", default: false, chosen: false },
        ];
      case "remove_skill": {
        const { key } = args as { key: string };
        const gone = skills.skills.find((s) => s.key === key);
        skills = { ...skills, skills: skills.skills.filter((s) => s.key !== key) };
        return { trashed: gone ? gone.folder : null, links: 0, inventory: skills };
      }
      case "dismiss_skill_issue": {
        const { fingerprint, dismissed } = args as {
          fingerprint: string;
          dismissed: boolean;
        };
        skills = {
          ...skills,
          issues: skills.issues.map((i) =>
            i.fingerprint === fingerprint ? { ...i, dismissed } : i,
          ),
        };
        return skills;
      }
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
      case "dismiss_removed_features_notice":
        if (registry.removedFeaturesNotice)
          registry.removedFeaturesNotice.dismissed = true;
        return registry;
      case "open_exports_dir":
      case "open_gateway_log":
        return null;
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
      case "personal_sync_local_only": {
        const server = registry.servers.find((server) => server.id === args.serverId);
        if (server) server.syncLocalOnly = args.localOnly as boolean;
        return structuredClone(registry);
      }
      case "personal_sync_new_servers_local_only":
        if (registry.team?.personalSyncState)
          registry.team.personalSyncState.newServersLocalOnly = args.localOnly as boolean;
        return structuredClone(registry);
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
      case "get_ui_language": {
        const saved = sessionStorage.getItem("toolport-fixture-language");
        return isLanguageChoice(saved) ? saved : "system";
      }
      case "get_ui_language_suggestion": {
        const pending =
          sessionStorage.getItem("toolport-fixture-language-suggestion") === "true";
        if (
          !calls.set_ui_language &&
          new URLSearchParams(location.search).has("language-suggestion-delay")
        ) {
          return new Promise((resolve) => {
            setTimeout(() => {
              sessionStorage.setItem(
                "toolport-fixture-delayed-suggestion-returned",
                String(
                  Number(
                    sessionStorage.getItem(
                      "toolport-fixture-delayed-suggestion-returned",
                    ) ?? "0",
                  ) + 1,
                ),
              );
              resolve(pending);
            }, 1000);
          });
        }
        return pending;
      }
      case "set_ui_language":
        if (new URLSearchParams(location.search).has("language-save-error"))
          throw new Error("Fixture settings are read-only");
        if (!isLanguageChoice(args.language)) throw new Error("Invalid language choice");
        sessionStorage.setItem("toolport-fixture-language", args.language);
        sessionStorage.setItem("toolport-fixture-language-suggestion", "false");
        return null;
      case "get_registry":
        return registry;
      case "client_location_types":
        return [
          { id: "claude-code", name: "Claude Code", configFile: ".claude.json" },
          { id: "codex", name: "Codex", configFile: "config.toml" },
        ];
      case "add_client_location":
        fixtureLocations.push({
          id: `${String(args.clientType)}@fixture-${fixtureLocations.length}`,
          name: `${args.clientType === "codex" ? "Codex" : "Claude Code"} (${String(args.folder).split("/").pop()?.replace(/^\./, "")})`,
          usesConnectors: false,
          locationRemovable: true,
          configPath: `${String(args.folder)}/${args.clientType === "codex" ? "config.toml" : ".claude.json"}`,
          configExists: true,
          appPresent: true,
          servers: [],
          pluginServers: [],
          gatewayInstalled: false,
          entryState: "absent",
          discovery: {
            autoMode: args.clientType === "codex" ? "full" : "lazy",
            nativeToolSearch: true,
            toolsListChanged: null,
            evidence: "fixture",
          },
          error: null,
        });
        return fixtureLocations.at(-1)!.id;
      case "remove_client_location": {
        const index = fixtureLocations.findIndex((client) => client.id === args.clientId);
        if (index >= 0) fixtureLocations.splice(index, 1);
        return null;
      }
      case "detect_clients":
        return [
          ...fixtureLocations,
          ...(detailStates ? detailClients : []),
          ...(claudeProfiles ? claudeProfileClients : []),
          ...(args.inspectConfigs && !claudeProfiles
            ? [
                {
                  id: "claude-code@claude2",
                  name: "Claude Code (claude2)",
                  usesConnectors: false,
                  configPath: "/fixture/.claude2/.claude.json",
                  configExists: true,
                  appPresent: true,
                  servers: [],
                  pluginServers: [],
                  gatewayInstalled: true,
                  entryState: "managed",
                  error: null,
                },
              ]
            : []),
          {
            id: "codex",
            name: "Codex",
            usesConnectors: false,
            configPath: "/fixture/codex.toml",
            configExists: true,
            appPresent: true,
            servers: setupFixture || connectedImportFixture ? setupItems : [],
            pluginServers: [],
            gatewayInstalled: !setupFixture || setupConnected,
            entryState: !setupFixture || setupConnected ? "managed" : "absent",
            discovery: {
              autoMode: "full",
              nativeToolSearch: true,
              toolsListChanged: null,
              evidence: "fixture",
            },
            error: null,
          },
        ].filter(
          // The Codex home in the profile set replaces the plain Codex row.
          (client, index, all) =>
            all.findIndex((other) => other.id === client.id) === index,
        );
      case "team_sync_status":
        return { state: "offline", lastSuccessMs: 1791504000000 };
      case "team_sync":
        throw new Error("Team server is unreachable");
      case "probe_servers":
        if (pagesTruth)
          return [
            {
              serverId: servers[0].id,
              ok: false,
              toolCount: 0,
              error: "HTTP 401 (needs authentication)",
              authRequired: true,
              authTarget: "endpoint",
              failure: { kind: "auth", target: "endpoint" },
            },
            {
              serverId: servers[1].id,
              ok: false,
              toolCount: 0,
              error: "Connection refused",
              authRequired: false,
              failure: { kind: "unavailable", after_send: false },
            },
            {
              serverId: servers[2].id,
              ok: false,
              toolCount: 0,
              error: "Server exited with status 1",
              authRequired: false,
              failure: { kind: "internal" },
            },
          ];
        return servers.map((s, i) => ({
          serverId: s.id,
          ok: !authGuidance || i === 2,
          authRequired: authGuidance && i < 2,
          authTarget:
            authGuidance && i < 2 ? (i === 0 ? "endpoint" : "service_credential") : null,
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
      case "get_client_sessions": {
        if (new URLSearchParams(location.search).has("raw-sessions")) return sessionRows;
        const grouped = new Map<string, ClientActivity>();
        for (const row of sessionRows) {
          const key = `${row.clientName}:${row.clientLabel ?? ""}`;
          const previous = grouped.get(key);
          grouped.set(key, {
            ...row,
            lastActiveMs: Date.now() - 180000,
            callsToday: (previous?.callsToday ?? 0) + 12,
            sessionCount: (previous?.sessionCount ?? 0) + 1,
          });
        }
        return [...grouped.values()];
      }
      case "activity_health":
        return {
          telemetry: {
            queueDropped: 0,
            writeFailedRecords: 0,
            writeFailures: 0,
            incompleteFlushes: 0,
          },
          gatewayNotes: [],
        };
      case "release_activity_revision":
        return null;
      case "activity_revision":
        return JSON.stringify([auditRows, driftEvents]);
      case "get_audit_log":
        return auditRows;
      case "audit_stats":
        return {
          total: dogfoodFixture ? 5225 : 200,
          errors: 0,
          errorRate: 0,
          servers: [
            {
              server: "GitHub",
              calls: dogfoodFixture ? 5225 : 200,
              errors: 0,
              errorRate: 0,
              avgMs: 12,
              p95Ms: 12,
              lastTs: auditRows[auditRows.length - 1].ts,
              tools: [
                {
                  tool: "list_issues",
                  calls: dogfoodFixture ? 5225 : 200,
                  errors: 0,
                  errorRate: 0,
                  avgMs: 12,
                  p95Ms: 12,
                  lastTs: auditRows[auditRows.length - 1].ts,
                },
              ],
            },
          ],
        };
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
      case "data_removal_plan":
        return {
          dataDir: "/fixture/home/.config/Toolport",
          resources: [
            "All contents of /fixture/home/.config/Toolport: registry, settings, logs, caches, migration exports, client backups, encrypted secrets and published gateways in bin/",
            "Only credentials owned by this registry in conduit-mcp. Shared master keys and accounts needed by other detected installs are preserved. Use --all-installs in the CLI only to explicitly clear the entire shared service.",
            "Toolport's daemon for this data directory, after all active sessions have closed",
            ...["Toolport", "Conduit", "conduit", "ToolportNativePreview"].map(
              (name) =>
                `/fixture/home/.config/autostart/${name}.desktop (only Toolport's startup entry)`,
            ),
          ],
          reportPath: "/fixture/home/Toolport-removal-report.json",
        };
      case "remove_toolport_data":
        return null;
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
        pendingApproval = pendingApproval.filter((approval) => approval.id !== args.id);
        return null;
      case "get_baseline_notice":
        return null;
      case "get_security_events":
        return driftEvents;
      case "release_quarantine": {
        const event = driftEvents.find(
          (event) => event.profile === args.profile && event.tool === args.tool,
        );
        if (!event || event.new_fp !== args.expectedFingerprint)
          throw new Error("The reviewed definition changed");
        event.blocked = false;
        event.blocked_profiles = [];
        return null;
      }
      case "clients_needing_restart":
      case "list_allowed_tools":
      case "list_quarantined":
        return [];
      case "get_search_traces":
        return auditRows
          .filter((row) => row.kind === "internal" && row.tool === "search")
          .map((row) => ({
            ts: row.ts,
            client: row.clientName,
            query: "GitHub issues",
            top: "github_list_issues",
            names: ["github_list_issues"],
            returned: 1,
            total: 75,
            returnedTokens: 100,
            flatTokens: 1000,
            savedTokens: 900,
            responseContentBytes: 400,
            escalated: false,
            mode: "lexical",
          }));
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

if (
  new URLSearchParams(location.search).has("localization-controls") ||
  new URLSearchParams(location.search).has("logos") ||
  memberReviewFixture ||
  personalFixture
)
  await initializeLanguage();

if (new URLSearchParams(location.search).has("localization-controls")) {
  createRoot(document.getElementById("root")!).render(
    <LanguageProvider>
      <LocalizationControls server={servers[0]} registry={registry} />
    </LanguageProvider>,
  );
} else if (new URLSearchParams(location.search).has("logos")) {
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
} else if (memberReviewFixture || personalFixture) {
  function ReviewFixture() {
    const [current, setCurrent] = useState(registry);
    return (
      <main>
        <PageHeader
          title={personalFixture ? "Sync" : "Team"}
          subtitle={
            personalFixture
              ? "Your setup, on every machine"
              : "Share one MCP server set across your team"
          }
        />
        <div className="p-6">
          <TeamsView registry={current} onRegistryChange={setCurrent} />
        </div>
      </main>
    );
  }
  createRoot(document.getElementById("root")!).render(
    <LanguageProvider>
      <ReviewFixture />
    </LanguageProvider>,
  );
} else {
  await import("../main");
}
