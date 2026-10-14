import { useI18n, Trans } from "@/lib/i18n";
import { useEffect, useState } from "react";
import { ChevronDown, Copy, KeyRound, LogIn, Pencil, Trash2, Users } from "lucide-react";
import { isDownloadLauncher } from "@/lib/launcher";
import { errorHeadline, shortenUrls } from "@/lib/errors";
import { toast } from "sonner";
import type { ProbeResult, Registry, ServerEntry } from "@/lib/types";
import { Switch } from "@/components/ui/switch";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { TransportPill } from "@/components/TransportPill";
import { SecretsDialog } from "@/components/SecretsDialog";
import { ServerDialog } from "@/components/ServerDialog";
import { LaunchSetupDialog } from "@/components/LaunchSetupDialog";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { ServerLogo } from "@/components/ServerLogo";

interface Props {
  server: ServerEntry;
  registry: Registry | null;
  enabled: boolean;
  busy?: boolean;
  health?: ProbeResult;
  onToggle: (enabled: boolean) => void;
  onRemove: () => void;
  onRegistryChange: (registry: Registry) => void;
  /** Re-run the health probe (e.g. after authenticating). */
  onReprobe?: () => void;
}

type Status = "disabled" | "checking" | "connected" | "needs-auth" | "error";

function statusOf(enabled: boolean, health?: ProbeResult): Status {
  if (!enabled) return "disabled";
  // No probe result yet (loading, or queued behind an in-flight probe): checking.
  if (!health) return "checking";
  if (health.ok) return "connected";
  if (health.authRequired) return "needs-auth";
  return "error";
}

const STATUS_TEXT: Record<Status, string> = {
  disabled: "text-muted-foreground",
  checking: "text-muted-foreground",
  connected: "text-muted-foreground",
  "needs-auth": "text-warning",
  error: "text-destructive",
};

const STATUS_ARIA_LABEL: Record<Status, string> = {
  disabled: "Server disabled",
  checking: "Checking connection",
  connected: "Connected",
  "needs-auth": "Authentication required",
  error: "Connection error",
};

const ACTION =
  "inline-flex items-center gap-1.5 rounded-md px-2 py-1 text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring";

