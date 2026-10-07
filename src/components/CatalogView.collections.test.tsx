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

const collection: Stack = {
  id: "developer",
  name: "Developer",
  description: "A developer Collection.",
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

describe("CatalogView collection loading", () => {
  it("keeps the catalog visible and retries a failed collections fetch", async () => {
    vi.mocked(listStacks)
      .mockRejectedValueOnce(new Error("registry unavailable"))
      .mockResolvedValueOnce([collection]);
    const user = userEvent.setup();

    render(<CatalogView registry={registry} onAdded={vi.fn()} />);

    expect(await screen.findByText("Collections couldn't load")).toBeInTheDocument();
    expect(screen.getByText("GitHub")).toBeInTheDocument();
    expect(screen.queryByText("Catalog couldn't load")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Try again" }));

    expect(await screen.findByText("Developer")).toBeInTheDocument();
    expect(screen.queryByText("Collections couldn't load")).not.toBeInTheDocument();
    expect(listStacks).toHaveBeenCalledTimes(2);
  });

  it("shows collections when the popular catalog is empty", async () => {
    vi.mocked(popularCatalog).mockResolvedValueOnce([]);
    vi.mocked(listStacks).mockResolvedValueOnce([collection]);

    render(<CatalogView registry={registry} onAdded={vi.fn()} />);

    expect(await screen.findByText("Developer")).toBeInTheDocument();
    expect(screen.getByText("No popular servers available")).toBeInTheDocument();
  });

  it("keeps collection failure and retry visible when the popular catalog is empty", async () => {
    vi.mocked(popularCatalog).mockResolvedValueOnce([]);
    vi.mocked(listStacks)
      .mockRejectedValueOnce(new Error("registry unavailable"))
      .mockResolvedValueOnce([collection]);
    const user = userEvent.setup();

    render(<CatalogView registry={registry} onAdded={vi.fn()} />);

    expect(await screen.findByText("Collections couldn't load")).toBeInTheDocument();
    expect(screen.getByText("No popular servers available")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Try again" }));

    expect(await screen.findByText("Developer")).toBeInTheDocument();
    expect(screen.queryByText("Collections couldn't load")).not.toBeInTheDocument();
    expect(listStacks).toHaveBeenCalledTimes(2);
  });

  it("shows a skeleton while loading and stays quiet for an empty collection catalog", async () => {
    const pending = deferred<Stack[]>();
    vi.mocked(listStacks).mockReturnValueOnce(pending.promise);

    render(<CatalogView registry={registry} onAdded={vi.fn()} />);

    expect(
      await screen.findByRole("status", { name: "Loading collections" }),
    ).toBeInTheDocument();

    await act(async () => pending.resolve([]));

    await waitFor(() =>
      expect(
        screen.queryByRole("status", { name: "Loading collections" }),
      ).not.toBeInTheDocument(),
    );
    expect(screen.queryByText("Collections couldn't load")).not.toBeInTheDocument();
    expect(
      screen.queryByRole("heading", { name: /^Collections/ }),
    ).not.toBeInTheDocument();
    expect(screen.getByText("GitHub")).toBeInTheDocument();
  });
});
