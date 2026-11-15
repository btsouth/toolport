import { useCallback, useEffect, useRef, useState } from "react";
import { Check, TriangleAlert } from "lucide-react";
import {
  dismissClientRecovery,
  discardClientRecovery,
  listClientRecoveries,
  retryClientRecovery,
  type ClientRecovery,
  type RecoveryAttempt,
} from "@/lib/api";
import { awaitLanguagePersistence, useI18n } from "@/lib/i18n";
import { Button } from "@/components/ui/button";
import { ConfirmDialog } from "@/components/ConfirmDialog";

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
  const focusTarget = useRef<string | null>(null);
  const details = useRef(new Map<string, HTMLDetailsElement>());
  const confirmationSubmitted = useRef(false);
  const [rows, setRows] = useState<ClientRecovery[]>([]);
  const [error, setError] = useState("");
  const [messages, setMessages] = useState<Record<string, string>>({});
  const [successes, setSuccesses] = useState<RecoveryAttempt[]>([]);
  const [busy, setBusy] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);
  const [confirmation, setConfirmation] = useState<{
    row: ClientRecovery;
    kind: "dismiss" | "discard";
  } | null>(null);
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
    if (!busy && !confirmation && focusTarget.current) {
      root.current
        ?.querySelector<HTMLElement>(`[data-recovery-focus="${focusTarget.current}"]`)
        ?.focus();
      focusTarget.current = null;
    }
  }, [busy, rows, successes, confirmation]);
  function openConfirmation(row: ClientRecovery, kind: "dismiss" | "discard") {
    confirmationSubmitted.current = false;
    setConfirmation({ row, kind });
  }
  async function retry(row: ClientRecovery) {
    const expanded = details.current.get(row.id);
    if (expanded) expanded.open = false;
    if (!row.canDiscard) {
      setBusy(row.id);
      try {
        const next = await listClientRecoveries();
        setRows(next);
        if (next.some((item) => item.id === row.id)) {
          focusTarget.current = `retry-${row.id}`;
        } else {
          setSuccesses((previous) => [
            ...previous.filter((item) => item.clientId !== row.clientId),
            {
              id: row.id,
              clientId: row.clientId,
              clientName: row.clientName,
              restored: 0,
              accounted: 0,
              remaining: 0,
              pending: null,
              message: t("{client}: the recovery information can be read again.", {
                client: row.clientName,
              }),
            },
          ]);
          focusTarget.current = `success-${row.clientId}`;
        }
      } catch (cause) {
        setMessages((previous) => ({ ...previous, [row.id]: safeError(cause) }));
        focusTarget.current = `retry-${row.id}`;
      } finally {
        setBusy(null);
      }
      return;
    }
    setBusy(row.id);
    setMessages((previous) => ({ ...previous, [row.id]: "" }));
    try {
      await awaitLanguagePersistence();
      const result = await retryClientRecovery(row.id);
      setRows((previous) =>
        result.pending
          ? previous.map((item) => (item.id === row.id ? result.pending! : item))
          : previous.filter((item) => item.id !== row.id),
      );
      setMessages((previous) => ({ ...previous, [row.id]: result.message }));
      focusTarget.current = result.pending
        ? `retry-${row.id}`
        : `success-${row.clientId}`;
      if (!result.pending) {
        setSuccesses((previous) => [
          ...previous.filter((item) => item.clientId !== row.clientId),
          result,
        ]);
        onRecovered?.();
      }
    } catch (cause) {
      setMessages((previous) => ({ ...previous, [row.id]: safeError(cause) }));
      setAttempt((value) => value + 1);
      focusTarget.current = `retry-${row.id}`;
    } finally {
      setBusy(null);
    }
  }
  async function action(row: ClientRecovery, kind: "dismiss" | "show" | "discard") {
    setBusy(row.id);
    setMessages((previous) => ({ ...previous, [row.id]: "" }));
    try {
      if (kind === "discard") {
        await discardClientRecovery(row.id);
        setRows((previous) => previous.filter((item) => item.id !== row.id));
        const result: RecoveryAttempt = {
          id: row.id,
          clientId: row.clientId,
          clientName: row.clientName,
          restored: 0,
          accounted: 0,
          remaining: 0,
          pending: null,
          message: t(
            "{client}'s saved recovery copies were discarded. The current client config was not changed.",
            { client: row.clientName },
          ),
        };
        setSuccesses((previous) => [
          ...previous.filter((item) => item.clientId !== row.clientId),
          result,
        ]);
        focusTarget.current = `success-${row.clientId}`;
        onRecovered?.();
      } else {
        const dismissed = kind === "dismiss";
        await dismissClientRecovery(row.id, dismissed);
        setRows((previous) =>
          previous.map((item) => (item.id === row.id ? { ...item, dismissed } : item)),
        );
        focusTarget.current = `${dismissed ? "show" : "retry"}-${row.id}`;
      }
    } catch (cause) {
      setMessages((previous) => ({ ...previous, [row.id]: safeError(cause) }));
      focusTarget.current = `${kind === "show" ? "show" : "retry"}-${row.id}`;
    } finally {
      setBusy(null);
    }
  }
  const visible = rows.filter((row) => !clientId || row.clientId === clientId);
  const results = successes.filter((row) => !clientId || row.clientId === clientId);
  if (!visible.length && !error && !results.length) return null;
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
      {results.map((result) => (
        <p
          key={result.clientId}
          role="status"
          tabIndex={-1}
          data-recovery-focus={`success-${result.clientId}`}
          className="toolport-alert flex select-text items-start gap-2 break-words border-success p-3 text-sm"
        >
          <Check className="mt-0.5 size-4 shrink-0" aria-hidden="true" />
          {result.message}
        </p>
      ))}
      {visible.map((row) => (
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
              {messages[row.id] && (
                <p role="status" className="mt-3 whitespace-pre-wrap break-words">
                  {messages[row.id]}
                </p>
              )}
              <Button
                data-recovery-focus={`show-${row.id}`}
                className="mt-3"
                variant="outline"
                disabled={!!busy}
                onClick={() => void action(row, "show")}
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
              <div className="mt-3 flex flex-wrap gap-2">
                <Button
                  data-recovery-focus={`retry-${row.id}`}
                  disabled={!!busy}
                  onClick={() => void retry(row)}
                >
                  {t("Retry")}
                </Button>
                {row.canDiscard && (
                  <>
                    <Button
                      data-recovery-focus={`dismiss-${row.id}`}
                      variant="outline"
                      disabled={!!busy}
                      onClick={() => openConfirmation(row, "dismiss")}
                    >
                      {t("Dismiss")}
                    </Button>
                    <Button
                      data-recovery-focus={`discard-${row.id}`}
                      variant="outline"
                      disabled={!!busy}
                      onClick={() => openConfirmation(row, "discard")}
                    >
                      {t("Discard these saved values")}
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
              if (!confirmationSubmitted.current)
                focusTarget.current = `${confirmation.kind}-${confirmation.row.id}`;
              setConfirmation(null);
            }
          }}
          title={t(
            confirmation.kind === "dismiss"
              ? "Dismiss recovery notice?"
              : "Discard these saved values?",
          )}
          destructive={confirmation.kind === "discard"}
          confirmLabel={t(
            confirmation.kind === "dismiss" ? "Dismiss" : "Discard these saved values",
          )}
          description={
            <div className="select-text whitespace-pre-wrap break-words [overflow-wrap:anywhere]">
              {confirmation.kind === "discard" && (
                <>
                  <p>
                    {t(
                      "Toolport will permanently delete the saved original config and its recovery copies for {client} at {path}. The saved copies of the values listed below will be lost. The current client config and Toolport's server credentials will not change.",
                      {
                        client: confirmation.row.clientName,
                        path: confirmation.row.path,
                      },
                    )}
                  </p>
                  {values(confirmation.row.discardValues, t("Saved values to discard"))}
                </>
              )}
              <p className="mt-3">{confirmation.row.retention}</p>
            </div>
          }
          onConfirm={() => {
            const chosen = confirmation;
            confirmationSubmitted.current = true;
            setConfirmation(null);
            void action(chosen.row, chosen.kind);
          }}
        />
      )}
    </div>
  );
}
