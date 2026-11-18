import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Registry } from "@/lib/types";
import { ServerToolsPanel } from "./ServerToolsPanel";

const api = vi.hoisted(() => ({
  listServerTools: vi.fn(),
  callTool: vi.fn(),
  cancelToolCall: vi.fn(),
  onToolRunProgress: vi.fn(),
  setToolEnabled: vi.fn(),
  listServerResources: vi.fn(),
  listServerPrompts: vi.fn(),
  readResource: vi.fn(),
  getPrompt: vi.fn(),
}));
vi.mock("@/lib/api", () => api);

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}
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
    api.cancelToolCall.mockResolvedValue(undefined);
    api.onToolRunProgress.mockResolvedValue(() => {});
    api.listServerResources.mockResolvedValue([]);
    api.listServerPrompts.mockResolvedValue([]);
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
    expect(screen.getByText("Off")).toBeVisible();
    expect(screen.getByText("Quarantined")).toBeVisible();
    expect(screen.queryByText("Pick a server…")).not.toBeInTheDocument();
  });

  it("names each tool switch by its tool and describes its current state", async () => {
    const { rerender } = render(
      <ServerToolsPanel
        serverId="alpha"
        registry={registry}
        onRegistryChange={vi.fn()}
      />,
    );
    const on = await screen.findByRole("switch", { name: "read" });
    const off = screen.getByRole("switch", { name: "remove" });
    expect(on).toBeChecked();
    expect(on).toHaveAttribute("title", "Exposed to clients. Hide it.");
    expect(off).not.toBeChecked();
    expect(off).toHaveAttribute("title", "Hidden from clients. Expose it.");

    rerender(
      <ServerToolsPanel
        serverId="alpha"
        registry={{
          ...registry,
          servers: [{ ...registry.servers[0], disabledTools: ["read"] }],
        }}
        onRegistryChange={vi.fn()}
      />,
    );
    expect(screen.getByRole("switch", { name: "read" })).not.toBeChecked();
    expect(screen.getByRole("switch", { name: "read" })).toHaveAttribute(
      "title",
      "Hidden from clients. Expose it.",
    );
    expect(screen.getByRole("switch", { name: "remove" })).toBeChecked();
    expect(screen.queryByRole("switch", { name: /^Enable / })).not.toBeInTheDocument();

    // With destructive tools hidden by the global switch, a destructive tool's
    // switch says so whichever way it is set; other tools keep their wording.
    const blocked =
      "Destructive tools are hidden from clients by the global switch even when individually enabled.";
    for (const disabledTools of [["read"], ["read", "remove"]]) {
      rerender(
        <ServerToolsPanel
          serverId="alpha"
          registry={{
            ...registry,
            denyDestructive: true,
            servers: [{ ...registry.servers[0], disabledTools }],
          }}
          onRegistryChange={vi.fn()}
        />,
      );
      expect(screen.getByRole("switch", { name: "remove" })).toHaveAttribute(
        "title",
        blocked,
      );
      expect(screen.getByRole("switch", { name: "read" })).toHaveAttribute(
        "title",
        "Hidden from clients. Expose it.",
      );
    }
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
      expect(api.callTool).toHaveBeenCalledWith(
        "alpha",
        "read",
        { id: 42 },
        expect.any(String),
      ),
    );
    expect(await screen.findByText("Record found")).toBeVisible();
  });

  it("says the server is starting and cancels that Run in the gateway", async () => {
    const call = deferred<unknown>();
    api.callTool.mockReturnValue(call.promise);
    let progress!: (p: { runId: string; phase: string }) => void;
    api.onToolRunProgress.mockImplementation(async (handler) => {
      progress = handler;
      return () => {};
    });
    render(
      <ServerToolsPanel
        serverId="alpha"
        registry={registry}
        onRegistryChange={vi.fn()}
      />,
    );
    fireEvent.click(await screen.findByRole("button", { name: /Read a record/ }));
    fireEvent.change(screen.getByLabelText(/id/), { target: { value: "42" } });
    fireEvent.click(screen.getByRole("button", { name: "Call tool" }));
    await waitFor(() => expect(api.callTool).toHaveBeenCalled());
    const runId = api.callTool.mock.calls[0][3] as string;
    // Another Run's progress is not this one's.
    act(() => progress({ runId: "someone-else", phase: "starting" }));
    expect(screen.getByRole("button", { name: /Calling/ })).toBeVisible();
    act(() => progress({ runId, phase: "starting" }));
    expect(screen.getByRole("button", { name: /Starting Alpha/ })).toBeVisible();
    act(() => progress({ runId, phase: "started" }));
    expect(screen.getByRole("button", { name: /Calling/ })).toBeVisible();
    act(() => progress({ runId, phase: "starting" }));
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(api.cancelToolCall).toHaveBeenCalledWith(runId);
    expect(screen.getByText(/The server was told to cancel the call/)).toBeVisible();
    expect(screen.getByRole("button", { name: "Call tool" })).toBeEnabled();
    // A late result after the cancel is dropped.
    await act(async () => call.resolve({ content: [{ type: "text", text: "Late" }] }));
    expect(screen.queryByText("Late")).not.toBeInTheDocument();
  });

  it("shows a result without Toolport's provenance, keeping the server's own _meta", async () => {
    const labelled = { "app.toolport/provenance": { trust: "untrusted" }, own: 7 };
    api.callTool.mockResolvedValue({
      _meta: labelled,
      content: [
        { type: "text", text: "Record found", _meta: labelled },
        { type: "image", data: "AAA", mimeType: "image/png", _meta: labelled },
        {
          type: "resource",
          resource: { uri: "a:b", _meta: { "app.toolport/provenance": {} } },
        },
      ],
    });
    render(
      <ServerToolsPanel
        serverId="alpha"
        registry={registry}
        onRegistryChange={vi.fn()}
      />,
    );
    fireEvent.click(await screen.findByRole("button", { name: /Read a record/ }));
    fireEvent.change(screen.getByLabelText(/id/), { target: { value: "42" } });
    fireEvent.click(screen.getByRole("button", { name: "Call tool" }));
    const shown = await screen.findByText(/Record found/);
    const text = shown.textContent ?? "";
    expect(text).toContain("image/png");
    expect(text).toContain('"own": 7');
    expect(text).not.toContain("provenance");
    expect(text).not.toContain('"_meta": {}');

    api.callTool.mockResolvedValue({ _meta: labelled, content: [] });
    fireEvent.click(screen.getByRole("button", { name: "Call tool" }));
    await waitFor(() =>
      expect(screen.queryByText(/Record found/)).not.toBeInTheDocument(),
    );
    const fallback = screen.getByText(/"own": 7/).textContent ?? "";
    expect(fallback).not.toContain("provenance");
  });

  it("refreshes quarantine state when the default access changes", async () => {
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
        registry={{ ...registry, defaultAccessProfileId: "other" }}
        onRegistryChange={vi.fn()}
      />,
    );
    await waitFor(() =>
      expect(screen.queryByText("Quarantined")).not.toBeInTheDocument(),
    );
    expect(screen.queryByText("Not quarantined")).not.toBeInTheDocument();
    expect(screen.getByText("On")).toBeVisible();
    expect(screen.getByText("Off")).toBeVisible();
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

  it("retries a failed resources list in place with the same server id", async () => {
    api.listServerResources
      .mockRejectedValueOnce(new Error("resources down"))
      .mockResolvedValueOnce([
        { uri: "file:///notes.txt", name: "notes.txt", mimeType: "text/plain" },
      ]);
    render(
      <ServerToolsPanel
        serverId="alpha"
        registry={registry}
        onRegistryChange={vi.fn()}
      />,
    );
    fireEvent.click(await screen.findByRole("button", { name: "Resources" }));
    expect(await screen.findByText("Error: resources down")).toBeVisible();
    expect(api.listServerResources).toHaveBeenCalledTimes(1);
    expect(api.listServerResources).toHaveBeenCalledWith("alpha");
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(await screen.findByText("notes.txt")).toBeVisible();
    expect(screen.queryByText("Error: resources down")).not.toBeInTheDocument();
    expect(api.listServerResources).toHaveBeenCalledTimes(2);
    expect(api.listServerResources).toHaveBeenNthCalledWith(2, "alpha");
  });

  it("does not duplicate a resources list request when Retry is clicked twice", async () => {
    api.listServerResources
      .mockRejectedValueOnce(new Error("resources down"))
      .mockResolvedValueOnce([
        { uri: "file:///notes.txt", name: "notes.txt", mimeType: "text/plain" },
      ]);
    render(
      <ServerToolsPanel
        serverId="alpha"
        registry={registry}
        onRegistryChange={vi.fn()}
      />,
    );
    fireEvent.click(await screen.findByRole("button", { name: "Resources" }));
    const retry = await screen.findByRole("button", { name: "Retry" });
    fireEvent.click(retry);
    fireEvent.click(retry);
    expect(await screen.findByText("notes.txt")).toBeVisible();
    expect(api.listServerResources).toHaveBeenCalledTimes(2);
  });

  it("ignores a late resources list response after the panel unmounts", async () => {
    const pending = deferred<Array<{ uri: string; name: string }>>();
    api.listServerResources.mockReturnValueOnce(pending.promise);
    const view = render(
      <ServerToolsPanel
        serverId="alpha"
        registry={registry}
        onRegistryChange={vi.fn()}
      />,
    );
    fireEvent.click(await screen.findByRole("button", { name: "Resources" }));
    expect(api.listServerResources).toHaveBeenCalledWith("alpha");
    view.unmount();
    pending.resolve([{ uri: "file:///late.txt", name: "late.txt" }]);
    await Promise.resolve();
    expect(screen.queryByText("late.txt")).not.toBeInTheDocument();
  });

  it("retries a failed prompts list in place with the same server id", async () => {
    api.listServerPrompts
      .mockRejectedValueOnce(new Error("prompts down"))
      .mockResolvedValueOnce([{ name: "summarize", description: "Summarize text" }]);
    render(
      <ServerToolsPanel
        serverId="alpha"
        registry={registry}
        onRegistryChange={vi.fn()}
      />,
    );
    fireEvent.click(await screen.findByRole("button", { name: "Prompts" }));
    expect(await screen.findByText("Error: prompts down")).toBeVisible();
    expect(api.listServerPrompts).toHaveBeenCalledTimes(1);
    expect(api.listServerPrompts).toHaveBeenCalledWith("alpha");
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(await screen.findByText("summarize")).toBeVisible();
    expect(screen.queryByText("Error: prompts down")).not.toBeInTheDocument();
    expect(api.listServerPrompts).toHaveBeenCalledTimes(2);
    expect(api.listServerPrompts).toHaveBeenNthCalledWith(2, "alpha");
  });

  it("does not duplicate a prompts list request when Retry is clicked twice", async () => {
    api.listServerPrompts
      .mockRejectedValueOnce(new Error("prompts down"))
      .mockResolvedValueOnce([{ name: "summarize", description: "Summarize text" }]);
    render(
      <ServerToolsPanel
        serverId="alpha"
        registry={registry}
        onRegistryChange={vi.fn()}
      />,
    );
    fireEvent.click(await screen.findByRole("button", { name: "Prompts" }));
    const retry = await screen.findByRole("button", { name: "Retry" });
    fireEvent.click(retry);
    fireEvent.click(retry);
    expect(await screen.findByText("summarize")).toBeVisible();
    expect(api.listServerPrompts).toHaveBeenCalledTimes(2);
  });

  it("ignores a late prompts list response after the panel unmounts", async () => {
    const pending = deferred<Array<{ name: string }>>();
    api.listServerPrompts.mockReturnValueOnce(pending.promise);
    const view = render(
      <ServerToolsPanel
        serverId="alpha"
        registry={registry}
        onRegistryChange={vi.fn()}
      />,
    );
    fireEvent.click(await screen.findByRole("button", { name: "Prompts" }));
    expect(api.listServerPrompts).toHaveBeenCalledWith("alpha");
    view.unmount();
    pending.resolve([{ name: "late-prompt" }]);
    await Promise.resolve();
    expect(screen.queryByText("late-prompt")).not.toBeInTheDocument();
  });

  const twoResources = [
    { uri: "file:///a.txt", name: "a.txt" },
    { uri: "file:///b.txt", name: "b.txt" },
  ];
  const twoPrompts = [
    { name: "alpha-prompt", description: "First prompt" },
    { name: "beta-prompt", description: "Second prompt" },
  ];

  function renderPanel() {
    return render(
      <ServerToolsPanel
        serverId="alpha"
        registry={registry}
        onRegistryChange={vi.fn()}
      />,
    );
  }

  it("keeps a resource read with the selection that requested it", async () => {
    const readA = deferred<unknown>();
    const readB = deferred<unknown>();
    api.listServerResources.mockResolvedValue(twoResources);
    api.readResource.mockImplementation((_id: string, uri: string) =>
      uri === "file:///a.txt" ? readA.promise : readB.promise,
    );
    renderPanel();
    fireEvent.click(await screen.findByRole("button", { name: "Resources" }));
    fireEvent.click(await screen.findByRole("button", { name: /a\.txt/ }));
    fireEvent.click(screen.getByRole("button", { name: /b\.txt/ }));
    expect(api.readResource).toHaveBeenNthCalledWith(1, "alpha", "file:///a.txt");
    expect(api.readResource).toHaveBeenNthCalledWith(2, "alpha", "file:///b.txt");
    expect(screen.getByRole("button", { name: /b\.txt/ })).toHaveAttribute(
      "aria-pressed",
      "true",
    );

    readB.resolve("contents of B");
    expect(await screen.findByText("contents of B")).toBeVisible();
    readA.resolve("contents of A");
    await act(async () => {
      await readA.promise;
    });
    expect(screen.queryByText("contents of A")).not.toBeInTheDocument();
    expect(screen.getByText("contents of B")).toBeVisible();
  });

  it("does not let a stale resource read error or finish the current read", async () => {
    const readA = deferred<unknown>();
    const readB = deferred<unknown>();
    api.listServerResources.mockResolvedValue(twoResources);
    api.readResource.mockImplementation((_id: string, uri: string) =>
      uri === "file:///a.txt" ? readA.promise : readB.promise,
    );
    renderPanel();
    fireEvent.click(await screen.findByRole("button", { name: "Resources" }));
    fireEvent.click(await screen.findByRole("button", { name: /a\.txt/ }));
    fireEvent.click(screen.getByRole("button", { name: /b\.txt/ }));

    readA.reject(new Error("read A failed"));
    await act(async () => {
      await readA.promise.catch(() => {});
    });
    expect(screen.queryByText("Error: read A failed")).not.toBeInTheDocument();
    expect(screen.getByRole("status", { name: "Reading resource…" })).toBeVisible();

    readB.resolve("contents of B");
    expect(await screen.findByText("contents of B")).toBeVisible();
    expect(
      screen.queryByRole("status", { name: "Reading resource…" }),
    ).not.toBeInTheDocument();
  });

  it("ignores a resource read that finishes after the panel unmounts", async () => {
    const readA = deferred<unknown>();
    api.listServerResources.mockResolvedValue(twoResources);
    api.readResource.mockReturnValueOnce(readA.promise);
    const errors = vi.spyOn(console, "error").mockImplementation(() => {});
    const view = renderPanel();
    fireEvent.click(await screen.findByRole("button", { name: "Resources" }));
    fireEvent.click(await screen.findByRole("button", { name: /a\.txt/ }));
    view.unmount();
    readA.resolve("contents of A");
    await act(async () => {
      await readA.promise;
    });
    expect(screen.queryByText("contents of A")).not.toBeInTheDocument();
    expect(errors).not.toHaveBeenCalled();
    errors.mockRestore();
  });

  it("keeps a prompt result with the selection that requested it", async () => {
    const runA = deferred<unknown>();
    const runB = deferred<unknown>();
    api.listServerPrompts.mockResolvedValue(twoPrompts);
    api.getPrompt.mockImplementation((_id: string, name: string) =>
      name === "alpha-prompt" ? runA.promise : runB.promise,
    );
    renderPanel();
    fireEvent.click(await screen.findByRole("button", { name: "Prompts" }));
    fireEvent.click(await screen.findByRole("button", { name: /First prompt/ }));
    fireEvent.click(screen.getByRole("button", { name: "Get prompt" }));
    expect(screen.getByRole("button", { name: "Getting…" })).toBeDisabled();

    fireEvent.click(screen.getByRole("button", { name: /Second prompt/ }));
    fireEvent.click(screen.getByRole("button", { name: "Get prompt" }));
    expect(api.getPrompt).toHaveBeenNthCalledWith(1, "alpha", "alpha-prompt", {});
    expect(api.getPrompt).toHaveBeenNthCalledWith(2, "alpha", "beta-prompt", {});

    runB.resolve("messages for B");
    expect(await screen.findByText("messages for B")).toBeVisible();
    runA.resolve("messages for A");
    await act(async () => {
      await runA.promise;
    });
    expect(screen.queryByText("messages for A")).not.toBeInTheDocument();
    expect(screen.getByText("messages for B")).toBeVisible();
  });

  it("does not let a stale prompt error or finish the current request", async () => {
    const runA = deferred<unknown>();
    const runB = deferred<unknown>();
    api.listServerPrompts.mockResolvedValue(twoPrompts);
    api.getPrompt.mockImplementation((_id: string, name: string) =>
      name === "alpha-prompt" ? runA.promise : runB.promise,
    );
    renderPanel();
    fireEvent.click(await screen.findByRole("button", { name: "Prompts" }));
    fireEvent.click(await screen.findByRole("button", { name: /First prompt/ }));
    fireEvent.click(screen.getByRole("button", { name: "Get prompt" }));
    fireEvent.click(screen.getByRole("button", { name: /Second prompt/ }));
    fireEvent.click(screen.getByRole("button", { name: "Get prompt" }));

    runA.reject(new Error("prompt A failed"));
    await act(async () => {
      await runA.promise.catch(() => {});
    });
    expect(screen.queryByText("Error: prompt A failed")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Getting…" })).toBeDisabled();

    runB.resolve("messages for B");
    expect(await screen.findByText("messages for B")).toBeVisible();
    expect(screen.getByRole("button", { name: "Get prompt" })).toBeEnabled();
  });

  it("drops a pending prompt when the selection changes without a new request", async () => {
    const runA = deferred<unknown>();
    api.listServerPrompts.mockResolvedValue(twoPrompts);
    api.getPrompt.mockReturnValueOnce(runA.promise);
    renderPanel();
    fireEvent.click(await screen.findByRole("button", { name: "Prompts" }));
    fireEvent.click(await screen.findByRole("button", { name: /First prompt/ }));
    fireEvent.click(screen.getByRole("button", { name: "Get prompt" }));
    fireEvent.click(screen.getByRole("button", { name: /Second prompt/ }));
    expect(screen.getByRole("button", { name: "Get prompt" })).toBeEnabled();

    runA.resolve("messages for A");
    await act(async () => {
      await runA.promise;
    });
    expect(screen.queryByText("messages for A")).not.toBeInTheDocument();
    expect(api.getPrompt).toHaveBeenCalledTimes(1);
  });

  it("ignores a prompt that finishes after the panel unmounts", async () => {
    const runA = deferred<unknown>();
    api.listServerPrompts.mockResolvedValue(twoPrompts);
    api.getPrompt.mockReturnValueOnce(runA.promise);
    const errors = vi.spyOn(console, "error").mockImplementation(() => {});
    const view = renderPanel();
    fireEvent.click(await screen.findByRole("button", { name: "Prompts" }));
    fireEvent.click(await screen.findByRole("button", { name: /First prompt/ }));
    fireEvent.click(screen.getByRole("button", { name: "Get prompt" }));
    view.unmount();
    runA.reject(new Error("prompt A failed"));
    await act(async () => {
      await runA.promise.catch(() => {});
    });
    expect(screen.queryByText("Error: prompt A failed")).not.toBeInTheDocument();
    expect(errors).not.toHaveBeenCalled();
    errors.mockRestore();
  });
});
