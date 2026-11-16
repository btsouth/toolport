import { beforeEach, describe, it, expect, vi } from "vitest";
import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ProbeResult, Registry, ServerEntry } from "@/lib/types";

const api = vi.hoisted(() => ({
  addServer: vi.fn(),
  addSnippetServers: vi.fn(),
  setServerEnabled: vi.fn(),
  parseServerSnippet: vi.fn(),
  setSecret: vi.fn(),
  setLaunchSecret: vi.fn(),
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

import { toastError } from "@/lib/toast";
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
    api.setServerEnabled.mockResolvedValue(savedRegistry("demo"));
  });

  it("waits for each field to blur before showing its validation error", async () => {
    const user = userEvent.setup();
    render(<ServerDialog autoOpen onSaved={vi.fn()} />);
    expect(screen.queryByText("Give the server a name.")).not.toBeInTheDocument();
    expect(screen.queryByText(/Enter the command to run/)).not.toBeInTheDocument();
    await user.click(screen.getByLabelText("Name"));
    await user.tab();
    expect(screen.getByText("Give the server a name.")).toBeInTheDocument();
    expect(screen.queryByText(/Enter the command to run/)).not.toBeInTheDocument();
    await user.type(screen.getByLabelText("Name"), "demo");
    expect(screen.queryByText("Give the server a name.")).not.toBeInTheDocument();
    await user.click(screen.getByLabelText("Command"));
    await user.tab();
    expect(screen.getByText(/Enter the command to run/)).toBeInTheDocument();
  });

  it("vaults composed launch inputs and clears bindings when arguments change", async () => {
    const initial: ServerEntry = {
      id: "",
      name: "Twilio",
      transport: "stdio",
      command: "npx",
      args: ["-y", "@twilio-alpha/mcp", "<launch-input>"],
      env: [],
      url: null,
      source: "catalog:curated",
      launch: {
        inputs: [
          { key: "ACCOUNT", label: "Account SID", secret: false, required: true },
          { key: "KEY", label: "API Key SID", secret: true, required: true },
          { key: "SECRET", label: "API Secret", secret: true, required: true },
        ],
        bindings: [
          {
            index: 2,
            parts: [
              { kind: "input", key: "ACCOUNT" },
              { kind: "literal", value: "/" },
              { kind: "input", key: "KEY" },
              { kind: "literal", value: ":" },
              { kind: "input", key: "SECRET" },
            ],
          },
        ],
      },
    };
    const added = savedRegistry("twilio");
    api.addServer.mockResolvedValue(added);
    api.setLaunchSecret.mockResolvedValue(added);
    const user = userEvent.setup();
    render(<ServerDialog autoOpen initial={initial} onSaved={vi.fn()} />);
    await user.type(screen.getByLabelText("Account SID *"), "AC123");
    await user.type(screen.getByLabelText("API Key SID *"), "SK123");
    await user.type(screen.getByLabelText("API Secret *"), "private-value");
    await user.click(screen.getByRole("button", { name: "Add" }));
    await waitFor(() => expect(api.setLaunchSecret).toHaveBeenCalledTimes(2));
    expect(api.addServer).toHaveBeenCalledWith(
      expect.objectContaining({
        launch: expect.objectContaining({
          inputs: [
            expect.objectContaining({ value: "AC123" }),
            expect.objectContaining({ value: null }),
            expect.objectContaining({ value: null }),
          ],
        }),
      }),
    );
    expect(api.setLaunchSecret).toHaveBeenCalledWith("twilio", "SECRET", "private-value");

    render(<ServerDialog autoOpen initial={initial} onSaved={vi.fn()} />);
    await user.type(screen.getAllByLabelText("Arguments")[0], " extra");
    expect(screen.getByText(/Catalog launch setup was removed/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Add" })).toBeDisabled();
  });

  it("clears requirements when customizing a preset without argument bindings", async () => {
    const initial: ServerEntry = {
      id: "qdrant",
      name: "Qdrant",
      transport: "stdio",
      command: "uvx",
      args: ["mcp-server-qdrant"],
      env: [{ key: "QDRANT_URL", value: null, secret: true }],
      url: null,
      source: "catalog:curated",
      launch: {
        inputs: [],
        bindings: [],
        requiredEnv: ["QDRANT_URL"],
      },
    };
    api.updateServer.mockResolvedValue(savedRegistry("qdrant"));
    const user = userEvent.setup();
    render(<ServerDialog autoOpen editId="qdrant" initial={initial} onSaved={vi.fn()} />);

    await user.type(screen.getByLabelText("Arguments"), " --transport stdio");
    expect(screen.getByText(/Catalog launch setup was removed/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(api.updateServer).toHaveBeenCalledTimes(1));
    expect(api.updateServer).toHaveBeenCalledWith(
      expect.objectContaining({ launch: null, source: "manual" }),
    );
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
    expect(screen.getByLabelText("Startup timeout (optional)")).toHaveAttribute(
      "placeholder",
      "30",
    );
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

  it("keeps fields the dialog does not show and saves the shell environment switch", async () => {
    const initial = {
      id: "local",
      name: "Local",
      transport: "stdio",
      command: "local-server",
      args: [],
      env: [],
      url: null,
      source: "manual",
      disabledTools: ["delete_everything"],
      inheritEnv: true,
      futureField: "kept",
    } as ServerEntry;
    api.updateServer.mockResolvedValue(savedRegistry("local"));
    const user = userEvent.setup();

    render(<ServerDialog autoOpen editId="local" initial={initial} onSaved={vi.fn()} />);
    const inherit = screen.getByRole("switch", { name: "Use my shell environment" });
    expect(inherit).toBeChecked();
    await user.type(screen.getByLabelText("Name"), " renamed");
    await user.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(api.updateServer).toHaveBeenCalledTimes(1));
    expect(api.updateServer).toHaveBeenCalledWith(
      expect.objectContaining({
        name: "Local renamed",
        disabledTools: ["delete_everything"],
        inheritEnv: true,
        futureField: "kept",
      }),
    );
  });

  it("turns the shell environment off for a server", async () => {
    const initial: ServerEntry = {
      id: "local",
      name: "Local",
      transport: "stdio",
      command: "local-server",
      args: [],
      env: [],
      url: null,
      source: "manual",
      inheritEnv: true,
    };
    api.updateServer.mockResolvedValue(savedRegistry("local"));
    const user = userEvent.setup();

    render(<ServerDialog autoOpen editId="local" initial={initial} onSaved={vi.fn()} />);
    await user.click(screen.getByRole("switch", { name: "Use my shell environment" }));
    await user.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(api.updateServer).toHaveBeenCalledTimes(1));
    expect(api.updateServer).toHaveBeenCalledWith(
      expect.objectContaining({ inheritEnv: false }),
    );
  });

  it("hides a server's prompts and keeps the rest of the entry", async () => {
    const initial: ServerEntry = {
      id: "docs",
      name: "Docs",
      transport: "stdio",
      command: "docs-server",
      args: [],
      env: [],
      url: null,
      source: "manual",
      disabledTools: ["delete_everything"],
    };
    api.updateServer.mockResolvedValue(savedRegistry("docs"));
    const user = userEvent.setup();

    render(<ServerDialog autoOpen editId="docs" initial={initial} onSaved={vi.fn()} />);
    const hide = screen.getByRole("switch", { name: "Hide prompts" });
    expect(hide).not.toBeChecked();
    await user.click(hide);
    await user.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(api.updateServer).toHaveBeenCalledTimes(1));
    expect(api.updateServer).toHaveBeenCalledWith(
      expect.objectContaining({
        hidePrompts: true,
        disabledTools: ["delete_everything"],
      }),
    );
  });

  it("never offers variables on a remote server, and keeps the ones it has", async () => {
    const refusal =
      "Remote servers don't read environment variables, so Toolport would send API_KEY to this server as its bearer token. Remove it, or name it Authorization if it is this server's token.";
    const note =
      "Remote servers have no environment. When this server has no other token, Toolport sends the first stored value to it as its bearer token.";
    const user = userEvent.setup();
    // A new remote server has no Environment variables section.
    const { unmount } = render(
      <ServerDialog trigger={<button>Add server</button>} onSaved={vi.fn()} />,
    );
    await user.click(screen.getByRole("button", { name: "Add server" }));
    expect(screen.getByRole("button", { name: "Add variable" })).toBeVisible();
    await user.click(screen.getByLabelText("Transport"));
    await user.click(screen.getByRole("option", { name: "http (remote)" }));
    expect(screen.queryByText("Environment variables")).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Add variable" }),
    ).not.toBeInTheDocument();
    unmount();

    // A remote server that already keeps its token in a variable shows it,
    // says what it is, and saves.
    const legacy: ServerEntry = {
      id: "remote",
      name: "Remote",
      transport: "http",
      command: null,
      args: [],
      env: [{ key: "MY_MCP_TOKEN", value: null, secret: true }],
      url: "https://mcp.example.com/mcp",
      source: "manual",
    };
    const second = render(
      <ServerDialog autoOpen editId="remote" initial={legacy} onSaved={vi.fn()} />,
    );
    expect(screen.getByDisplayValue("MY_MCP_TOKEN")).toBeVisible();
    expect(screen.getByText(note)).toBeVisible();
    expect(
      screen.queryByRole("button", { name: "Add variable" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByText(refusal.replace("API_KEY", "MY_MCP_TOKEN")),
    ).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Save" })).toBeEnabled();
    second.unmount();

    // A stdio server's variable does not come along to http.
    const local: ServerEntry = {
      id: "local",
      name: "Local",
      transport: "stdio",
      command: "local-server",
      args: [],
      env: [{ key: "API_KEY", value: "plain", secret: false }],
      url: null,
      source: "manual",
    };
    render(<ServerDialog autoOpen editId="local" initial={local} onSaved={vi.fn()} />);
    await user.click(screen.getByLabelText("Transport"));
    await user.click(screen.getByRole("option", { name: "http (remote)" }));
    await user.type(screen.getByLabelText("URL"), "https://mcp.example.com/mcp");
    expect(screen.getByText(refusal)).toBeVisible();
    expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
    const key = screen.getByDisplayValue("API_KEY");
    await user.clear(key);
    await user.type(key, "Authorization");
    expect(screen.queryByText(refusal)).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Save" })).toBeEnabled();
  });

  it("refuses an older remote plain value made secret before saving", async () => {
    // The backend keeps an older remote row only if it could already be sent.
    // A plain value moved to the keychain would become the server's token.
    const user = userEvent.setup();
    const older: ServerEntry = {
      id: "remote",
      name: "Remote",
      transport: "http",
      command: null,
      args: [],
      env: [{ key: "REGION", value: "eu", secret: false }],
      url: "https://mcp.example.com/mcp",
      source: "manual",
    };
    render(<ServerDialog autoOpen editId="remote" initial={older} onSaved={vi.fn()} />);
    const refusal =
      "Remote servers don't read environment variables, so Toolport would send REGION to this server as its bearer token. Remove it, or name it Authorization if it is this server's token.";
    expect(screen.queryByText(refusal)).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Save" })).toBeEnabled();
    await user.click(screen.getByLabelText("Keep REGION in keychain"));
    expect(screen.getByText(refusal)).toBeVisible();
    expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Test connection" })).toBeDisabled();
  });

  it("sends an edited Authorization value with the save, not after it", async () => {
    // The save stores the token before it writes the registry, so a locked
    // keychain cannot leave the server saved without it.
    const user = userEvent.setup();
    const local: ServerEntry = {
      id: "local",
      name: "Local",
      transport: "stdio",
      command: "local-server",
      args: [],
      env: [{ key: "TOKEN", value: "tok-1", secret: false }],
      url: null,
      source: "manual",
    };
    api.updateServer.mockResolvedValueOnce(savedRegistry("local"));
    render(<ServerDialog autoOpen editId="local" initial={local} onSaved={vi.fn()} />);
    await user.click(screen.getByLabelText("Transport"));
    await user.click(screen.getByRole("option", { name: "http (remote)" }));
    await user.type(screen.getByLabelText("URL"), "https://mcp.example.com/mcp");
    const key = screen.getByDisplayValue("TOKEN");
    await user.clear(key);
    await user.type(key, "Authorization");
    await user.click(screen.getByLabelText("Keep Authorization in keychain"));
    await user.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(api.updateServer).toHaveBeenCalledTimes(1));
    expect(api.updateServer.mock.calls[0][0].env).toEqual([
      { key: "Authorization", value: "tok-1", secret: true },
    ]);
    expect(api.setSecret).not.toHaveBeenCalled();
  });

  it("does not rename a saved credential", async () => {
    // Its value is in the keychain under its name, so a new name would have
    // none. GTK shows the same hint on its saved rows.
    const initial: ServerEntry = {
      id: "local",
      name: "Local",
      transport: "stdio",
      command: "local-server",
      args: [],
      env: [
        { key: "API_KEY", value: null, secret: true },
        { key: "REGION", value: "eu", secret: false },
      ],
      url: null,
      source: "manual",
    };
    render(<ServerDialog autoOpen editId="local" initial={initial} onSaved={vi.fn()} />);
    const stored = screen.getByDisplayValue("API_KEY");
    expect(stored).toHaveAttribute("readonly");
    expect(stored).toHaveAttribute(
      "title",
      "A saved credential can't be renamed. Remove it and add a new one with the name you want.",
    );
    expect(screen.getByDisplayValue("REGION")).not.toHaveAttribute("readonly");
  });

  it("saves the startup timeout in milliseconds", async () => {
    const initial: ServerEntry = {
      id: "remote",
      name: "Remote",
      transport: "http",
      command: null,
      args: [],
      env: [],
      url: "https://mcp.example.com/mcp",
      source: "manual",
      requestTimeoutMs: 90_000,
      initializeTimeoutMs: 240_000,
    };
    api.updateServer.mockResolvedValueOnce(savedRegistry("remote"));
    const user = userEvent.setup();

    render(<ServerDialog autoOpen editId="remote" initial={initial} onSaved={vi.fn()} />);
    const input = screen.getByLabelText("Startup timeout (optional)");
    expect(input).toHaveValue(240);
    expect(input).toHaveAttribute("placeholder", "90");
    await user.clear(input);
    await user.type(input, "300.5");
    await user.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() => expect(api.updateServer).toHaveBeenCalledTimes(1));
    expect(api.updateServer).toHaveBeenCalledWith(
      expect.objectContaining({
        id: "remote",
        initializeTimeoutMs: 300_500,
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

  it("leaves a new plain value's sync choice to the default unless changed", async () => {
    api.addServer.mockResolvedValueOnce(savedRegistry("demo"));
    const user = userEvent.setup();
    render(<ServerDialog trigger={<button>Add server</button>} onSaved={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: "Add server" }));
    await fillServer(user, "npx");
    for (let i = 0; i < 3; i += 1) {
      await user.click(screen.getByRole("button", { name: "Add variable" }));
    }
    const keyInputs = screen.getAllByPlaceholderText("ENV_NAME");
    for (const [i, [key, value]] of [
      ["REGION", "west"],
      ["NOTES_DIR", "/home/me/notes"],
      ["NODE_OPTIONS", "--max-old-space-size=4096"],
    ].entries()) {
      await user.type(keyInputs[i], key);
      await user.click(screen.getByRole("checkbox", { name: `Keep ${key} in keychain` }));
      await user.type(screen.getAllByPlaceholderText("value")[i], value);
    }
    const sync = screen.getAllByRole("checkbox", { name: "Same on every machine" });
    expect(sync.map((box) => (box as HTMLInputElement).checked)).toEqual([
      true,
      false,
      false,
    ]);
    expect(sync[2]).toBeDisabled();
    await user.click(sync[1]);

    await user.click(screen.getByRole("button", { name: "Add" }));

    await waitFor(() => expect(api.addServer).toHaveBeenCalledTimes(1));
    const env = api.addServer.mock.calls[0][0].env;
    expect(env).toEqual([
      { key: "REGION", value: "west", secret: false },
      { key: "NOTES_DIR", value: "/home/me/notes", secret: false, portable: true },
      { key: "NODE_OPTIONS", value: "--max-old-space-size=4096", secret: false },
    ]);
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

  it("names pasted keys that still need a value until they are filled", async () => {
    api.parseServerSnippet.mockResolvedValueOnce([
      {
        name: "gh",
        transport: "http",
        command: null,
        args: [],
        url: "https://api.githubcopilot.com/mcp",
        env: [
          { key: "Authorization", value: null, secret: true },
          { key: "REGION", value: "eu", secret: false },
        ],
      },
    ]);
    const user = userEvent.setup();
    render(<ServerDialog trigger={<button>Add server</button>} onSaved={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: "Add server" }));
    await user.click(screen.getByRole("button", { name: "Paste from client config" }));
    await user.type(screen.getByPlaceholderText(/Paste a config snippet/i), "pasted");
    await user.click(screen.getByRole("button", { name: "Parse & fill" }));

    const notice =
      "Add a value for Authorization before saving. The pasted config did not include it.";
    expect(await screen.findByText(notice)).toBeInTheDocument();
    const values = screen.getAllByPlaceholderText("value");
    await user.type(values[0], "Bearer abc");
    expect(screen.queryByText(notice)).not.toBeInTheDocument();
  });

  it("saves pasted empty and padded arguments exactly", async () => {
    const args = ["server.js", "", " padded "];
    api.parseServerSnippet.mockResolvedValueOnce([
      { name: "demo", transport: "stdio", command: "node", args, url: null, env: [] },
    ]);
    api.addServer.mockResolvedValueOnce(savedRegistry("demo"));
    const user = userEvent.setup();
    render(<ServerDialog trigger={<button>Add server</button>} onSaved={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: "Add server" }));
    await user.click(screen.getByRole("button", { name: "Paste from client config" }));
    await user.type(screen.getByPlaceholderText(/Paste a config snippet/i), "pasted");
    await user.click(screen.getByRole("button", { name: "Parse & fill" }));
    await waitFor(() =>
      expect(screen.getByLabelText("Arguments")).toHaveValue('server.js "" " padded "'),
    );
    await user.click(screen.getByRole("button", { name: "Add" }));

    await waitFor(() => expect(api.addServer).toHaveBeenCalledTimes(1));
    expect(api.addServer.mock.calls[0][0].args).toEqual(args);
  });

  it("refuses two variables with one name, as GTK does", async () => {
    api.parseServerSnippet.mockResolvedValueOnce([
      {
        name: "dup",
        transport: "stdio",
        command: "npx",
        args: [],
        url: null,
        env: [
          { key: "API_KEY", value: "abc", secret: true },
          { key: "TOKEN", value: null, secret: true },
        ],
      },
    ]);
    const user = userEvent.setup();
    render(<ServerDialog trigger={<button>Add server</button>} onSaved={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: "Add server" }));
    await user.click(screen.getByRole("button", { name: "Paste from client config" }));
    await user.type(screen.getByPlaceholderText(/Paste a config snippet/i), "pasted");
    await user.click(screen.getByRole("button", { name: "Parse & fill" }));
    const token = await screen.findByDisplayValue("TOKEN");
    await user.clear(token);
    await user.type(token, "API_KEY");

    expect(
      screen.getByText(
        "Use each variable name once. Rename or remove the second API_KEY.",
      ),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Add" })).toBeDisabled();
    // The renamed row is no longer a name the paste left out.
    expect(
      screen.queryByText(/The pasted config did not include it/),
    ).not.toBeInTheDocument();
  });

  it("forgets pasted gaps when the dialog is opened again", async () => {
    api.parseServerSnippet.mockResolvedValueOnce([
      {
        name: "gh",
        transport: "stdio",
        command: "npx",
        args: [],
        url: null,
        env: [{ key: "TOKEN", value: null, secret: true }],
      },
    ]);
    const user = userEvent.setup();
    render(<ServerDialog trigger={<button>Add server</button>} onSaved={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: "Add server" }));
    await user.click(screen.getByRole("button", { name: "Paste from client config" }));
    await user.type(screen.getByPlaceholderText(/Paste a config snippet/i), "pasted");
    await user.click(screen.getByRole("button", { name: "Parse & fill" }));
    await screen.findByText(/Add a value for TOKEN/);

    await user.click(screen.getByRole("button", { name: "Cancel" }));
    await user.click(screen.getByRole("button", { name: "Add server" }));
    await user.click(screen.getByRole("button", { name: /Add variable/i }));
    await user.type(screen.getAllByPlaceholderText("ENV_NAME").at(-1)!, "TOKEN");
    expect(screen.queryByText(/Add a value for TOKEN/)).not.toBeInTheDocument();
  });

  it("shows the parser's own message when a paste is refused", async () => {
    const message = "Write each -e value as KEY=value.";
    api.parseServerSnippet.mockRejectedValueOnce(message);
    const user = userEvent.setup();
    render(<ServerDialog trigger={<button>Add server</button>} onSaved={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: "Add server" }));
    await user.click(screen.getByRole("button", { name: "Paste from client config" }));
    await user.type(screen.getByPlaceholderText(/claude mcp add \.\.\. \//), "bad");
    await user.click(screen.getByRole("button", { name: "Parse & fill" }));

    await waitFor(() => expect(toastError).toHaveBeenCalledWith(message));
  });

  it("leaves the form as it was when a paste is refused", async () => {
    const header = "Toolport can send only an Authorization: Bearer header for now.";
    const variable =
      "Replace the shell variable or ~ in the server name, command or URL.";
    api.parseServerSnippet
      .mockRejectedValueOnce(header)
      .mockResolvedValueOnce([
        {
          name: "first",
          transport: "stdio",
          command: "npx",
          args: ["a"],
          url: null,
          env: [],
        },
      ])
      .mockRejectedValueOnce(variable);
    const user = userEvent.setup();
    render(<ServerDialog trigger={<button>Add server</button>} onSaved={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: "Add server" }));
    // The user starts on the name, then goes to the paste box instead.
    await user.click(screen.getByLabelText("Name"));
    const paste = async (text: string) => {
      if (!screen.queryByPlaceholderText(/claude mcp add \.\.\. \//))
        await user.click(
          screen.getByRole("button", { name: "Paste from client config" }),
        );
      const box = screen.getByPlaceholderText(/claude mcp add \.\.\. \//);
      await user.clear(box);
      await user.type(box, text);
      await user.click(screen.getByRole("button", { name: "Parse & fill" }));
    };
    await paste("refused header");
    await waitFor(() => expect(toastError).toHaveBeenCalledWith(header));
    expect(screen.queryByText("Give the server a name.")).not.toBeInTheDocument();
    expect(screen.getByLabelText("Name")).toHaveValue("");

    await paste("good");
    await waitFor(() => expect(screen.getByLabelText("Name")).toHaveValue("first"));
    await paste("refused variable");
    await waitFor(() => expect(toastError).toHaveBeenCalledWith(variable));
    // The earlier paste's fields stay, as they were before the refused one.
    expect(screen.getByLabelText("Name")).toHaveValue("first");
    expect(screen.getByLabelText("Command")).toHaveValue("npx");
  });

  it("shows only the refusal after a paste, whichever empty field was visited", async () => {
    const header = "Toolport can send only an Authorization: Bearer header for now.";
    api.parseServerSnippet.mockRejectedValue(header);
    const user = userEvent.setup();
    render(<ServerDialog trigger={<button>Add server</button>} onSaved={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: "Add server" }));
    await user.type(screen.getByLabelText("Name"), "alpha");
    const paste = async () => {
      if (!screen.queryByPlaceholderText(/claude mcp add \.\.\. \//))
        await user.click(
          screen.getByRole("button", { name: "Paste from client config" }),
        );
      await user.type(screen.getByPlaceholderText(/claude mcp add \.\.\. \//), "x");
      await user.click(screen.getByRole("button", { name: "Parse & fill" }));
      await waitFor(() => expect(toastError).toHaveBeenCalledWith(header));
      vi.mocked(toastError).mockClear();
    };
    // Arguments shows the empty command's error once visited.
    await user.click(screen.getByLabelText("Arguments"));
    await paste();
    expect(
      screen.queryByText("Enter the command to run (e.g. npx)."),
    ).not.toBeInTheDocument();
    // After a switch to http, Arguments' visit would show the empty URL's.
    await user.click(screen.getByLabelText("Arguments"));
    await user.click(screen.getByLabelText("Transport"));
    await user.click(screen.getByRole("option", { name: "http (remote)" }));
    await user.click(screen.getByLabelText("URL"));
    await paste();
    expect(screen.queryByText("Enter the server URL.")).not.toBeInTheDocument();
    expect(screen.getByLabelText("Name")).toHaveValue("alpha");
  });

  it("reads the form as it is when a paste is refused, not as it was when sent", async () => {
    const refusal = deferred<never>();
    api.parseServerSnippet.mockReturnValueOnce(refusal.promise);
    const user = userEvent.setup();
    render(<ServerDialog trigger={<button>Add server</button>} onSaved={vi.fn()} />);
    await user.click(screen.getByRole("button", { name: "Add server" }));
    await user.type(screen.getByLabelText("Name"), "alpha");
    await user.click(screen.getByRole("button", { name: "Paste from client config" }));
    await user.type(screen.getByPlaceholderText(/claude mcp add \.\.\. \//), "x");
    await user.click(screen.getByRole("button", { name: "Parse & fill" }));
    // While the paste is parsed, the user empties the name and moves on.
    await user.clear(screen.getByLabelText("Name"));
    await user.click(screen.getByPlaceholderText(/claude mcp add \.\.\. \//));
    expect(screen.getByText("Give the server a name.")).toBeVisible();
    await act(async () => refusal.reject("refused"));
    await waitFor(() => expect(toastError).toHaveBeenCalledWith("refused"));
    expect(screen.queryByText("Give the server a name.")).not.toBeInTheDocument();
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

it("marks a new server local-only in its first saved definition", async () => {
  const user = userEvent.setup();
  api.addServer.mockResolvedValue(savedRegistry("demo"));
  render(<ServerDialog autoOpen onSaved={vi.fn()} />);
  await fillServer(user, "echo");
  await user.click(screen.getByRole("checkbox", { name: "This machine only" }));
  await user.click(screen.getByRole("button", { name: "Add" }));
  await waitFor(() =>
    expect(api.addServer).toHaveBeenCalledWith(
      expect.objectContaining({ syncLocalOnly: true }),
    ),
  );
});
