import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useI18n } from "@/lib/i18n";
import { isPersonalSync } from "@/lib/personalSync";
import { toastError } from "@/lib/toast";
import type { Registry, ServerEntry } from "@/lib/types";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";

interface Props {
  server: ServerEntry;
  registry: Registry | null;
  onSaved: (registry: Registry) => void;
  onClose: () => void;
}

/** A value that can be marked the same on every machine: not secret, not a reference. */
interface Portable {
  kind: "env" | "input";
  key: string;
  label: string;
  portable: boolean;
}

/**
 * A server's sync settings, as the GTK shell's Sync settings dialog: keep it on
 * this machine, and which nonsecret values are the same on every machine.
 * Opened from the server row's menu.
 */
export function SyncSettingsDialog({ server, registry, onSaved, onClose }: Props) {
  const { t } = useI18n();
  const personal = isPersonalSync(registry);
  const values: Portable[] = [
    ...server.env
      .filter((entry) => !entry.secret && !entry.source)
      .map((entry) => ({
        kind: "env" as const,
        key: entry.key,
        label: entry.key,
        portable: entry.portable === true,
      })),
    ...(server.launch?.inputs ?? [])
      .filter((input) => !input.secret && !input.source)
      .map((input) => ({
        kind: "input" as const,
        key: input.key,
        label: input.label,
        portable: input.portable === true,
      })),
  ];
  const [localOnly, setLocalOnly] = useState(server.syncLocalOnly === true);
  // Keyed by kind and key, so a registry update that reorders the values keeps
  // each mark on its own value.
  const id = (value: Portable) => `${value.kind}:${value.key}`;
  const [marks, setMarks] = useState<Record<string, boolean>>(() =>
    Object.fromEntries(values.map((value) => [id(value), value.portable])),
  );
  const marked = (value: Portable) => marks[id(value)] ?? value.portable;
  const [saving, setSaving] = useState(false);

  async function save() {
    setSaving(true);
    try {
      let next: Registry | null = null;
      // Lock first, unlock last (as GTK): if a mark fails to save, the server
      // stays on this machine instead of syncing a value it excludes.
      const setLocalOnly = (on: boolean) =>
        invoke<Registry>("personal_sync_local_only", {
          serverId: server.id,
          localOnly: on,
        });
      if (personal && localOnly) next = await setLocalOnly(true);
      // Every mark in one write: a refused mark saves none of them, so a
      // failed save never leaves a value the user unchecked shared.
      if (values.length)
        next = await invoke<Registry>("personal_sync_portable_many", {
          serverId: server.id,
          marks: values.map((value) => ({
            kind: value.kind,
            key: value.key,
            enabled: marked(value),
          })),
        });
      if (personal && !localOnly) next = await setLocalOnly(false);
      if (next) onSaved(next);
      onClose();
    } catch (error) {
      toastError(t("Could not save sync settings") + `: ${String(error)}`);
    } finally {
      setSaving(false);
    }
  }

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{t("Sync settings")}</DialogTitle>
          <DialogDescription>
            {t(
              "Only values marked nonsecret can be the same on every machine. Secret values and approvals always stay on this machine.",
            )}
          </DialogDescription>
        </DialogHeader>
        <div className="flex flex-col gap-3 text-sm">
          <label className="flex items-start gap-2">
            <input
              type="checkbox"
              className="mt-0.5"
              checked={localOnly}
              disabled={!personal}
              onChange={(event) => setLocalOnly(event.target.checked)}
            />
            <span>
              {t("Keep on this machine only")}
              <span className="block text-meta text-muted-foreground">
                {t(
                  "Stops future sync for this server on this machine. Copies already on your other machines stay there.",
                )}
              </span>
            </span>
          </label>
          {values.map((value) => (
            <label key={id(value)} className="flex items-center gap-2">
              <input
                type="checkbox"
                checked={marked(value)}
                onChange={(event) => {
                  const checked = event.target.checked;
                  setMarks((current) => ({ ...current, [id(value)]: checked }));
                }}
              />
              {t("{value0}: same on every machine", { value0: value.label })}
            </label>
          ))}
        </div>
        <DialogFooter>
          <Button variant="ghost" onClick={onClose}>
            {t("Cancel")}
          </Button>
          <Button onClick={() => void save()} disabled={saving}>
            {t("Save")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
