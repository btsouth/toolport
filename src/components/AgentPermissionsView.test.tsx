import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { PermissionsView as PermissionsViewData } from "@/lib/types";

const api = vi.hoisted(() => ({
  agentPermissionsView: vi.fn(),
  agentPermissionsSetEnabled: vi.fn(),
  agentPermissionsSetRules: vi.fn(),
  agentPermissionsPreview: vi.fn(),
  agentGuardView: vi.fn(),
  agentGuardSetCursorMode: vi.fn(),
  agentGuardPreview: vi.fn(),
}));
vi.mock("@/lib/api", () => api);

import { AgentPermissionsView } from "./AgentPermissionsView";

function view(over: Partial<PermissionsViewData> = {}): PermissionsViewData {
  return {
    enabled: false,
    rules: [],
    profiles: [{ path: "/home/a/.claude/settings.json", state: "off", added: 0 }],
    presets: [
      {
        label: "Never force-push",
        rules: [
          { pattern: "Bash(git push --force*)", action: "deny" },
          { pattern: "Bash(git push -f *)", action: "deny" },
        ],
      },
    ],
    ...over,
  };
}

describe("AgentPermissionsView", () => {
  const guard = (over: Partial<import("@/lib/types").GuardView> = {}) => ({
    cursorMode: "off" as const,
    cursor: { path: "/home/a/.cursor/hooks.json", installed: false },
    events: ["beforeShellExecution", "beforeMCPExecution", "beforeReadFile"],
    binary: "/opt/toolport/toolport-gateway",
    ...over,
  });

  beforeEach(() => {
    vi.clearAllMocks();
    api.agentPermissionsView.mockResolvedValue(view());
    api.agentGuardView.mockResolvedValue(guard());
  });

  it("the Cursor guard starts off, and choosing Observe then Enforce calls through", async () => {
    api.agentGuardSetCursorMode
      .mockResolvedValueOnce(
        guard({
          cursorMode: "observe",
          cursor: { path: "/home/a/.cursor/hooks.json", installed: true },
        }),
      )
      .mockResolvedValueOnce(
        guard({
          cursorMode: "enforce",
          cursor: { path: "/home/a/.cursor/hooks.json", installed: true },
        }),
      );
    render(<AgentPermissionsView />);
    expect(await screen.findByLabelText("Cursor guard Off")).toBeChecked();
    expect(screen.getByText(/no guard installed/)).toBeInTheDocument();
    await userEvent.click(screen.getByLabelText("Cursor guard Observe"));
    await waitFor(() =>
      expect(api.agentGuardSetCursorMode).toHaveBeenCalledWith("observe"),
    );
    expect(await screen.findByText(/guard installed/)).toBeInTheDocument();
    await userEvent.click(screen.getByLabelText("Cursor guard Enforce"));
    await waitFor(() =>
      expect(api.agentGuardSetCursorMode).toHaveBeenLastCalledWith("enforce"),
    );
    expect(screen.getByLabelText("Cursor guard Enforce")).toBeChecked();
  });

  it("the Cursor preview names hooks.json's key, not Claude Code's", async () => {
    api.agentGuardPreview.mockResolvedValue({
      path: "/home/a/.cursor/hooks.json",
      before: "{}\n",
      after: '{\n  "version": 1,\n  "hooks": { "beforeShellExecution": [] }\n}\n',
    });
    render(<AgentPermissionsView />);
    await screen.findByLabelText("Cursor guard Off");
    await userEvent.click(screen.getByRole("button", { name: "Preview hooks.json" }));
    await waitFor(() => expect(api.agentGuardPreview).toHaveBeenCalledWith("observe"));
    expect(await screen.findByText("What would be written")).toBeInTheDocument();
    expect(screen.getByText(/Only the/).textContent).toContain("hooks");
    expect(screen.getByText(/Only the/).textContent).not.toContain("permissions");
  });

  it("without a published gateway binary the guard cannot be switched on", async () => {
    api.agentGuardView.mockResolvedValue(guard({ binary: undefined }));
    render(<AgentPermissionsView />);
    expect(await screen.findByLabelText("Cursor guard Observe")).toBeDisabled();
    expect(screen.getByLabelText("Cursor guard Off")).toBeEnabled();
    expect(screen.getByText(/No gateway binary has been published/)).toBeInTheDocument();
  });

  it("shows a Cursor guard load error and retries it", async () => {
    api.agentGuardView
      .mockRejectedValueOnce(new Error("registry unavailable"))
      .mockResolvedValueOnce(guard());
    render(<AgentPermissionsView />);
    expect(await screen.findByText(/registry unavailable/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(await screen.findByLabelText("Cursor guard Off")).toBeChecked();
  });

  it("starts off and empty, and adding a rule sends the whole list without writing", async () => {
    api.agentPermissionsSetRules.mockResolvedValue(
      view({ rules: [{ pattern: "Bash(rm -rf *)", action: "deny" }] }),
    );
    render(<AgentPermissionsView />);
    expect(
      await screen.findByLabelText("Enforce my permission rules in Claude Code"),
    ).not.toBeChecked();
    expect(screen.getByText(/No rules yet/)).toBeInTheDocument();
    await userEvent.type(screen.getByLabelText("Rule pattern"), "Bash(rm -rf *)");
    await userEvent.click(screen.getByRole("button", { name: "Add rule" }));
    await waitFor(() =>
      expect(api.agentPermissionsSetRules).toHaveBeenCalledWith([
        { pattern: "Bash(rm -rf *)", action: "deny" },
      ]),
    );
    // The rule row is there (the syntax help also mentions this pattern, hence the button).
    expect(
      screen.getByRole("button", { name: "Remove rule Bash(rm -rf *)" }),
    ).toBeInTheDocument();
    // Badge + the <option> both read "Never"; the row's badge is the second occurrence.
    expect(screen.getAllByText("Never").length).toBeGreaterThanOrEqual(2);
    expect(api.agentPermissionsSetEnabled).not.toHaveBeenCalled();
  });

  it("a preset adds its rules, a rule can be removed, and the switch calls through", async () => {
    const withPreset = view({
      rules: [
        { pattern: "Bash(git push --force*)", action: "deny" },
        { pattern: "Bash(git push -f *)", action: "deny" },
      ],
    });
    api.agentPermissionsSetRules.mockResolvedValueOnce(withPreset);
    render(<AgentPermissionsView />);
    await screen.findByText(/No rules yet/);
    await userEvent.click(screen.getByRole("button", { name: "Never force-push" }));
    await waitFor(() =>
      expect(api.agentPermissionsSetRules).toHaveBeenCalledWith(withPreset.rules),
    );
    // The preset is now "already in the list" and disabled.
    expect(screen.getByRole("button", { name: "Never force-push" })).toBeDisabled();

    api.agentPermissionsSetRules.mockResolvedValueOnce(
      view({ rules: [{ pattern: "Bash(git push -f *)", action: "deny" }] }),
    );
    await userEvent.click(
      screen.getByRole("button", { name: "Remove rule Bash(git push --force*)" }),
    );
    await waitFor(() =>
      expect(api.agentPermissionsSetRules).toHaveBeenLastCalledWith([
        { pattern: "Bash(git push -f *)", action: "deny" },
      ]),
    );

    api.agentPermissionsSetEnabled.mockResolvedValue(
      view({
        enabled: true,
        rules: [{ pattern: "Bash(git push -f *)", action: "deny" }],
        profiles: [{ path: "/home/a/.claude/settings.json", state: "applied", added: 1 }],
      }),
    );
    await userEvent.click(
      screen.getByLabelText("Enforce my permission rules in Claude Code"),
    );
    await waitFor(() =>
      expect(api.agentPermissionsSetEnabled).toHaveBeenCalledWith(true),
    );
    expect(await screen.findByText("Applied")).toBeInTheDocument();
  });

  it("an invalid rule is surfaced and the list is reseated from the backend", async () => {
    api.agentPermissionsSetRules.mockRejectedValue(
      new Error('"rm -rf *" is not a tool name. Use Claude Code\'s syntax'),
    );
    render(<AgentPermissionsView />);
    await screen.findByText(/No rules yet/);
    await userEvent.type(screen.getByLabelText("Rule pattern"), "rm -rf *");
    await userEvent.click(screen.getByRole("button", { name: "Add rule" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("not a tool name");
    expect(screen.getByText(/No rules yet/)).toBeInTheDocument();
    expect(api.agentPermissionsView).toHaveBeenCalledTimes(2);
    // The refused pattern stays in the box to be fixed, not wiped.
    expect(screen.getByLabelText("Rule pattern")).toHaveValue("rm -rf *");
  });

  it("a rule that saved but failed to reach one profile still clears the input and shows the error", async () => {
    api.agentPermissionsSetRules.mockRejectedValue(
      new Error(
        "The policy was saved, but one profile could not be updated: /x/settings.json: not JSON",
      ),
    );
    // The refresh after the error shows the rule in the list (it was saved).
    api.agentPermissionsView
      .mockResolvedValueOnce(view())
      .mockResolvedValueOnce(
        view({ rules: [{ pattern: "Bash(rm -rf *)", action: "deny" }] }),
      );
    render(<AgentPermissionsView />);
    await screen.findByText(/No rules yet/);
    await userEvent.type(screen.getByLabelText("Rule pattern"), "Bash(rm -rf *)");
    await userEvent.click(screen.getByRole("button", { name: "Add rule" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("could not be updated");
    await waitFor(() => expect(screen.getByLabelText("Rule pattern")).toHaveValue(""));
    expect(
      screen.getByRole("button", { name: "Remove rule Bash(rm -rf *)" }),
    ).toBeInTheDocument();
  });

  it("a refused duplicate keeps the typed pattern even though the list already has it", async () => {
    api.agentPermissionsView.mockResolvedValue(
      view({ rules: [{ pattern: "Bash(rm -rf *)", action: "deny" }] }),
    );
    api.agentPermissionsSetRules.mockRejectedValue(
      new Error('"Bash(rm -rf *)" appears more than once. A pattern maps to one action.'),
    );
    render(<AgentPermissionsView />);
    await screen.findByRole("button", { name: "Remove rule Bash(rm -rf *)" });
    await userEvent.type(screen.getByLabelText("Rule pattern"), "Bash(rm -rf *)");
    await userEvent.click(screen.getByRole("button", { name: "Add rule" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("more than once");
    expect(screen.getByLabelText("Rule pattern")).toHaveValue("Bash(rm -rf *)");
  });

  it("a preset whose patterns are all present, even under another action, is disabled", async () => {
    api.agentPermissionsView.mockResolvedValue(
      view({
        rules: [
          { pattern: "Bash(git push --force*)", action: "ask" },
          { pattern: "Bash(git push -f *)", action: "ask" },
        ],
      }),
    );
    render(<AgentPermissionsView />);
    expect(
      await screen.findByRole("button", { name: "Never force-push" }),
    ).toBeDisabled();
  });

  it("preview shows the bytes and writes nothing", async () => {
    api.agentPermissionsPreview.mockResolvedValue([
      {
        path: "/home/a/.claude/settings.json",
        before: "{}\n",
        after: '{\n  "permissions": { "deny": ["Bash(rm -rf *)"] }\n}\n',
      },
    ]);
    render(<AgentPermissionsView />);
    await screen.findByText(/No rules yet/);
    await userEvent.click(
      screen.getByRole("button", { name: "Preview what would be written" }),
    );
    expect(await screen.findByText("What would be written")).toBeInTheDocument();
    expect(screen.getByText(/"deny": \["Bash\(rm -rf \*\)"\]/)).toBeInTheDocument();
    expect(api.agentPermissionsSetRules).not.toHaveBeenCalled();
    expect(api.agentPermissionsSetEnabled).not.toHaveBeenCalled();
  });
});
