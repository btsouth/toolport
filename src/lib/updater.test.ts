import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Update } from "@tauri-apps/plugin-updater";

const invoke = vi.fn();
const relaunch = vi.fn();

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invoke(...args),
}));
vi.mock("@tauri-apps/plugin-updater", () => ({ check: vi.fn() }));
vi.mock("@tauri-apps/plugin-process", () => ({
  relaunch: (...args: unknown[]) => relaunch(...args),
}));

import { installUpdate, UpdateInstallError, type UpdateProgress } from "./updater";

const cleanShutdown = {
  killed: ["toolport-gateway (pid 10)"],
  kept: [],
  failed: [],
  remaining: [],
  needsRestart: [],
  restartClients: [],
  unattributedExternalStopped: [],
  httpBridgePort: null,
  lifecycleErrors: [],
};

beforeEach(() => {
  invoke.mockReset().mockResolvedValue(cleanShutdown);
  relaunch.mockReset().mockResolvedValue(undefined);
});

describe("installUpdate", () => {
  it("downloads with progress before disconnecting gateways and installing", async () => {
    const calls: string[] = [];
    const download = vi.fn(async (onEvent: (event: unknown) => void) => {
      calls.push("download");
      onEvent({ event: "Started", data: { contentLength: 10 } });
      onEvent({ event: "Progress", data: { chunkLength: 4 } });
      onEvent({ event: "Progress", data: { chunkLength: 6 } });
      onEvent({ event: "Finished" });
    });
    const install = vi.fn(async () => {
      calls.push("install");
    });
    invoke.mockImplementation(async () => {
      calls.push("stop");
      return cleanShutdown;
    });
    relaunch.mockImplementation(async () => {
      calls.push("relaunch");
    });
    const progress: UpdateProgress[] = [];

    await installUpdate({ download, install } as unknown as Update, (event) => {
      progress.push(event);
    });

    expect(calls).toEqual(["download", "stop", "install", "relaunch"]);
    expect(progress).toEqual([
      { phase: "downloading", downloadedBytes: 0, totalBytes: 10 },
      { phase: "downloading", downloadedBytes: 4, totalBytes: 10 },
      { phase: "downloading", downloadedBytes: 10, totalBytes: 10 },
      { phase: "installing" },
    ]);
    expect(invoke).toHaveBeenCalledWith("stop_spawned_gateways");
  });

  it("refuses installation while a targeted gateway remains alive", async () => {
    const download = vi.fn().mockResolvedValue(undefined);
    const install = vi.fn().mockResolvedValue(undefined);
    invoke.mockImplementation(async (command: string) => {
      if (command === "stop_spawned_gateways") {
        return {
          ...cleanShutdown,
          failed: ["toolport-gateway.exe (pid 42 @ C:\\Toolport\\toolport-gateway.exe)"],
          remaining: [
            "toolport-gateway.exe (pid 42 @ C:\\Toolport\\toolport-gateway.exe)",
          ],
          httpBridgePort: 8765,
        };
      }
      if (command === "recover_update_gateways") {
        return { httpBridgeRecovered: true };
      }
      throw new Error(`unexpected command ${command}`);
    });

    const failure = await installUpdate({ download, install } as unknown as Update).catch(
      (error: unknown) => error,
    );

    expect(failure).toBeInstanceOf(UpdateInstallError);
    expect(failure).toMatchObject({ phase: "shutdown" });
    expect((failure as UpdateInstallError).recoveryAdvice).toContain(
      "Close the client apps that own these gateway processes",
    );
    expect((failure as UpdateInstallError).recoveryAdvice).toContain(
      "restored its HTTP endpoint on port 8765",
    );
    expect(install).not.toHaveBeenCalled();
    expect(relaunch).not.toHaveBeenCalled();
    expect(invoke).toHaveBeenCalledWith("recover_update_gateways", {
      httpBridgePort: 8765,
    });
  });

  it("blocks on an owned-endpoint lifecycle error without blaming an external client", async () => {
    const download = vi.fn().mockResolvedValue(undefined);
    const install = vi.fn().mockResolvedValue(undefined);
    invoke.mockImplementation(async (command: string) => {
      if (command === "stop_spawned_gateways") {
        return {
          ...cleanShutdown,
          lifecycleErrors: ["could not persist the HTTP bridge recovery state"],
        };
      }
      if (command === "recover_update_gateways") {
        return { httpBridgeRecovered: false };
      }
      throw new Error(`unexpected command ${command}`);
    });

    const failure = await installUpdate({ download, install } as unknown as Update).catch(
      (error: unknown) => error,
    );

    expect(failure).toBeInstanceOf(UpdateInstallError);
    expect(failure).toMatchObject({ phase: "shutdown" });
    expect((failure as UpdateInstallError).recoveryAdvice).toContain(
      "could not safely prepare its managed HTTP endpoint",
    );
    expect((failure as UpdateInstallError).recoveryAdvice).not.toContain(
      "Close the client apps",
    );
    expect((failure as UpdateInstallError).recoveryAdvice).not.toContain(
      "Restart any MCP client",
    );
    expect(install).not.toHaveBeenCalled();
  });

  it("reports exact client restart guidance when install rejects after shutdown", async () => {
    const download = vi.fn().mockResolvedValue(undefined);
    const install = vi.fn().mockRejectedValue(new Error("package signature rejected"));
    invoke.mockImplementation(async (command: string) => {
      if (command === "stop_spawned_gateways") {
        return {
          ...cleanShutdown,
          restartClients: [
            {
              client: "Cursor.exe",
              clientPid: 77,
              gateway: "toolport-gateway.exe",
            },
          ],
          httpBridgePort: 8765,
        };
      }
      if (command === "recover_update_gateways") {
        return { httpBridgeRecovered: true };
      }
      throw new Error(`unexpected command ${command}`);
    });

    const failure = await installUpdate({ download, install } as unknown as Update).catch(
      (error: unknown) => error,
    );

    expect(failure).toBeInstanceOf(UpdateInstallError);
    expect(failure).toMatchObject({
      phase: "install",
      message: "package signature rejected",
    });
    expect((failure as UpdateInstallError).recoveryAdvice).toContain(
      "Restart Cursor.exe to recreate its Toolport connection.",
    );
    expect((failure as UpdateInstallError).recoveryAdvice).toContain(
      "Toolport restored its HTTP endpoint",
    );
    expect((failure as UpdateInstallError).recoveryAdvice).not.toContain(
      "Toolport restored Cursor.exe",
    );
    expect(relaunch).not.toHaveBeenCalled();
  });

  it("reports both named and proven unnamed external clients", async () => {
    const download = vi.fn().mockResolvedValue(undefined);
    const install = vi.fn().mockRejectedValue(new Error("installer rejected"));
    invoke.mockImplementation(async (command: string) => {
      if (command === "stop_spawned_gateways") {
        return {
          ...cleanShutdown,
          restartClients: [
            { client: "Cursor.exe", clientPid: 77, gateway: "toolport-gateway.exe" },
          ],
          unattributedExternalStopped: ["toolport-gateway.exe (pid 11)"],
        };
      }
      if (command === "recover_update_gateways") {
        return { httpBridgeRecovered: false, cleanupWarning: null };
      }
      throw new Error(`unexpected command ${command}`);
    });

    const failure = await installUpdate({ download, install } as unknown as Update).catch(
      (error: unknown) => error,
    );
    const advice = (failure as UpdateInstallError).recoveryAdvice;
    expect(advice).toContain("Restart Cursor.exe");
    expect(advice).toContain("Restart any MCP client that disconnected");
  });

  it("reports a restored endpoint separately from resume-marker cleanup failure", async () => {
    const download = vi.fn().mockResolvedValue(undefined);
    const install = vi.fn().mockRejectedValue(new Error("installer rejected"));
    invoke.mockImplementation(async (command: string) => {
      if (command === "stop_spawned_gateways") {
        return { ...cleanShutdown, httpBridgePort: 8765 };
      }
      if (command === "recover_update_gateways") {
        return {
          httpBridgeRecovered: true,
          cleanupWarning: "access denied",
        };
      }
      throw new Error(`unexpected command ${command}`);
    });

    const failure = await installUpdate({ download, install } as unknown as Update).catch(
      (error: unknown) => error,
    );
    const advice = (failure as UpdateInstallError).recoveryAdvice;
    expect(advice).toContain("restored its HTTP endpoint on port 8765");
    expect(advice).toContain("endpoint is available");
    expect(advice).toContain("access denied");
    expect(advice).not.toContain("could not restore its HTTP endpoint");
  });

  it("gives generic restart guidance only for a proven but unnamed external client", async () => {
    const download = vi.fn().mockResolvedValue(undefined);
    const install = vi.fn().mockRejectedValue(new Error("installer rejected"));
    invoke.mockImplementation(async (command: string) => {
      if (command === "stop_spawned_gateways") {
        return {
          ...cleanShutdown,
          unattributedExternalStopped: ["toolport-gateway.exe (pid 10)"],
        };
      }
      if (command === "recover_update_gateways") {
        return { httpBridgeRecovered: false };
      }
      throw new Error(`unexpected command ${command}`);
    });

    const failure = await installUpdate({ download, install } as unknown as Update).catch(
      (error: unknown) => error,
    );

    expect(failure).toBeInstanceOf(UpdateInstallError);
    expect(failure).toMatchObject({ message: "installer rejected" });
    expect((failure as UpdateInstallError).recoveryAdvice).toBe(
      "Restart any MCP client that disconnected during the update, then retry.",
    );
  });

  it("does not republish the old helper when installation succeeds but relaunch fails", async () => {
    const download = vi.fn().mockResolvedValue(undefined);
    const install = vi.fn().mockResolvedValue(undefined);
    invoke.mockResolvedValue({
      ...cleanShutdown,
      unattributedExternalStopped: ["toolport-gateway.exe (pid 10)"],
      httpBridgePort: 8765,
    });
    relaunch.mockRejectedValue(new Error("relaunch denied"));

    const failure = await installUpdate({ download, install } as unknown as Update).catch(
      (error: unknown) => error,
    );

    expect(failure).toBeInstanceOf(UpdateInstallError);
    expect(failure).toMatchObject({ phase: "relaunch", message: "relaunch denied" });
    expect((failure as UpdateInstallError).recoveryAdvice).toContain(
      "Quit and restart Toolport to finish applying the installed update.",
    );
    expect((failure as UpdateInstallError).recoveryAdvice).toContain(
      "Restart any MCP client that disconnected during the update.",
    );
    expect((failure as UpdateInstallError).recoveryAdvice).not.toContain("then retry");
    expect((failure as UpdateInstallError).recoveryAdvice).not.toContain(
      "restored its HTTP endpoint",
    );
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).not.toHaveBeenCalledWith("recover_update_gateways", expect.anything());
  });
});
