import { useEffect, useState, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { ArgBinding, Registry } from "@/lib/types";
import {
  teamConnect,
  teamDisconnect,
  teamSync,
  teamJoinPoll,
  getRegistry,
  reconnectSync,
} from "@/lib/api";
import { HOSTED_TEAMS_URL, TEAMS_CREATE_URL, teamUrlError } from "@/lib/teamUrl";
import { accountStatusText, planName, syncSignInUrl } from "@/lib/personalSync";
import { argumentValues } from "@/lib/executionReview";
import { PRO_LINE, TEAMS_FREE_LINE } from "@/lib/teamsPlan";
import { openExternal } from "@/lib/openUrl";
import { visibleExecutionText as visibleText } from "@/lib/visibleExecutionText";
import { Button } from "./ui/button";
import { Input } from "./ui/input";
import { Callout } from "./Callout";
import { ServerLogo } from "./ServerLogo";

function SectionLabel({
  children,
  className = "",
}: {
  children: string;
  className?: string;
}) {
  return (
    <h3
      className={`mb-2 text-xs font-semibold tracking-[0.05em] text-muted-foreground uppercase ${className}`}
    >
      {children}
    </h3>
  );
}

/** One line in the Needs you block: a bold title, a muted detail, one action. */
function AttentionItem({
  title,
  detail,
  label,
  role,
  open,
  expanded,
  children,
}: {
  title: string;
  detail?: string;
  label?: string;
  role?: string;
  open?: boolean;
  expanded?: ReactNode;
  children?: ReactNode;
}) {
  return (
    <div className="px-3.5 py-2.5" aria-label={label} role={role}>
      <div className="flex items-center gap-3">
        <div className="min-w-0 flex-1">
          <p className="text-sm font-semibold">{title}</p>
          {detail && <p className="text-xs text-muted-foreground">{detail}</p>}
        </div>
        {children}
      </div>
      {open && <div className="mt-3">{expanded}</div>}
    </div>
  );
}

function conflictFields(value: unknown): Record<string, string> {
  if (!value || typeof value !== "object") return { Server: "Removed" };
  const v = value as Record<string, unknown>;
  const fields: Record<string, string> = {};
  for (const [key, label] of Object.entries({
    name: "Name",
    transport: "Transport",
    command: "Command",
    cwd: "Working directory",
    url: "URL",
    disabled: "Disabled",
    requestTimeoutMs: "Request timeout",
    initializeTimeoutMs: "Startup timeout",
    disabledTools: "Disabled tools",
  })) {
    if (v[key] != null) fields[label] = visibleText(String(v[key]));
  }
  if (Array.isArray(v.args))
    v.args.forEach((arg, i) => {
      fields[`Argument ${i + 1}`] = visibleText(String(arg));
    });
  // Same plain labels and masking as the execution review.
  const launch = v.launch as { inputs?: unknown[]; bindings?: ArgBinding[] } | undefined;
  for (const [label, rows] of [
    ["Environment", v.env],
    ["Input", launch?.inputs],
    ["Header", v.headerKeys],
  ] as const) {
    if (Array.isArray(rows))
      for (const row of rows as {
        key: string;
        env?: unknown;
        value?: unknown;
        secret?: boolean;
        source?: { ref?: string };
      }[])
        fields[`${label}: ${visibleText(row.key)}`] =
          label === "Header" && typeof row.env === "string"
            ? `Uses environment: ${visibleText(row.env)}`
            : row.source?.ref
              ? `Password manager: ${visibleText(row.source.ref)}`
              : row.secret
                ? "<masked secret>"
                : row.value != null
                  ? visibleText(
                      typeof row.value === "string"
                        ? row.value
                        : JSON.stringify(row.value),
                    )
                  : "Set on this machine";
  }
  if (Array.isArray(launch?.bindings) && launch.bindings.length)
    fields["Argument values"] = argumentValues(launch.bindings);
  return fields;
}
function ConflictVersions({ local, remote }: { local: unknown; remote: unknown }) {
  const left = conflictFields(local),
    right = conflictFields(remote);
  const keys = [...new Set([...Object.keys(left), ...Object.keys(right)])];
  return (
    <div className="grid gap-4 sm:grid-cols-2">
      {[
        ["This machine", left],
        ["Other machine", right],
      ].map(([title, fields]) => (
        <div key={String(title)}>
          <h4 className="font-medium">{String(title)}</h4>
          <dl className="text-sm">
            {keys.map((key) => (
              <div
                key={key}
                className={left[key] !== right[key] ? "bg-amber-500/10" : ""}
              >
                <dt>{key}</dt>
                <dd className="whitespace-pre-wrap">
                  {(fields as Record<string, string>)[key] ?? "Not set"}
                </dd>
              </div>
            ))}
          </dl>
        </div>
      ))}
    </div>
  );
}
export function PersonalSyncView({
  registry,
  onRegistryChange,
  onOpenServers,
}: {
  registry: Registry;
  onRegistryChange: (r: Registry) => void;
  onOpenServers?: () => void;
}) {
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [url, setUrl] = useState(HOSTED_TEAMS_URL);
  const [code, setCode] = useState("");
  const [pending, setPending] = useState<{ url: string; token: string } | null>(null);
  useEffect(() => {
    if (!pending) return;
    let cancelled = false;
    const timer = setTimeout(() => {
      void teamJoinPoll(pending.url, pending.token)
        .then((result) => {
          if (cancelled) return;
          if (result.status === "connected") {
            setPending(null);
            if (result.registry) onRegistryChange(result.registry);
          } else if (result.status === "pending") setPending({ ...pending });
          else {
            setPending(null);
            setError("The invitation was declined or expired. Get a new manual code.");
          }
        })
        .catch((e) => {
          if (!cancelled) {
            setPending(null);
            setError(String(e));
          }
        });
    }, 2500);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [pending, onRegistryChange]);
  const team = registry.team;
  const status = team?.accountStatus;
  const sync = team?.personalSyncState;
  // The sidebar badge counts these, so name them here and point to where the
  // review happens.
  const arrivals = registry.servers.filter(
    (s) => s.teamEnableReview === true && !s.enabled && !s.syncLocalOnly,
  );
  const [comparing, setComparing] = useState<string | null>(null);
  // Read the clock in state so "Last synced" stays current without impure renders.
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(timer);
  }, []);
  const needsYou =
    !!sync?.chooseLocalServers ||
    arrivals.length > 0 ||
    !!Object.keys(sync?.conflicts ?? {}).length ||
    !!Object.keys(sync?.publishErrors ?? {}).length;
  const lastSynced = sync?.lastSyncedAt
    ? now - sync.lastSyncedAt < 60_000
      ? "Last synced just now"
      : `Last synced ${Math.floor((now - sync.lastSyncedAt) / 60_000)} ${
          Math.floor((now - sync.lastSyncedAt) / 60_000) === 1 ? "minute" : "minutes"
        } ago`
    : null;
  const summary = Object.keys(sync?.conflicts ?? {}).length
    ? "Changes need your choice"
    : arrivals.length
      ? `${arrivals.length} ${arrivals.length === 1 ? "server is" : "servers are"} waiting for review on this machine`
      : !lastSynced
        ? "Waiting for first sync"
        : Object.keys(sync?.pending ?? {}).length
          ? "Changes waiting to sync"
          : "Sync is up to date";
  const healthy = summary === "Sync is up to date";
  const statusLine = lastSynced ? `${summary} · ${lastSynced}` : summary;
  async function run(work: () => Promise<void>) {
    setBusy(true);
    setError(null);
    try {
      await work();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <div className="mx-auto max-w-2xl space-y-4">
      <h2 className="text-base font-semibold">Sync</h2>
      {!team && (
        <p className="text-sm text-muted-foreground">
          Set up once. Your servers follow you to every machine. Secret values and
          approvals stay on this machine.
        </p>
      )}
      {(error || sync?.error || team?.accountStatusError) && (
        <Callout variant="danger">
          <p role="alert">{error || sync?.error || team?.accountStatusError}</p>
        </Callout>
      )}
      {Object.entries(sync?.warnings ?? {}).map(([id, warning]) => (
        <Callout key={id} variant="warning">
          <p role="alert">{warning}</p>
        </Callout>
      ))}
      {!team ? (
        <>
          <Button
            onClick={() =>
              void run(() => {
                const invalid = teamUrlError(url);
                if (invalid) throw new Error(invalid);
                return openExternal(syncSignInUrl(url));
              })
            }
            disabled={busy}
          >
            Sign in to sync
          </Button>
          <p className="text-sm">
            {TEAMS_FREE_LINE} {PRO_LINE}
          </p>
          {pending && (
            <Callout variant="warning">
              <p>
                Waiting for invitation approval. Leave this open, it finishes on its own.
              </p>
              <Button variant="outline" onClick={() => setPending(null)}>
                Cancel request
              </Button>
            </Callout>
          )}
          <details className="rounded-lg border p-4">
            <summary className="cursor-pointer text-sm font-medium">
              Use a manual code
            </summary>
            <p className="mt-3 text-sm text-muted-foreground">
              If your browser cannot open Toolport, copy the manual code shown after
              approving this device.
            </p>
            <label className="mt-3 block text-sm">
              Sync service URL
              <Input value={url} onChange={(e) => setUrl(e.target.value)} />
            </label>
            <label className="mt-3 block text-sm">
              Manual code
              <Input value={code} onChange={(e) => setCode(e.target.value)} />
            </label>
            <Button
              className="mt-3"
              disabled={busy || !code.trim()}
              onClick={() =>
                void run(async () => {
                  const invalid = teamUrlError(url);
                  if (invalid) throw new Error(invalid);
                  const result = await teamConnect(url, code.trim());
                  if (result.status === "pending" && result.requestToken) {
                    setPending({ url, token: result.requestToken });
                    return;
                  }
                  if (result.status !== "connected")
                    throw new Error("Could not sign in. Get a new manual code.");
                  onRegistryChange(result.registry ?? (await getRegistry()));
                })
              }
            >
              Sign in with code
            </Button>
          </details>
        </>
      ) : (
        <>
          <div className="flex items-center gap-3">
            <p
              className={`flex-1 text-sm ${healthy ? "text-success" : "text-muted-foreground"}`}
            >
              {statusLine}
            </p>
            {sync?.signInRequired ? (
              // Sync cannot succeed without sign-in.
              <Button disabled={busy} onClick={() => void run(() => reconnectSync())}>
                Sign in
              </Button>
            ) : (
              <Button
                disabled={busy}
                onClick={() =>
                  void run(async () => {
                    await teamSync();
                    onRegistryChange(await getRegistry());
                  })
                }
              >
                Sync now
              </Button>
            )}
          </div>
          {needsYou && (
            <section aria-label="Needs you">
              <SectionLabel>Needs you</SectionLabel>
              <div className="divide-y divide-warning/15 rounded-r-lg border-l-3 border-warning bg-warning/7">
                {sync?.chooseLocalServers && (
                  <AttentionItem
                    title="Choose which servers sync"
                    detail="Servers you already had start on This machine. Switch any you want everywhere below."
                  >
                    <Button
                      size="sm"
                      variant="outline"
                      disabled={busy}
                      onClick={() =>
                        void run(async () =>
                          onRegistryChange(
                            await invoke<Registry>("personal_sync_finish_selection"),
                          ),
                        )
                      }
                    >
                      Done
                    </Button>
                  </AttentionItem>
                )}
                {arrivals.map((s) => (
                  <AttentionItem
                    key={s.id}
                    title={`${visibleText(s.name)} arrived from another machine`}
                    detail="Review it in Servers before it runs here."
                  >
                    {onOpenServers && (
                      <Button size="sm" variant="outline" onClick={onOpenServers}>
                        Open in Servers
                      </Button>
                    )}
                  </AttentionItem>
                ))}
                {Object.entries(sync?.conflicts ?? {}).map(([id, remote]) => {
                  const name =
                    registry.servers.find((s) => s.id === sync?.pending?.[id]?.localId)
                      ?.name ??
                    (remote as { name?: string } | null)?.name ??
                    id;
                  return (
                    <AttentionItem
                      key={id}
                      label={`Sync conflict ${id}`}
                      title={`${visibleText(name)} changed on two machines`}
                      detail="Pick which version to keep. This machine's version is saved until you choose."
                      open={comparing === id}
                      expanded={
                        <div className="space-y-3">
                          <ConflictVersions
                            local={sync?.pending?.[id]?.after}
                            remote={remote}
                          />
                          {[true, false].map((keepMine) => (
                            <Button
                              key={String(keepMine)}
                              className="mr-2"
                              size="sm"
                              variant="outline"
                              disabled={busy}
                              onClick={() =>
                                void run(async () => {
                                  onRegistryChange(
                                    await invoke<Registry>(
                                      "personal_sync_resolve_conflict",
                                      {
                                        id,
                                        expected: sync?.conflictVersions?.[id],
                                        keepMine,
                                      },
                                    ),
                                  );
                                  await teamSync();
                                  onRegistryChange(await getRegistry());
                                })
                              }
                            >
                              {keepMine
                                ? "Keep this machine's version"
                                : "Use synced version"}
                            </Button>
                          ))}
                        </div>
                      }
                    >
                      <Button
                        size="sm"
                        variant="outline"
                        aria-expanded={comparing === id}
                        onClick={() => setComparing(comparing === id ? null : id)}
                      >
                        Compare
                      </Button>
                    </AttentionItem>
                  );
                })}
                {Object.entries(sync?.publishErrors ?? {}).map(([id, message]) => (
                  <AttentionItem
                    key={id}
                    role="alert"
                    title={`${
                      registry.servers.find((s) => s.id === sync?.pending?.[id]?.localId)
                        ?.name ?? id
                    } could not sync`}
                    detail={message}
                  />
                ))}
              </div>
            </section>
          )}
          <section aria-label="Servers">
            <div className="flex items-center gap-2">
              <SectionLabel className="flex-1">Servers</SectionLabel>
              <label className="flex items-center gap-2 text-xs text-muted-foreground">
                Sync new servers
                <select
                  className="rounded-md border bg-background px-2 py-1 text-xs text-foreground"
                  value={sync?.newServersLocalOnly ? "here" : "every"}
                  disabled={busy}
                  onChange={(e) => {
                    const localOnly = e.target.value === "here";
                    void run(async () =>
                      onRegistryChange(
                        await invoke<Registry>("personal_sync_new_servers_local_only", {
                          localOnly,
                        }),
                      ),
                    );
                  }}
                >
                  <option value="every">Every machine</option>
                  <option value="here">This machine</option>
                </select>
              </label>
            </div>
            <ul className="divide-y">
              {registry.servers.map((s) => {
                const conflict = sync?.conflicts?.[s.teamOriginalId ?? s.id];
                const tag = conflict
                  ? "changed on two machines"
                  : arrivals.includes(s)
                    ? "needs review"
                    : null;
                return (
                  <li key={s.id} className="flex items-center gap-3 py-2.5">
                    <ServerLogo name={s.name} transport={s.transport} />
                    <span className="min-w-0 flex-1 truncate font-medium" title={s.name}>
                      {visibleText(s.name)}
                      {tag && <span className="ml-2 text-xs text-warning">{tag}</span>}
                    </span>
                    <div
                      role="radiogroup"
                      aria-label={`Where ${s.name} lives`}
                      className="flex shrink-0 rounded-lg border bg-muted/40 p-0.5"
                    >
                      {[false, true].map((localOnly) => {
                        const active = !!s.syncLocalOnly === localOnly;
                        return (
                          <button
                            key={String(localOnly)}
                            type="button"
                            role="radio"
                            aria-checked={active}
                            disabled={busy}
                            title={
                              localOnly
                                ? "Stop syncing. Other machines keep their copy."
                                : "Sync this server to your other machines"
                            }
                            className={`rounded-md px-2.5 py-1 text-xs ${
                              active
                                ? "bg-background font-semibold text-foreground shadow-sm"
                                : "text-muted-foreground"
                            }`}
                            onClick={() => {
                              if (active) return;
                              void run(async () =>
                                onRegistryChange(
                                  await invoke<Registry>("personal_sync_local_only", {
                                    serverId: s.id,
                                    localOnly,
                                  }),
                                ),
                              );
                            }}
                          >
                            {localOnly ? "This machine" : "Every machine"}
                          </button>
                        );
                      })}
                    </div>
                  </li>
                );
              })}
              {!registry.servers.length && (
                <li className="py-2.5 text-sm text-muted-foreground">
                  No servers yet. Servers you add here show up on your other machines.
                </li>
              )}
            </ul>
            <p className="mt-2 text-xs text-muted-foreground">
              Keys and sign-ins never leave a machine. Switching a server to This machine
              stops syncing it; other machines keep their copy until you remove it there.
            </p>
          </section>
          <section aria-label="Your account">
            <SectionLabel>Account</SectionLabel>
            <div className="divide-y">
              <div className="flex items-center gap-3 py-2.5">
                <div className="min-w-0 flex-1">
                  <p className="font-medium">Your account</p>
                  {sync?.signInRequired ? (
                    <p className="text-xs text-muted-foreground">
                      Saved account plan: {planName(status?.plan)}. Sign in to confirm
                      your account and resume sync.
                    </p>
                  ) : status ? (
                    accountStatusText(status).map((line) => (
                      <p key={line} className="text-xs text-muted-foreground">
                        {line}
                      </p>
                    ))
                  ) : (
                    <p className="text-xs text-muted-foreground">
                      Account status unavailable. Retry sync.
                    </p>
                  )}
                </div>
                <Button
                  size="sm"
                  variant="outline"
                  onClick={() => void run(() => openExternal(team.serverUrl))}
                >
                  Manage plan
                </Button>
                <Button
                  size="sm"
                  variant="outline"
                  disabled={busy}
                  onClick={() =>
                    void run(async () => onRegistryChange(await teamDisconnect()))
                  }
                >
                  Sign out
                </Button>
              </div>
              <div className="flex items-center gap-3 py-2.5">
                <div className="min-w-0 flex-1">
                  <p className="font-medium">Teams</p>
                  <p className="text-xs text-muted-foreground">
                    Share servers with other people. Your own servers stay yours.
                  </p>
                </div>
                <Button
                  size="sm"
                  variant="outline"
                  onClick={() => void run(() => openExternal(TEAMS_CREATE_URL))}
                >
                  Create a team
                </Button>
              </div>
            </div>
          </section>
        </>
      )}
    </div>
  );
}
