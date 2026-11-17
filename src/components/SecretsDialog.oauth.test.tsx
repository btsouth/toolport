import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { SecretsDialog } from "./SecretsDialog";
import type { ServerEntry } from "@/lib/types";
import { hasAuthToken, setAuthToken, clearAuthToken } from "@/lib/api";

const api = vi.hoisted(() => ({
  start: vi.fn(),
  cancel: vi.fn(),
  authenticate: vi.fn(),
  changed: vi.fn(),
  success: vi.fn(),
  error: vi.fn(),
}));
vi.mock("@/lib/api", () => ({
  startOauthAttempt: api.start,
  cancelOauthAttempt: api.cancel,
  authenticateOauth: api.authenticate,
  secretStatus: vi.fn().mockResolvedValue([]),
  hasAuthToken: vi.fn().mockResolvedValue(false),
  hasClientSecret: vi.fn().mockResolvedValue(false),
  probeAuth: vi.fn().mockResolvedValue({ kind: "oauth" }),
  setClientCredentials: vi.fn(),
  clearClientCredentials: vi.fn(),
  deleteSecret: vi.fn(),
  setSecret: vi.fn(),
  setAuthToken: vi.fn(),
  clearAuthToken: vi.fn(),
  setUiLanguage: vi.fn().mockResolvedValue(undefined),
}));
vi.mock("sonner", () => ({ toast: { success: api.success } }));
vi.mock("@/lib/toast", () => ({ toastError: api.error }));
vi.mock("@/lib/openUrl", () => ({ openExternal: vi.fn() }));

const server: ServerEntry = {
  id: "trello-test",
  name: "Trello",
  transport: "http",
  command: null,
  args: [],
  env: [],
  url: "https://mcp.trello.com/v1",
  source: "manual",
};
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}
async function open() {
  const user = userEvent.setup();
  const view = render(
    <SecretsDialog server={server} onSaved={vi.fn()} onChanged={api.changed} />,
  );
  await user.click(screen.getByRole("button", { name: /Manage secrets/ }));
  return { user, view };
}

