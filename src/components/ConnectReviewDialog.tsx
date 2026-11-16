import { translate, useI18n } from "@/lib/i18n";
import { useEffect, useRef, useState } from "react";
import { Check, Loader2 } from "lucide-react";
import { migrateClient, previewClientSetup } from "@/lib/api";
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
  function close() {
    closing.current = true;
    onClose();
  }
  function restoreFocus(event: Event) {
    event.preventDefault();
    if (closing.current && opener?.isConnected && opener !== document.body)
      opener.focus();
  }
  const [review, setReview] = useState<ClientSetupReview | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [attempt, setAttempt] = useState(0);
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
  }, [clientId, attempt]);
  async function connect(
    selected: string[],
    secretChoices?: Record<string, Record<string, boolean>>,
    credentialInputs?: Record<string, Record<string, string>>,
  ) {
    if (!review) return;
    setBusy(true);
    setError("");
    try {
      const args = [clientId, profile, force, selected, review.revision] as const;
      const next = credentialInputs
        ? await migrateClient(...args, secretChoices, credentialInputs)
        : secretChoices
          ? await migrateClient(...args, secretChoices)
          : await migrateClient(...args);
      setResult(next);
      onConnected(next.registry);
    } catch (e) {
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
        <DialogContent className="sm:max-w-lg" onCloseAutoFocus={restoreFocus}>
          <DialogHeader>
            <DialogTitle className={connectDialogTitle}>
              {result
                ? t("{value0} connected", { value0: clientName })
                : t("Connect {value0}", { value0: clientName })}
            </DialogTitle>
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
            <>
              <p role="alert" className="text-warning">
                {error}
              </p>
              <DialogFooter>
                <Button
                  onClick={() => {
                    setError("");
                    setAttempt(attempt + 1);
                  }}
                >
                  {t("Retry")}
                </Button>
              </DialogFooter>
            </>
          ) : (
            <p>
              <Loader2 className="inline size-4 animate-spin" />{" "}
              {t("Reading client config...")}
            </p>
          )}
          {result && (
            <DialogFooter>
              <Button onClick={close}>{t("Done")}</Button>
            </DialogFooter>
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
