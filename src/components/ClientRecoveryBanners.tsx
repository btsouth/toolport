import { useCallback, useEffect, useRef, useState } from "react";
import { Check, Info, Loader2, TriangleAlert } from "lucide-react";
import {
  dismissClientRecovery,
  discardClientRecovery,
  listClientRecoveries,
  prepareClientRecoveryDiscard,
  refreshClientRecoveryNotice,
  refreshClientRecoveryStatus,
  retryClientRecovery,
  type ClientRecovery,
  type DiscardConfirmation,
} from "@/lib/api";
import { awaitLanguagePersistence, useI18n } from "@/lib/i18n";
import { Button } from "@/components/ui/button";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { CLIENT_RECOVERY_SCAN_EVENT } from "@/lib/clientRecoveryEvents";

/** Dispatch on `window` for an explicit Scan (Ctrl+R or the Clients refresh button).
 * Only this event checks saved values; ordinary listing never opens the credential store. */
export { CLIENT_RECOVERY_SCAN_EVENT } from "@/lib/clientRecoveryEvents";

/** One latest result in the whole shell. A new action replaces it. */
interface RecoveryResult {
  clientId: string;
  message: string;
  tone: "success" | "info";
}

type Confirmation =
  | { kind: "dismiss"; row: ClientRecovery }
  | { kind: "discard"; row: ClientRecovery; prepared: DiscardConfirmation };

