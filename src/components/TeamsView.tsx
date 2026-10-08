import { TeamSharePreview } from "./TeamSharePreview";
import { teamShareAction } from "@/lib/teamShare";
import { useEffect, useState } from "react";
import {
  RefreshCw,
  LogOut,
  Upload,
  ShieldCheck,
  Users,
  Server,
  AlertTriangle,
  FileText,
  ArrowUpRight,
  Check,
} from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
} from "@/components/ui/dialog";
import { listen } from "@tauri-apps/api/event";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Callout } from "@/components/Callout";
import { RuleStateBadge } from "@/components/RuleStateBadge";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import {
  teamConnect,
  teamAccountLink,
  teamJoinPoll,
  teamSync,
  teamDisconnect,
  teamPushPreview,
  getRegistry,
  teamUseManaged,
  teamPush,
  teamInstructionsStatus,
  setServerEnabled,
} from "@/lib/api";
import {
  HOSTED_TEAMS_URL,
  TEAMS_CREATE_URL,
  TEAMS_MARKETING_URL,
  TEAMS_PRICING_URL,
  TEAMS_SELFHOST_URL,
  teamUrlError,
} from "@/lib/teamUrl";
import { TEAMS_FREE_LINE, TEAMS_PAID_LINE, TEAMS_TRIAL_DAYS } from "@/lib/teamsPlan";
import { openExternal } from "@/lib/openUrl";
import { isEnabled, activeProfile } from "@/lib/types";
import type { TeamPushPreview } from "@/lib/api";
import type { Registry, InstructionsStatusView } from "@/lib/types";

interface MemberChange {
  key: string;
  title: string;
  hash: string;
  fields: { field: string; before: string; after: string }[];
  labels: {
    author?: { name?: string };
    at?: number;
    via?: string;
    approvedBy?: { name?: string };
  }[];
}

interface MemberReviewState {
  pending: Record<string, MemberChange>;
}

/**
 * Toolport Teams: join a team and have its shared MCP server set appear locally. The
 * team server holds only the server set + non-secret config, never a key, so after
 * connecting you still vault each server's secrets locally (Servers tab). That keeps
 * "no keys in the cloud" true even on a team.
 */
