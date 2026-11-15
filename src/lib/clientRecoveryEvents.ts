import { refreshClientRecoveryStatus } from "@/lib/api";

export const CLIENT_RECOVERY_SCAN_EVENT = "toolport:client-recovery-scan";

/** Await the visible banner's check, or check directly if it is not mounted. */
export function requestClientRecoveryScan(): Promise<void> {
  return new Promise((resolve, reject) => {
    const event = new CustomEvent(CLIENT_RECOVERY_SCAN_EVENT, {
      cancelable: true,
      detail: { done: resolve },
    });
    window.dispatchEvent(event);
    if (!event.defaultPrevented) void refreshClientRecoveryStatus().then(resolve, reject);
  });
}
