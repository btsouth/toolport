import { useEffect, useState } from "react";
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
import { HOSTED_TEAMS_URL, teamUrlError } from "@/lib/teamUrl";
import { accountStatusText, planName, syncSignInUrl } from "@/lib/personalSync";
import { argumentValues } from "@/lib/executionReview";
import { PRO_LINE, TEAMS_FREE_LINE } from "@/lib/teamsPlan";
import { openExternal } from "@/lib/openUrl";
import { visibleExecutionText as visibleText } from "@/lib/visibleExecutionText";
import { Button } from "./ui/button";
import { Input } from "./ui/input";
import { Callout } from "./Callout";

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
  const waiting = registry.servers.filter(
    (s) => s.teamEnableReview === true && !s.enabled,
  ).length;
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
      <p className="text-sm text-muted-foreground">
        Set up once. Your servers follow you to every machine. Secret values and approvals
        stay on this machine.
      </p>
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
          <section className="space-y-3 rounded-lg border p-4" aria-label="Your account">
            <h3 className="font-medium">Your account</h3>
            {sync?.signInRequired ? (
              <p>
                Saved account plan: {planName(status?.plan)}. Sign in to confirm your
                account and resume sync.
              </p>
            ) : status ? (
              accountStatusText(status).map((line) => (
                <p key={line} className="text-sm">
                  {line}
                </p>
              ))
            ) : (
              <p>Account status unavailable. Retry sync.</p>
            )}
            {!sync?.signInRequired && (
              <p className="text-sm">
                {sync?.lastSyncedAt
                  ? `Last synced ${new Date(sync.lastSyncedAt).toLocaleString()}`
                  : "Waiting for first sync"}
              </p>
            )}
            {!!Object.keys(sync?.pending ?? {}).length && (
              <p className="text-sm">Changes waiting to sync</p>
            )}
            {waiting > 0 && (
              <p className="text-sm">
                {waiting === 1
                  ? "1 server is waiting for review on this machine."
                  : `${waiting} servers are waiting for review on this machine.`}
              </p>
            )}
            <div className="flex gap-2">
              {sync?.signInRequired ? (
                // Sync cannot succeed without sign-in. Sign out stays: it clears
                // the saved account and its token.
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
              {waiting > 0 && onOpenServers && (
                <Button variant="outline" onClick={onOpenServers}>
                  Review in Servers
                </Button>
              )}
              <Button
                variant="outline"
                onClick={() => void run(() => openExternal(team.serverUrl))}
              >
                Your account
              </Button>
              <Button
                variant="outline"
                disabled={busy}
                onClick={() =>
                  void run(async () => onRegistryChange(await teamDisconnect()))
                }
              >
                Sign out
              </Button>
            </div>
          </section>
          {sync?.chooseLocalServers && (
            <section
              className="space-y-3 rounded-lg border p-4"
              aria-label="Choose local servers to sync"
            >
              <h3 className="font-medium">Choose local servers to sync</h3>
              <p className="text-sm">
                Your account now has one person. Existing local servers stay on this
                machine unless you choose them here.
              </p>
              {registry.servers
                .filter((s) => !s.source?.startsWith("team:"))
                .map((s) => (
                  <label key={s.id} className="flex items-center gap-2 text-sm">
                    <input
                      type="checkbox"
                      checked={!s.syncLocalOnly}
                      disabled={busy}
                      onChange={(e) => {
                        const localOnly = !e.target.checked;
                        void run(async () =>
                          onRegistryChange(
                            await invoke<Registry>("personal_sync_local_only", {
                              serverId: s.id,
                              localOnly,
                            }),
                          ),
                        );
                      }}
                    />
                    {s.name}
                  </label>
                ))}
              <Button
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
            </section>
          )}
          {Object.entries(sync?.publishErrors ?? {}).map(([id, message]) => (
            <Callout key={id} variant="warning">
              <p role="alert">
                {registry.servers.find((s) => s.id === sync?.pending?.[id]?.localId)
                  ?.name ?? id}
                : {message}
              </p>
            </Callout>
          ))}
          {Object.entries(sync?.conflicts ?? {}).map(([id, remote]) => (
            <section
              key={id}
              className="space-y-2 rounded-lg border p-4"
              aria-label={`Sync conflict ${id}`}
            >
              <h3 className="font-medium">
                {registry.servers.find((s) => s.id === sync?.pending?.[id]?.localId)
                  ?.name ??
                  (remote as { name?: string } | null)?.name ??
                  id}{" "}
                changed on both machines
              </h3>
              <p className="text-sm">
                Choose which version to keep. Your local version is saved until you
                choose.
              </p>
              <ConflictVersions local={sync?.pending?.[id]?.after} remote={remote} />
              {[true, false].map((keepMine) => (
                <Button
                  key={String(keepMine)}
                  className="mr-2"
                  variant="outline"
                  disabled={busy}
                  onClick={() =>
                    void run(async () => {
                      onRegistryChange(
                        await invoke<Registry>("personal_sync_resolve_conflict", {
                          id,
                          expected: sync?.conflictVersions?.[id],
                          keepMine,
                        }),
                      );
                      await teamSync();
                      onRegistryChange(await getRegistry());
                    })
                  }
                >
                  {keepMine ? "Keep this machine's version" : "Use synced version"}
                </Button>
              ))}
            </section>
          ))}
          <p className="text-sm text-muted-foreground">
            Edit your servers in Servers. Changes sync automatically. New commands need
            approval on each machine. Use “Keep on this machine only” for a local setup.
          </p>
        </>
      )}
    </div>
  );
}
