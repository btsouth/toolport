import { useEffect, useRef, useState } from "react";
import { Check, TriangleAlert } from "lucide-react";
import {
  dismissClientRecovery,
  listClientRecoveries,
  retryClientRecovery,
  type ClientRecovery,
} from "@/lib/api";
import { awaitLanguagePersistence, useI18n } from "@/lib/i18n";
import { Button } from "@/components/ui/button";
import { ConfirmDialog } from "@/components/ConfirmDialog";

/** Native receipts are authoritative; a remount/restart rereads them without opening the vault. */
export function ClientRecoveryBanners({
  clientId,
  refreshKey,
  onRecovered,
}: {
  clientId?: string;
  refreshKey: unknown;
  onRecovered?: () => void;
}) {
  const { t, locale } = useI18n();
  const root = useRef<HTMLDivElement>(null);
  const focusTarget = useRef<string | null>(null);
  const details = useRef(new Map<string, HTMLDetailsElement>());
  const [rows, setRows] = useState<ClientRecovery[]>([]);
  const [error, setError] = useState("");
  const [successClient, setSuccessClient] = useState<{ id: string; name: string } | null>(
    null,
  );
  const [retryPending, setRetryPending] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    let active = true;
    void awaitLanguagePersistence()
      .then(listClientRecoveries)
      .then((next) => {
        if (active) {
          setRows(next);
          setError("");
        }
      })
      .catch((cause: unknown) => {
        if (active) setError(String(cause));
      });
    return () => {
      active = false;
    };
  }, [locale, refreshKey, attempt]);

  useEffect(() => {
    if (!busy && focusTarget.current) {
      root.current
        ?.querySelector<HTMLElement>(`[data-recovery-focus="${focusTarget.current}"]`)
        ?.focus();
      focusTarget.current = null;
    }
  }, [busy, rows, successClient]);

  async function retry(row: ClientRecovery) {
    const expanded = details.current.get(row.id);
    if (expanded) expanded.open = false;
    setBusy(row.id);
    setError("");
    try {
      await awaitLanguagePersistence();
      const pending = await retryClientRecovery(row.id);
      setRows((previous) =>
        pending
          ? previous.map((item) => (item.id === row.id ? pending : item))
          : previous.filter((item) => item.id !== row.id),
      );
      setRetryPending(pending ? row.id : null);
      focusTarget.current = pending ? `retry-${row.id}` : "success";
      if (!pending) {
        setSuccessClient({ id: row.clientId, name: row.clientName });
        onRecovered?.();
      }
    } catch (cause) {
      setError(String(cause));
    } finally {
      setBusy(null);
    }
  }

  async function dismiss(row: ClientRecovery, dismissed: boolean) {
    setBusy(row.id);
    try {
      await dismissClientRecovery(row.id, dismissed);
      setRows((previous) =>
        previous.map((item) => (item.id === row.id ? { ...item, dismissed } : item)),
      );
      focusTarget.current = `${dismissed ? "show" : "retry"}-${row.id}`;
    } catch (cause) {
      setError(String(cause));
      throw cause;
    } finally {
      setBusy(null);
    }
  }

  const visible = rows.filter((row) => !clientId || row.clientId === clientId);
  const showSuccess = successClient && (!clientId || successClient.id === clientId);
  if (!visible.length && !error && !showSuccess) return null;
  return (
    <div ref={root} className="flex min-w-0 flex-col gap-3" data-client-recoveries>
      {error && (
        <div
          role="alert"
          className="toolport-alert select-text break-words border-warning p-3 text-sm"
        >
          <p>{error}</p>
          <Button variant="outline" onClick={() => setAttempt((value) => value + 1)}>
            {t("Retry")}
          </Button>
        </div>
      )}
      {showSuccess && (
        <p
          role="status"
          tabIndex={-1}
          data-recovery-focus="success"
          className="toolport-alert flex select-text items-start gap-2 break-words border-success p-3 text-sm"
        >
          <Check className="mt-0.5 size-4 shrink-0" aria-hidden="true" />
          {t("{client} config is fully restored. The missing values are back.", {
            client: successClient.name,
          })}
        </p>
      )}
      {visible.map((row) => {
        const separator = row.message.indexOf("\n\n");
        const heading =
          separator < 0 ? t("Client config recovery") : row.message.slice(0, separator);
        const body = separator < 0 ? row.message : row.message.slice(separator + 2);
        return (
          <section
            key={row.id}
            aria-label={`${t("Client config recovery")}: ${row.clientName}`}
            className="toolport-alert min-w-0 select-text border-warning bg-warning/5 p-4 text-sm"
          >
            {row.dismissed ? (
              <>
                <p className="font-semibold">{t("Recovery notice dismissed")}</p>
                <p className="mt-2 whitespace-pre-wrap break-words [overflow-wrap:anywhere]">
                  {row.retention}
                </p>
                <Button
                  data-recovery-focus={`show-${row.id}`}
                  className="mt-3"
                  variant="outline"
                  disabled={busy === row.id}
                  onClick={() => void dismiss(row, false).catch(() => undefined)}
                >
                  {t("Show recovery")}
                </Button>
              </>
            ) : (
              <>
                <div className="flex items-start gap-2">
                  <TriangleAlert
                    className="mt-0.5 size-4 shrink-0 text-warning"
                    aria-hidden="true"
                  />
                  <p className="min-w-0 whitespace-pre-wrap break-words font-semibold [overflow-wrap:anywhere]">
                    {heading}
                  </p>
                </div>
                <p className="mt-3 whitespace-pre-wrap break-words [overflow-wrap:anywhere]">
                  {body}
                </p>
                <p className="mt-3 whitespace-pre-wrap break-words [overflow-wrap:anywhere]">
                  {row.instructions}
                </p>
                <p className="mt-3 whitespace-pre-wrap break-words text-muted-foreground [overflow-wrap:anywhere]">
                  {row.retention}
                </p>
                {retryPending === row.id && (
                  <p role="status" className="mt-2 whitespace-pre-wrap break-words">
                    {t(
                      "Toolport still could not read the missing values. Unlock your keyring, then choose Retry.",
                    )}
                  </p>
                )}
                <div className="mt-3 flex flex-wrap gap-2">
                  <Button
                    data-recovery-focus={`retry-${row.id}`}
                    disabled={busy === row.id}
                    onClick={() => void retry(row)}
                  >
                    {t("Retry")}
                  </Button>
                  <ConfirmDialog
                    trigger={
                      <Button variant="outline" disabled={busy === row.id}>
                        {t("Dismiss")}
                      </Button>
                    }
                    title={t("Dismiss recovery notice?")}
                    description={
                      <div className="select-text whitespace-pre-wrap break-words [overflow-wrap:anywhere]">
                        {row.retention}
                      </div>
                    }
                    confirmLabel={t("Dismiss")}
                    onConfirm={() => dismiss(row, true)}
                  />
                </div>
                <details
                  className="mt-3 min-w-0"
                  ref={(element) => {
                    if (element) details.current.set(row.id, element);
                    else details.current.delete(row.id);
                  }}
                >
                  <summary
                    tabIndex={0}
                    className="cursor-pointer rounded focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                  >
                    {t("Details")}
                  </summary>
                  <p className="mt-2 whitespace-pre-wrap break-words font-mono text-meta [overflow-wrap:anywhere]">
                    {row.details}
                  </p>
                </details>
              </>
            )}
          </section>
        );
      })}
    </div>
  );
}