export function RegistryServerRow({
  registry,
  server,
  enabled,
  busy,
  health,
  onToggle,
  onRemove,
  onRegistryChange,
  onReprobe,
}: Props) {
  const { t } = useI18n();
  const [expanded, setExpanded] = useState(false);

  const target =
    server.command !== null
      ? [server.command, ...server.args].join(" ")
      : (server.url ?? "");
  const secretCount = server.env.filter((e) => e.secret).length;
  const status = statusOf(enabled, health);
  const requiredLaunch = server.launch?.inputs.filter((input) => input.required) ?? [];
  const missingPlainLaunch = requiredLaunch.filter(
    (input) => !input.secret && !input.value?.trim(),
  );
  // A probe that sits in "checking" past a few seconds is still starting. Name
  // that wait so a slow initialize reads as progress rather than a missing route.
  // Download launchers keep the more specific "Installing…" label.
  const launcher = isDownloadLauncher(server.command, server.args);
  const [initializing, setInitializing] = useState(false);
  // Reset during render (not in the effect) so a later re-check starts back at
  // "Checking…" instead of flashing a stale "Installing…".
  if (initializing && status !== "checking") setInitializing(false);
  useEffect(() => {
    if (status !== "checking") return;
    const t = setTimeout(() => setInitializing(true), 4000);
    return () => clearTimeout(t);
  }, [status]);
  // Team-synced servers are tagged `team:<id>`. An admin manages them centrally, so a
  // member sees a Team badge and can't edit/remove them locally (a local change would
  // just re-sync away), but still authenticates them (keys stay on their own machine).
  const isTeam = server.source?.startsWith("team:") ?? false;

  const label =
    status === "connected"
      ? t(health?.toolCount === 1 ? "Ready · {value1} tool" : "Ready · {value1} tools", {
          value1: health?.toolCount ?? 0,
        })
      : status === "error"
        ? t("Error")
        : status === "checking"
          ? initializing
            ? launcher
              ? t("Installing…")
              : t("Initializing…")
            : t("Checking…")
          : requiredLaunch.length
            ? missingPlainLaunch.length
              ? t("Setup required: {value1}", {
                  value1: missingPlainLaunch.map((input) => input.label).join(", "),
                })
              : t("Disabled · check launch setup")
            : t("Disabled");

  // Next free "Name (N)" for the duplicate-for-another-account action.
  const existingNames = new Set(registry?.servers.map((s) => s.name.toLowerCase()) ?? []);
  const baseName = server.name.replace(/\s\(\d+\)$/, "");
  let duplicateName = `${baseName} (2)`;
  let index = 2;
  while (existingNames.has(duplicateName.toLowerCase())) {
    index++;
    duplicateName = `${baseName} (${index})`;
  }

  const stop = (e: { stopPropagation: () => void }) => e.stopPropagation();

  return (
    <div
      className={`border-b border-border/60 last:border-b-0 ${enabled ? "" : "opacity-60"}`}
    >
      {/* Mouse users can click anywhere on the row to expand. Keyboard and screen
          reader users use the chevron button at the end. The row itself is not a
          button, so the toggle and Authenticate controls aren't nested inside one. */}
      <div
        onClick={() => setExpanded((v) => !v)}
        className="flex cursor-pointer items-center gap-3 px-3.5 py-2 transition-colors hover:bg-accent/40"
      >
        <span className="flex items-center" onClick={stop}>
          <Switch
            checked={enabled}
            disabled={busy}
            onCheckedChange={onToggle}
            aria-label={t("Toggle {value1}", { value1: server.name })}
          />
        </span>

        <ServerLogo name={server.name} transport={server.transport} size={28} />

        <span className="min-w-0 truncate text-sm font-medium">{server.name}</span>

        {isTeam ? (
          <span className="hidden shrink-0 items-center gap-1 rounded bg-info/15 px-1.5 py-0.5 text-[11px] font-medium text-info md:inline-flex">
            <Trans
              message={"{value1}Team"}
              values={{ value1: <Users className="size-3" aria-hidden="true" /> }}
            />
          </span>
        ) : (
          <span className="hidden max-w-40 shrink-0 truncate rounded bg-muted px-1.5 py-0.5 text-[11px] text-muted-foreground md:inline">
            <Trans
              message={"Personal {value1}"}
              values={{
                value1: server.source?.startsWith("imported:")
                  ? ` · ${server.source.replace("imported:", t("from "))}`
                  : "",
              }}
            />
          </span>
        )}

        <span className="ml-auto flex shrink-0 items-center gap-2.5">
          <span
            role="status"
            aria-label={
              initializing
                ? launcher
                  ? t("Installing the server package")
                  : t("Server initializing")
                : status === "connected"
                  ? label.replace(" · ", ", ")
                  : t(STATUS_ARIA_LABEL[status])
            }
            className="sr-only"
          />
          {status === "needs-auth" ? (
            <SecretsDialog
              server={server}
              onSaved={onRegistryChange}
              onChanged={onReprobe}
              trigger={
                <button
                  onClick={stop}
                  className="inline-flex items-center gap-1.5 rounded-md border border-warning/40 px-2.5 py-1 text-xs text-warning transition-colors hover:bg-warning/10 focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-warning"
                >
                  <Trans
                    message={"{value1}Authenticate"}
                    values={{ value1: <LogIn className="size-3.5" /> }}
                  />
                </button>
              }
            />
          ) : (
            <StatusLabel status={status} label={label} error={health?.error ?? null} />
          )}

          <TransportPill transport={server.transport} />

          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation();
              setExpanded((v) => !v);
            }}
            aria-expanded={expanded}
            aria-label={
              expanded
                ? t("Hide {value1} details", { value1: server.name })
                : t("Show {value1} details", { value1: server.name })
            }
            className="rounded p-0.5 text-muted-foreground/50 transition-colors hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
          >
            <ChevronDown
              className={`size-4 transition-transform ${expanded ? "rotate-180" : ""}`}
              aria-hidden="true"
            />
          </button>
        </span>
      </div>

      {expanded && (
        <div className="flex flex-col gap-2.5 px-3.5 pt-0.5 pb-3 pl-12">
          {!!requiredLaunch.length && (
            <p className="text-xs text-muted-foreground">
              <Trans
                message={
                  "Launch setup: {value1}. Open Launch setup to add or review these values before enabling."
                }
                values={{ value1: requiredLaunch.map((input) => input.label).join(", ") }}
              />
            </p>
          )}
          {target && (
            <code className="block rounded-md bg-muted px-2 py-1.5 font-mono text-xs break-all text-muted-foreground">
              {target}
            </code>
          )}
          {status === "error" && health?.error && (
            <div className="flex flex-col gap-1">
              {/* Lead with a readable headline so the useful signal (exit status,
                  EADDRINUSE, a 401) isn't buried under a stack trace + a giant
                  OAuth URL; the full output stays below, bounded and scrollable
                  with long URLs shortened. */}
              <div className="flex items-start justify-between gap-2">
                <p className="text-xs font-medium text-warning">
                  {errorHeadline(health.error)}
                </p>
                <button
                  type="button"
                  onClick={(e) => {
                    e.stopPropagation();
                    void navigator.clipboard.writeText(health?.error ?? "");
                    toast.success(t("Error copied"));
                  }}
                  title={t("Copy the full error")}
                  className="inline-flex shrink-0 items-center gap-1 rounded px-1.5 py-0.5 text-[11px] text-muted-foreground transition-colors hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
                >
                  <Trans
                    message={"{value1}Copy"}
                    values={{ value1: <Copy className="size-3" /> }}
                  />
                </button>
              </div>
              <p className="max-h-32 overflow-y-auto font-mono text-[11px] break-words whitespace-pre-wrap text-muted-foreground">
                {shortenUrls(health.error)}
              </p>
            </div>
          )}

          <div className="flex flex-wrap items-center gap-1">
            <SecretsDialog
              server={server}
              onSaved={onRegistryChange}
              onChanged={onReprobe}
              trigger={
                <button className={ACTION}>
                  <Trans
                    message={"{value1}Secrets{value2}"}
                    values={{
                      value1: <KeyRound className="size-3.5" />,
                      value2: secretCount > 0 ? ` (${secretCount})` : "",
                    }}
                  />
                </button>
              }
            />

            {!!server.launch?.inputs.length && (
              <LaunchSetupDialog
                server={server}
                onSaved={onRegistryChange}
                onChanged={onReprobe}
                trigger={<button className={ACTION}>{t("Launch setup")}</button>}
              />
            )}

            {isTeam ? (
              <span className="inline-flex items-center gap-1.5 px-2 py-1 text-xs text-muted-foreground">
                <Trans
                  message={
                    "{value1}Managed by your team. Ask an admin to change or remove it."
                  }
                  values={{ value1: <Users className="size-3.5" aria-hidden="true" /> }}
                />
              </span>
            ) : (
              <>
                <ServerDialog
                  onSaved={onRegistryChange}
                  initial={{ ...server, name: duplicateName }}
                  existingNames={registry?.servers.map((s) => s.name) ?? []}
                  trigger={
                    <button className={ACTION} title={t("Add another account")}>
                      <Trans
                        message={"{value1}Duplicate"}
                        values={{ value1: <Copy className="size-3.5" /> }}
                      />
                    </button>
                  }
                />

                <ServerDialog
                  onSaved={onRegistryChange}
                  editId={server.id}
                  initial={server}
                  existingNames={registry?.servers.map((s) => s.name) ?? []}
                  trigger={
                    <button className={ACTION}>
                      <Trans
                        message={"{value1}Edit"}
                        values={{ value1: <Pencil className="size-3.5" /> }}
                      />
                    </button>
                  }
                />

                <ConfirmDialog
                  trigger={
                    <button
                      disabled={busy}
                      className={`${ACTION} hover:bg-destructive/10 hover:text-destructive`}
                    >
                      <Trans
                        message={"{value1}Remove"}
                        values={{ value1: <Trash2 className="size-3.5" /> }}
                      />
                    </button>
                  }
                  title={t("Remove {value1}?", { value1: server.name })}
                  description={t(
                    "This deletes the server from Toolport. Any saved secrets stay in your keychain.",
                  )}
                  confirmLabel={t("Remove")}
                  destructive
                  onConfirm={onRemove}
                />
              </>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

function StatusLabel({
  status,
  label,
  error,
}: {
  status: Status;
  label: string;
  error: string | null;
}) {
  const text = (
    <span
      aria-hidden="true"
      className={`text-xs font-medium whitespace-nowrap ${STATUS_TEXT[status]}`}
    >
      {label}
    </span>
  );
  if (status === "error" && error) {
    return (
      <Tooltip>
        <TooltipTrigger asChild>{text}</TooltipTrigger>
        <TooltipContent side="top" className="max-w-xs">
          <p className="text-xs text-warning">{errorHeadline(error)}</p>
        </TooltipContent>
      </Tooltip>
    );
  }
  return text;
}
