import { useI18n, Trans } from "@/lib/i18n";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { Check, Loader2, ShieldAlert } from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import type { ImportItem } from "@/lib/types";

import { runsShell, isPrivateHostUrl } from "@/lib/importReview";

interface Props {
  open: boolean;
  items: ImportItem[];
  busy?: boolean;
  title?: string;
  /** A short progress line under the title, such as one step of a batch. */
  step?: string;
  description?: string;
  confirmLabel?: string;
  allowEmpty?: boolean;
  requireCredentials?: boolean;
  details?: string;
  /** Shown under the description, before the user confirms. */
  notice?: ReactNode;
  error?: string;
  /** Inside a batch: offered beside Retry after a failure, to move on to the next step. */
  onSkip?: () => void;
  /** Where focus goes when the dialog closes; Radix defaults to its trigger. */
  onCloseAutoFocus?: (event: Event) => void;
  onOpenChange: (open: boolean) => void;
  onConfirm: (
    keys: string[],
    secretChoices?: Record<string, Record<string, boolean>>,
    credentialInputs?: Record<string, Record<string, string>>,
  ) => void;
}

/** A disclosure toggle with the same hover and focus ring as the dialog's buttons. */
export const disclosureSummary =
  "w-fit cursor-pointer rounded-md -mx-1 px-1 transition-colors outline-none hover:bg-muted hover:text-foreground focus-visible:ring-3 focus-visible:ring-ring/50";

/** Review and choose detected client servers before adding them to Toolport. */
export function ImportReviewDialog({
  open,
  items,
  busy = false,
  title,
  step,
  description,
  confirmLabel,
  allowEmpty = false,
  requireCredentials = false,
  details,
  notice,
  error,
  onSkip,
  onCloseAutoFocus,
  onOpenChange,
  onConfirm,
}: Props) {
  const { t } = useI18n();
  if (!open) return null;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <ImportReviewContent
        items={items}
        busy={busy}
        title={title ?? t("Review servers to import")}
        step={step}
        description={description}
        confirmLabel={confirmLabel}
        allowEmpty={allowEmpty}
        requireCredentials={requireCredentials}
        details={details}
        notice={notice}
        error={error}
        onSkip={onSkip}
        onCloseAutoFocus={onCloseAutoFocus}
        onOpenChange={onOpenChange}
        onConfirm={onConfirm}
      />
    </Dialog>
  );
}

