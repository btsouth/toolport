import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { ToolChanges, groupToolChanges, sanitizeSegment } from "./ToolChanges";
import { securityFixture } from "@/test/security-fixture";
import type { Registry } from "@/lib/types";
const release = vi.fn().mockResolvedValue(undefined);
vi.mock("@/lib/api", () => ({
  releaseQuarantine: (...args: unknown[]) => release(...args),
}));
vi.mock("@/lib/toast", () => ({ toastError: vi.fn() }));
const registry = {
  servers: [
    { id: "cloudflare-full-api", name: "Cloudflare (Full API)" },
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
  it("keeps both definitions when a tool changes twice within a minute", () => {
    const event = securityFixture()[0];
    const groups = groupToolChanges([
      { ...event, new_fp: "v3:first" },
      { ...event, ts: event.ts + 1, new_fp: "v3:second" },
    ]);
    expect(groups).toHaveLength(1);
    expect(groups[0].tools).toHaveLength(2);
  });
  it("retains simultaneous profile findings with different severities", async () => {
    release.mockClear();
    const event = {
      ...securityFixture()[0],
      blocked: true,
      new_fp: "v3:same",
      profile: undefined,
    };
    const events = [
      { ...event, severity: "high" as const, blocked_profiles: ["a"], profiles: ["a"] },
      { ...event, severity: "warn" as const, blocked_profiles: ["b"], profiles: ["b"] },
    ];
    expect(groupToolChanges(events)[0].tools).toHaveLength(2);
    render(<ToolChanges events={events} registry={registry} onAccept={vi.fn()} />);
    fireEvent.click(screen.getByRole("button", { name: "Accept all for this server" }));
    await waitFor(() => expect(release).toHaveBeenCalledTimes(2));
    expect(release).toHaveBeenCalledWith("a", event.tool, "v3:same");
    expect(release).toHaveBeenCalledWith("b", event.tool, "v3:same");
  });
  it("matches Rust sanitization for ASCII and Unicode scalar values", () => {
    expect(sanitizeSegment("file-system")).toBe("file_system");
    expect(sanitizeSegment("file.🛠️é_01")).toBe("file_____01");
    expect(sanitizeSegment("already_ok")).toBe("already_ok");
  });
  it("uses singular wording for one new tool", () => {
    render(
      <ToolChanges
        events={[{ ...securityFixture()[0], change: "added" }]}
        registry={registry}
        onAccept={vi.fn()}
      />,
    );
    expect(screen.getByText("Cloudflare (Full API): 1 new tool")).toBeInTheDocument();
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
    // Usable, so shown and enabled.
    expect(
      screen.getByRole("button", { name: "Accept all for this server" }),
    ).toBeEnabled();
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
    // Never usable for a suspicious group: hidden, and the line says what to do.
    expect(
      screen.queryByRole("button", { name: "Accept all for this server" }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByText(
        "Suspicious content found. Review and accept each tool separately.",
      ),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /Cloudflare \(Full API\):/ }));
    expect(screen.getByText(/Matched signals: instruction_override/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Accept this tool" }));
    await waitFor(() =>
      expect(release).toHaveBeenCalledWith("work", events[0].tool, "v2:reviewed"),
    );
  });
  it("routes captured reviews to Settings without accepting a field summary", () => {
    release.mockClear();
    const event = {
      ...securityFixture()[0],
      blocked: true,
      blocked_profiles: ["work"],
      new_fp: "v3:captured",
      review_required: true,
      changed_fields: [],
    };
    const accept = vi.fn();
    const review = vi.fn();
    render(
      <ToolChanges
        events={[event]}
        registry={registry}
        onAccept={accept}
        onReview={review}
      />,
    );
    expect(
      screen.queryByRole("button", { name: "Accept all for this server" }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByText("Review the captured definition in Settings before accepting it."),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /Cloudflare \(Full API\):/ }));
    expect(
      screen.queryByRole("button", { name: "Accept this tool" }),
    ).not.toBeInTheDocument();
    expect(
      screen.getAllByRole("button", { name: "Review in Settings" }).length,
    ).toBeGreaterThan(0);
    fireEvent.click(screen.getAllByRole("button", { name: "Review in Settings" })[0]);
    expect(review).toHaveBeenCalledOnce();
    expect(release).not.toHaveBeenCalled();
    expect(accept).not.toHaveBeenCalled();
  });
  it("waits for capture without a Settings review, then routes the captured review", () => {
    const event = {
      ...securityFixture()[0],
      blocked: true,
      blocked_profiles: [""],
      new_fp: "v3:new",
      review_required: true,
    };
    const capturing =
      "Toolport is capturing this tool's new definition. It stays blocked until then.";
    const { unmount } = render(
      <ToolChanges
        events={[{ ...event, pending_capture: true }]}
        registry={registry}
        onAccept={vi.fn()}
        onReview={vi.fn()}
      />,
    );
    expect(screen.getByText(capturing)).toBeInTheDocument();
    expect(screen.queryByText(/Review the captured definition/)).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /Cloudflare \(Full API\):/ }));
    expect(
      screen.queryByRole("button", { name: "Review in Settings" }),
    ).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Accept/ })).not.toBeInTheDocument();
    unmount();
    // Once captured, the normal review card returns.
    render(
      <ToolChanges
        events={[event]}
        registry={registry}
        onAccept={vi.fn()}
        onReview={vi.fn()}
      />,
    );
    expect(screen.queryByText(capturing)).not.toBeInTheDocument();
    expect(
      screen.getByText("Review the captured definition in Settings before accepting it."),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Review in Settings" }),
    ).toBeInTheDocument();
  });
  it("keeps Review in Settings for a captured tool beside one being captured", () => {
    const event = {
      ...securityFixture()[0],
      blocked: true,
      blocked_profiles: [""],
      new_fp: "v3:new",
      review_required: true,
    };
    const capturing =
      "Toolport is capturing this tool's new definition. It stays blocked until then.";
    const review = vi.fn();
    render(
      <ToolChanges
        events={[event, { ...event, tool: `${event.tool}_2`, pending_capture: true }]}
        registry={registry}
        onAccept={vi.fn()}
        onReview={review}
      />,
    );
    expect(
      screen.getByText("Review the captured definition in Settings before accepting it."),
    ).toBeInTheDocument();
    expect(screen.queryByText(capturing)).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Accept all for this server" }),
    ).not.toBeInTheDocument();
    fireEvent.click(screen.getAllByRole("button", { name: "Review in Settings" })[0]);
    expect(review).toHaveBeenCalledOnce();
    // Expanded: the captured tool routes to Settings, the capturing tool says
    // it is being captured, and neither offers an accept.
    fireEvent.click(screen.getByRole("button", { name: /Cloudflare \(Full API\):/ }));
    expect(screen.getAllByRole("button", { name: "Review in Settings" })).toHaveLength(2);
    expect(screen.getByText(capturing)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Accept/ })).not.toBeInTheDocument();
    // Only the captured tool says it is blocked until accepted.
    expect(screen.getAllByText("Blocked until you accept the changes.")).toHaveLength(1);
  });
  it("explains an unknown status before a block in a mixed group", () => {
    const event = securityFixture()[0];
    render(
      <ToolChanges
        events={[
          { ...event, blocked: true, blocked_profiles: ["work"], new_fp: "v2:a" },
          { ...event, tool: `${event.tool}_2`, blocked: null },
        ]}
        registry={registry}
        onAccept={vi.fn()}
      />,
    );
    expect(
      screen.getByText(
        "Blocking status unavailable. Review in Settings > Quarantined tools.",
      ),
    ).toBeInTheDocument();
    expect(
      screen.queryByText(/Review the changes or accept them/),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Accept all for this server" }),
    ).not.toBeInTheDocument();
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
      screen.queryByRole("button", { name: "Accept all for this server" }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByText(
        "Blocking status unavailable. Review in Settings > Quarantined tools.",
      ),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /Cloudflare \(Full API\):/ }));
    expect(screen.getByRole("button", { name: "Accept this tool" })).toBeDisabled();
  });
});
