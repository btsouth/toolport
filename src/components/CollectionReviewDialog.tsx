import { useState } from "react";
import { catalogInstalledIdentities, installed } from "@/lib/catalogIdentity";
import { addCollection } from "@/lib/collections";
import { toastError } from "@/lib/toast";
import type { Registry, Stack } from "@/lib/types";
import { ImportReviewDialog } from "./ImportReviewDialog";

export function CollectionReviewDialog({
  collection,
  registry,
  onAdded,
  onClose,
}: {
  collection: Stack;
  registry: Registry | null;
  onAdded: (registry: Registry) => void;
  onClose: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const have = new Set((registry?.servers ?? []).flatMap(catalogInstalledIdentities));
  const entries = collection.servers.filter((e) => !installed(have, e));
  async function confirm(keys: string[]) {
    setBusy(true);
    try {
      await addCollection(
        entries.filter((_, i) => keys.includes(String(i))),
        new Set(
          collection.servers
            .filter((e) => installed(have, e))
            .map((e) => e.name.toLowerCase()),
        ),
        onAdded,
      );
      onClose();
    } catch (e) {
      toastError(String(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <ImportReviewDialog
      open
      items={entries.map((e, i) => ({
        key: String(i),
        name: e.name,
        transport: e.transport,
        command: e.command,
        args: e.args,
        url: e.url ?? e.urlHint ?? null,
        envKeys: [...e.envKeys, ...(e.launch?.inputs.map((i) => i.label) ?? [])],
        isNew: true,
      }))}
      busy={busy}
      title={`Review ${collection.name}`}
      confirmLabel="Add selected servers"
      description="Review what each server runs. Valid servers turn on. Servers needing credentials or launch values stay off until setup is complete."
      onOpenChange={(open) => {
        if (!open && !busy) onClose();
      }}
      onConfirm={confirm}
    />
  );
}
