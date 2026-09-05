// Separate development entry. Never imported by the shipping application.
import { mockIPC } from "@tauri-apps/api/mocks";
import { createRoot } from "react-dom/client";
import { ClientLogo } from "@/components/ClientLogo";
import { ServerLogo } from "@/components/ServerLogo";
import type { Registry, ServerEntry } from "@/lib/types";
import "../index.css";

if (!import.meta.env.DEV) throw new Error("Fixtures require the development server");

const servers: ServerEntry[] = ["GitHub", "Linear", "Stripe"].map((name, i) => ({
  id: `fixture-${i}`,
  name,
  transport: "stdio",
  command: "fixture-only",
  args: [],
  env: [],
  url: null,
  source: "manual",
}));
const registry: Registry = {
  version: 1,
  servers,
  profiles: [
    { id: "local", name: "Local fixture", enabledServerIds: servers.map((s) => s.id) },
  ],
  activeProfileId: "local",
};
const auditRows = Array.from({ length: 200 }, (_, i) => ({
  ts: 1_700_000_000_000 - i * 1000,
  server: "GitHub",
  tool: "list_issues",
  ok: true,
  durationMs: 12,
}));
const calls: Record<string, number> = {};
const missing: string[] = [];
Object.assign(window, { toolportFixture: { calls, missing } });
localStorage.setItem("toolport.onboarded", "1");

mockIPC(
  (command) => {
    calls[command] = (calls[command] ?? 0) + 1;
    switch (command) {
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
            servers: [],
            pluginServers: [],
            gatewayInstalled: true,
            entryState: "managed",
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
      case "plugin:updater|check":
      case "savings_summary":
        return null;
      case "plugin:app|version":
        return "1.18.0-fixture";
      case "get_audit_log":
        return auditRows;
      case "audit_stats":
        return { total: 200, errors: 0, errorRate: 0, servers: [] };
      case "list_quarantined":
      case "list_pending_approvals":
      case "list_routine_suggestions":
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
              {servers.map((s) => (
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
} else {
  await import("../main");
}
