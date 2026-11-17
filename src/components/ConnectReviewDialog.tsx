import { translate, useI18n } from "@/lib/i18n";
import { useEffect, useRef, useState } from "react";
import { Check, Loader2 } from "lucide-react";
import { keyringLockedAfter, migrateClient, previewClientSetup } from "@/lib/api";
import type { ClientSetupReview, MigrateResult, Registry } from "@/lib/types";
import { GatewaySettingsNotice } from "./GatewaySettingsNotice";
import {
  connectDialogTitle,
  DisclosureSummary,
  ImportReviewDialog,
  pathText,
} from "./ImportReviewDialog";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "./ui/dialog";
import { fmtTs } from "@/lib/utils";
import { Button } from "./ui/button";

export function ConnectReviewDialog({
  clientId,
  clientName,
  profile,
  force,
  step,
  hasNext = false,
  onSkip,
  onClose,
  onConnected,
  onCloseAutoFocus,
  keyringQuiet = false,
  onKeyringLocked,
}: {
  clientId: string;
  clientName: string;
  profile?: string;
  force?: boolean;
  /** A progress line under the title while connecting several clients in a row. */
  step?: string;
  /** Inside a batch with more steps to come: the completion button says Next. */
  hasNext?: boolean;
  /** Inside a batch: after a failure, Skip moves on to the next step. Without it only
   * Retry and Cancel are offered. */
  onSkip?: () => void;
  /** `connected` is true once the connection was made, false when the review was
   * cancelled. `advance` is true only from the completion screen's Next or Done; Escape
   * or the close button there is false, so a batch stops as it does anywhere else. */
  onClose: (connected: boolean, advance: boolean) => void;
  onConnected: (registry: Registry) => void;
  /** Where focus goes once the dialog has closed, in place of the opener. */
  onCloseAutoFocus?: (event: Event) => void;
  /** Inside a batch whose keyring stayed locked: this step's first try can't ask to
   * unlock it. A Retry the user chooses may (the backend's `connect_step`). */
  keyringQuiet?: boolean;
  /** Inside a batch: a try failed with the keyring still locked, so later steps are
   * quiet. */
  onKeyringLocked?: () => void;
}) {
  const { t } = useI18n();
  // The dialog renders without a Radix trigger and swaps between its loading,
  // review and result views, so Radix cannot return focus on its own. Remember
  // the control that opened it and return there once the whole flow closes.
  const [opener] = useState(() =>
    typeof document === "undefined"
      ? null
      : (document.activeElement as HTMLElement | null),
  );
  const closing = useRef(false);
  function close(advance = false) {
    closing.current = true;
    onClose(result !== null, advance);
  }
  function restoreFocus(event: Event) {
    if (onCloseAutoFocus) return onCloseAutoFocus(event);
    event.preventDefault();
    if (closing.current && opener?.isConnected && opener !== document.body)
      opener.focus();
  }
  const [review, setReview] = useState<ClientSetupReview | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [attempt, setAttempt] = useState(0);
  const [result, setResult] = useState<MigrateResult | null>(null);
  // Next or Done on the completion screen, or Retry when the config can't be read:
  // focused when it appears, so the next key press carries on.
  const primary = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (result || (error && !review)) primary.current?.focus();
  }, [result, error, review]);
  // A step's first try follows the batch; Retry is the user's own and may ask.
  const quietFirst = useRef(keyringQuiet);
  // Answered before the error (and its Skip) shows, so the next step is already quiet
  // when Skip moves on, as GTK records it before showing the error.
  // Whether this step is still open: a check that answers after it closed (Cancel, then
  // a new run) must not make that new run quiet.
  const open = useRef(true);
  useEffect(() => {
    open.current = true;
    return () => {
      open.current = false;
    };
  }, []);
  const noteFailure = async (message: string) => {
    if (!onKeyringLocked) return;
    try {
      if ((await keyringLockedAfter(message)) && open.current) onKeyringLocked();
    } catch {
      // Unknown: the next step keeps the batch's current mode.
    }
  };
  useEffect(() => {
    let active = true;
    const quiet = quietFirst.current && attempt === 0;
    (quiet ? previewClientSetup(clientId, true) : previewClientSetup(clientId))
      .then((r) => {
        if (!active) return;
        // In a batch, a review that asked to unlock and was dismissed was the run's one
        // ask: this step's Connect is quiet too, and the rest of the run. Retry may ask.
        if (r.keyringAsked && onKeyringLocked) {
          quietFirst.current = true;
          onKeyringLocked();
        }
        setReview(r);
      })
      .catch(async (e) => {
        if (!active) return;
        await noteFailure(String(e));
        if (active) setError(String(e));
      });
    return () => {
      active = false;
    };
    // `noteFailure` only reads props that stay the same for this step.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [clientId, attempt]);
  async function connect(
    selected: string[],
    secretChoices?: Record<string, Record<string, boolean>>,
    credentialInputs?: Record<string, Record<string, string>>,
  ) {
    if (!review) return;
    setBusy(true);
    setError("");
    // Only the step's first connect follows a quiet batch; Retry may ask.
    const quiet = quietFirst.current;
    quietFirst.current = false;
    try {
      const args = [clientId, profile, force, selected, review.revision] as const;
      const next = quiet
        ? await migrateClient(...args, secretChoices, credentialInputs, true)
        : credentialInputs
          ? await migrateClient(...args, secretChoices, credentialInputs)
          : secretChoices
            ? await migrateClient(...args, secretChoices)
            : await migrateClient(...args);
      setResult(next);
      onConnected(next.registry);
    } catch (e) {
      await noteFailure(String(e));
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }
  if (result || !review)
    return (
      <Dialog
        open
        onOpenChange={(open) => {
          if (!open && !busy) close();
        }}
      >
        <DialogContent
          className="sm:max-w-lg"
          onOpenAutoFocus={(event) => {
            if (!primary.current) return;
            event.preventDefault();
            primary.current.focus();
          }}
          onCloseAutoFocus={restoreFocus}
        >
          <DialogHeader>
            <DialogTitle className={connectDialogTitle}>
              {result
                ? t("{value0} connected", { value0: clientName })
                : t("Connect {value0}", { value0: clientName })}
            </DialogTitle>
            {step && <p className="text-meta text-muted-foreground">{step}</p>}
          </DialogHeader>
          {result ? (
            <div className="flex flex-col gap-3 text-sm">
              <Check className="mx-auto size-10 text-success" />
              <p>{t("Restart {client} to load Toolport.", { client: clientName })}</p>
              {result.servers.length > 0 && (
                <ul className="divide-y rounded-lg border">
                  {result.servers.map((row) => {
                    return (
                      <li
                        key={row.name}
                        className="flex items-center justify-between gap-3 p-3"
                      >
                        <div>
                          <p className="font-medium">{row.name}</p>
                          <p className="text-meta text-muted-foreground">
                            {translate("{count} tools", { count: row.toolCount })} ·{" "}
                            {row.credentialState === "stored"
                              ? translate("Stored in keychain")
                              : row.credentialState === "missing"
                                ? translate("Needs input")
                                : translate("No credentials needed")}
                          </p>
                        </div>
                      </li>
                    );
                  })}
                </ul>
              )}
              {result.outcome.warnings?.map((warning) => (
                <p key={warning} className="text-meta text-muted-foreground">
                  {warning}
                </p>
              ))}
              <details>
                <DisclosureSummary>{t("What your agent sees")}</DisclosureSummary>
                {/* Said when nothing moved, rather than opening on nothing; GTK says
                    the same. */}
                {result.tools.length === 0 ? (
                  <p className="mt-2 text-meta text-muted-foreground">
                    {t("No servers moved, so your agent has no new tools.")}
                  </p>
                ) : (
                  <ul className="mt-2 font-mono text-meta">
                    {result.tools.map((t) => (
                      <li key={t.name}>{t.name}</li>
                    ))}
                  </ul>
                )}
              </details>
              <details>
                <DisclosureSummary>{t("Details")}</DisclosureSummary>
                <p className={`mt-2 break-all ${pathText}`}>
                  {t("Config: {path}", { path: result.outcome.path })}
                </p>
                {result.outcome.backup && (
                  <>
                    <p>
                      {result.backupDate
                        ? t("Backup saved {value0}", {
                            value0: fmtTs(result.backupDate * 1000, "dateTime"),
                          })
                        : t("Backup saved")}
                    </p>
                    <p className={`break-all ${pathText}`}>
                      {t("Backup: {path}", { path: result.outcome.backup })}
                    </p>
                  </>
                )}
              </details>
            </div>
          ) : error ? (
            <p role="alert" className="text-warning">
              {error}
            </p>
          ) : (
            <p>
              <Loader2 className="inline size-4 animate-spin" />{" "}
              {t("Reading client config...")}
            </p>
          )}
          {/* In the footer, outside the scrolling body, so a focus ring is never cut. */}
          {result ? (
            <DialogFooter>
              <Button ref={primary} onClick={() => close(true)}>
                {hasNext ? t("Next") : t("Done")}
              </Button>
            </DialogFooter>
          ) : (
            error && (
              <DialogFooter className="justify-between">
                <Button variant="ghost" onClick={() => close()}>
                  {t("Cancel")}
                </Button>
                {onSkip && (
                  <Button variant="outline" className="ml-auto" onClick={onSkip}>
                    {t("Skip")}
                  </Button>
                )}
                <Button
                  ref={primary}
                  onClick={() => {
                    setError("");
                    // The user's own Retry may ask, here and in the connect after it,
                    // as GTK's.
                    quietFirst.current = false;
                    setAttempt(attempt + 1);
                  }}
                >
                  {t("Retry")}
                </Button>
              </DialogFooter>
            )
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
      requireCredentials
      error={error}
      details={t("Config: {value0}\nBackups will be saved in {value1}", {
        value0: review.configPath,
        value1: review.backupDir,
      })}
      notice={
        <GatewaySettingsNotice clientName={clientName} lines={review.gatewaySettings} />
      }
      title={t("Review and connect {value0}", { value0: clientName })}
      step={step}
      onSkip={onSkip}
      confirmLabel={
        busy ? t("Checking gateway...") : error ? t("Retry") : t("Connect to Toolport")
      }
      description={
        force
          ? t(
              "Selected entries move into Toolport after verification. This replaces the customized Toolport entry.",
            )
          : t("Selected entries move into Toolport after verification.")
      }
      onCloseAutoFocus={restoreFocus}
      onOpenChange={(open) => {
        if (!open && !busy) close();
      }}
      onConfirm={connect}
    />
  );
}
