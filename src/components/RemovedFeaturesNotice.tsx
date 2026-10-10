import { useState } from "react";
import { dismissRemovedFeaturesNotice, openExportsDir } from "@/lib/api";
import { openExternal } from "@/lib/openUrl";
import {
  GO_BACK_TO_1X_URL,
  removedFeaturesIssueUrl,
  removedFeaturesMessage,
} from "@/lib/removedFeatures";
import type { Registry } from "@/lib/types";
import { toastError } from "@/lib/toast";

/** Shown once to upgraders who used a 1.x feature that 2.0 removed. */
export function RemovedFeaturesNotice({
  registry,
  onRegistryChange,
}: {
  registry: Registry | null;
  onRegistryChange: (registry: Registry) => void;
}) {
  const [busy, setBusy] = useState(false);
  const notice = registry?.removedFeaturesNotice;
  const features = notice && !notice.dismissed ? (notice.features ?? []) : [];
  if (features.length === 0) return null;
  return (
    <div
      role="status"
      className="flex shrink-0 flex-wrap items-center gap-2 border-b bg-info/10 px-3 py-2 text-sm sm:gap-3 sm:px-6 sm:py-3"
    >
      <p className="min-w-0 flex-1">{removedFeaturesMessage(features)}</p>
      <button
        onClick={() =>
          openExportsDir().catch((error) =>
            toastError("Could not open the exports folder", {
              description: String(error),
            }),
          )
        }
        className="rounded border px-2 py-1"
      >
        Open exports folder
      </button>
      <button
        onClick={() => void openExternal(removedFeaturesIssueUrl(features))}
        className="rounded border px-2 py-1"
      >
        I need this
      </button>
      <button
        onClick={() => void openExternal(GO_BACK_TO_1X_URL)}
        className="rounded border px-2 py-1"
      >
        Go back to 1.24
      </button>
      <button
        disabled={busy}
        onClick={async () => {
          setBusy(true);
          try {
            onRegistryChange(await dismissRemovedFeaturesNotice());
          } catch (error) {
            toastError("Could not dismiss the notice", { description: String(error) });
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
