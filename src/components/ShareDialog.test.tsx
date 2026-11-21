import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

const mocks = vi.hoisted(() => ({
  exportConfig: vi.fn(),
  fetchSharedSetup: vi.fn(),
  getRegistry: vi.fn(),
  importConfig: vi.fn(),
  importSharedHandler: null as null | ((event: { payload: string }) => void),
  previewImport: vi.fn(),
  shareStack: vi.fn(),
  success: vi.fn(),
  toastError: vi.fn(),
  open: vi.fn(),
  save: vi.fn(),
}));

vi.mock("sonner", () => ({ toast: { success: mocks.success } }));
vi.mock("@/lib/toast", () => ({ toastError: mocks.toastError }));
vi.mock("@/lib/api", () => ({
  exportConfig: mocks.exportConfig,
  exportConfigToPath: vi.fn(),
  fetchSharedSetup: mocks.fetchSharedSetup,
  getRegistry: mocks.getRegistry,
  importConfig: mocks.importConfig,
  previewImport: mocks.previewImport,
  readSetupFile: vi.fn(),
  shareStack: mocks.shareStack,
  takePendingShared: vi.fn().mockResolvedValue(null),
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: mocks.open,
  save: mocks.save,
}));
vi.mock("@tauri-apps/api/path", () => ({
  downloadDir: vi.fn().mockResolvedValue("/home/u/Downloads"),
  homeDir: vi.fn().mockResolvedValue("/home/u"),
  join: vi.fn((...parts: string[]) => Promise.resolve(parts.join("/"))),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi
    .fn()
    .mockImplementation(
      (_event: string, handler: (event: { payload: string }) => void) => {
        mocks.importSharedHandler = handler;
        return Promise.resolve(() => {});
      },
    ),
}));

import { ShareDialog } from "./ShareDialog";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, reject, resolve };
}

