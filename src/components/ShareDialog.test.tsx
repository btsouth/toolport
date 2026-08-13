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
  open: vi.fn(),
  save: vi.fn(),
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

    expect(await screen.findByText("Loading servers…")).toBeVisible();
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

    const duplicateButtons = screen.getAllByRole("button", { name: "Duplicate" });
    await userEvent.click(duplicateButtons[0]);
    await waitFor(() =>
      expect(mocks.exportConfig).toHaveBeenLastCalledWith("", "", ["duplicate-b"]),
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
