import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { Registry, ServerEntry } from "@/lib/types";

const api = vi.hoisted(() => ({
  setLaunchInputValue: vi.fn(),
  setLaunchSecret: vi.fn(),
}));
vi.mock("@/lib/api", () => api);
vi.mock("sonner", () => ({ toast: { success: vi.fn() } }));
vi.mock("@/lib/toast", () => ({ toastError: vi.fn() }));

import { LaunchSetupDialog } from "./LaunchSetupDialog";

const server: ServerEntry = {
  id: "team_twilio",
  name: "Twilio",
  transport: "stdio",
  command: "npx",
  args: ["<launch-input>"],
  env: [],
  url: null,
  source: "team:one",
  launch: {
    inputs: [
      { key: "SID", label: "Account SID", secret: false, required: true, value: "ACold" },
      { key: "SECRET", label: "API Secret", secret: true, required: true },
    ],
    bindings: [
      {
        index: 0,
        parts: [
          { kind: "input", key: "SID" },
          { kind: "input", key: "SECRET" },
        ],
      },
    ],
  },
};
const saved: Registry = {
  version: 1,
  servers: [server],
  profiles: [],
  activeProfileId: null,
};

describe("LaunchSetupDialog", () => {
  beforeEach(() => vi.clearAllMocks());

  it("updates a team member's plain input and keeps a blank vaulted secret", async () => {
    api.setLaunchInputValue.mockResolvedValue(saved);
    const onSaved = vi.fn();
    const user = userEvent.setup();
    render(
      <LaunchSetupDialog
        server={server}
        trigger={<button>Launch setup</button>}
        onSaved={onSaved}
      />,
    );
    await user.click(screen.getByRole("button", { name: "Launch setup" }));
    await user.clear(screen.getByLabelText("Account SID *"));
    await user.type(screen.getByLabelText("Account SID *"), "ACnew");
    await user.click(screen.getByRole("button", { name: "Save setup" }));
    expect(api.setLaunchInputValue).toHaveBeenCalledWith("team_twilio", "SID", "ACnew");
    expect(api.setLaunchSecret).not.toHaveBeenCalled();
    expect(onSaved).toHaveBeenCalledWith(saved);
  });

  it("vaults a new secret under the existing team server id", async () => {
    api.setLaunchSecret.mockResolvedValue(saved);
    const user = userEvent.setup();
    render(
      <LaunchSetupDialog
        server={server}
        trigger={<button>Launch setup</button>}
        onSaved={vi.fn()}
      />,
    );
    await user.click(screen.getByRole("button", { name: "Launch setup" }));
    await user.type(screen.getByLabelText("API Secret *"), "new-secret");
    await user.click(screen.getByRole("button", { name: "Save setup" }));
    expect(api.setLaunchSecret).toHaveBeenCalledWith(
      "team_twilio",
      "SECRET",
      "new-secret",
    );
    expect(api.setLaunchInputValue).not.toHaveBeenCalled();
  });
});
