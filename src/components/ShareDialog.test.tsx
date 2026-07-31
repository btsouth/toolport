import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

const mocks = vi.hoisted(() => ({
  exportConfig: vi.fn(),
  getRegistry: vi.fn(),
  shareStack: vi.fn(),
  success: vi.fn(),
  toastError: vi.fn(),
}));

vi.mock("sonner", () => ({ toast: { success: mocks.success } }));
vi.mock("@/lib/toast", () => ({ toastError: mocks.toastError }));
vi.mock("@/lib/api", () => ({
  exportConfig: mocks.exportConfig,
  exportConfigToPath: vi.fn(),
  fetchSharedSetup: vi.fn(),
  getRegistry: mocks.getRegistry,
  importConfig: vi.fn(),
  previewImport: vi.fn(),
  readSetupFile: vi.fn(),
  shareStack: mocks.shareStack,
  takePendingShared: vi.fn().mockResolvedValue(null),
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(),
  save: vi.fn(),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}));

import { ShareDialog } from "./ShareDialog";

describe("ShareDialog share links", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.exportConfig.mockResolvedValue('{"servers":[]}');
    mocks.getRegistry.mockResolvedValue({ servers: [] });
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
});
