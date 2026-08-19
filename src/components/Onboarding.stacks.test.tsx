import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { listStacks } from "@/lib/api";
import type { Registry, Stack } from "@/lib/types";
import { Onboarding } from "./Onboarding";

vi.mock("@/lib/api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/api")>();
  return {
    ...actual,
    listStacks: vi.fn(),
  };
});
vi.mock("sonner", () => ({ toast: { success: vi.fn(), error: vi.fn() } }));
vi.mock("@/lib/toast", () => ({ toastError: vi.fn() }));
vi.mock("@/components/ClientLogo", () => ({ ClientLogo: () => null }));

const registry: Registry = {
  version: 1,
  servers: [],
  profiles: [{ id: "default", name: "Default", enabledServerIds: [] }],
  activeProfileId: "default",
};

const stack: Stack = {
  id: "developer",
  name: "Developer",
  description: "A developer stack.",
  servers: [],
};

const props = {
  initialStep: 1,
  clients: [],
  registry,
  onRegistryChange: vi.fn(),
  onClientsRefresh: vi.fn(),
  onBrowseCatalog: vi.fn(),
  onProbe: vi.fn().mockResolvedValue([]),
  onOpenPlayground: vi.fn(),
  onOpenRules: vi.fn(),
  onFinish: vi.fn(),
};

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((res) => {
    resolve = res;
  });
  return { promise, resolve };
}

beforeEach(() => vi.clearAllMocks());

describe("Onboarding stack loading", () => {
  it("keeps onboarding usable and retries a failed stacks fetch", async () => {
    vi.mocked(listStacks)
      .mockRejectedValueOnce(new Error("registry unavailable"))
      .mockResolvedValueOnce([stack]);
    const user = userEvent.setup();

    render(<Onboarding {...props} />);

    expect(await screen.findByText("Starter stacks couldn't load")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Browse the full catalog" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "I'll add servers later" })).toBeEnabled();

    await user.click(screen.getByRole("button", { name: "Try again" }));

    expect(await screen.findByRole("button", { name: "Developer" })).toBeInTheDocument();
    expect(screen.queryByText("Starter stacks couldn't load")).not.toBeInTheDocument();
    expect(listStacks).toHaveBeenCalledTimes(2);
  });

  it("shows a skeleton while loading and stays quiet for an empty stack catalog", async () => {
    const pending = deferred<Stack[]>();
    vi.mocked(listStacks).mockReturnValueOnce(pending.promise);

    render(<Onboarding {...props} />);

    expect(
      screen.getByRole("status", { name: "Loading starter stacks" }),
    ).toBeInTheDocument();

    await act(async () => pending.resolve([]));

    await waitFor(() =>
      expect(
        screen.queryByRole("status", { name: "Loading starter stacks" }),
      ).not.toBeInTheDocument(),
    );
    expect(screen.queryByText("Starter stacks couldn't load")).not.toBeInTheDocument();
    expect(screen.queryByText("What do you work on?")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Browse the full catalog" })).toBeEnabled();
  });
});
