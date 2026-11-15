import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { refreshClientRecoveryStatus } from "@/lib/api";
import {
  CLIENT_RECOVERY_SCAN_EVENT,
  requestClientRecoveryScan,
} from "./clientRecoveryEvents";

vi.mock("@/lib/api", () => ({ refreshClientRecoveryStatus: vi.fn() }));
const handlers: EventListener[] = [];
beforeEach(() => {
  vi.mocked(refreshClientRecoveryStatus).mockReset();
});
afterEach(() => {
  for (const handler of handlers.splice(0))
    window.removeEventListener(CLIENT_RECOVERY_SCAN_EVENT, handler);
});

it("checks the backend when no recovery view is mounted", async () => {
  vi.mocked(refreshClientRecoveryStatus).mockResolvedValue(undefined);
  await requestClientRecoveryScan();
  expect(refreshClientRecoveryStatus).toHaveBeenCalledTimes(1);
});

it("waits for the visible view's check without starting a duplicate", async () => {
  let done!: () => void;
  const handler: EventListener = (event) => {
    event.preventDefault();
    done = (event as CustomEvent<{ done: () => void }>).detail.done;
  };
  handlers.push(handler);
  window.addEventListener(CLIENT_RECOVERY_SCAN_EVENT, handler);
  let settled = false;
  const checking = requestClientRecoveryScan().then(() => {
    settled = true;
  });
  await Promise.resolve();
  expect(settled).toBe(false);
  expect(refreshClientRecoveryStatus).not.toHaveBeenCalled();
  done();
  await checking;
  expect(settled).toBe(true);
});

it("reports a failed check when no view can show the error", async () => {
  vi.mocked(refreshClientRecoveryStatus).mockRejectedValue(
    "credential store unavailable",
  );
  const outcome = await requestClientRecoveryScan().then(
    () => ({ resolved: true, error: undefined }),
    (error: unknown) => ({ resolved: false, error }),
  );
  expect(outcome).toEqual({ resolved: false, error: "credential store unavailable" });
});
