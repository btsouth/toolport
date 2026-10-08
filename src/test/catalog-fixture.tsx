// Development-only catalog states. IPC and registry search never reach a network.
import { mockIPC } from "@tauri-apps/api/mocks";
import { createRoot } from "react-dom/client";
import { CatalogView } from "@/components/CatalogView";
import type { CatalogEntry, Registry } from "@/lib/types";
import "../index.css";

if (!import.meta.env.DEV) throw new Error("Fixtures require the development server");
const state = new URLSearchParams(location.search).get("state") ?? "normal";
const entries: CatalogEntry[] = [
  {
    name: "GitHub",
    description: "Repos, issues, PRs, and code search.",
    transport: "http",
    command: null,
    args: [],
    url: "https://api.githubcopilot.com/mcp/",
    envKeys: [],
    source: "curated",
    homepage: "https://github.com/github/github-mcp-server",
    category: "Code & infrastructure",
  },
  {
    name: "Memory",
    description: "A knowledge graph the agent reads and writes across sessions.",
    transport: "stdio",
    command: "npx",
    args: ["-y", "@modelcontextprotocol/server-memory@2026.8.31"],
    url: null,
    envKeys: [],
    source: "curated",
    homepage: "https://github.com/modelcontextprotocol/servers",
    category: "Local tools",
  },
  {
    name: "Redis",
    description: "Inspect and manage a Redis database.",
    transport: "stdio",
    command: "uvx",
    args: [
      "--from",
      "redis-mcp-server==0.5.1",
      "redis-mcp-server",
      "--url",
      "<launch-input>",
    ],
    url: null,
    envKeys: [],
    source: "curated",
    homepage: "https://github.com/redis/mcp-redis",
    category: "Databases",
  },
];
const registry: Registry = {
  version: 3,
  servers:
    state === "installed"
      ? [
          {
            id: "renamed",
            name: "My repositories",
            enabled: false,
            transport: "http",
            command: null,
            args: [],
            url: "HTTPS://API.GITHUBCOPILOT.COM:443/mcp?token=fixture-only",
            env: [],
            source: "manual",
          },
          {
            id: "same-name",
            name: "Memory",
            enabled: false,
            transport: "stdio",
            command: "npx",
            args: ["-y", "different-memory-package"],
            url: null,
            env: [],
            source: "manual",
          },
        ]
      : [],
  profiles: [],
  activeProfileId: null,
};
mockIPC((command) => {
  switch (command) {
    case "popular_catalog":
      return entries;
    case "search_catalog":
      return {
        entries: state === "empty-outage" ? [] : entries.slice(0, 1),
        registryStatus: state.includes("outage")
          ? "unavailable"
          : state === "timeout"
            ? "timedOut"
            : "available",
      };
    default:
      throw new Error(`Unexpected catalog fixture command: ${command}`);
  }
});
createRoot(document.getElementById("root")!).render(
  <main className="max-w-5xl mx-auto p-6">
    <h1 className="text-xl font-semibold mb-4">Browse catalog</h1>
    <CatalogView
      registry={registry}
      onAdded={() => {
        throw new Error("Read-only fixture");
      }}
    />
  </main>,
);
