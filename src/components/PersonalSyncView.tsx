import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { Registry } from "@/lib/types";
import {
  teamConnect,
  teamDisconnect,
  teamSync,
  teamJoinPoll,
  getRegistry,
} from "@/lib/api";
import { HOSTED_TEAMS_URL, teamUrlError } from "@/lib/teamUrl";
import { accountStatusText, SYNC_SIGN_IN_URL } from "@/lib/personalSync";
import { PRO_LINE, TEAMS_FREE_LINE } from "@/lib/teamsPlan";
import { openExternal } from "@/lib/openUrl";
import { Button } from "./ui/button";
import { Input } from "./ui/input";
import { Callout } from "./Callout";

export function PersonalSyncView({
  registry,
  onRegistryChange,
}: {
  registry: Registry;
  onRegistryChange: (r: Registry) => void;
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
      {(error || sync?.error) && (
        <Callout variant="danger">
          <p role="alert">{error || sync?.error}</p>
        </Callout>
      )}
      {!team ? (
        <>
          <Button
            onClick={() => void run(() => openExternal(SYNC_SIGN_IN_URL))}
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
            {status ? (
              accountStatusText(status).map((line) => (
                <p key={line} className="text-sm">
                  {line}
                </p>
              ))
            ) : (
              <p>Account status unavailable. Retry sync.</p>
            )}
            <p className="text-sm">
              {sync?.lastSyncedAt
                ? `Last synced ${new Date(sync.lastSyncedAt).toLocaleString()}`
                : "Waiting for first sync"}
            </p>
            {!!Object.keys(sync?.pending ?? {}).length && (
              <p className="text-sm">Changes waiting to sync</p>
            )}
            <div className="flex gap-2">
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
          {Object.entries(sync?.conflicts ?? {}).map(([id, remote]) => (
            <section
              key={id}
              className="space-y-2 rounded-lg border p-4"
              aria-label={`Sync conflict ${id}`}
            >
              <h3 className="font-medium">
                {registry.servers.find((s) => s.id === sync?.pending?.[id]?.localId)
                  ?.name ?? id}{" "}
                changed on both machines
              </h3>
              <p className="text-sm">
                Choose which version to keep. Your local version is saved until you
                choose.
              </p>
              <pre className="max-h-48 overflow-auto text-xs">
                {JSON.stringify(remote, null, 2)}
              </pre>
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
                          expected: remote,
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
