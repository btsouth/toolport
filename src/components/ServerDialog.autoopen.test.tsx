import { describe, expect, it, vi, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { ServerDialog } from "./ServerDialog";

// The dialog talks to Tauri on save; stub the surface it touches.
vi.mock("@/lib/api", () => ({
  addServerWithRows: vi.fn(async () => ({
    registry: { servers: [], profiles: [] },
    declaredWithoutValue: [],
  })),
  updateServer: vi.fn(async () => ({ servers: [], profiles: [] })),
  setSecret: vi.fn(async () => undefined),
  testServer: vi.fn(async () => ({ ok: true, toolCount: 1, error: null })),
}));

vi.mock("sonner", () => ({
  toast: { success: vi.fn(), error: vi.fn(), warning: vi.fn(), message: vi.fn() },
}));

/** A parent that opens ServerDialog the way a keyboard shortcut does: mount-on-demand
 * with `autoOpen`, and unmount when the dialog reports that it closed. */
function ShortcutHost() {
  const [open, setOpen] = useState(false);
  return (
    <div>
      <button onClick={() => setOpen(true)}>shortcut</button>
      <span data-testid="mounted">{open ? "yes" : "no"}</span>
      {open && (
        <ServerDialog autoOpen onClose={() => setOpen(false)} onSaved={() => undefined} />
      )}
    </div>
  );
}

async function saveAServer(user: ReturnType<typeof userEvent.setup>) {
  await user.type(screen.getByLabelText(/name/i), "demo");
  await user.type(screen.getByLabelText(/command/i), "npx");
  const save = screen.getByRole("button", { name: /^add$|^save$/i });
  await waitFor(() => expect(save).toBeEnabled());
  await user.click(save);
}

describe("ServerDialog opened by a shortcut (autoOpen)", () => {
  beforeEach(() => vi.clearAllMocks());

  it("stays reopenable after a successful save", async () => {
    const user = userEvent.setup();
    render(<ShortcutHost />);

    await user.click(screen.getByText("shortcut"));
    expect(screen.getByTestId("mounted")).toHaveTextContent("yes");
    await saveAServer(user);

    // The save must release the parent's state, or the dialog can never reopen.
    await waitFor(() => expect(screen.getByTestId("mounted")).toHaveTextContent("no"));

    await user.click(screen.getByText("shortcut"));
    expect(screen.getByTestId("mounted")).toHaveTextContent("yes");
  });

  it("cancelling does release the parent", async () => {
    const user = userEvent.setup();
    render(<ShortcutHost />);

    await user.click(screen.getByText("shortcut"));
    await user.click(screen.getByRole("button", { name: /cancel/i }));

    await waitFor(() => expect(screen.getByTestId("mounted")).toHaveTextContent("no"));
  });
});
