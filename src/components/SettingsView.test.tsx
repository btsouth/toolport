import { describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { ThemeProvider } from "@/lib/theme";
import { SettingsView } from "./SettingsView";
import { listServerTools } from "@/lib/api";
import type { Registry } from "@/lib/types";

vi.mock("@/lib/api", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/api")>();

  return {
    ...actual,
    listServerTools: vi.fn(),
  };
});

const mockedListServerTools = vi.mocked(listServerTools);

const registry: Registry = {
  version: 1,
  servers: [
    {
      id: "github",
      name: "GitHub",
      transport: "stdio",
      command: null,
      args: [],
      env: [],
      url: null,
      source: null,
    },
    {
      id: "slack",
      name: "Slack",
      transport: "stdio",
      command: null,
      args: [],
      env: [],
      url: null,
      source: null,
    },
  ],
  profiles: [
    {
      id: "default",
      name: "Default",
      enabledServerIds: ["github", "slack"],
    },
  ],
  activeProfileId: "default",
};

function renderSettings() {
  render(
    <ThemeProvider>
      <SettingsView registry={registry} onRegistryChange={vi.fn()} />
    </ThemeProvider>,
  );
}

function deferred<T>() {
  let resolve!: (value: T) => void;

  const promise = new Promise<T>((res) => {
    resolve = res;
  });

  return {
    promise,
    resolve,
  };
}

describe("SettingsView tool loading", () => {
  it("keeps loading state scoped to each server", async () => {
    const user = userEvent.setup();

    const githubRequest = deferred<{ name: string }[]>();
    const slackRequest = deferred<{ name: string }[]>();

    mockedListServerTools
      .mockReturnValueOnce(githubRequest.promise)
      .mockReturnValueOnce(slackRequest.promise);

    renderSettings();

    // Open the profile.
    await user.click(
      screen.getByRole("button", {
        name: /default active 2 servers/i,
      }),
    );

    // Expand GitHub (request A starts).
    await user.click(
      screen.getByRole("button", {
        name: /github/i,
      }),
    );

    expect(screen.getByText("Loading tools…")).toBeInTheDocument();

    // Expand Slack while GitHub is still pending (request B starts).
    await user.click(
      screen.getByRole("button", {
        name: /slack/i,
      }),
    );

    // Slack is now the visible expanded server.
    expect(screen.getByText("Loading tools…")).toBeInTheDocument();

    // Resolve GitHub first.
    githubRequest.resolve([{ name: "repo-search" }]);

    // Slack should still be loading because loading is tracked per server.
    await waitFor(() => {
      expect(screen.getByText("Loading tools…")).toBeInTheDocument();
    });

    // Resolve Slack afterwards.
    slackRequest.resolve([{ name: "send-message" }]);

    expect(await screen.findByText("send-message")).toBeInTheDocument();

    await waitFor(() => {
      expect(screen.queryByText("Loading tools…")).not.toBeInTheDocument();
    });
  });
});
