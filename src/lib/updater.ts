import { invoke } from "@tauri-apps/api/core";
import { check, type DownloadEvent, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";

/** Outcome of an update check. `error` is distinct from `current` so the UI can
 * tell "you're up to date" apart from "couldn't reach the update server". */
export type UpdateCheck =
  | { kind: "update"; update: Update }
  | { kind: "current" }
  | { kind: "error"; message: string };

export type UpdateProgress =
  | { phase: "downloading"; downloadedBytes: number; totalBytes?: number }
  | { phase: "installing" };

export interface RestartClient {
  client: string;
  clientPid: number;
  gateway: string;
}

export interface UpdateShutdownReport {
  killed: string[];
  kept: string[];
  failed: string[];
  remaining: string[];
  needsRestart: RestartClient[];
  restartClients: RestartClient[];
  unattributedExternalStopped: string[];
  httpBridgePort: number | null;
  lifecycleErrors: string[];
}

interface UpdateRecoveryReport {
  httpBridgeRecovered: boolean;
  cleanupWarning: string | null;
}

type UpdateFailurePhase = "shutdown" | "install" | "relaunch";

/** An update error after gateways may have been stopped. The UI gets explicit,
 * truthful recovery guidance without pretending relaunching Toolport recreates
 * stdio pipes owned by external MCP clients. */
export class UpdateInstallError extends Error {
  readonly recoveryAdvice: string;

  constructor(
    readonly phase: UpdateFailurePhase,
    cause: unknown,
    readonly shutdown: UpdateShutdownReport,
    recovery: UpdateRecoveryReport | null,
    recoveryError: string | null,
  ) {
    const detail = cause instanceof Error ? cause.message : String(cause);
    super(detail);
    this.name = "UpdateInstallError";
    this.recoveryAdvice = recoveryAdvice(phase, shutdown, recovery, recoveryError);
  }
}

function unique(values: string[]): string[] {
  return [...new Set(values)];
}

function recoveryAdvice(
  phase: UpdateFailurePhase,
  shutdown: UpdateShutdownReport,
  recovery: UpdateRecoveryReport | null,
  recoveryError: string | null,
): string {
  const advice: string[] = [];
  if (phase === "relaunch") {
    // Installation already succeeded, so this process and its bundled helper are
    // now the old version. It must not republish/restart that helper; only starting
    // the newly installed Toolport can finish the update safely.
    advice.push("Quit and restart Toolport to finish applying the installed update.");
  }
  const blocked = unique([...shutdown.failed, ...shutdown.remaining]);
  if (blocked.length > 0) {
    advice.push(
      `Close the client apps that own these gateway processes, then retry: ${blocked.join("; ")}.`,
    );
  }
  if (shutdown.lifecycleErrors.length > 0) {
    advice.push(
      `Toolport could not safely prepare its managed HTTP endpoint: ${unique(shutdown.lifecycleErrors).join("; ")}. Resolve that error, then retry.`,
    );
  }

  const clients = unique(shutdown.restartClients.map((client) => client.client));
  if (clients.length > 0) {
    advice.push(
      `Restart ${clients.join(", ")} to recreate ${clients.length === 1 ? "its" : "their"} Toolport connection.`,
    );
  }
  if (shutdown.unattributedExternalStopped.length > 0) {
    // Parent attribution is best-effort and deliberately omitted for unreadable
    // processes. The Rust report separately proves these had external parents;
    // never infer that from the broad killed list, which also includes orphans and
    // Toolport-owned helpers.
    advice.push(
      phase === "relaunch"
        ? "Restart any MCP client that disconnected during the update."
        : "Restart any MCP client that disconnected during the update, then retry.",
    );
  }

  if (phase !== "relaunch" && shutdown.httpBridgePort != null) {
    if (recovery?.httpBridgeRecovered) {
      advice.push(
        `Toolport restored its HTTP endpoint on port ${shutdown.httpBridgePort}.`,
      );
      if (recovery.cleanupWarning) {
        advice.push(
          `The endpoint is available, but Toolport could not clear its update recovery marker: ${recovery.cleanupWarning}.`,
        );
      }
    } else if (recoveryError) {
      advice.push(
        `Toolport could not restore its HTTP endpoint on port ${shutdown.httpBridgePort}: ${recoveryError}. Start it again from Settings.`,
      );
    }
  }
  return advice.join(" ");
}

async function recoverAfterFailure(shutdown: UpdateShutdownReport): Promise<{
  recovery: UpdateRecoveryReport | null;
  error: string | null;
}> {
  try {
    return {
      recovery: await invoke<UpdateRecoveryReport>("recover_update_gateways", {
        httpBridgePort: shutdown.httpBridgePort,
      }),
      error: null,
    };
  } catch (error) {
    return {
      recovery: null,
      error: error instanceof Error ? error.message : String(error),
    };
  }
}

/** Check for a newer release via the Tauri updater. Never throws; failures
 * (dev build, offline, or no manifest published yet) come back as `error`. */
export async function checkForUpdate(): Promise<UpdateCheck> {
  try {
    const u = await check();
    return u?.available ? { kind: "update", update: u } : { kind: "current" };
  } catch (e) {
    return { kind: "error", message: String(e) };
  }
}

/** Download + install the update, reporting real byte progress before relaunch. */
export async function installUpdate(
  update: Update,
  onProgress?: (progress: UpdateProgress) => void,
): Promise<void> {
  let downloadedBytes = 0;
  let totalBytes: number | undefined;
  const report = (event: DownloadEvent) => {
    if (event.event === "Started") {
      downloadedBytes = 0;
      totalBytes = event.data.contentLength;
      onProgress?.({ phase: "downloading", downloadedBytes, totalBytes });
    } else if (event.event === "Progress") {
      downloadedBytes += event.data.chunkLength;
      onProgress?.({ phase: "downloading", downloadedBytes, totalBytes });
    }
  };
  // Keep MCP clients connected during the potentially slow download. Only stop
  // gateways once the package is local and the installer is ready to replace files.
  await update.download(report);
  onProgress?.({ phase: "installing" });
  const shutdown = await invoke<UpdateShutdownReport>("stop_spawned_gateways");
  const blocked = unique([
    ...shutdown.failed,
    ...shutdown.remaining,
    ...shutdown.lifecycleErrors,
  ]);
  if (blocked.length > 0) {
    const recovered = await recoverAfterFailure(shutdown);
    throw new UpdateInstallError(
      "shutdown",
      `Could not safely prepare gateway shutdown (${blocked.length} ${blocked.length === 1 ? "issue" : "issues"}); installation did not start.`,
      shutdown,
      recovered.recovery,
      recovered.error,
    );
  }

  try {
    await update.install();
  } catch (error) {
    const recovered = await recoverAfterFailure(shutdown);
    throw new UpdateInstallError(
      "install",
      error,
      shutdown,
      recovered.recovery,
      recovered.error,
    );
  }

  try {
    await relaunch();
  } catch (error) {
    throw new UpdateInstallError("relaunch", error, shutdown, null, null);
  }
}