function ImportReviewContent({
  items,
  busy = false,
  title,
  step,
  description,
  confirmLabel,
  allowEmpty = false,
  requireCredentials = false,
  details,
  notice,
  error,
  onSkip,
  onCloseAutoFocus,
  onOpenChange,
  onConfirm,
}: Omit<Props, "open">) {
  const { t } = useI18n();
  const contentRef = useRef<HTMLDivElement>(null);
  const cancelRef = useRef<HTMLButtonElement>(null);
  // Starting a check disables the focused button; keep focus in the dialog
  // instead of letting it fall to the page behind.
  useEffect(() => {
    const content = contentRef.current;
    if (!busy || !content) return;
    const active = document.activeElement;
    if (!active || !content.contains(active) || active.matches(":disabled"))
      content.focus();
  }, [busy]);
  // After a failure, focus goes to the confirm button (Retry), with Skip and Cancel
  // beside it; the buttons were disabled while busy, which dropped focus.
  const confirmRef = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (!error || busy) return;
    (confirmRef.current?.disabled ? cancelRef.current : confirmRef.current)?.focus();
  }, [error, busy]);
  const keyedItems = items.map((item, index) => ({
    item,
    key: item.key ?? `${item.name}-${index}`,
  }));
  const [selected, setSelected] = useState<Set<string>>(
    () =>
      new Set(keyedItems.filter(({ item }) => !item.unsupported).map(({ key }) => key)),
  );

  const [secretChoices, setSecretChoices] = useState<
    Record<string, Record<string, boolean>>
  >(() =>
    Object.fromEntries(
      items.map((item) => [
        item.name,
        Object.fromEntries((item.credentials ?? []).map((env) => [env.key, env.secret])),
      ]),
    ),
  );
  const [credentialInputs, setCredentialInputs] = useState<
    Record<string, Record<string, string>>
  >({});
  const [editing, setEditing] = useState<{ name: string; key: string } | null>(null);
  const [value, setValue] = useState("");
  const selectedCount = selected.size;
  const missingRequired = requireCredentials
    ? keyedItems.flatMap(({ item, key }) =>
        selected.has(key)
          ? (item.credentials ?? [])
              .filter(
                (env) =>
                  env.required &&
                  !env.present &&
                  !credentialInputs[item.name]?.[env.key]?.trim(),
              )
              .map((env) =>
                t("Enter {value0} or deselect {value1}", {
                  value0: env.key,
                  value1: item.name,
                }),
              )
          : [],
      )
    : [];

  return (
    <DialogContent
      ref={contentRef}
      className="sm:max-w-lg"
      closeDisabled={busy}
      // Open on Cancel, as the GTK review does, so Enter never confirms or toggles
      // a disclosure by accident.
      onOpenAutoFocus={(event) => {
        event.preventDefault();
        cancelRef.current?.focus();
      }}
      onCloseAutoFocus={onCloseAutoFocus}
    >
      <DialogHeader>
        <DialogTitle>{title ?? t("Review servers to import")}</DialogTitle>
        {step && <p className="text-meta text-muted-foreground">{step}</p>}
      </DialogHeader>
      <div className="flex flex-col gap-4 py-1">
        <p className="text-meta text-muted-foreground">
          {description ??
            t(
              "Review the commands and URLs before adding them. Leave any server unchecked to keep it as it is.",
            )}
        </p>
        {notice}
        {details && (
          <details>
            <summary className={`text-meta ${disclosureSummary}`}>{t("Details")}</summary>
            <p className="mt-2 whitespace-pre-wrap break-all text-meta text-muted-foreground">
              {details}
            </p>
          </details>
        )}
        {error && (
          <p role="alert" className="text-sm text-warning">
            {error}
          </p>
        )}
        <div className="flex max-h-72 flex-col gap-2 overflow-y-auto">
          {keyedItems.map(({ item, key }) => {
            const isSelected = selected.has(key);
            return (
              <div key={key}>
                <button
                  type="button"
                  aria-pressed={isSelected}
                  disabled={busy || !!item.unsupported}
                  data-failed={!!error?.includes(item.name)}
                  className={`w-full rounded-md text-left transition-colors outline-none hover:bg-muted/60 focus-visible:ring-3 focus-visible:ring-ring/50 disabled:hover:bg-transparent ${
                    error?.includes(item.name)
                      ? "ring-1 ring-warning"
                      : isSelected
                        ? "ring-1 ring-success/60"
                        : ""
                  }`}
                  onClick={() =>
                    setSelected((previous) => {
                      const next = new Set(previous);
                      if (isSelected) next.delete(key);
                      else next.add(key);
                      return next;
                    })
                  }
                >
                  <ImportRow
                    item={item}
                    selected={isSelected}
                    status={
                      <>
                        {!!item.credentials?.length && (
                          <p className="mt-1 text-meta text-muted-foreground">
                            {item.credentials.some(
                              (env) =>
                                env.required &&
                                !env.present &&
                                !credentialInputs[item.name]?.[env.key],
                            )
                              ? t("Needs input")
                              : item.credentials.some(
                                    (env) =>
                                      !env.present &&
                                      !credentialInputs[item.name]?.[env.key],
                                  )
                                ? t("Optional")
                                : item.credentials.some(
                                      (env) =>
                                        secretChoices[item.name]?.[env.key] ?? env.secret,
                                    )
                                  ? t("Found, goes to keychain")
                                  : t("Found")}
                          </p>
                        )}
                        {busy && isSelected && (
                          <p role="status" className="mt-1 flex gap-2 text-meta">
                            <Loader2 className="size-3 animate-spin" />
                            {t("Checking {name}...", { name: item.name })}
                          </p>
                        )}
                      </>
                    }
                  />
                </button>
                {!!item.credentials?.length && (
                  <details className="px-3 pb-3 text-meta">
                    <summary className={disclosureSummary}>
                      {item.name} {t("credentials and settings")}
                    </summary>
                    <div className="flex flex-col gap-2 pt-2">
                      {item.credentials.map((env) => (
                        <div key={env.key} className="flex items-center gap-2">
                          <label className="flex items-center gap-2">
                            <input
                              type="checkbox"
                              disabled={
                                busy || !!item.unsupported || env.key.startsWith("__")
                              }
                              checked={secretChoices[item.name]?.[env.key] ?? env.secret}
                              onChange={(e) =>
                                setSecretChoices((previous) => ({
                                  ...previous,
                                  [item.name]: {
                                    ...previous[item.name],
                                    [env.key]: e.target.checked,
                                  },
                                }))
                              }
                            />
                            {t("Keep {key} in keychain", { key: env.key })}{" "}
                            {!env.required && (
                              <span className="text-muted-foreground">
                                {t("Optional")}
                              </span>
                            )}
                            <span className="ml-auto text-muted-foreground">
                              {env.present
                                ? t("Found")
                                : credentialInputs[item.name]?.[env.key]
                                  ? t("Ready for connection")
                                  : env.required
                                    ? t("Missing")
                                    : ""}
                            </span>
                          </label>
                          {!env.present && (
                            <Button
                              variant="outline"
                              size="sm"
                              disabled={busy}
                              onClick={() => {
                                setValue(credentialInputs[item.name]?.[env.key] ?? "");
                                setEditing({ name: item.name, key: env.key });
                              }}
                            >
                              {t("Enter value")}
                            </Button>
                          )}
                        </div>
                      ))}
                    </div>
                  </details>
                )}
                {item.unsupported && (
                  <p className="px-3 pb-2 text-meta text-warning">
                    {t("Unsupported:")}
                    {item.unsupported}
                  </p>
                )}
              </div>
            );
          })}
        </div>
      </div>
      {editing && (
        <Dialog
          open
          onOpenChange={(open) => {
            if (!open) setEditing(null);
          }}
        >
          <DialogContent>
            <DialogHeader>
              <DialogTitle>
                {editing.name} {t("credentials")}
              </DialogTitle>
            </DialogHeader>
            <p className="text-sm text-muted-foreground">
              {t(
                "This value is used only for the selected server. It is saved only if connection succeeds.",
              )}
            </p>
            <label className="flex flex-col gap-2 text-sm">
              {editing.key}
              <input
                type="password"
                autoComplete="off"
                className="rounded-md border bg-background px-3 py-2"
                value={value}
                onChange={(e) => setValue(e.target.value)}
              />
            </label>
            <DialogFooter>
              <Button variant="ghost" onClick={() => setEditing(null)}>
                {t("Cancel")}
              </Button>
              <Button
                disabled={!value.trim()}
                onClick={() => {
                  setCredentialInputs((previous) => ({
                    ...previous,
                    [editing.name]: { ...previous[editing.name], [editing.key]: value },
                  }));
                  setValue("");
                  setEditing(null);
                }}
              >
                {t("Use for connection")}
              </Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>
      )}
      {missingRequired.map((reason) => (
        <p key={reason} className="text-sm text-warning">
          {reason}
        </p>
      ))}
      <DialogFooter className="justify-between">
        <Button
          ref={cancelRef}
          variant="ghost"
          onClick={() => onOpenChange(false)}
          disabled={busy}
        >
          {t("Cancel")}
        </Button>
        {error && onSkip && (
          <Button variant="outline" className="ml-auto" onClick={onSkip} disabled={busy}>
            {t("Skip")}
          </Button>
        )}
        <Button
          ref={confirmRef}
          onClick={() => {
            if (items.some((item) => item.credentials?.length))
              onConfirm(
                Array.from(selected),
                secretChoices,
                ...(Object.keys(credentialInputs).length
                  ? ([credentialInputs] as const)
                  : ([] as const)),
              );
            else onConfirm(Array.from(selected));
          }}
          disabled={
            busy || missingRequired.length > 0 || (!allowEmpty && selectedCount === 0)
          }
        >
          <Check className="size-4" />
          {confirmLabel ??
            (selectedCount === 0
              ? t("Select a server")
              : t(
                  selectedCount === 1
                    ? "Import {count} server"
                    : "Import {count} servers",
                  { count: selectedCount },
                ))}
        </Button>
      </DialogFooter>
    </DialogContent>
  );
}

