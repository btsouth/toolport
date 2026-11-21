import { useI18n, Trans } from "@/lib/i18n";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { Check, ChevronRight, Loader2, ShieldAlert } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import type { ImportItem } from "@/lib/types";
import { cn } from "@/lib/utils";

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

/** A disclosure toggle with the same hover and focus ring as the dialog's buttons. The
 * browser's marker is hidden (flex drops it in Chromium, the pseudo-element in WebKit). */
export const disclosureSummary =
  "flex w-fit cursor-pointer list-none items-center gap-1 rounded-md -mx-1 px-1 transition-colors outline-none hover:bg-muted hover:text-foreground focus-visible:ring-3 focus-visible:ring-ring/50 [&::-webkit-details-marker]:hidden";

/** Section title in the connect-flow dialogs (spec §2: 17 / 600). */
export const connectDialogTitle = "text-[17px] font-semibold";

/** Paths in the Details folds (spec §2 mono: 13 / 400). */
export const pathText = "font-mono text-[13px] font-normal";

/** A fold's summary: a 16px chevron that turns 90° when the fold opens. */
export function DisclosureSummary({
  className,
  children,
}: {
  className?: string;
  children: ReactNode;
}) {
  return (
    <summary className={cn(disclosureSummary, className)}>
      <ChevronRight
        aria-hidden="true"
        data-slot="disclosure-chevron"
        className="size-4 shrink-0 transition-transform [details[open]>summary>&]:rotate-90"
      />
      {children}
    </summary>
  );
}

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
  const applyValue = () => {
    if (!editing) return;
    setCredentialInputs((previous) => ({
      ...previous,
      [editing.name]: { ...previous[editing.name], [editing.key]: value },
    }));
    setValue("");
    setEditing(null);
  };
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
              .map((env) => ({
                key: `${key}:${env.key}`,
                reason: t("Enter {value0} or deselect {value1}", {
                  value0: env.key,
                  value1: item.name,
                }),
              }))
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
        <DialogTitle className={connectDialogTitle}>
          {title ?? t("Review servers to import")}
        </DialogTitle>
        {step && <p className="text-meta text-muted-foreground">{step}</p>}
      </DialogHeader>
      <div className="flex min-h-0 flex-col gap-4 py-1">
        <p className="shrink-0 text-meta text-muted-foreground">
          {description ??
            t(
              "Review the commands and URLs before adding them. Leave any server unchecked to keep it as it is.",
            )}
        </p>
        {notice}
        {details && (
          <details className="shrink-0">
            <DisclosureSummary className="text-meta">{t("Details")}</DisclosureSummary>
            <p
              className={`mt-2 whitespace-pre-wrap break-all text-muted-foreground ${pathText}`}
            >
              {details}
            </p>
          </details>
        )}
        {error && (
          <p role="alert" className="shrink-0 text-sm text-warning">
            {error}
          </p>
        )}
        {/* The inset keeps each card's selection and focus rings inside the
            scroller and the dialog body, which both clip them. */}
        <div
          data-slot="review-list"
          className={`-my-1 flex flex-col gap-2 overflow-y-auto p-1 ${
            keyedItems.length > 1
              ? "min-h-24"
              : keyedItems.length === 1
                ? "shrink-0"
                : "min-h-0"
          }`}
        >
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
                    <DisclosureSummary>
                      {item.name} {t("credentials and settings")}
                    </DisclosureSummary>
                    <div className="flex flex-col gap-2 pt-2">
                      {item.credentials.map((env) => (
                        <div key={env.key} className="flex items-center gap-2">
                          {/* A long key wraps instead of pushing the state and Enter
                              value out of the dialog, as GTK's line does. */}
                          <label className="flex min-w-0 flex-1 items-center gap-2 [overflow-wrap:anywhere]">
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
                            <span className="ml-auto shrink-0 text-muted-foreground">
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
                    {t("Unsupported: {value0}", { value0: item.unsupported })}
                  </p>
                )}
              </div>
            );
          })}
        </div>
        {missingRequired.map(({ key, reason }) => (
          <p
            key={key}
            className="shrink-0 text-sm text-warning [overflow-wrap:anywhere] last:pb-1"
          >
            {reason}
          </p>
        ))}
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
              <DialogTitle className={connectDialogTitle}>
                {editing.name} {t("credentials")}
              </DialogTitle>
            </DialogHeader>
            <p className="text-meta text-muted-foreground">
              {t(
                "This value is used only for the selected server. It is saved only if connection succeeds.",
              )}
            </p>
            <label className="flex flex-col gap-2 text-meta">
              {/* A long key wraps instead of widening the sheet, as GTK's does. */}
              <span className="[overflow-wrap:anywhere]">{editing.key}</span>
              {/* Spec §13: surface fill and a 2px accent focus ring. */}
              <Input
                type="password"
                autoComplete="off"
                className="bg-card focus-visible:ring-2 dark:bg-card"
                value={value}
                onChange={(e) => setValue(e.target.value)}
                onKeyDown={(e) => {
                  // Return uses the value, as the GTK prompt's default button does. Not
                  // while an input method is composing: that Enter picks a candidate
                  // (229 is the IME key code).
                  const composing = e.nativeEvent.isComposing || e.keyCode === 229;
                  if (e.key === "Enter" && !composing && value.trim()) {
                    e.preventDefault();
                    applyValue();
                  }
                }}
              />
            </label>
            <DialogFooter>
              <Button variant="ghost" onClick={() => setEditing(null)}>
                {t("Cancel")}
              </Button>
              <Button disabled={!value.trim()} onClick={applyValue}>
                {t("Use for connection")}
              </Button>
            </DialogFooter>
          </DialogContent>
        </Dialog>
      )}
      <DialogFooter>
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
        <span className="truncate text-sm font-medium">
          {item.sourceName ?? item.name}
        </span>
        {
          <span className="ml-auto shrink-0 text-meta text-muted-foreground">
            {item.unsupported
              ? t("Needs review")
              : item.updates?.length
                ? t("Updates existing server")
                : item.isNew
                  ? t("New")
                  : t("In Toolport")}
          </span>
        }
      </div>
      {/* An unsupported server is not imported, as in the GTK review. */}
      {item.sourceName && item.sourceName !== item.name && !item.unsupported && (
        <p className="mt-1 break-words text-meta text-muted-foreground [overflow-wrap:anywhere]">
          {t("Import as {value0}", { value0: item.name })}
        </p>
      )}
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
