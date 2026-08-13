import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { Onboarding } from "./Onboarding";
import type { DetectedClient, Registry } from "@/lib/types";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

const registry = {
  version: 1,
  servers: [{ id: "server-1", name: "Server", transport: "stdio" }],
  profiles: [{ id: "default", name: "Default", enabledServerIds: [] }],
  activeProfileId: "default",
} as unknown as Registry;

const client = {
  id: "cursor",
  name: "Cursor",
  appPresent: true,
  gatewayInstalled: true,
  servers: [],
  pluginServers: [],
} as unknown as DetectedClient;

const props = {
  initialStep: 3,
  clients: [client],
  registry,
  onRegistryChange: vi.fn(),
  onClientsRefresh: vi.fn(),
  onBrowseCatalog: vi.fn(),
  onOpenPlayground: vi.fn(),
  onFinish: vi.fn(),
};

describe("Onboarding health verification", () => {
  it("does not present setup as ready while the health probe is in flight", async () => {
    const probe = deferred<[]>();
    render(<Onboarding {...props} onProbe={() => probe.promise} />);

    expect(await screen.findByText("Checking server health…")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Continue without verification" }),
    ).toBeEnabled();
    expect(screen.queryByText("You're set up")).not.toBeInTheDocument();

    probe.resolve([]);
    expect(await screen.findByText("You're set up")).toBeInTheDocument();
  });

  it("does not dress an unconfigured setup in verification language", async () => {
    const probe = deferred<[]>();
    const disconnected = { ...client, gatewayInstalled: false } as DetectedClient;
    render(
      <Onboarding {...props} clients={[disconnected]} onProbe={() => probe.promise} />,
    );

    // Servers exist but no client is connected: the step explains what's missing,
    // so neither the probe's pending state nor its failure may relabel the finish
    // button or show verification status blocks.
    expect(await screen.findByText("Setup started")).toBeInTheDocument();
    expect(screen.queryByText("Checking server health…")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Got it" })).toBeEnabled();

    await act(async () => probe.reject(new Error("gateway not running")));

    expect(screen.getByRole("button", { name: "Got it" })).toBeEnabled();
    expect(
      screen.queryByText(/couldn't verify your servers started/i),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Continue without verification" }),
    ).not.toBeInTheDocument();
  });

  it("keeps a failed probe unavailable and offers an authoritative retry", async () => {
    const onProbe = vi
      .fn()
      .mockRejectedValueOnce(new Error("backend unavailable"))
      .mockResolvedValueOnce([]);

    render(<Onboarding {...props} onProbe={onProbe} />);
    expect(
      await screen.findByText(/couldn't verify your servers started/i),
    ).toBeInTheDocument();
    expect(screen.getByText("Setup couldn't be verified")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Continue without verification" }),
    ).toBeEnabled();
    expect(screen.queryByText(/server.*couldn't start/i)).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    await waitFor(() => expect(onProbe).toHaveBeenCalledTimes(2));
    await waitFor(() =>
      expect(
        screen.queryByText(/couldn't verify your servers started/i),
      ).not.toBeInTheDocument(),
    );
  });
});
