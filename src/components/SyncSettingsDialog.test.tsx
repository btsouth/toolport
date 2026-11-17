import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Registry, ServerEntry } from "@/lib/types";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

import { SyncSettingsDialog } from "./SyncSettingsDialog";

const registry = {
  team: { role: "admin", accountStatus: { personalSync: true } },
} as unknown as Registry;

function server(env: ServerEntry["env"], syncLocalOnly = false): ServerEntry {
  return {
    id: "server-1",
    name: "Example",
    transport: "stdio",
    command: "example",
    args: [],
    env,
    url: null,
    source: "manual",
    syncLocalOnly,
  } as ServerEntry;
}

const region = {
  key: "REGION",
  value: "eu",
  secret: false,
} as ServerEntry["env"][number];
const team = {
  key: "TEAM",
  value: "core",
  secret: false,
  portable: true,
} as ServerEntry["env"][number];

describe("SyncSettingsDialog", () => {
  beforeEach(() => invoke.mockReset());

  it("keeps each mark on its own value when an update reorders them", async () => {
    invoke.mockResolvedValue(registry);
    const props = { registry, onSaved: vi.fn(), onClose: vi.fn() };
    const view = render(
      <SyncSettingsDialog server={server([region, team])} {...props} />,
    );
    view.rerender(<SyncSettingsDialog server={server([team, region])} {...props} />);

    expect(screen.getByRole("checkbox", { name: /TEAM/ })).toBeChecked();
    expect(screen.getByRole("checkbox", { name: /REGION/ })).not.toBeChecked();
    await userEvent.click(screen.getByRole("button", { name: "Save" }));

    const marks = invoke.mock.calls
      .filter(([command]) => command === "personal_sync_portable")
      .map(([, args]) => [args.key, args.enabled]);
    expect(marks).toEqual([
      ["TEAM", true],
      ["REGION", false],
    ]);
  });

  it("turns local-only off last, and not at all when a mark fails", async () => {
    invoke.mockImplementation((command: string) =>
      command === "personal_sync_portable"
        ? Promise.reject(new Error("disk full"))
        : Promise.resolve(registry),
    );
    render(
      <SyncSettingsDialog
        server={server([region], true)}
        registry={registry}
        onSaved={vi.fn()}
        onClose={vi.fn()}
      />,
    );
    await userEvent.click(
      screen.getByRole("checkbox", { name: /Keep on this machine only/ }),
    );
    await userEvent.click(screen.getByRole("button", { name: "Save" }));

    expect(invoke).toHaveBeenCalledWith("personal_sync_portable", expect.anything());
    expect(invoke).not.toHaveBeenCalledWith(
      "personal_sync_local_only",
      expect.anything(),
    );
  });
});
