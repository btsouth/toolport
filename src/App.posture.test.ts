import { describe, expect, it } from "vitest";
import { serverPostureCopy } from "./App";

describe("serverPostureCopy", () => {
  it("reports a confirmed healthy check", () => {
    expect(
      serverPostureCopy({
        backendReachable: true,
        probing: false,
        enabled: 3,
        checked: 3,
        connected: 3,
        attention: 0,
        disabled: 1,
      }),
    ).toEqual({
      healthy: true,
      title: "3 enabled servers reachable",
      detail: "1 turned off.",
    });
  });

  it("does not claim health while a check is incomplete", () => {
    const posture = serverPostureCopy({
      backendReachable: true,
      probing: true,
      enabled: 3,
      checked: 1,
      connected: 1,
      attention: 0,
      disabled: 0,
    });

    expect(posture.healthy).toBe(false);
    expect(posture.title).toBe("Checking server reachability");
    expect(posture.detail).toBe("1 of 3 checked so far.");
  });

  it("distinguishes an idle incomplete check from an active probe", () => {
    expect(
      serverPostureCopy({
        backendReachable: true,
        probing: false,
        enabled: 2,
        checked: 0,
        connected: 0,
        attention: 0,
        disabled: 0,
      }),
    ).toEqual({
      healthy: false,
      title: "Reachability check incomplete",
      detail: "0 of 2 checked. Refresh to try again.",
    });
  });

  it("reports when no servers are enabled", () => {
    expect(
      serverPostureCopy({
        backendReachable: true,
        probing: false,
        enabled: 0,
        checked: 0,
        connected: 0,
        attention: 0,
        disabled: 2,
      }),
    ).toEqual({
      healthy: false,
      title: "No servers enabled",
      detail: "2 servers turned off.",
    });
  });

  it("names attention without turning it into a global protection claim", () => {
    const posture = serverPostureCopy({
      backendReachable: true,
      probing: false,
      enabled: 3,
      checked: 3,
      connected: 2,
      attention: 1,
      disabled: 0,
    });

    expect(posture.healthy).toBe(false);
    expect(posture.title).toBe("2 of 3 enabled servers reachable");
    expect(posture.detail).toBe("1 needs a quick check.");
  });

  it("treats a failed health read as unknown, never all-clear", () => {
    const posture = serverPostureCopy({
      backendReachable: false,
      probing: false,
      enabled: 3,
      checked: 3,
      connected: 3,
      attention: 0,
      disabled: 0,
    });

    expect(posture.healthy).toBe(false);
    expect(posture.title).toBe("Reachability status unavailable");
    expect(posture.detail).toContain("Status may be out of date");
  });

  it("does not invent last-known health when the backend read failed", () => {
    expect(
      serverPostureCopy({
        backendReachable: false,
        probing: false,
        enabled: 2,
        checked: 0,
        connected: 0,
        attention: 0,
        disabled: 0,
      }),
    ).toEqual({
      healthy: false,
      title: "Reachability status unavailable",
      detail: "The last health check did not complete.",
    });
  });

  it("preserves an all-failed last-known check when the backend becomes unavailable", () => {
    expect(
      serverPostureCopy({
        backendReachable: false,
        probing: false,
        enabled: 2,
        checked: 2,
        connected: 0,
        attention: 2,
        disabled: 0,
      }),
    ).toEqual({
      healthy: false,
      title: "Reachability status unavailable",
      detail: "Last known: 0 reachable; 2 need a quick check. Status may be out of date.",
    });
  });
});