describe("ShareDialog share links", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.exportConfig.mockResolvedValue('{"servers":[]}');
    mocks.getRegistry.mockResolvedValue({ servers: [] });
    mocks.previewImport.mockResolvedValue([]);
    mocks.fetchSharedSetup.mockResolvedValue('{"servers":[]}');
    mocks.importConfig.mockResolvedValue({ servers: [] });
    mocks.importSharedHandler = null;
    mocks.shareStack.mockResolvedValue("https://toolport.app/s/example");
  });

  // Both tests replace navigator.clipboard, so restore the real descriptor
  // afterwards. Without this the second test's `undefined` sticks, which makes
  // any later test in this file order-dependent.
  const originalClipboard = Object.getOwnPropertyDescriptor(navigator, "clipboard");
  afterEach(() => {
    if (originalClipboard) {
      Object.defineProperty(navigator, "clipboard", originalClipboard);
    } else {
      delete (navigator as { clipboard?: unknown }).clipboard;
    }
  });

  it("keeps the generated link visible when clipboard access fails", async () => {
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText: vi.fn().mockRejectedValue(new Error("denied")) },
    });

    render(<ShareDialog trigger={<button>Share</button>} onImported={vi.fn()} />);
    await userEvent.click(screen.getByRole("button", { name: "Share" }));
    const create = await screen.findByRole("button", { name: /create share link/i });
    await waitFor(() => expect(create).toBeEnabled());
    await userEvent.click(create);

    expect(await screen.findByText("https://toolport.app/s/example")).toBeVisible();
    expect(mocks.success).toHaveBeenCalledWith("Share link created");
    expect(mocks.toastError).toHaveBeenCalledWith(
      "Couldn't copy automatically. Select the link and copy it.",
    );
  });

  it("does not report link creation as failed when the clipboard API is absent", async () => {
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: undefined,
    });

    render(<ShareDialog trigger={<button>Share</button>} onImported={vi.fn()} />);
    await userEvent.click(screen.getByRole("button", { name: "Share" }));
    const create = await screen.findByRole("button", { name: /create share link/i });
    await waitFor(() => expect(create).toBeEnabled());
    await userEvent.click(create);

    expect(await screen.findByText("https://toolport.app/s/example")).toBeVisible();
    expect(mocks.success).toHaveBeenCalledWith("Share link created");
    expect(mocks.toastError).not.toHaveBeenCalledWith(
      expect.stringContaining("Couldn't create a link"),
    );
  });

  it("does not export or enable share actions until the registry is loaded", async () => {
    const registry = deferred<{ servers: never[] }>();
    mocks.getRegistry.mockReturnValue(registry.promise);

    render(<ShareDialog trigger={<button>Share</button>} onImported={vi.fn()} />);
    await userEvent.click(screen.getByRole("button", { name: "Share" }));

    expect(await screen.findByRole("status", { name: "Loading servers…" })).toBeVisible();
    expect(screen.getByRole("button", { name: /create share link/i })).toBeDisabled();
    expect(screen.getByRole("button", { name: /^copy$/i })).toBeDisabled();
    expect(screen.getByRole("button", { name: /save to file/i })).toBeDisabled();
    expect(mocks.exportConfig).not.toHaveBeenCalled();

    await act(async () => registry.resolve({ servers: [] }));

    await waitFor(() => expect(mocks.exportConfig).toHaveBeenCalledTimes(1));
    expect(mocks.exportConfig).toHaveBeenLastCalledWith("", "", []);
    await waitFor(() =>
      expect(screen.getByRole("button", { name: /create share link/i })).toBeEnabled(),
    );
  });

  it("surfaces registry load errors and keeps every share action disabled", async () => {
    mocks.getRegistry.mockRejectedValueOnce(new Error("registry unavailable"));

    render(<ShareDialog trigger={<button>Share</button>} onImported={vi.fn()} />);
    await userEvent.click(screen.getByRole("button", { name: "Share" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("registry unavailable");
    expect(screen.getByRole("button", { name: /create share link/i })).toBeDisabled();
    expect(screen.getByRole("button", { name: /^copy$/i })).toBeDisabled();
    expect(screen.getByRole("button", { name: /save to file/i })).toBeDisabled();
    expect(mocks.exportConfig).not.toHaveBeenCalled();
  });

  it("clears stale export and link output as soon as the setup changes", async () => {
    const nextExport = deferred<string>();
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText: vi.fn().mockResolvedValue(undefined) },
    });

    render(<ShareDialog trigger={<button>Share</button>} onImported={vi.fn()} />);
    await userEvent.click(screen.getByRole("button", { name: "Share" }));
    const create = await screen.findByRole("button", { name: /create share link/i });
    await waitFor(() => expect(create).toBeEnabled());
    await userEvent.click(create);
    expect(await screen.findByText("https://toolport.app/s/example")).toBeVisible();

    mocks.exportConfig.mockReturnValueOnce(nextExport.promise);
    await userEvent.type(screen.getByPlaceholderText("Name (optional)"), "Updated");

    expect(screen.queryByText("https://toolport.app/s/example")).not.toBeInTheDocument();
    expect(screen.getByLabelText("Exported setup")).toHaveValue("");
    expect(create).toBeDisabled();

    await waitFor(() => expect(mocks.exportConfig).toHaveBeenCalledTimes(2));
    await act(async () => nextExport.resolve('{"name":"Updated"}'));
    await waitFor(() => expect(create).toBeEnabled());
  });

  it("lets only the latest export request publish its result", async () => {
    const first = deferred<string>();
    const second = deferred<string>();

    render(<ShareDialog trigger={<button>Share</button>} onImported={vi.fn()} />);
    await userEvent.click(screen.getByRole("button", { name: "Share" }));
    const create = await screen.findByRole("button", { name: /create share link/i });
    await waitFor(() => expect(create).toBeEnabled());

    mocks.exportConfig
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise);
    const name = screen.getByPlaceholderText("Name (optional)");
    await userEvent.type(name, "A");
    await waitFor(() => expect(mocks.exportConfig).toHaveBeenCalledTimes(2));
    await userEvent.type(name, "B");
    await waitFor(() => expect(mocks.exportConfig).toHaveBeenCalledTimes(3));

    await act(async () => second.resolve('{"name":"AB"}'));
    await waitFor(() =>
      expect(screen.getByLabelText("Exported setup")).toHaveValue('{"name":"AB"}'),
    );
    await act(async () => first.resolve('{"name":"A"}'));
    expect(screen.getByLabelText("Exported setup")).toHaveValue('{"name":"AB"}');
  });

  it("ignores a link response after the exported setup was invalidated", async () => {
    const link = deferred<string>();
    mocks.shareStack.mockReturnValueOnce(link.promise);

    render(<ShareDialog trigger={<button>Share</button>} onImported={vi.fn()} />);
    await userEvent.click(screen.getByRole("button", { name: "Share" }));
    const create = await screen.findByRole("button", { name: /create share link/i });
    await waitFor(() => expect(create).toBeEnabled());
    await userEvent.click(create);
    await userEvent.type(
      screen.getByPlaceholderText("Description (optional)"),
      "Changed",
    );

    await act(async () => link.resolve("https://toolport.app/s/stale"));
    expect(screen.queryByText("https://toolport.app/s/stale")).not.toBeInTheDocument();
  });

  it("keeps share actions disabled when export generation fails", async () => {
    mocks.exportConfig.mockRejectedValueOnce(new Error("export unavailable"));

    render(<ShareDialog trigger={<button>Share</button>} onImported={vi.fn()} />);
    await userEvent.click(screen.getByRole("button", { name: "Share" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("export unavailable");
    expect(screen.getByRole("button", { name: /create share link/i })).toBeDisabled();
    expect(screen.getByRole("button", { name: /^copy$/i })).toBeDisabled();
    expect(screen.getByRole("button", { name: /save to file/i })).toBeDisabled();
  });

  it("selects duplicate display names independently by stable server id", async () => {
    mocks.getRegistry.mockResolvedValue({
      servers: [
        { id: "duplicate-a", name: "Duplicate", transport: "stdio" },
        { id: "duplicate-b", name: "Duplicate", transport: "http" },
      ],
    });

    render(<ShareDialog trigger={<button>Share</button>} onImported={vi.fn()} />);
    await userEvent.click(screen.getByRole("button", { name: "Share" }));
    await waitFor(() =>
      expect(mocks.exportConfig).toHaveBeenLastCalledWith("", "", [
        "duplicate-a",
        "duplicate-b",
      ]),
    );

    // Same-name servers show their ids so the two can be told apart, as in GTK.
    const first = screen.getByRole("button", { name: "Duplicate (duplicate-a)" });
    expect(screen.getByRole("button", { name: "Duplicate (duplicate-b)" })).toBeVisible();
    expect(first).toHaveAttribute("aria-pressed", "true");
    await userEvent.click(first);
    expect(first).toHaveAttribute("aria-pressed", "false");
    await waitFor(() =>
      expect(mocks.exportConfig).toHaveBeenLastCalledWith("", "", ["duplicate-b"]),
    );
  });

  it("says why the share actions are off with nothing selected", async () => {
    mocks.getRegistry.mockResolvedValue({
      servers: [{ id: "linear", name: "Linear", transport: "stdio" }],
    });
    render(<ShareDialog trigger={<button>Share</button>} onImported={vi.fn()} />);
    await userEvent.click(screen.getByRole("button", { name: "Share" }));
    const create = await screen.findByRole("button", { name: /create share link/i });
    await waitFor(() => expect(create).toBeEnabled());
    expect(create).not.toHaveAttribute("title");
    // One right-aligned row, as GTK's footer: the exports, then the primary.
    const row = create.parentElement!;
    expect(row).toHaveClass("justify-end");
    expect(
      Array.from(row.querySelectorAll("button")).map((b) => b.textContent?.trim()),
    ).toEqual(["Copy", "Save to file", "Create share link"]);
    const reason = "Select at least one server to share.";
    expect(screen.queryByText(reason, { selector: "p" })).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "None" }));
    // The reason is a line by the buttons, not only a tooltip.
    expect(screen.getByText(reason, { selector: "p" })).toBeVisible();
    for (const name of [/create share link/i, /^copy$/i, /save to file/i]) {
      const button = screen.getByRole("button", { name });
      expect(button).toBeDisabled();
      expect(button).toHaveAttribute("title", "Select at least one server to share.");
    }
    // A disabled button takes no hover, so its row shows the tooltip.
    expect(create.parentElement).toHaveAttribute(
      "title",
      "Select at least one server to share.",
    );
  });

  it("leads a failed paste with what to do before the parser detail", async () => {
    mocks.previewImport.mockRejectedValueOnce(
      "That doesn't look like a Toolport setup. The text isn't JSON.",
    );
    render(<ShareDialog trigger={<button>Share</button>} onImported={vi.fn()} />);
    await userEvent.click(screen.getByRole("button", { name: "Share" }));
    fireEvent.change(screen.getByLabelText("Paste a shared setup"), {
      target: { value: "{x" },
    });
    const review = screen.getByRole("button", { name: /review and import/i });
    // Load from file, then the one primary, at the right as in GTK.
    expect(review.parentElement).toHaveClass("justify-end");
    expect(review.parentElement!.lastElementChild).toBe(review);
    await userEvent.click(review);
    // An error on the field shows under it, not as a toast.
    const error = await screen.findByRole("alert");
    expect(error).toHaveTextContent(
      "Couldn't read the pasted setup. Check that you copied all of it, then try again.",
    );
    expect(error).toHaveTextContent(
      "That doesn't look like a Toolport setup. The text isn't JSON.",
    );
    const field = screen.getByLabelText("Paste a shared setup");
    expect(field).toHaveAttribute("aria-describedby", error.id);
    expect(field.nextElementSibling).toBe(error);
    expect(mocks.toastError).not.toHaveBeenCalled();
    fireEvent.change(field, { target: { value: "{}" } });
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("starts both choosers in Downloads and offers every file after setup files", async () => {
    mocks.open.mockResolvedValue(null);
    mocks.save.mockResolvedValue(null);
    mocks.getRegistry.mockResolvedValue({
      servers: [{ id: "linear", name: "Linear", transport: "stdio" }],
    });
    render(<ShareDialog trigger={<button>Share</button>} onImported={vi.fn()} />);
    await userEvent.click(screen.getByRole("button", { name: "Share" }));
    const save = await screen.findByRole("button", { name: /save to file/i });
    await waitFor(() => expect(save).toBeEnabled());
    await userEvent.click(save);
    await userEvent.click(screen.getByRole("button", { name: /load from file/i }));
    const filters = [
      { name: "Toolport setup", extensions: ["json"] },
      { name: "All files", extensions: ["*"] },
    ];
    await waitFor(() =>
      expect(mocks.open).toHaveBeenCalledWith(
        expect.objectContaining({ filters, defaultPath: "/home/u/Downloads" }),
      ),
    );
    expect(mocks.save).toHaveBeenCalledWith(
      expect.objectContaining({
        filters,
        defaultPath: "/home/u/Downloads/toolport-setup.json",
      }),
    );
  });

  it("imports only the servers left checked and follows every toggle", async () => {
    const server = (name: string, isNew = true) => ({
      name,
      transport: "stdio" as const,
      command: "npx",
      args: [],
      url: null,
      isNew,
    });
    mocks.previewImport.mockResolvedValue([
      server("Linear"),
      server("GitHub"),
      server("Stripe", false),
    ]);
    render(<ShareDialog trigger={<button>Share</button>} onImported={vi.fn()} />);
    await userEvent.click(screen.getByRole("button", { name: "Share" }));
    fireEvent.change(screen.getByLabelText("Paste a shared setup"), {
      target: { value: '{"servers":[]}' },
    });
    await userEvent.click(screen.getByRole("button", { name: /review and import/i }));
    expect(await screen.findByRole("button", { name: "Import 2 servers" })).toBeEnabled();
    expect(screen.getByRole("button", { name: /Stripe/ })).toBeDisabled();
    const linear = screen.getByRole("button", { name: /Linear/ });
    expect(linear).toHaveAttribute("aria-pressed", "true");
    await userEvent.click(linear);
    expect(linear).toHaveAttribute("aria-pressed", "false");
    await userEvent.click(screen.getByRole("button", { name: /GitHub/ }));
    expect(screen.getByRole("button", { name: "Select a server" })).toBeDisabled();
    await userEvent.click(screen.getByRole("button", { name: /GitHub/ }));
    await userEvent.click(screen.getByRole("button", { name: "Import 1 server" }));
    await waitFor(() =>
      expect(mocks.importConfig).toHaveBeenCalledWith('{"servers":[]}', ["GitHub"]),
    );
  });

  it("explains a setup whose servers are all present", async () => {
    mocks.previewImport.mockResolvedValue([
      {
        name: "Stripe",
        transport: "stdio" as const,
        command: "npx",
        args: [],
        url: null,
        isNew: false,
      },
    ]);
    render(<ShareDialog trigger={<button>Share</button>} onImported={vi.fn()} />);
    await userEvent.click(screen.getByRole("button", { name: "Share" }));
    fireEvent.change(screen.getByLabelText("Paste a shared setup"), {
      target: { value: '{"servers":[]}' },
    });
    await userEvent.click(screen.getByRole("button", { name: /review and import/i }));
    const button = await screen.findByRole("button", { name: "Nothing new to import" });
    expect(button).toBeDisabled();
    expect(button).toHaveAccessibleDescription(
      "Every server in this setup is already present, so there is nothing to import.",
    );
  });

  it("does not let a preview from a closed dialog enter a later session", async () => {
    const preview = deferred<never[]>();
    mocks.previewImport.mockReturnValueOnce(preview.promise);

    render(<ShareDialog trigger={<button>Share</button>} onImported={vi.fn()} />);
    await userEvent.click(screen.getByRole("button", { name: "Share" }));
    fireEvent.change(screen.getByLabelText("Paste a shared setup"), {
      target: { value: '{"servers":[]}' },
    });
    await userEvent.click(screen.getByRole("button", { name: /review and import/i }));
    await userEvent.click(screen.getByRole("button", { name: "Close" }));
    await userEvent.click(screen.getByRole("button", { name: "Share" }));

    await act(async () => preview.resolve([]));
    expect(screen.getByText("Share setup")).toBeInTheDocument();
    expect(screen.queryByText("Review this setup")).not.toBeInTheDocument();
  });

  it("lets only the newest deep link publish an import review", async () => {
    const first = deferred<string>();
    const second = deferred<string>();
    mocks.fetchSharedSetup
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise);

    render(<ShareDialog trigger={<button>Share</button>} onImported={vi.fn()} />);
    await waitFor(() => expect(mocks.importSharedHandler).not.toBeNull());
    act(() => mocks.importSharedHandler?.({ payload: "first" }));
    act(() => mocks.importSharedHandler?.({ payload: "second" }));

    await act(async () => second.resolve('{"name":"newest","servers":[]}'));
    await waitFor(() => expect(mocks.previewImport).toHaveBeenCalledTimes(1));
    expect(mocks.previewImport).toHaveBeenLastCalledWith(
      '{"name":"newest","servers":[]}',
    );

    await act(async () => first.resolve('{"name":"stale","servers":[]}'));
    expect(mocks.previewImport).toHaveBeenCalledTimes(1);
  });

  it("invalidates a visible review as soon as a newer deep link arrives", async () => {
    const item = {
      name: "Reviewed A",
      transport: "stdio" as const,
      command: "npx",
      args: [],
      url: null,
      isNew: true,
    };
    const newer = deferred<string>();
    mocks.previewImport.mockResolvedValueOnce([item]);

    render(<ShareDialog trigger={<button>Share</button>} onImported={vi.fn()} />);
    await userEvent.click(screen.getByRole("button", { name: "Share" }));
    fireEvent.change(screen.getByLabelText("Paste a shared setup"), {
      target: { value: '{"name":"a","servers":[]}' },
    });
    await userEvent.click(screen.getByRole("button", { name: /review and import/i }));
    expect(await screen.findByRole("button", { name: "Import 1 server" })).toBeEnabled();

    mocks.fetchSharedSetup.mockReturnValueOnce(newer.promise);
    act(() => mocks.importSharedHandler?.({ payload: "newer" }));

    expect(
      screen.queryByRole("button", { name: "Import 1 server" }),
    ).not.toBeInTheDocument();
    expect(screen.getByText("Share setup")).toBeInTheDocument();

    await act(async () => newer.reject(new Error("fetch failed")));
    await waitFor(() =>
      expect(mocks.toastError).toHaveBeenCalledWith(
        expect.stringContaining("fetch failed"),
      ),
    );
    // The failed fetch must leave no review to act on: no review view, no
    // "Reviewing…" spinner stuck on, and nothing staged in the paste field.
    // (The review-and-import button being disabled alone would also hold for the
    // unrelated reason that opening the dialog cleared the paste field.)
    expect(screen.queryByText("Review this setup")).not.toBeInTheDocument();
    expect(screen.queryByText("Reviewing…")).not.toBeInTheDocument();
    expect(screen.getByLabelText("Paste a shared setup")).toHaveValue("");
  });

  it("tells the user imported servers stay off and where to turn them on", async () => {
    mocks.previewImport.mockResolvedValue([
      {
        name: "Reviewed server",
        transport: "stdio" as const,
        command: "npx",
        args: [],
        url: null,
        isNew: true,
      },
    ]);
    render(<ShareDialog trigger={<button>Share</button>} onImported={vi.fn()} />);
    await userEvent.click(screen.getByRole("button", { name: "Share" }));
    fireEvent.change(screen.getByLabelText("Paste a shared setup"), {
      target: { value: '{"name":"a","servers":[]}' },
    });
    await userEvent.click(screen.getByRole("button", { name: /review and import/i }));
    await userEvent.click(await screen.findByRole("button", { name: "Import 1 server" }));
    await waitFor(() =>
      expect(mocks.success).toHaveBeenCalledWith("Imported shared setup", {
        description:
          "Imported servers stay off. Turn each one on in Servers when you're ready.",
      }),
    );
  });

  it("does not let an older import completion close a newer review", async () => {
    const item = {
      name: "Reviewed server",
      transport: "stdio" as const,
      command: "npx",
      args: [],
      url: null,
      isNew: true,
    };
    const importing = deferred<{ servers: [] }>();
    mocks.previewImport.mockResolvedValue([item]);
    mocks.importConfig.mockReturnValueOnce(importing.promise);

    const onImported = vi.fn();
    render(<ShareDialog trigger={<button>Share</button>} onImported={onImported} />);
    await userEvent.click(screen.getByRole("button", { name: "Share" }));
    fireEvent.change(screen.getByLabelText("Paste a shared setup"), {
      target: { value: '{"name":"a","servers":[]}' },
    });
    await userEvent.click(screen.getByRole("button", { name: /review and import/i }));
    await userEvent.click(await screen.findByRole("button", { name: "Import 1 server" }));

    act(() => mocks.importSharedHandler?.({ payload: "newer" }));
    expect(await screen.findByText("Review this setup")).toBeInTheDocument();

    await act(async () => importing.resolve({ servers: [] }));
    expect(screen.getByText("Review this setup")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Import 1 server" })).toBeEnabled();
    expect(onImported).toHaveBeenCalledWith({ servers: [] });
    expect(mocks.success).toHaveBeenCalledWith(
      "Imported the setup you already confirmed",
    );
  });
});
