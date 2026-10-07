import { useState } from "react";
import { dismissDestructiveConfirmationNotice } from "@/lib/api";
import { toastError } from "@/lib/toast";
import type { Registry } from "@/lib/types";
import { Button } from "@/components/ui/button";

export function DestructiveConfirmationNotice({
  registry,
  onRegistryChange,
  onSettings,
}: {
  registry: Registry | null;
  onRegistryChange: (registry: Registry) => void;
  onSettings: () => void;
}) {
  const [busy, setBusy] = useState(false);
  if (!registry || registry.destructiveConfirmationNoticeSeen) return null;

  async function dismiss() {
    setBusy(true);
    try {
      onRegistryChange(await dismissDestructiveConfirmationNotice());
    } catch (error) {
      toastError(`Couldn't dismiss the notice: ${error}`);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div
      role="status"
      className="mx-6 mt-4 flex flex-wrap items-center gap-3 rounded-lg border p-3"
    >
      <p className="min-w-0 flex-1 text-sm">
        New Toolport installs ask the agent to confirm before a tool marked destructive
        runs. Your setting has not changed. You can turn this on in Settings.
      </p>
      <Button variant="outline" size="sm" onClick={onSettings}>
        Settings
      </Button>
      <Button variant="ghost" size="sm" disabled={busy} onClick={() => void dismiss()}>
        Dismiss
      </Button>
    </div>
  );
}