// Simulated IPC: these tests cover the React UI's ownership of pending work.
// Backend loopback, keychain and real-provider behavior are separate evidence.
describe("browser OAuth recovery (simulated IPC)", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    api.start.mockReset().mockResolvedValue("first");
    api.cancel.mockReset().mockResolvedValue(true);
    api.authenticate.mockReset();
  });

  it("offers cancellation for abandoned authorization and gives recovery instructions", async () => {
    api.authenticate.mockReturnValue(new Promise(() => {}));
    const { user } = await open();
    await user.click(screen.getByRole("button", { name: "Sign in with browser" }));
    expect(screen.getByRole("button", { name: /Waiting for browser/ })).toBeDisabled();
    expect(screen.getByText(/attempt waits up to 3 minutes/)).toHaveTextContent(
      /tray.*Quit Toolport/,
    );
    expect(screen.getByText(/pasted access token is optional/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Cancel sign-in" }));
    await waitFor(() => expect(api.cancel).toHaveBeenCalledWith("first"));
    expect(screen.getByRole("button", { name: "Sign in with browser" })).toBeEnabled();
    expect(screen.getByRole("status")).toHaveTextContent(/cancelled/);
  });

  it.each(["success", "failure"])(
    "ignores a cancelled attempt's late %s while retry is pending",
    async (outcome) => {
      const old = deferred<void>();
      const retry = deferred<void>();
      api.start.mockResolvedValueOnce("old").mockResolvedValueOnce("new");
      api.authenticate
        .mockReturnValueOnce(old.promise)
        .mockReturnValueOnce(retry.promise);
      const { user } = await open();
      await user.click(screen.getByRole("button", { name: "Sign in with browser" }));
      await user.click(screen.getByRole("button", { name: "Cancel sign-in" }));
      await user.click(screen.getByRole("button", { name: "Sign in with browser" }));
      await waitFor(() =>
        expect(api.authenticate).toHaveBeenLastCalledWith(server.id, server.url, "new"),
      );
      await act(async () => {
        if (outcome === "success") old.resolve();
        else old.reject("late old failure");
      });
      expect(screen.getByRole("button", { name: /Waiting for browser/ })).toBeDisabled();
      expect(api.changed).not.toHaveBeenCalled();
      expect(api.success).not.toHaveBeenCalled();
      expect(api.error).not.toHaveBeenCalled();
      await act(async () => retry.resolve());
      expect(screen.getByRole("button", { name: "Sign in with browser" })).toBeEnabled();
      expect(api.changed).toHaveBeenCalledOnce();
      expect(api.success).toHaveBeenCalledWith("Authenticated");
      expect(screen.getByText("vaulted")).toBeInTheDocument();
    },
  );

  it("cancels on panel close and permits reopening before the old worker settles", async () => {
    const old = deferred<void>();
    api.authenticate.mockReturnValue(old.promise);
    const { user } = await open();
    await user.click(screen.getByRole("button", { name: "Sign in with browser" }));
    await user.keyboard("{Escape}");
    await waitFor(() => expect(api.cancel).toHaveBeenCalledWith("first"));
    await user.click(screen.getByRole("button", { name: /Manage secrets/ }));
    expect(screen.getByRole("button", { name: "Sign in with browser" })).toBeEnabled();
    await act(async () => old.resolve());
    expect(api.changed).not.toHaveBeenCalled();
    expect(screen.queryByText("vaulted")).not.toBeInTheDocument();
  });

  it("cancels an id that arrives after unmount without dispatching authorization", async () => {
    const start = deferred<string>();
    api.start.mockReturnValue(start.promise);
    const { user, view } = await open();
    await user.click(screen.getByRole("button", { name: "Sign in with browser" }));
    view.unmount();
    await act(async () => start.resolve("queued"));
    expect(api.cancel).toHaveBeenCalledWith("queued");
    expect(api.authenticate).not.toHaveBeenCalled();
  });

  it("retires an allocated id when authorization dispatch fails", async () => {
    api.authenticate.mockRejectedValue("IPC failed before dispatch");
    const { user } = await open();
    await user.click(screen.getByRole("button", { name: "Sign in with browser" }));
    await waitFor(() => expect(api.cancel).toHaveBeenCalledWith("first"));
    expect(screen.getByRole("button", { name: "Sign in with browser" })).toBeEnabled();
    expect(screen.getByRole("status")).toHaveTextContent(/IPC failed/);
  });

  it("checks token status if cancellation arrives after sign-in finished", async () => {
    api.authenticate.mockReturnValue(new Promise(() => {}));
    api.cancel.mockResolvedValue(false);
    const { user } = await open();
    await user.click(screen.getByRole("button", { name: "Sign in with browser" }));
    await user.click(screen.getByRole("button", { name: "Cancel sign-in" }));
    expect(screen.getByRole("status")).toHaveTextContent(/already finished/);
    expect(screen.getByRole("button", { name: "Sign in with browser" })).toBeEnabled();
  });

  it("recovers if attempt allocation fails while cancellation is requested", async () => {
    const start = deferred<string>();
    api.start.mockReturnValue(start.promise);
    const { user } = await open();
    await user.click(screen.getByRole("button", { name: "Sign in with browser" }));
    await user.click(screen.getByRole("button", { name: "Cancel sign-in" }));
    await act(async () => start.reject("allocation failed"));
    expect(screen.getByRole("button", { name: "Sign in with browser" })).toBeEnabled();
    expect(api.authenticate).not.toHaveBeenCalled();
    expect(api.cancel).not.toHaveBeenCalled();
  });

  it("allows retry after a timeout and after a failed cancellation request", async () => {
    api.authenticate.mockRejectedValueOnce("Browser sign-in timed out. Try again.");
    const { user } = await open();
    await user.click(screen.getByRole("button", { name: "Sign in with browser" }));
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Sign in with browser" })).toBeEnabled(),
    );
    api.authenticate.mockReturnValue(new Promise(() => {}));
    api.cancel.mockRejectedValueOnce("IPC unavailable");
    await user.click(screen.getByRole("button", { name: "Sign in with browser" }));
    await user.click(screen.getByRole("button", { name: "Cancel sign-in" }));
    await waitFor(() =>
      expect(screen.getByRole("status")).toHaveTextContent(/Could not cancel/),
    );
    await user.click(screen.getByRole("button", { name: "Cancel sign-in" }));
    expect(screen.getByRole("button", { name: "Sign in with browser" })).toBeEnabled();
  });
});

describe("access token in the dialog (simulated IPC)", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(setAuthToken).mockResolvedValue(undefined);
    vi.mocked(clearAuthToken).mockResolvedValue(undefined);
  });

  it("keeps focus in the token field after Save", async () => {
    const { user } = await open();
    const field = screen.getByPlaceholderText("paste access token");
    await user.type(field, "synthetic-token");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(setAuthToken).toHaveBeenCalledWith(server.id, "synthetic-token"),
    );
    await waitFor(() => expect(field).toHaveFocus());
    expect(field).toHaveValue("");
  });

  it("asks before removing the token and then puts focus in the token field", async () => {
    vi.mocked(hasAuthToken).mockResolvedValueOnce(true);
    const { user } = await open();
    await user.click(await screen.findByRole("button", { name: "Clear auth token" }));
    expect(clearAuthToken).not.toHaveBeenCalled();
    await user.click(screen.getByRole("button", { name: "Remove" }));
    await waitFor(() => expect(clearAuthToken).toHaveBeenCalledWith(server.id));
    await waitFor(() =>
      expect(screen.getByPlaceholderText("paste access token")).toHaveFocus(),
    );
  });
});
