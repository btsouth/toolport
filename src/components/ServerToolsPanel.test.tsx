import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Registry } from "@/lib/types";
import { ServerToolsPanel } from "./ServerToolsPanel";

const api = vi.hoisted(() => ({
  listServerTools: vi.fn(),
  callTool: vi.fn(),
  setToolEnabled: vi.fn(),
}));
vi.mock("@/lib/api", () => api);
vi.mock("@/lib/toast", () => ({ toastError: vi.fn() }));
const registry = {
  servers: [{ id: "alpha", name: "Alpha", disabledTools: ["remove"] }],
  profiles: [{ id: "local", name: "Local", enabledServerIds: ["alpha"] }],
  activeProfileId: "local",
} as Registry;
const tools = [
  {
    name: "read",
    toolportQuarantine: "clear",
    description: "Read a record",
    annotations: { readOnlyHint: true },
    inputSchema: {
      type: "object",
      required: ["id"],
      properties: { id: { type: "integer" } },
    },
  },
  {
    name: "remove",
    toolportQuarantine: "quarantined",
    description: "Remove a record",
    annotations: { destructiveHint: true },
    inputSchema: { type: "object" },
  },
];

describe("server Tools panel", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    api.listServerTools.mockResolvedValue(tools);
    api.callTool.mockResolvedValue({ content: [{ type: "text", text: "Record found" }] });
  });

  it("loads only its server and shows hints, disabled tools and quarantine", async () => {
    render(
      <ServerToolsPanel
        serverId="alpha"
        registry={registry}
        onRegistryChange={vi.fn()}
      />,
    );
    await screen.findByText("read");
    expect(api.listServerTools).toHaveBeenCalledWith("alpha");
    expect(screen.getByText("read-only")).toBeVisible();
    expect(screen.getByText("destructive")).toBeVisible();
    expect(screen.getByText("Disabled")).toBeVisible();
    expect(screen.getByText("Quarantined")).toBeVisible();
    expect(screen.queryByText("Pick a server…")).not.toBeInTheDocument();
  });

  it("validates schema arguments, calls the selected server and renders the result", async () => {
    render(
      <ServerToolsPanel
        serverId="alpha"
        registry={registry}
        onRegistryChange={vi.fn()}
      />,
    );
    fireEvent.click(await screen.findByRole("button", { name: /Read a record/ }));
    fireEvent.click(screen.getByRole("button", { name: "Call tool" }));
    expect(api.callTool).not.toHaveBeenCalled();
    expect(screen.getByText(/Fill in required field/)).toBeVisible();
    fireEvent.change(screen.getByLabelText(/id/), { target: { value: "42" } });
    fireEvent.click(screen.getByRole("button", { name: "Call tool" }));
    await waitFor(() =>
      expect(api.callTool).toHaveBeenCalledWith("alpha", "read", { id: 42 }),
    );
    expect(await screen.findByText("Record found")).toBeVisible();
  });

  it("refreshes quarantine state when the active profile changes", async () => {
    const view = render(
      <ServerToolsPanel
        serverId="alpha"
        registry={registry}
        onRegistryChange={vi.fn()}
      />,
    );
    await screen.findByText("Quarantined");
    api.listServerTools.mockResolvedValue(
      tools.map((tool) => ({ ...tool, toolportQuarantine: "clear" })),
    );
    view.rerender(
      <ServerToolsPanel
        serverId="alpha"
        registry={{ ...registry, activeProfileId: "other" }}
        onRegistryChange={vi.fn()}
      />,
    );
    await waitFor(() =>
      expect(screen.queryByText("Quarantined")).not.toBeInTheDocument(),
    );
    expect(screen.getAllByText("Not quarantined")).toHaveLength(2);
  });

  it("reports unknown quarantine when the retained state cannot be read", async () => {
    api.listServerTools.mockResolvedValue(
      tools.map((tool) => ({ ...tool, toolportQuarantine: "unknown" })),
    );
    render(
      <ServerToolsPanel
        serverId="alpha"
        registry={registry}
        onRegistryChange={vi.fn()}
      />,
    );
    await screen.findByText("read");
    expect(screen.getAllByText("Quarantine unknown")).toHaveLength(2);
  });
});