/** One reviewable server: name, what it runs, and the relevant safety flags. */
export function ImportRow({
  item,
  selected,
  status,
}: {
  item: ImportItem;
  selected?: boolean;
  status?: ReactNode;
}) {
  const { t } = useI18n();
  const runs =
    item.command != null ? [item.command, ...item.args].join(" ") : (item.url ?? "");
  const shell = runsShell(item.command, item.args);
  const privateHost = isPrivateHostUrl(item.url);
  return (
    <div className="rounded-md border px-3 py-2">
      <div className="flex items-center gap-2">
        {selected !== undefined && (
          <span
            aria-hidden="true"
            className={`size-3 rounded-sm border ${
              selected ? "border-success bg-success" : "border-muted-foreground"
            }`}
          />
        )}
        <span className="truncate text-sm font-medium">{item.name}</span>
        {
          <span className="ml-auto shrink-0 text-meta text-muted-foreground">
            {item.updates?.length
              ? t("Updates existing server")
              : item.isNew
                ? t("New")
                : t("In Toolport")}
          </span>
        }
      </div>
      {item.referenceReview?.map((line) => (
        <p key={line} className="mt-1 text-meta break-all">
          {line}
        </p>
      ))}
      {!!item.updates?.length && (
        <p className="mt-1 text-meta text-muted-foreground">
          {t("Changes: {changes}", { changes: item.updates.join(", ") })}
        </p>
      )}
      {runs && (
        <p
          title={runs}
          aria-label={runs}
          className="mt-1 flex min-w-0 font-mono text-meta text-muted-foreground"
        >
          {runs.length > 72 ? (
            <>
              <span className="min-w-0 truncate">{runs.slice(0, -28)}</span>
              <span className="shrink-0">{runs.slice(-28)}</span>
            </>
          ) : (
            runs
          )}
        </p>
      )}
      {!!item.envKeys?.length && !item.credentials?.length && (
        <p className="mt-1 text-meta text-warning">
          {t("Credentials: {keys}. Review their status before connecting.", {
            keys: item.envKeys.join(", "),
          })}
        </p>
      )}
      {shell && (
        <p className="mt-1.5 flex items-center gap-1.5 text-meta text-warning">
          <Trans
            message={"{value1}Runs a shell command. Only import setups you trust."}
            values={{ value1: <ShieldAlert className="size-3.5 shrink-0" /> }}
          />
        </p>
      )}
      {privateHost && (
        <p className="mt-1.5 flex items-center gap-1.5 text-meta text-warning">
          <Trans
            message={
              "{value1}Connects to a private or internal address. Only import setups you trust."
            }
            values={{ value1: <ShieldAlert className="size-3.5 shrink-0" /> }}
          />
        </p>
      )}
      {status}
    </div>
  );
}
