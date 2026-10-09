import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { addCatalogServer, popularCatalog, searchCatalog } from "@/lib/api";
import type { CatalogEntry, CatalogSearch, Registry } from "@/lib/types";
import identities from "../../src-tauri/tests/fixtures/catalog-identities.json";
import {
  catalogIdentity,
  catalogInstalledIdentities,
  installed,
} from "@/lib/catalogIdentity";
import { CatalogView } from "./CatalogView";

vi.mock("@/lib/api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/api")>();
  return {
    ...actual,
    addCatalogServer: vi.fn(),
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
  vi.mocked(searchCatalog).mockResolvedValue({
    entries: [],
    registryStatus: "available",
  });
  vi.mocked(addCatalogServer).mockResolvedValue(registry);
});

describe("CatalogView search and installed identity", () => {
  it.each(identities)("$case", (fixture) => {
    const { catalog, server, equal } = fixture;
    const a = catalogIdentity({
      ...entry,
      ...catalog,
      transport: catalog.transport as CatalogEntry["transport"],
    });
    expect(
      a !== null &&
        a ===
          catalogIdentity({
            ...entry,
            ...server,
            transport: server.transport as CatalogEntry["transport"],
          }),
    ).toBe(equal);
    expect(
      installed(
        new Set(
          catalogInstalledIdentities({
            ...server,
            source: server.source ?? null,
            transport: server.transport as CatalogEntry["transport"],
          }),
        ),
        { ...entry, source: "", ...catalog } as CatalogEntry,
      ),
    ).toBe("installedEqual" in fixture ? fixture.installedEqual : equal);
  });

  it.each(["unavailable", "timedOut"] as const)(
    "keeps curated cards during %s and retries",
    async (registryStatus) => {
      vi.mocked(searchCatalog)
        .mockResolvedValueOnce({ entries: [entry], registryStatus })
        .mockResolvedValueOnce({ entries: [entry], registryStatus: "available" });
      const user = userEvent.setup();
      render(<CatalogView registry={registry} onAdded={vi.fn()} />);
      await user.type(screen.getByRole("textbox"), "github");
      expect(await screen.findByText(/Showing curated matches only/)).toBeInTheDocument();
      expect(
        screen.getByText(
          registryStatus === "timedOut"
            ? /took too long to respond/
            : /Registry is unavailable/,
        ),
      ).toBeInTheDocument();
      expect(screen.getByText("GitHub")).toBeInTheDocument();
      expect(screen.queryByText(/No catalog results/)).not.toBeInTheDocument();
      await user.click(screen.getByRole("button", { name: "Try again" }));
      await waitFor(() => expect(searchCatalog).toHaveBeenCalledTimes(2));
      await waitFor(() =>
        expect(
          screen.queryByText(/Showing curated matches only/),
        ).not.toBeInTheDocument(),
      );
    },
  );

  it("keeps search results visible while a new query and retry are pending", async () => {
    const pendingSearch = deferred<CatalogSearch>();
    const pendingRetry = deferred<CatalogSearch>();
    const result = { ...entry, name: "Search-only result" };
    vi.mocked(searchCatalog)
      .mockResolvedValueOnce({ entries: [result], registryStatus: "available" })
      .mockReturnValueOnce(pendingSearch.promise)
      .mockReturnValueOnce(pendingRetry.promise);
    const user = userEvent.setup();
    render(<CatalogView registry={registry} onAdded={vi.fn()} />);
    await user.type(screen.getByRole("textbox"), "github");
    await screen.findByText(result.name);
    await user.type(screen.getByRole("textbox"), "x");
    await waitFor(() => expect(searchCatalog).toHaveBeenCalledTimes(2));
    expect(screen.getByText(result.name)).toBeInTheDocument();
    expect(screen.getByText("Searching the MCP Registry…")).toBeInTheDocument();
    await act(async () =>
      pendingSearch.resolve({ entries: [result], registryStatus: "timedOut" }),
    );
    await user.click(await screen.findByRole("button", { name: "Try again" }));
    await waitFor(() => expect(searchCatalog).toHaveBeenCalledTimes(3));
    expect(screen.getByText(result.name)).toBeInTheDocument();
    expect(screen.queryByText(/took too long/)).not.toBeInTheDocument();
    expect(screen.getByText("Searching the MCP Registry…")).toBeInTheDocument();
    await act(async () =>
      pendingRetry.resolve({ entries: [result], registryStatus: "available" }),
    );
    expect(screen.getByText(/1 result/)).toBeInTheDocument();
  });

  it("does not call an outage with no curated matches no results", async () => {
    vi.mocked(searchCatalog).mockResolvedValue({
      entries: [],
      registryStatus: "unavailable",
    });
    const user = userEvent.setup();
    render(<CatalogView registry={registry} onAdded={vi.fn()} />);
    await user.type(screen.getByRole("textbox"), "unknown");
    expect(await screen.findByText(/No curated matches/)).toBeInTheDocument();
    expect(screen.getByText(/Showing curated matches only/)).toBeInTheDocument();
    expect(screen.queryByText(/No catalog results/)).not.toBeInTheDocument();
  });

  it.each([true, false])("uses identity for installed cards: match=%s", async (match) => {
    render(
      <CatalogView
        registry={{
          ...registry,
          servers: [
            {
              id: "installed",
              enabled: false,
              name: match ? "Renamed" : entry.name,
              transport: "stdio",
              command: "npx",
              args: match ? entry.args : ["other-package"],
              env: [],
              url: null,
              source: "manual",
            },
          ],
        }}
        onAdded={vi.fn()}
      />,
    );
    await screen.findByText("GitHub");
    if (match) {
      expect(screen.getByText("in Toolport")).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: "Add" })).not.toBeInTheDocument();
    } else {
      expect(screen.queryByText("in Toolport")).not.toBeInTheDocument();
      expect(screen.getByRole("button", { name: "Add" })).toBeEnabled();
    }
  });
});