/** Native receipts are authoritative; notice reads never open the credential store. */
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
  // Candidates in order; the first one that exists after the rebuild gets focus.
  const focusTargets = useRef<string[]>([]);
  const details = useRef(new Map<string, HTMLDetailsElement>());
  const confirmationSubmitted = useRef(false);
  const busyRef = useRef<string | null>(null);
  const [rows, setRows] = useState<ClientRecovery[]>([]);
  const [error, setError] = useState("");
  const [messages, setMessages] = useState<Record<string, string>>({});
  const [result, setResult] = useState<RecoveryResult | null>(null);
  const [busy, setBusyState] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);
  const [confirmation, setConfirmation] = useState<Confirmation | null>(null);
  const setBusy = (value: string | null) => {
    busyRef.current = value;
    setBusyState(value);
  };
  const safeError = useCallback(
    (cause: unknown) =>
      typeof cause === "string"
        ? cause
        : t(
            "Recovery did not finish reporting its result. Choose Retry to check the current status.",
          ),
    [t],
  );
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
        if (active) setError(safeError(cause));
      });
    return () => {
      active = false;
    };
  }, [locale, refreshKey, attempt, safeError]);
  useEffect(() => {
    // Scan leaves actions enabled so a focused button keeps focus; the backend
    // serializes it with recovery actions under the same locks.
    let scanning = false;
    function scan(event: Event) {
      event.preventDefault();
      const done = (event as CustomEvent<{ done?: () => void }>).detail?.done;
      if (scanning || busyRef.current) {
        done?.();
        return;
      }
      scanning = true;
      const focused = (document.activeElement as HTMLElement | null)?.dataset
        .recoveryFocus;
      void awaitLanguagePersistence()
        .then(refreshClientRecoveryStatus)
        .then(listClientRecoveries)
        .then((next) => {
          if (focused && !busyRef.current) focusTargets.current = [focused];
          setRows(next);
          setError("");
        })
        .catch((cause: unknown) => setError(safeError(cause)))
        .finally(() => {
          scanning = false;
          done?.();
        });
    }
    window.addEventListener(CLIENT_RECOVERY_SCAN_EVENT, scan);
    return () => window.removeEventListener(CLIENT_RECOVERY_SCAN_EVENT, scan);
  }, [safeError]);
  useEffect(() => {
    if (busy || confirmation || !focusTargets.current.length) return;
    for (const target of focusTargets.current) {
      const element = root.current?.querySelector<HTMLElement>(
        `[data-recovery-focus="${target}"]`,
      );
      if (element) {
        element.focus();
        element.scrollIntoView?.({ block: "nearest" });
        break;
      }
    }
    focusTargets.current = [];
  }, [busy, rows, result, messages, confirmation]);
  /** Every new action supersedes the shown result, and this row's old feedback only. */
  function begin(row: ClientRecovery) {
    setBusy(row.id);
    setResult(null);
    setMessages((previous) => {
      const next = { ...previous };
      delete next[row.id];
      return next;
    });
  }
  function fail(row: ClientRecovery, cause: unknown, focus: string[]) {
    setMessages((previous) => ({ ...previous, [row.id]: safeError(cause) }));
    focusTargets.current = focus;
  }
  async function relist(removed?: string) {
    try {
      setRows(await listClientRecoveries());
    } catch {
      if (removed) setRows((previous) => previous.filter((item) => item.id !== removed));
    }
  }
  async function retry(row: ClientRecovery) {
    const expanded = details.current.get(row.id);
    if (expanded) expanded.open = false;
    begin(row);
    if (!row.canDiscard) {
      // An unreadable or untrusted record. The backend reclassifies the full
      // inventory; a replacement error keeps focus and never reports success.
      try {
        const update = await refreshClientRecoveryNotice(
          row.id,
          row.clientId,
          row.backupPath,
        );
        setRows(update.rows);
        if (update.message) {
          setResult({ clientId: row.clientId, message: update.message, tone: "info" });
          focusTargets.current = ["result"];
        } else {
          focusTargets.current = [
            ...(update.focusId ? [`retry-${update.focusId}`] : []),
            `retry-${row.id}`,
          ];
        }
      } catch (cause) {
        fail(row, cause, [`retry-${row.id}`]);
      } finally {
        setBusy(null);
      }
      return;
    }
    try {
      await awaitLanguagePersistence();
      const outcome = await retryClientRecovery(row.id);
      setRows((previous) =>
        outcome.pending
          ? previous.map((item) => (item.id === row.id ? outcome.pending! : item))
          : previous.filter((item) => item.id !== row.id),
      );
      if (outcome.pending) {
        setMessages((previous) => ({ ...previous, [row.id]: outcome.message }));
        focusTargets.current = [`retry-${row.id}`];
        // Waiting client configs were rewritten: reload the client rows.
        if (outcome.refreshed) onRecovered?.();
      } else {
        setResult({ clientId: row.clientId, message: outcome.message, tone: "success" });
        focusTargets.current = ["result"];
        onRecovered?.();
      }
    } catch (cause) {
      fail(row, cause, [`retry-${row.id}`]);
      setAttempt((value) => value + 1);
    } finally {
      setBusy(null);
    }
  }
  async function prepareDiscard(row: ClientRecovery) {
    begin(row);
    // The row stays busy ("Working on this recovery…", actions disabled) while
    // its confirmation is open, as in the GTK shell. Closing it releases the row.
    let opened = false;
    try {
      await awaitLanguagePersistence();
      const prepared = await prepareClientRecoveryDiscard(row.id);
      confirmationSubmitted.current = false;
      setConfirmation({ kind: "discard", row, prepared });
      opened = true;
    } catch (cause) {
      fail(row, cause, [`discard-${row.id}`, `retry-${row.id}`]);
      await relist();
    } finally {
      if (!opened) setBusy(null);
    }
  }
  async function discard(row: ClientRecovery, prepared: DiscardConfirmation) {
    begin(row);
    try {
      const message = await discardClientRecovery(row.id, prepared.manifest);
      await relist(row.id);
      setResult({ clientId: row.clientId, message, tone: "success" });
      focusTargets.current = ["result"];
      onRecovered?.();
    } catch (cause) {
      // A failed Discard can leave a started phase; show the current row state.
      fail(row, cause, [`discard-${row.id}`, `retry-${row.id}`]);
      await relist();
    } finally {
      setBusy(null);
    }
  }
  async function setDismissed(row: ClientRecovery, dismissed: boolean) {
    begin(row);
    try {
      await dismissClientRecovery(row.id, dismissed);
      setRows((previous) =>
        previous.map((item) => (item.id === row.id ? { ...item, dismissed } : item)),
      );
      focusTargets.current = [`${dismissed ? "show" : "retry"}-${row.id}`];
    } catch (cause) {
      fail(row, cause, [`${dismissed ? "retry" : "show"}-${row.id}`]);
    } finally {
      setBusy(null);
    }
  }
  const visible = rows.filter((row) => !clientId || row.clientId === clientId);
  const shownResult =
    result && (!clientId || result.clientId === clientId) ? result : null;
  // An open confirmation must render even when a reload emptied the list, or
  // its row would stay busy with no dialog left to release it.
  if (!visible.length && !error && !shownResult && !confirmation) return null;
  // Keyboard focus is required to scroll this bounded list without a pointer.
  /* eslint-disable jsx-a11y/no-noninteractive-tabindex */
  const values = (lines: string[], label: string) => (
    <ul
      aria-label={label}
      tabIndex={0}
      className="mt-3 max-h-[240px] list-disc space-y-1 overflow-y-auto rounded pl-5 pr-2 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
    >
      {lines.map((line, index) => (
        <li
          key={`${index}-${line}`}
          className="whitespace-pre-wrap break-words [overflow-wrap:anywhere]"
        >
          {line}
        </li>
      ))}
    </ul>
  );
  /* eslint-enable jsx-a11y/no-noninteractive-tabindex */
  const working = (
    <p aria-live="polite" className="mt-3 flex items-center gap-2" data-recovery-busy>
      <Loader2 className="size-4 shrink-0 animate-spin" aria-hidden="true" />
      {t("Working on this recovery…")}
    </p>
  );
  return (
    <div ref={root} className="flex min-w-0 flex-col gap-3" data-client-recoveries>
      {error && (
        <div
          role="alert"
          className="toolport-alert select-text break-words border-warning p-3 text-sm"
        >
          <p>{error}</p>
          <Button variant="outline" onClick={() => setAttempt((n) => n + 1)}>
            {t("Retry")}
          </Button>
        </div>
      )}
      {shownResult && (
        <p
          role="status"
          tabIndex={-1}
          data-recovery-focus="result"
          className={`toolport-alert flex select-text items-start gap-2 whitespace-pre-wrap break-words p-3 text-sm [overflow-wrap:anywhere] ${shownResult.tone === "success" ? "border-success" : "border-border"}`}
        >
          {shownResult.tone === "success" ? (
            <Check className="mt-0.5 size-4 shrink-0" aria-hidden="true" />
          ) : (
            <Info className="mt-0.5 size-4 shrink-0" aria-hidden="true" />
          )}
          {shownResult.message}
        </p>
      )}
      {visible.map((row) => (
        <section
          key={row.id}
          aria-label={`${t("Client config recovery")}: ${row.clientName}`}
          aria-busy={busy === row.id || undefined}
          className="toolport-alert min-w-0 select-text border-warning bg-warning/5 p-4 text-sm"
        >
          {row.dismissed ? (
            <>
              <p className="font-semibold">{t("Recovery notice dismissed")}</p>
              <p className="mt-2 whitespace-pre-wrap break-words [overflow-wrap:anywhere]">
                {row.retention}
              </p>
              {messages[row.id] && (
                <p role="status" className="mt-3 whitespace-pre-wrap break-words">
                  {messages[row.id]}
                </p>
              )}
              {busy === row.id && working}
              <Button
                data-recovery-focus={`show-${row.id}`}
                className="mt-3"
                variant="outline"
                disabled={!!busy}
                onClick={() => void setDismissed(row, false)}
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
                  {row.message.includes("\n\n")
                    ? row.message.split("\n\n")[0]
                    : t("Client config recovery")}
                </p>
              </div>
              <p className="mt-3 whitespace-pre-wrap break-words [overflow-wrap:anywhere]">
                {row.message.includes("\n\n") ? row.description : row.message}
              </p>
              {!!row.valueLines.length &&
                values(row.valueLines, t("Missing saved values"))}
              <p className="mt-3 whitespace-pre-wrap break-words [overflow-wrap:anywhere]">
                {row.instructions}
              </p>
              <p className="mt-3 whitespace-pre-wrap break-words text-muted-foreground [overflow-wrap:anywhere]">
                {row.retention}
              </p>
              {!messages[row.id] && row.error && row.error !== row.description && (
                <p role="alert" className="mt-3 whitespace-pre-wrap break-words">
                  {row.error}
                </p>
              )}
              {messages[row.id] && (
                <p role="status" className="mt-3 whitespace-pre-wrap break-words">
                  {messages[row.id]}
                </p>
              )}
              {busy === row.id && working}
              <div className="mt-3 flex flex-wrap gap-2">
                {row.canRetry && (
                  <Button
                    data-recovery-focus={`retry-${row.id}`}
                    disabled={!!busy}
                    onClick={() => void retry(row)}
                  >
                    {t("Retry")}
                  </Button>
                )}
                {(row.canDiscard || row.canDiscardFile) && (
                  <>
                    {row.canDiscard && !row.discardStarted && (
                      <Button
                        data-recovery-focus={`dismiss-${row.id}`}
                        variant="outline"
                        disabled={!!busy}
                        onClick={() => {
                          confirmationSubmitted.current = false;
                          setConfirmation({ kind: "dismiss", row });
                        }}
                      >
                        {t("Dismiss")}
                      </Button>
                    )}
                    <Button
                      data-recovery-focus={`discard-${row.id}`}
                      variant={row.discardStarted ? "destructive" : "outline"}
                      disabled={!!busy}
                      onClick={() => void prepareDiscard(row)}
                    >
                      {row.discardAction || t("Discard these saved values")}
                    </Button>
                  </>
                )}
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
      ))}
      {confirmation && (
        <ConfirmDialog
          open
          onCloseAutoFocus={(event) => event.preventDefault()}
          onOpenChange={(open) => {
            if (!open) {
              if (!confirmationSubmitted.current) {
                focusTargets.current = [`${confirmation.kind}-${confirmation.row.id}`];
                if (confirmation.kind === "discard") setBusy(null);
              }
              setConfirmation(null);
            }
          }}
          title={
            confirmation.kind === "discard"
              ? confirmation.prepared.heading
              : t("Dismiss recovery notice?")
          }
          destructive={confirmation.kind === "discard"}
          confirmLabel={
            confirmation.kind === "discard"
              ? confirmation.prepared.actionLabel
              : t("Dismiss")
          }
          description={
            <div className="select-text whitespace-pre-wrap break-words [overflow-wrap:anywhere]">
              {confirmation.kind === "discard" ? (
                <>
                  <p>{confirmation.prepared.description}</p>
                  {!!confirmation.prepared.lines.length &&
                    values(
                      confirmation.prepared.lines,
                      t("Saved values and files to delete"),
                    )}
                  {confirmation.prepared.location && (
                    <p className="mt-3">{confirmation.prepared.location}</p>
                  )}
                </>
              ) : (
                <p>{confirmation.row.retention}</p>
              )}
            </div>
          }
          onConfirm={() => {
            const chosen = confirmation;
            confirmationSubmitted.current = true;
            setConfirmation(null);
            if (chosen.kind === "discard") void discard(chosen.row, chosen.prepared);
            else void setDismissed(chosen.row, true);
          }}
        />
      )}
    </div>
  );
}
