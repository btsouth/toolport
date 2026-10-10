import { useState } from "react";
import { dismissAccessUpgradeNotice, stopStaleGateways } from "@/lib/api";
import type { Registry } from "@/lib/types";
import { toastError } from "@/lib/toast";

export function AccessUpgradeNotice({
  registry,
  onRegistryChange,
}: {
  registry: Registry | null;
  onRegistryChange: (registry: Registry) => void;
}) {
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState("");
  if (
    !registry ||
    registry.version < 3 ||
    !registry.accessUpgradeNoticePending ||
    registry.accessUpgradeNoticeDismissed
  )
    return null;
  return (
    <div
      role="status"
      className="access-upgrade-notice flex shrink-0 flex-wrap items-center gap-2 border-b bg-info/10 px-3 py-2 text-sm sm:gap-3 sm:px-6 sm:py-3"
    >
      {/* eslint-disable jsx-a11y/no-noninteractive-tabindex -- The labelled scroll region needs keyboard focus. */}
      <p
        role="region"
        aria-label="Upgrade details"
        tabIndex={0}
        className="min-w-0 flex-1"
      >
        Old Toolport gateways may still be running from before the upgrade. Stop old
        gateways, then restart any apps still using them so they use the new client access
        controls.
        {result && <span className="block">{result}</span>}
      </p>
      {/* eslint-enable jsx-a11y/no-noninteractive-tabindex */}
      <button
        disabled={busy}
        onClick={async () => {
          setBusy(true);
          try {
            const outcome = await stopStaleGateways();
            setResult(
              [
                `Stopped ${outcome.killed.length} old gateways.`,
                outcome.failed.length
                  ? `Could not stop: ${outcome.failed.join("; ")}.`
                  : "",
                outcome.needsRestart.length
                  ? `Restart: ${outcome.needsRestart.map((app) => app.client).join(", ")}.`
                  : "",
              ]
                .filter(Boolean)
                .join(" "),
            );
          } catch (error) {
            toastError("Could not stop old gateways", { description: String(error) });
          } finally {
            setBusy(false);
          }
        }}
        className="rounded border px-2 py-1"
      >
        Stop old gateways
      </button>
      <button
        disabled={busy}
        onClick={async () => {
          setBusy(true);
          try {
            onRegistryChange(await dismissAccessUpgradeNotice());
          } catch (error) {
            toastError("Could not dismiss upgrade notice", {
              description: String(error),
            });
          } finally {
            setBusy(false);
          }
        }}
        className="rounded border px-2 py-1"
      >
        Dismiss
      </button>
    </div>
  );
}
