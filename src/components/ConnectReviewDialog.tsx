import { useEffect, useState } from "react";
import { migrateClient, previewClientSetup } from "@/lib/api";
import type { ClientSetupReview, MigrateResult, Registry } from "@/lib/types";
import { ImportReviewDialog } from "./ImportReviewDialog";
import { Dialog, DialogContent, DialogHeader, DialogTitle } from "./ui/dialog";
import { Button } from "./ui/button";

export function ConnectReviewDialog({
  clientId,
  clientName,
  profile,
  force,
  onClose,
  onConnected,
}: {
  clientId: string;
  clientName: string;
  profile?: string;
  force?: boolean;
  onClose: () => void;
  onConnected: (registry: Registry) => void;
}) {
  const [review, setReview] = useState<ClientSetupReview | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [result, setResult] = useState<MigrateResult | null>(null);
  useEffect(() => {
    let active = true;
    previewClientSetup(clientId)
      .then((r) => {
        if (active) setReview(r);
      })
      .catch((e) => {
        if (active) setError(String(e));
      });
    return () => {
      active = false;
    };
  }, [clientId]);
  async function connect(selected: string[]) {
    if (!review) return;
    setBusy(true);
    setError("");
    try {
      const next = await migrateClient(
        clientId,
        profile,
        force,
        selected,
        review.revision,
      );
      setResult(next);
      onConnected(next.registry);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }
  if (result || !review || error)
    return (
      <Dialog
        open
        onOpenChange={(open) => {
          if (!open && !busy) onClose();
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>
              {result ? `Connected to ${clientName}` : `Connect ${clientName}`}
            </DialogTitle>
          </DialogHeader>
          {error ? (
            <>
              <p role="alert" className="text-sm text-warning">
                {error}
              </p>
              <Button onClick={() => setError("")}>Back to review</Button>
            </>
          ) : result ? (
            <div className="flex flex-col gap-3 text-sm">
              <p>Restart {clientName} to load Toolport.</p>
              <p className="break-all">Config: {result.outcome.path}</p>
              {result.outcome.backup && (
                <p className="break-all">Backup: {result.outcome.backup}</p>
              )}
              <p>Gateway tools your agent will see:</p>
              <ul className="max-h-60 overflow-auto font-mono text-xs">
                {result.tools.map((t) => (
                  <li key={t.name}>{t.name}</li>
                ))}
              </ul>
              <Button onClick={onClose}>Done</Button>
            </div>
          ) : (
            <p>Reading client config...</p>
          )}
        </DialogContent>
      </Dialog>
    );
  return (
    <ImportReviewDialog
      open
      items={review.items}
      busy={busy}
      allowEmpty
      title={`Review and connect ${clientName}`}
      confirmLabel={busy ? "Checking gateway..." : "Connect to Toolport"}
      description={`Config: ${review.configPath}. Backup saved to ${review.backupDir}. Selected direct entries move into Toolport after verification. Unchecked entries and plugin servers stay in place.${force ? " This replaces the customized Toolport entry." : ""}`}
      onOpenChange={(open) => {
        if (!open && !busy) onClose();
      }}
      onConfirm={connect}
    />
  );
}
