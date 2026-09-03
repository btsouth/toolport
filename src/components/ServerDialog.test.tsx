import { beforeEach, describe, it, expect, vi } from "vitest";
import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ProbeResult, Registry, ServerEntry } from "@/lib/types";

const api = vi.hoisted(() => ({
  addServer: vi.fn(),
  parseServerSnippet: vi.fn(),
  setSecret: vi.fn(),
  testServer: vi.fn(),
  updateServer: vi.fn(),
}));
const toast = vi.hoisted(() => ({
  success: vi.fn(),
  warning: vi.fn(),
}));

vi.mock("@/lib/api", () => api);
vi.mock("sonner", () => ({ toast }));
vi.mock("@/lib/toast", () => ({ toastError: vi.fn() }));

import { ServerDialog } from "./ServerDialog";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

const success: ProbeResult = {
  serverId: "",
  ok: true,
  toolCount: 2,
  error: null,
  authRequired: false,
};

const failure: ProbeResult = {
  serverId: "",
  ok: false,
  toolCount: 0,
  error: "The updated command could not connect.",
  authRequired: false,
};

function savedRegistry(id: string): Registry {
  return {
    version: 1,
    servers: [
      {
        id,
        name: "demo",
        transport: "stdio",
        command: "npx",
        args: [],
        env: ["KEY_A", "KEY_B", "KEY_C"].map((key) => ({
          key,
          value: null,
          secret: true,
        })),
        url: null,
        source: "manual",
      },
    ],
    profiles: [],
    activeProfileId: null,
  };
}

async function fillServer(user: ReturnType<typeof userEvent.setup>, command: string) {
  await user.type(screen.getByLabelText("Name"), "demo");
  await user.type(screen.getByLabelText("Command"), command);
}