export function TeamsView({
  registry,
  onRegistryChange,
}: {
  registry: Registry | null;
  onRegistryChange: (r: Registry) => void;
}) {
  const team = registry?.team ?? null;
  const isAdmin = team?.role === "admin";
  const review = (
    team as (NonNullable<Registry["team"]> & { memberReview?: MemberReviewState }) | null
  )?.memberReview;
  const changes = Object.values(review?.pending ?? {});
  const [reviewOpen, setReviewOpen] = useState(false);
  const [proposal, setProposal] = useState<{ confirmUrl: string } | null>(null);
  const teamServers = (registry?.servers ?? []).filter((s) =>
    s.source?.startsWith("team:"),
  );

  const [serverUrl, setServerUrl] = useState(HOSTED_TEAMS_URL);
  const [inviteCode, setInviteCode] = useState("");
  const [memberName, setMemberName] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [skipNote, setSkipNote] = useState<string | null>(null);
  const [selectedIds, setSelectedIds] = useState<string[]>([]);
  const personalServers = (registry?.servers ?? []).filter(
    (s) =>
      !s.source?.startsWith("team:") &&
      !["toolport-gateway", "conduit-gateway"].includes(s.id),
  );
  const [pushPreview, setPushPreview] = useState<TeamPushPreview | null>(null);
  // The member-facing Team Instructions status on this machine (spec W4): what the org pushed
  // and how each installed AI client currently holds it. Refetched on connect and whenever a sync
  // bumps the config version, so it tracks the files the writer just wrote.
  const [instrSnapshot, setInstrSnapshot] = useState<{
    teamId: string;
    status: InstructionsStatusView | null;
  } | null>(null);
  const [instrErrorTeamId, setInstrErrorTeamId] = useState<string | null>(null);
  const [instrRetry, setInstrRetry] = useState(0);
  const instrTeamId = team?.teamId ?? null;
  // A last-good result is safe to preserve across version refreshes for the same team, but
  // must never appear after switching organizations.
  const instr = instrSnapshot?.teamId === instrTeamId ? instrSnapshot.status : null;
  const instrError = instrTeamId !== null && instrErrorTeamId === instrTeamId;
  useEffect(() => {
    // The command returns null when there's no team (or no active instructions), so there's no
    // synchronous clear here — the result always lands via the async resolution.
    let cancelled = false;
    const requestedTeamId = instrTeamId;
    teamInstructionsStatus()
      .then((s) => {
        if (cancelled) return;
        setInstrSnapshot(requestedTeamId ? { teamId: requestedTeamId, status: s } : null);
        setInstrErrorTeamId(null);
      })
      .catch(() => {
        if (!cancelled) setInstrErrorTeamId(requestedTeamId);
      });
    return () => {
      cancelled = true;
    };
  }, [instrTeamId, team?.lastVersion, instrRetry]);
  // Set while an approval-gated join waits for an admin. Holds the connect inputs so a poll
  // uses the values from when the request was made, not whatever the fields say later.
  const [pending, setPending] = useState<{
    serverUrl: string;
    requestToken: string;
    memberName?: string;
  } | null>(null);

  // Local commands, LAN addresses and changed remote definitions require review
  // below; link-local/metadata URLs are blocked outright. The
  // backend emits the counts so the state is explained, not a silent mystery.
  useEffect(() => {
    const un = listen<{ review: number; blocked: number }>("team-servers-review", (e) => {
      const { review, blocked } = e.payload;
      const parts: string[] = [];
      if (review > 0)
        parts.push(
          `${review} team server${review === 1 ? "" : "s"} ${review === 1 ? "is" : "are"} off until you review and enable ${review === 1 ? "it" : "them"} below. Check the command, address and authentication before enabling.`,
        );
      if (blocked > 0)
        parts.push(
          `${blocked} ${blocked === 1 ? "was" : "were"} blocked as unsafe (link-local or cloud-metadata URLs).`,
        );
      setSkipNote(parts.join(" "));
    });
    return () => {
      un.then((f) => f());
    };
  }, []);

  async function run(label: string, fn: () => Promise<void>) {
    setBusy(label);
    setError(null);
    setNotice(null);
    setSkipNote(null);
    try {
      await fn();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(null);
    }
  }

  const onConnect = () =>
    run("connect", async () => {
      const urlError = teamUrlError(serverUrl);
      if (urlError) {
        throw new Error(urlError);
      }
      if (!inviteCode.trim()) {
        throw new Error("An invite or connect code is required.");
      }
      const su = serverUrl.trim();
      const mn = memberName.trim() || undefined;
      const r = await teamConnect(su, inviteCode.trim(), mn);
      if (r.status === "pending" && r.requestToken) {
        // An approval-gated link: nothing is joined yet. Hold here and poll (below) until an
        // admin approves or denies; the fields stay as-is so the request context is preserved.
        setPending({ serverUrl: su, requestToken: r.requestToken, memberName: mn });
        setNotice("Request sent. Waiting for an admin to approve you.");
        return;
      }
      if (r.status === "connected" && r.registry) {
        onRegistryChange(r.registry);
        setInviteCode("");
        setNotice("Connected. The team's servers were added to Toolport.");
        return;
      }
      throw new Error("The server returned an unexpected connect response.");
    });

  // While a join is pending admin approval, poll for the verdict. A transient network error
  // keeps the wait alive (a blip shouldn't cancel it); an explicit deny/expiry ends it.
  useEffect(() => {
    if (!pending) return;
    let cancelled = false;
    const tick = async () => {
      try {
        const r = await teamJoinPoll(
          pending.serverUrl,
          pending.requestToken,
          pending.memberName,
        );
        if (cancelled) return;
        if (r.status === "connected" && r.registry) {
          setPending(null);
          onRegistryChange(r.registry);
          setInviteCode("");
          setNotice("Approved. The team's servers were added to Toolport.");
        } else if (r.status === "denied") {
          setPending(null);
          setError("An admin declined your request to join this team.");
        } else if (r.status === "unknown") {
          setPending(null);
          setError("This join request expired. Ask for the link again and reconnect.");
        }
        // "pending" → keep waiting.
      } catch {
        // Transient: leave `pending` set so the next tick retries.
      }
    };
    void tick();
    const id = setInterval(tick, 4000);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pending]);

  const onSync = () =>
    run("sync", async () => {
      onRegistryChange(await teamSync());
      setNotice("Synced with the team.");
    });

  const onDisconnect = () =>
    run("disconnect", async () => {
      onRegistryChange(await teamDisconnect());
      setNotice("Left the team. Its servers were removed; your own are untouched.");
    });

  // The same local-only hint the GTK picker shows, from the last sync.
  const shareHint = (server: (typeof personalServers)[number]) => {
    const copies = teamServers.filter(
      (s) => team?.managedServerIds?.[s.id] === server.id,
    );
    if (copies.length === 1)
      return registry && isEnabled(registry, copies[0].id)
        ? "Shared. The Team copy is in use in this profile."
        : "Shared. The Team copy is not in use in this profile.";
    const name = server.name.trim().toLowerCase();
    return teamServers.some((s) => s.name.trim().toLowerCase() === name)
      ? "The team has a different server with this name. Sharing adds a separate definition."
      : null;
  };

  const onPreviewPush = () =>
    run("preview-push", async () => {
      setPushPreview(await teamPushPreview(selectedIds));
    });

  const onPush = () =>
    run("push", async () => {
      if (!pushPreview) throw new Error("Review the shared-server update before saving.");
      const v = await teamPush(pushPreview, selectedIds);
      setPushPreview(null);
      setProposal(
        (v as typeof v & { proposal?: { confirmUrl: string } }).proposal ?? null,
      );
      // The summary names each selection and which route is on in this profile.
      if (v.localSetupError || v.handoffs.some((h) => h.outcome === "attention"))
        setSkipNote(v.summary);
      else setNotice(v.summary);
      onRegistryChange(await getRegistry());
    });

  const onReview = (change: MemberChange, accept: boolean) =>
    run("review", async () => {
      onRegistryChange(
        await invoke<Registry>("team_review", {
          key: change.key,
          hash: change.hash,
          accept,
        }),
      );
      setNotice(
        accept
          ? "Accepted the reviewed change."
          : "Rejected. This content stays held across syncs.",
      );
    });

  // Member consent: enable a review server (local command / LAN URL) into the active
  // profile after the confirm. Nothing from a team runs until this explicit opt-in.
  const onEnable = (serverId: string) =>
    run("enable", async () => {
      const pid = registry ? activeProfile(registry)?.id : undefined;
      if (!pid) throw new Error("Access context unavailable.");
      // reviewed=true: this runs only from the ConfirmDialog below, which showed
      // the member the exact command/URL. The backend refuses without it.
      onRegistryChange(await setServerEnabled(pid, serverId, true, true));
      setNotice("Enabled. That server is now on.");
    });

  // One row in the Shared-servers list. Extracted so the review and active groups
  // below can each render it.
  const renderTeamServer = (s: (typeof teamServers)[number]) => {
    const personal = personalServers.find((p) => p.id === team?.managedServerIds?.[s.id]);
    const on = registry ? isEnabled(registry, s.id) : false;
    const held = (s as typeof s & { teamHeldChange?: boolean }).teamHeldChange === true;
    const isLocal = s.transport === "stdio" || !!s.command;
    const detail = s.command
      ? `Command: ${s.command}\nArguments: ${JSON.stringify(s.args ?? [])}\nWorking directory: ${s.cwd ?? "Inherit from client"}\nCredentials required: ${s.env.map((e) => e.key).join(", ") || "None declared"}`
      : `URL: ${s.url ?? ""}\nCredentials required: ${s.env.map((e) => e.key).join(", ") || "None declared"}`;
    return (
      <li
        key={s.id}
        className={`rounded-lg border px-3 py-2 text-sm ${on ? "border-border/60" : "border-warning/40 bg-warning/5"}`}
      >
        <div className="flex items-center gap-2">
          <Server className="size-3.5 shrink-0 text-muted-foreground" />
          <span className="truncate font-medium">
            {team?.teamName ?? "Team"}-managed {s.name}
          </span>
          {on ? (
            <Badge variant="success" className="ml-auto shrink-0">
              <ShieldCheck className="size-3" /> on
            </Badge>
          ) : (
            <Badge variant="warning" className="ml-auto shrink-0">
              <AlertTriangle className="size-3" /> needs review
            </Badge>
          )}
        </div>
        {!held && personal && registry && isEnabled(registry, personal.id) && (
          <ConfirmDialog
            trigger={
              <Button size="sm" variant="outline" disabled={busy !== null}>
                Use team-managed version for this profile
              </Button>
            }
            title={`Use team-managed ${s.name}?`}
            description={
              <div className="whitespace-pre-wrap">
                {detail}
                {"\n\n"}This enables the managed copy and disables Personal{" "}
                {personal.name} in this profile. The personal definition stays saved.
                Existing credentials and sign-in are reused locally only when the
                definitions match exactly. Signing out affects both copies.
              </div>
            }
            confirmLabel="Use managed version"
            onConfirm={() =>
              run("managed", async () => onRegistryChange(await teamUseManaged(s.id)))
            }
          />
        )}
        {!on && held && (
          <p className="mt-2 text-xs text-muted-foreground">
            This change is held off. Use the team change review above.
          </p>
        )}
        {!on && !held && (
          <div className="mt-2 flex items-end justify-between gap-3">
            <div className="min-w-0">
              <p className="text-xs text-muted-foreground">
                {isLocal
                  ? "Runs this local command on your machine:"
                  : "Connects to this address:"}
              </p>
              <code className="block whitespace-pre-wrap break-all font-mono text-xs text-foreground">
                {detail}
              </code>
            </div>
            <ConfirmDialog
              trigger={
                <Button
                  size="sm"
                  variant="outline"
                  disabled={busy !== null}
                  className="shrink-0"
                >
                  Enable
                </Button>
              }
              title={`Enable "${s.name}"?`}
              description={
                isLocal
                  ? `This runs a local command on your machine: ${detail}. Only enable it if you trust your team and recognize this command.`
                  : `This connects Toolport to ${detail} using this server's saved authentication. Verify the address and credentials before enabling it.`
              }
              confirmLabel="Enable"
              onConfirm={() => onEnable(s.id)}
            />
          </div>
        )}
      </li>
    );
  };

  // `registry` is null until the first `getRegistry()` resolves, and stays null for the
  // rest of the session if that call fails. `registry?.team ?? null` folds that state into
  // "no team", which would show a member of a team the sales pitch for the team they are
  // already in — the one audience this page must never pitch to. Nothing below can tell
  // the two apart, so answer the question here: not loaded yet is neither state.
  if (registry === null) {
    return (
      <div className="mx-auto max-w-2xl">
        <div className="mb-5 flex items-center gap-2">
          <Users className="size-5 text-muted-foreground" />
          <h2 className="text-base font-semibold">Toolport Teams</h2>
        </div>
        <div className="flex flex-col gap-2" aria-label="Loading Toolport Teams">
          {Array.from({ length: 4 }).map((_, i) => (
            <Skeleton key={i} className="h-14 w-full rounded-lg" />
          ))}
        </div>
      </div>
    );
  }

  return (
    // The connected view is a single column of cards and stays narrow. The
    // disconnected one runs two lanes side by side, which needs the extra width to
    // keep the connect form's fields from turning into a column of stubs.
    <div className={team ? "mx-auto max-w-2xl" : "mx-auto max-w-4xl"}>
      <div className="mb-5 flex items-center gap-2">
        <Users className="size-5 text-muted-foreground" />
        <h2 className="text-base font-semibold">Toolport Teams</h2>
      </div>

      {error && (
        <Callout variant="danger" className="mb-4">
          {error}
        </Callout>
      )}
      {skipNote && (
        <Callout variant="warning" className="mb-4 whitespace-pre-line">
          {skipNote}
        </Callout>
      )}
      {notice && (
        <Callout variant="success" className="mb-4 whitespace-pre-line">
          {notice}
        </Callout>
      )}
      {proposal && (
        <Button
          variant="outline"
          onClick={() =>
            void run("confirmation", async () => {
              await invoke("team_open_confirmation", { url: proposal.confirmUrl });
            })
          }
          disabled={busy !== null}
        >
          Open confirmation
        </Button>
      )}
      {changes.length > 0 && (
        <Callout variant="warning" className="mb-4">
          <p>
            {changes.length} team change{changes.length === 1 ? " is" : "s are"} waiting
            for your review. Held servers stay off and instructions stay unchanged. Safety
            floors can tighten immediately.
          </p>
          <Button variant="outline" className="mt-2" onClick={() => setReviewOpen(true)}>
            Review team changes
          </Button>
        </Callout>
      )}
      <Dialog open={reviewOpen} onOpenChange={setReviewOpen}>
        <DialogContent className="sm:max-w-2xl">
          <DialogHeader>
            <DialogTitle>Review team changes</DialogTitle>
            <DialogDescription>
              Accept only the content you recognize. Each decision applies to this member
              and the exact change shown.
            </DialogDescription>
          </DialogHeader>
          <div className="max-h-[65vh] overflow-y-auto space-y-4">
            {changes.length === 0 && <p>No changes waiting for review.</p>}
            {changes.map((change) => (
              <section
                key={change.key}
                className="rounded-lg border p-3 space-y-2"
                aria-label={change.title}
              >
                <h3 className="font-medium">{change.title}</h3>
                {change.labels.length === 0 ? (
                  <p className="text-xs text-muted-foreground">
                    Full diff from your accepted configuration. Change history labels
                    unavailable.
                  </p>
                ) : (
                  change.labels.map((label, index) => (
                    <p key={index} className="text-xs text-muted-foreground">
                      {label.author?.name ?? "Unknown author"} ·{" "}
                      {label.at ? new Date(label.at).toLocaleString() : "Unknown time"} ·
                      via {label.via ?? "unknown"}
                      {label.approvedBy?.name
                        ? ` · approved by ${label.approvedBy.name}`
                        : ""}
                    </p>
                  ))
                )}
                <dl className="space-y-2 text-sm">
                  {change.fields.map((field) => (
                    <div key={field.field}>
                      <dt className="font-medium">{field.field}</dt>
                      <dd className="whitespace-pre-wrap break-words rounded bg-muted p-2">
                        <span className="text-muted-foreground">Before: </span>
                        {field.before}
                        {"\n"}
                        <span className="text-muted-foreground">After: </span>
                        {field.after}
                      </dd>
                    </div>
                  ))}
                </dl>
                <div className="flex gap-2">
                  <Button
                    size="sm"
                    onClick={() => void onReview(change, true)}
                    disabled={busy !== null}
                  >
                    Accept
                  </Button>
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={() => void onReview(change, false)}
                    disabled={busy !== null}
                  >
                    Reject
                  </Button>
                </div>
              </section>
            ))}
          </div>
        </DialogContent>
      </Dialog>

      {!team ? (
        <div className="grid gap-4">
          <div>
            <h3 className="text-sm font-medium">
              Everyone gets the same MCP servers. Nobody shares a key.
            </h3>
            <p className="mt-1 max-w-prose text-sm text-muted-foreground">
              A Toolport Teams server holds the shared server list and its config. Each
              person still vaults their own credentials on their own machine.
            </p>
          </div>

          {/* Two lanes, and the order matters. Someone who opened this tab holding an
              invite code is the one conversion this page already has, so the form is
              first in the DOM and first on screen at every width; the pitch sits beside
              it, never above it. Below `lg` the lanes stack and the form stays first. */}
          <div className="grid gap-4 lg:grid-cols-5 lg:items-start">
            <div
              className={`rounded-xl border bg-card p-5 ${pending ? "lg:col-span-5" : "lg:col-span-3"}`}
            >
              <h3 className="text-sm font-medium">Have an invite or connect code?</h3>
              <p className="mt-1 mb-4 text-sm text-muted-foreground">
                Paste it here and the team's shared servers appear in Toolport, kept in
                sync as your admin updates them.
              </p>
              <div className="grid gap-3">
                <label className="grid gap-1 text-sm">
                  <span className="text-muted-foreground">Team server URL</span>
                  <Input
                    placeholder="https://toolport.yourcompany.com"
                    value={serverUrl}
                    onChange={(e) => setServerUrl(e.target.value)}
                  />
                  <span className="text-xs text-muted-foreground">
                    Defaults to hosted Toolport Teams. Self-hosting? Replace it with your
                    own server URL.
                  </span>
                </label>
                <label className="grid gap-1 text-sm">
                  <span className="text-muted-foreground">Invite or connect code</span>
                  <Input
                    placeholder="Paste your invite or connect code"
                    value={inviteCode}
                    onChange={(e) => setInviteCode(e.target.value)}
                  />
                  <span className="text-xs text-muted-foreground">
                    An invite code joins you to a team. A connect code links this device
                    to a seat you already have.
                  </span>
                </label>
                <label className="grid gap-1 text-sm">
                  <span className="text-muted-foreground">Your name (optional)</span>
                  <Input
                    placeholder="e.g. Tyler"
                    value={memberName}
                    onChange={(e) => setMemberName(e.target.value)}
                  />
                </label>
                <div>
                  {pending ? (
                    <div className="flex items-center gap-3 rounded-lg border border-primary/40 bg-primary/5 p-3 text-sm">
                      <RefreshCw className="size-4 shrink-0 animate-spin text-primary" />
                      <span className="text-muted-foreground">
                        Waiting for an admin to approve your request. Leave this open, it
                        finishes on its own once they approve.
                      </span>
                      <Button
                        variant="outline"
                        size="sm"
                        className="ml-auto shrink-0"
                        onClick={() => {
                          setPending(null);
                          setNotice(null);
                        }}
                      >
                        Cancel
                      </Button>
                    </div>
                  ) : (
                    <Button onClick={onConnect} disabled={busy !== null}>
                      {busy === "connect" ? "Connecting…" : "Connect"}
                    </Button>
                  )}
                </div>
              </div>
            </div>

            {/* Someone waiting on an admin has already picked a team, so the ask is
                answered: no "No team yet?", no prices, no second team to create beside
                the one they are joining. The lane is dropped rather than dimmed, and the
                form takes the full width while the wait lasts. */}
            {!pending && (
              <div className="rounded-xl border border-primary/25 bg-primary/[0.04] p-5 lg:col-span-2">
                <h3 className="text-sm font-medium">No team yet?</h3>
                <p className="mt-1 mb-3 text-sm text-muted-foreground">
                  Pick the MCP servers once and everyone's Claude, Cursor, and other
                  agents get the same stack.
                </p>
                <ul className="mb-3 grid gap-2">
                  {[
                    TEAMS_FREE_LINE,
                    "Hosted by us or self-hosted on your own network, same features either way.",
                    "Keys never reach the server, so there is no shared secret to rotate.",
                  ].map((point) => (
                    <li
                      key={point}
                      className="flex gap-2 text-2xs leading-relaxed text-muted-foreground"
                    >
                      <Check className="mt-0.5 size-3.5 shrink-0 text-primary" />
                      <span>{point}</span>
                    </li>
                  ))}
                </ul>
                <p className="mb-4 text-2xs leading-relaxed text-muted-foreground">
                  {TEAMS_PAID_LINE}
                </p>
                {/* The desktop app has no create-a-team flow, so this hands off to the
                  hosted app rather than pretending to start one here. */}
                <Button className="w-full" onClick={() => openExternal(TEAMS_CREATE_URL)}>
                  Create a free team
                  <ArrowUpRight className="size-4" />
                </Button>
                <p className="mt-2 text-2xs text-muted-foreground">
                  Opens in your browser. Google, GitHub, or an email link, no card. Team
                  features are free to try for {TEAMS_TRIAL_DAYS} days.
                </p>
                <div className="mt-4 flex flex-wrap gap-x-4 gap-y-1 border-t pt-3 text-2xs">
                  <button
                    type="button"
                    onClick={() => openExternal(TEAMS_MARKETING_URL)}
                    className="text-muted-foreground transition hover:text-foreground"
                  >
                    How it works →
                  </button>
                  <button
                    type="button"
                    onClick={() => openExternal(TEAMS_PRICING_URL)}
                    className="text-muted-foreground transition hover:text-foreground"
                  >
                    Pricing →
                  </button>
                  <button
                    type="button"
                    onClick={() => openExternal(TEAMS_SELFHOST_URL)}
                    className="text-muted-foreground transition hover:text-foreground"
                  >
                    Self-host it →
                  </button>
                </div>
              </div>
            )}
          </div>

          {/* Same reason as the lane above: these three are the argument for having a
              team at all, and it is settled once a request is out. */}
          {!pending && (
            <div className="grid gap-2.5 sm:grid-cols-3">
              {[
                {
                  icon: Server,
                  title: "New teammate, day one",
                  body: "Send one code instead of walking someone through every server by hand. Their agents come up already configured.",
                },
                {
                  icon: RefreshCw,
                  title: "No more config drift",
                  body: "Six people, six slightly different server lists, and a bug only one of them can reproduce. One shared set ends that.",
                },
                {
                  icon: ShieldCheck,
                  title: "No shared secrets",
                  body: "The team server stores config, never a credential. Every key stays in its owner's OS keychain.",
                },
              ].map(({ icon: Icon, title, body }) => (
                <div
                  key={title}
                  className="rounded-lg border border-border/60 bg-muted/20 p-3"
                >
                  <div className="flex items-center gap-1.5 text-sm font-medium">
                    <Icon className="size-3.5 text-primary" />
                    {title}
                  </div>
                  <p className="mt-1 text-2xs leading-relaxed text-muted-foreground">
                    {body}
                  </p>
                </div>
              ))}
            </div>
          )}
        </div>
      ) : (
        <div className="grid gap-4">
          <div className="rounded-xl border bg-card p-5">
            <div className="flex items-start justify-between gap-4">
              <div className="min-w-0">
                <div className="flex items-center gap-2">
                  <span className="text-sm font-medium">Connected</span>
                  <span className="rounded-full border px-2 py-0.5 text-xs text-muted-foreground capitalize">
                    {team.role}
                  </span>
                </div>
                <p className="mt-1 truncate text-sm text-muted-foreground">
                  {team.serverUrl}
                </p>
                <p className="mt-0.5 text-xs text-muted-foreground">
                  {team.teamName || `Team ${team.teamId}`} · config v
                  {team.lastVersion ?? 0} · {teamServers.length} shared{" "}
                  {teamServers.length === 1 ? "server" : "servers"}
                </p>
              </div>
              <div className="flex shrink-0 gap-2">
                {team.accountLinked !== true && (
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={busy !== null}
                    onClick={() =>
                      run("account-link", async () => {
                        await openExternal(await teamAccountLink());
                      })
                    }
                  >
                    Link portal account
                  </Button>
                )}
                <Button
                  variant="outline"
                  size="sm"
                  onClick={onSync}
                  disabled={busy !== null}
                >
                  <RefreshCw className="size-3.5" />
                  {busy === "sync" ? "Syncing…" : "Sync now"}
                </Button>
                <ConfirmDialog
                  trigger={
                    <Button variant="outline" size="sm" disabled={busy !== null}>
                      <LogOut className="size-3.5" />
                      Disconnect app
                    </Button>
                  }
                  title="Disconnect this app from the team?"
                  description="Team servers, instructions and policy are removed from this app. Your personal servers stay saved. Your Team membership and shared setup remain. Reconnect from the Teams website."
                  confirmLabel="Disconnect app"
                  destructive
                  onConfirm={onDisconnect}
                />
              </div>
            </div>

            {isAdmin && (
              <div className="mt-4 flex items-center justify-between gap-4 rounded-lg border border-dashed bg-muted/30 px-4 py-3">
                <div className="min-w-0 text-sm">
                  <div className="flex items-center gap-1.5 font-medium">
                    <ShieldCheck className="size-3.5 text-success" /> Admin
                  </div>
                  <p className="text-xs text-muted-foreground">
                    Select one working personal server to begin. Unrelated team servers
                    and your personal originals stay in place. Each member supplies
                    credentials locally.
                  </p>
                </div>
                <div className="grid gap-2">
                  {personalServers.map((server) => (
                    <label key={server.id} className="flex items-center gap-2 text-sm">
                      <input
                        type="checkbox"
                        checked={selectedIds.includes(server.id)}
                        disabled={busy !== null || pushPreview !== null}
                        onChange={(e) =>
                          setSelectedIds((ids) =>
                            e.target.checked
                              ? [...ids, server.id]
                              : ids.filter((id) => id !== server.id),
                          )
                        }
                      />
                      {server.name}{" "}
                      <span className="text-xs text-muted-foreground">
                        {server.env.length
                          ? `Local credentials: ${server.env.map((e) => e.key).join(", ")}`
                          : "No environment credentials declared"}
                        {shareHint(server) && (
                          <span className="block">{shareHint(server)}</span>
                        )}
                      </span>
                    </label>
                  ))}
                  {!personalServers.length && (
                    <p>Add a working personal server in Servers first.</p>
                  )}
                </div>
                <Button
                  size="sm"
                  onClick={onPreviewPush}
                  disabled={busy !== null || selectedIds.length === 0}
                >
                  <Upload className="size-3.5" />
                  {busy === "preview-push" ? "Comparing…" : "Share selected servers"}
                </Button>
                <ConfirmDialog
                  open={pushPreview !== null}
                  onOpenChange={(open) => {
                    if (!open) setPushPreview(null);
                  }}
                  title="Share selected servers with your team?"
                  contentClassName="sm:max-w-lg"
                  description={pushPreview && <TeamSharePreview preview={pushPreview} />}
                  confirmLabel={
                    (pushPreview && teamShareAction(pushPreview)) ?? "Share selected"
                  }
                  confirmDisabled={pushPreview !== null && !teamShareAction(pushPreview)}
                  onConfirm={onPush}
                />
              </div>
            )}
          </div>

          <div className="rounded-xl border bg-card p-5">
            <h3 className="mb-1 text-sm font-medium">Shared servers</h3>
            {teamServers.length === 0 ? (
              <p className="text-sm text-muted-foreground">
                No servers from the team yet. An admin pushes the set, then Sync brings it
                here.
              </p>
            ) : (
              <>
                {(() => {
                  // Attention-needed servers first (their own section), then the rest, each
                  // sorted alphabetically so the list is predictable to scan.
                  const byName = (a: (typeof teamServers)[number], b: typeof a) =>
                    a.name.localeCompare(b.name, undefined, { sensitivity: "base" });
                  const review = teamServers
                    .filter((s) => !(registry ? isEnabled(registry, s.id) : false))
                    .sort(byName);
                  const active = teamServers
                    .filter((s) => registry && isEnabled(registry, s.id))
                    .sort(byName);
                  return (
                    <>
                      {review.length > 0 && (
                        <div className="mt-3">
                          <div className="flex items-center gap-1.5 text-xs font-medium text-warning">
                            <AlertTriangle className="size-3.5" /> Needs review (
                            {review.length})
                          </div>
                          <p className="mt-1 mb-2 text-xs text-muted-foreground">
                            Review each server's command, address and authentication
                            before enabling it. Changed remote servers require a new
                            review too.
                          </p>
                          <ul className="grid gap-2">{review.map(renderTeamServer)}</ul>
                        </div>
                      )}
                      {active.length > 0 && (
                        <div className="mt-4">
                          <div className="flex items-center gap-1.5 text-xs font-medium text-success">
                            <ShieldCheck className="size-3.5" /> Active ({active.length})
                          </div>
                          <ul className="mt-2 grid gap-2">
                            {active.map(renderTeamServer)}
                          </ul>
                        </div>
                      )}
                    </>
                  );
                })()}
                <p className="mt-3 text-xs text-muted-foreground">
                  Add each server's secrets in the Servers tab, they stay in your OS
                  keychain.
                </p>
              </>
            )}
          </div>

          {(instr || instrError) && (
            <div className="rounded-xl border bg-card p-5">
              <div className="mb-1 flex items-center gap-2">
                <FileText className="size-4 text-muted-foreground" />
                <h3 className="text-sm font-medium">Team instructions</h3>
                {instr && (
                  <span className="text-xs text-muted-foreground">v{instr.version}</span>
                )}
              </div>
              {instr ? (
                <>
                  <p className="mb-3 text-xs text-muted-foreground">
                    Org-managed agent rules, written to your AI clients alongside — never
                    over — your own instructions. Leaving the team removes them.
                  </p>
                  {instrError && (
                    <Callout
                      variant="warning"
                      role="status"
                      className="mb-3 flex items-center justify-between gap-3"
                    >
                      <span>Couldn't refresh this status. Showing the last result.</span>
                      <Button
                        variant="outline"
                        size="sm"
                        className="shrink-0"
                        onClick={() => setInstrRetry((n) => n + 1)}
                      >
                        <RefreshCw className="size-3.5" />
                        Try again
                      </Button>
                    </Callout>
                  )}
                  <pre className="mb-3 max-h-40 overflow-auto whitespace-pre-wrap rounded-lg border bg-muted/20 p-3 font-mono text-xs text-foreground">
                    {instr.content}
                  </pre>
                  {instr.clients.length === 0 ? (
                    <p className="text-xs text-muted-foreground">
                      No supported AI clients detected on this machine.
                    </p>
                  ) : (
                    <ul className="grid gap-1.5">
                      {instr.clients.map((c) => (
                        <li
                          key={c.id}
                          className="flex items-center justify-between gap-3 text-sm"
                        >
                          <span className="truncate">{c.name}</span>
                          <RuleStateBadge state={c.state} />
                        </li>
                      ))}
                    </ul>
                  )}
                </>
              ) : (
                <Callout
                  variant="danger"
                  role="status"
                  className="mt-3 flex items-center justify-between gap-3"
                >
                  <span>Toolport couldn't load the instructions status.</span>
                  <Button
                    variant="outline"
                    size="sm"
                    className="shrink-0"
                    onClick={() => setInstrRetry((n) => n + 1)}
                  >
                    <RefreshCw className="size-3.5" />
                    Try again
                  </Button>
                </Callout>
              )}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
