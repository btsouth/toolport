import { beforeEach, describe, it, expect, vi } from "vitest";
import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ProbeResult } from "@/lib/types";

const api = vi.hoisted(() => ({
  addServer: vi.fn(),
  parseServerSnippet: vi.fn(),
  setSecret: vi.fn(),
  testServer: vi.fn(),
  updateServer: vi.fn(),
}));

vi.mock("@/lib/api", () => api);

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