describe("ServerDialog", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("opens from its trigger and shows the add form", async () => {
    render(<ServerDialog trigger={<button>Add server</button>} onSaved={vi.fn()} />);
    expect(screen.queryByText("Add MCP server")).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Add server" }));
    expect(screen.getByText("Add MCP server")).toBeInTheDocument();
    expect(screen.getByLabelText("Name")).toBeInTheDocument();
  });

  it("gates the Add button until the required fields are filled", async () => {
    render(<ServerDialog trigger={<button>Add server</button>} onSaved={vi.fn()} />);
    await userEvent.click(screen.getByRole("button", { name: "Add server" }));

    // Empty name + command => blocked.
    expect(screen.getByRole("button", { name: "Add" })).toBeDisabled();

    await userEvent.type(screen.getByLabelText("Name"), "stripe");
    // Name alone isn't enough for a stdio server; the command is still missing.
    expect(screen.getByRole("button", { name: "Add" })).toBeDisabled();

    await userEvent.type(screen.getByLabelText("Command"), "npx");
    expect(screen.getByRole("button", { name: "Add" })).toBeEnabled();
  });

  it("does not reactivate a local-only timeout when switching to HTTP", async () => {
    const initial: ServerEntry = {
      id: "local",
      name: "Local",
      transport: "stdio",
      command: "local-server",
      args: [],
      env: [],
      url: null,
      source: "manual",
      requestTimeoutMs: 90_000,
    };
    api.updateServer.mockResolvedValueOnce(savedRegistry("local"));
    const user = userEvent.setup();

    render(<ServerDialog autoOpen editId="local" initial={initial} onSaved={vi.fn()} />);
    await user.click(screen.getByLabelText("Transport"));
    await user.click(screen.getByRole("option", { name: "http (remote)" }));
    await user.type(screen.getByLabelText("URL"), "https://mcp.example.com/mcp");
    await user.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(api.updateServer).toHaveBeenCalledTimes(1));
    expect(api.updateServer).toHaveBeenCalledWith(
      expect.objectContaining({
        id: "local",
        transport: "http",
        requestTimeoutMs: null,
      }),
    );
  });

  it("closes on Cancel when it owns its open state (header add flow)", async () => {
    render(<ServerDialog trigger={<button>Add server</button>} onSaved={vi.fn()} />);
    await userEvent.click(screen.getByRole("button", { name: "Add server" }));
    expect(screen.getByText("Add MCP server")).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.queryByText("Add MCP server")).not.toBeInTheDocument();
  });

  it("delegates dismissal to onClose when the parent controls it (autoOpen flow)", async () => {
    const onClose = vi.fn();
    render(
      <ServerDialog
        trigger={<button>ignored</button>}
        onSaved={vi.fn()}
        autoOpen
        onClose={onClose}
      />,
    );
    // autoOpen renders it open immediately.
    expect(screen.getByText("Add MCP server")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("reports partial secret failure and retries by updating the added server", async () => {
    const added = savedRegistry("demo");
    const afterFirstSecret = savedRegistry("demo");
    const updated = savedRegistry("demo");
    const afterRetryA = savedRegistry("demo");
    const afterRetryB = savedRegistry("demo");
    const afterRetryC = savedRegistry("demo");
    api.addServer.mockResolvedValueOnce(added);
    api.updateServer.mockResolvedValueOnce(updated);
    api.setSecret
      .mockResolvedValueOnce(afterFirstSecret)
      .mockRejectedValueOnce(new Error("keychain locked"))
      .mockRejectedValueOnce(new Error("keychain locked"))
      .mockResolvedValueOnce(afterRetryA)
      .mockResolvedValueOnce(afterRetryB)
      .mockResolvedValueOnce(afterRetryC);
    const onSaved = vi.fn();
    const user = userEvent.setup();

    render(<ServerDialog trigger={<button>Add server</button>} onSaved={onSaved} />);
    await user.click(screen.getByRole("button", { name: "Add server" }));
    await fillServer(user, "npx");

    for (let i = 0; i < 3; i += 1) {
      await user.click(screen.getByRole("button", { name: "Add variable" }));
    }
    const keyInputs = screen.getAllByPlaceholderText("ENV_NAME");
    const valueInputs = screen.getAllByPlaceholderText("value");
    for (const [i, key] of ["KEY_A", "KEY_B", "KEY_C"].entries()) {
      await user.type(keyInputs[i], key);
      await user.type(valueInputs[i], `secret-${i}`);
    }

    await user.click(screen.getByRole("button", { name: "Add" }));

    await waitFor(() => expect(api.setSecret).toHaveBeenCalledTimes(3));
    expect(api.setSecret).toHaveBeenNthCalledWith(1, "demo", "KEY_A", "secret-0");
    expect(api.setSecret).toHaveBeenNthCalledWith(2, "demo", "KEY_B", "secret-1");
    expect(api.setSecret).toHaveBeenNthCalledWith(3, "demo", "KEY_C", "secret-2");
    expect(onSaved).toHaveBeenLastCalledWith(afterFirstSecret);
    expect(toast.warning).toHaveBeenCalledWith(
      "Added demo, but couldn't save: KEY_B, KEY_C",
    );
    expect(screen.getByText("Edit server")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Save" })).toBeEnabled();

    await user.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(api.setSecret).toHaveBeenCalledTimes(6));
    expect(api.addServer).toHaveBeenCalledTimes(1);
    expect(api.updateServer).toHaveBeenCalledTimes(1);
    expect(api.updateServer).toHaveBeenCalledWith(
      expect.objectContaining({ id: "demo", name: "demo" }),
    );
    expect(onSaved).toHaveBeenLastCalledWith(afterRetryC);
    expect(toast.success).toHaveBeenCalledWith("Saved demo");
    expect(screen.queryByText("Edit server")).not.toBeInTheDocument();
  });

  it("keeps an edited in-flight test busy and discards its result", async () => {
    const request = deferred<ProbeResult>();
    api.testServer.mockReturnValueOnce(request.promise);
    const user = userEvent.setup();

    render(<ServerDialog trigger={<button>Add server</button>} onSaved={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: "Add server" }));
    await fillServer(user, "working-command");
    await user.click(screen.getByRole("button", { name: "Test connection" }));

    expect(api.testServer).toHaveBeenCalledWith(
      expect.objectContaining({ command: "working-command" }),
    );
    expect(screen.getByRole("button", { name: /testing/i })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Add" })).toBeDisabled();

    await user.clear(screen.getByLabelText("Command"));
    await user.type(screen.getByLabelText("Command"), "broken-command");

    expect(screen.getByRole("button", { name: /testing/i })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Add" })).toBeDisabled();

    await act(async () => request.resolve(success));

    expect(screen.queryByText(/Connected\. Found/)).not.toBeInTheDocument();
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Test connection" })).toBeEnabled(),
    );
    expect(screen.getByRole("button", { name: "Add" })).toBeEnabled();
  });

  it("keeps an in-flight test busy when parsed config replaces the form", async () => {
    const request = deferred<ProbeResult>();
    api.testServer.mockReturnValueOnce(request.promise);
    api.parseServerSnippet.mockResolvedValueOnce([
      {
        name: "parsed",
        transport: "stdio",
        command: "parsed-command",
        args: [],
        url: null,
        env: [],
      },
    ]);
    const user = userEvent.setup();

    render(<ServerDialog trigger={<button>Add server</button>} onSaved={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: "Add server" }));
    await fillServer(user, "working-command");
    await user.click(screen.getByRole("button", { name: "Test connection" }));

    await user.click(screen.getByRole("button", { name: "Paste from client config" }));
    await user.type(
      screen.getByPlaceholderText(/Paste a config snippet/i),
      "parsed config",
    );
    await user.click(screen.getByRole("button", { name: "Parse & fill" }));

    await waitFor(() =>
      expect(screen.getByLabelText("Command")).toHaveValue("parsed-command"),
    );
    expect(screen.getByRole("button", { name: /testing/i })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Add" })).toBeDisabled();

    await act(async () => request.resolve(success));

    expect(screen.queryByText(/Connected\. Found/)).not.toBeInTheDocument();
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Test connection" })).toBeEnabled(),
    );
    expect(screen.getByRole("button", { name: "Add" })).toBeEnabled();
  });

  it("keeps a newer result when tests resolve out of order after reopening", async () => {
    const first = deferred<ProbeResult>();
    const second = deferred<ProbeResult>();
    api.testServer.mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);
    const user = userEvent.setup();

    render(<ServerDialog trigger={<button>Add server</button>} onSaved={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: "Add server" }));
    await fillServer(user, "working-command");
    await user.click(screen.getByRole("button", { name: "Test connection" }));

    await user.click(screen.getByRole("button", { name: "Cancel" }));
    await user.click(screen.getByRole("button", { name: "Add server" }));
    await fillServer(user, "broken-command");
    await user.click(screen.getByRole("button", { name: "Test connection" }));

    expect(api.testServer).toHaveBeenNthCalledWith(
      1,
      expect.objectContaining({ command: "working-command" }),
    );
    expect(api.testServer).toHaveBeenNthCalledWith(
      2,
      expect.objectContaining({ command: "broken-command" }),
    );

    await act(async () => second.resolve(failure));
    expect(screen.getByText(failure.error as string)).toBeInTheDocument();

    await act(async () => first.resolve(success));
    expect(screen.getByText(failure.error as string)).toBeInTheDocument();
    expect(screen.queryByText(/Connected\. Found/)).not.toBeInTheDocument();
  });
});
