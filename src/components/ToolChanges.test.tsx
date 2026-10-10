import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { ToolChanges, groupToolChanges } from "./ToolChanges";
import { securityFixture } from "@/test/security-fixture";
import type { Registry } from "@/lib/types";
const release = vi.fn().mockResolvedValue(undefined);
vi.mock("@/lib/api", () => ({
  releaseQuarantine: (...args: unknown[]) => release(...args),
}));
vi.mock("@/lib/toast", () => ({ toastError: vi.fn() }));
const registry = {
  servers: [
    { id: "cloudflare_full_api", name: "Cloudflare (Full API)" },
    { id: "revenuecat", name: "RevenueCat" },
  ],
} as Registry;

describe("server tool changes", () => {
  it("groups a full package refresh, deduplicates tools and keeps later updates separate", () => {
    const events = securityFixture();
    expect(groupToolChanges([...events, events[0]])).toHaveLength(2);
    expect(groupToolChanges(events)[0].tools).toHaveLength(26);
    expect(
      groupToolChanges([...events, { ...events[0], ts: events[0].ts + 120_000 }]),
    ).toHaveLength(3);
  });
  it("keeps profiles separate for the same server and tool", () => {
    const event = securityFixture()[0];
    expect(
      groupToolChanges([
        { ...event, profile: "a" },
        { ...event, profile: "b" },
      ]),
    ).toHaveLength(2);
  });
  it("shows plain summaries and parameter deltas with neutral styling when unblocked", () => {
    render(
      <ToolChanges events={securityFixture()} registry={registry} onAccept={vi.fn()} />,
    );
    expect(
      screen.getByText("Cloudflare (Full API): 26 tools changed their inputs"),
    ).toBeInTheDocument();
    expect(
      screen.getByText("RevenueCat: 4 tools changed their descriptions"),
    ).toBeInTheDocument();
    expect(
      screen.getAllByText("Not blocked. Review the changes or accept them."),
    ).toHaveLength(2);
    expect(
      screen.getByLabelText("Tool changes").querySelector(".text-destructive"),
    ).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: /Cloudflare \(Full API\):/ }));
    expect(screen.getAllByText("Added parameters: comment")).toHaveLength(26);
    expect(screen.getAllByText("Removed parameters: legacy_id")).toHaveLength(26);
    expect(screen.getAllByText("Changed parameters: ttl")).toHaveLength(26);
  });
  it("accepts every tool for one server, and releases its active blocks before marking reviewed", async () => {
    release.mockClear();
    const events = securityFixture()
      .slice(0, 2)
      .map((event) => ({
        ...event,
        blocked: true,
        blocked_profiles: ["work", "personal"],
        new_fp: "v2:reviewed",
      }));
    const accept = vi.fn();
    render(<ToolChanges events={events} registry={registry} onAccept={accept} />);
    fireEvent.click(screen.getByRole("button", { name: "Accept all for this server" }));
    await waitFor(() => expect(accept).toHaveBeenCalledWith(events));
    expect(release).toHaveBeenCalledTimes(4);
    expect(release).toHaveBeenCalledWith("work", events[0].tool, "v2:reviewed");
  });
  it("requires per-tool review for poison flagged changes and shows matched signals", async () => {
    release.mockClear();
    const events = [
      {
        ...securityFixture()[0],
        blocked: true,
        blocked_profiles: ["work"],
        new_fp: "v2:reviewed",
        signatures: ["instruction_override"],
      },
    ];
    const accept = vi.fn();
    render(<ToolChanges events={events} registry={registry} onAccept={accept} />);
    expect(
      screen.getByRole("button", { name: "Accept all for this server" }),
    ).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: /Cloudflare \(Full API\):/ }));
    expect(screen.getByText(/Matched signals: instruction_override/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Accept this tool" }));
    await waitFor(() =>
      expect(release).toHaveBeenCalledWith("work", events[0].tool, "v2:reviewed"),
    );
  });
  it("keeps unknown blocking state visible and disables acceptance", () => {
    render(
      <ToolChanges
        events={[{ ...securityFixture()[0], blocked: null }]}
        registry={registry}
        onAccept={vi.fn()}
      />,
    );
    expect(
      screen.getByRole("button", { name: "Accept all for this server" }),
    ).toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: /Cloudflare \(Full API\):/ }));
    expect(screen.getByRole("button", { name: "Accept this tool" })).toBeDisabled();
  });
});
