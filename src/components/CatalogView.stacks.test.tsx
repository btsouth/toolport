import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { addServer, listStacks, popularCatalog, searchCatalog } from "@/lib/api";
import type { CatalogEntry, Registry, Stack } from "@/lib/types";
import { CatalogView } from "./CatalogView";

vi.mock("@/lib/api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/api")>();
  return {
    ...actual,
    addServer: vi.fn(),
    listStacks: vi.fn(),
    popularCatalog: vi.fn(),
    searchCatalog: vi.fn(),
  };
});
vi.mock("sonner", () => ({ toast: { success: vi.fn(), error: vi.fn() } }));
vi.mock("@/lib/toast", () => ({ toastError: vi.fn() }));

const entry: CatalogEntry = {
  name: "GitHub",
  description: "Work with repositories and issues.",
  transport: "stdio",
  command: "npx",
  args: ["-y", "github-mcp"],
  url: null,
  envKeys: [],
  source: "curated",
  homepage: null,
  category: "Code & infrastructure",
};

const stack: Stack = {
  id: "developer",
  name: "Developer",
  description: "A developer stack.",
  servers: [entry],
};

const registry: Registry = {
  version: 1,
  servers: [],
  profiles: [],
  activeProfileId: null,
};

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((res) => {
    resolve = res;
  });
  return { promise, resolve };
}

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(popularCatalog).mockResolvedValue([entry]);
  vi.mocked(searchCatalog).mockResolvedValue([]);
  vi.mocked(addServer).mockResolvedValue(registry);
});

describe("CatalogView stack loading", () => {
  it("keeps the catalog visible and retries a failed stacks fetch", async () => {
    vi.mocked(listStacks)
      .mockRejectedValueOnce(new Error("registry unavailable"))
      .mockResolvedValueOnce([stack]);
    const user = userEvent.setup();

    render(<CatalogView registry={registry} onAdded={vi.fn()} />);

    expect(await screen.findByText("Stacks couldn't load")).toBeInTheDocument();
    expect(screen.getByText("GitHub")).toBeInTheDocument();
    expect(screen.queryByText("Catalog couldn't load")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Try again" }));

    expect(await screen.findByText("Developer")).toBeInTheDocument();
    expect(screen.queryByText("Stacks couldn't load")).not.toBeInTheDocument();
    expect(listStacks).toHaveBeenCalledTimes(2);
  });

  it("shows stacks when the popular catalog is empty", async () => {
    vi.mocked(popularCatalog).mockResolvedValueOnce([]);
    vi.mocked(listStacks).mockResolvedValueOnce([stack]);

    render(<CatalogView registry={registry} onAdded={vi.fn()} />);

    expect(await screen.findByText("Developer")).toBeInTheDocument();
    expect(screen.getByText("No popular servers available")).toBeInTheDocument();
  });

  it("keeps stack failure and retry visible when the popular catalog is empty", async () => {
    vi.mocked(popularCatalog).mockResolvedValueOnce([]);
    vi.mocked(listStacks)
      .mockRejectedValueOnce(new Error("registry unavailable"))
      .mockResolvedValueOnce([stack]);
    const user = userEvent.setup();

    render(<CatalogView registry={registry} onAdded={vi.fn()} />);

    expect(await screen.findByText("Stacks couldn't load")).toBeInTheDocument();
    expect(screen.getByText("No popular servers available")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Try again" }));

    expect(await screen.findByText("Developer")).toBeInTheDocument();
    expect(screen.queryByText("Stacks couldn't load")).not.toBeInTheDocument();
    expect(listStacks).toHaveBeenCalledTimes(2);
  });

  it("shows a skeleton while loading and stays quiet for an empty stack catalog", async () => {
    const pending = deferred<Stack[]>();
    vi.mocked(listStacks).mockReturnValueOnce(pending.promise);

    render(<CatalogView registry={registry} onAdded={vi.fn()} />);

    expect(
      await screen.findByRole("status", { name: "Loading stacks" }),
    ).toBeInTheDocument();

    await act(async () => pending.resolve([]));

    await waitFor(() =>
      expect(
        screen.queryByRole("status", { name: "Loading stacks" }),
      ).not.toBeInTheDocument(),
    );
    expect(screen.queryByText("Stacks couldn't load")).not.toBeInTheDocument();
    expect(screen.queryByRole("heading", { name: /^Stacks/ })).not.toBeInTheDocument();
    expect(screen.getByText("GitHub")).toBeInTheDocument();
  });
});
