import { LANGUAGES, type LanguageChoice, useI18n, translate } from "@/lib/i18n";
import { useCallback, useEffect, useMemo, useState, useRef } from "react";
import {
  Activity,
  Bot,
  Braces,
  Check,
  ChevronRight,
  Copy,
  Eye,
  EyeOff,
  FolderOpen,
  FolderTree,
  Globe,
  Layers,
  Monitor,
  Moon,
  Pin,
  Power,
  RefreshCw,
  ShieldAlert,
  ShieldCheck,
  ShieldX,
  Sun,
  Trash2,
  UserCheck,
  X,
} from "lucide-react";
import { open as openFolderDialog } from "@tauri-apps/plugin-dialog";
import { listen } from "@tauri-apps/api/event";
import { toastError } from "@/lib/toast";
import { Button } from "@/components/ui/button";
import {
  addHttpClient,
  disableAutostart,
  enableAutostart,
  isAutostartEnabled,
  approveRoutineSuggestion,
  dismissRoutineSuggestion,
  listRoutineSuggestions,
  clearInspectLog,
  httpBridgeStatus,
  listAllowedTools,
  listQuarantined,
  listServerTools,
  setProfileServerTools,
  releaseQuarantine,
  revokeAllowedTool,
  removeHttpClient,
  setAllowAgentControl,
  setAllowRoutineWrites,
  setConfirmDestructive,
  setDenyDestructive,
  setCodeMode,
  setHumanApproval,
  setLazyDiscovery,
  setFolderProfiles,
  setLiveInspect,
  setBlockOnInjection,
  setPiiRedaction,
  setQuarantineOnDrift,
  setToolPinned,
  startHttpBridge,
  stopHttpBridge,
  stopStaleGateways,
  clientsNeedingRestart,
  type ClientNeedingRestart,
  type HttpBridgeStatus,
  type QuarantinedTool,
  type ReapOutcome,
} from "@/lib/api";
import type {
  AllowedTool,
  FolderProfile,
  Profile,
  Registry,
  RoutineSuggestion,
} from "@/lib/types";
import { isGatewayServer } from "@/lib/types";
import { useTheme, type Theme } from "@/lib/theme";
import { Switch } from "@/components/ui/switch";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

/** Strong, repeated orchestration patterns the gateway queued for saving. Passive by
 * design: injection-hardened models measurably ignore save directives placed in tool
 * results (0/7 conversions, 2026-08-13), so the human converts here instead. One click
 * persists - the card carries the same disclosure the approval prompt would, so the
 * click IS the persistence authorization and no second prompt fires. */
function RoutineSuggestions() {
  const { t } = useI18n();
  const [suggestions, setSuggestions] = useState<RoutineSuggestion[]>([]);
  const [names, setNames] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState<string | null>(null);
  // SBS-879: a failed invoke is not an empty queue. Keep last-known cards and
  // surface retry; only a successful read of [] hides the section. "error" is a load
  // that never succeeded (nothing to show); "stale" is a failed refresh on top of a
  // queue we already have (keep the cards, say the list may be out of date).
  const [status, setStatus] = useState<"loading" | "ready" | "error" | "stale">(
    "loading",
  );
  const [refreshing, setRefreshing] = useState(false);
  // Mount, Retry and the "routine-suggestion" listener all call refresh(), so their
  // invokes can overlap. Same latest-wins guard loadTools uses below: a slow failure
  // must never overwrite a newer success (or vice versa).
  const requestId = useRef(0);

  const refresh = useCallback(async () => {
    const id = requestId.current + 1;
    requestId.current = id;
    setRefreshing(true);
    try {
      const next = await listRoutineSuggestions();
      if (requestId.current !== id) return;
      setSuggestions(next);
      setStatus("ready");
    } catch {
      if (requestId.current !== id) return;
      // Only a read that never succeeded is a hard error; anything after one is stale.
      setStatus((prev) => (prev === "ready" || prev === "stale" ? "stale" : "error"));
    } finally {
      if (requestId.current === id) setRefreshing(false);
    }
  }, []);

  /** Drop a suggestion the broker already consumed. Refreshing alone is not enough:
   * if the follow-up list fails we keep the previous queue, which would leave a ghost
   * card whose Save now fails with "no queued suggestion with that fingerprint". */
  const forget = useCallback((fingerprint: string) => {
    // Discard any list started before the broker dropped this fingerprint; it would
    // resurrect the card. The refresh that follows this call carries the newer id.
    requestId.current += 1;
    setSuggestions((prev) => prev.filter((s) => s.definitionFingerprint !== fingerprint));
    setNames((prev) => {
      if (!(fingerprint in prev)) return prev;
      const next = { ...prev };
      delete next[fingerprint];
      return next;
    });
  }, []);

  useEffect(() => {
    void refresh();
    // Event bridge may be absent (tests, plain-browser dev); polling refresh on
    // mount already covered the initial state, so a failed subscribe is benign.
    const unlisten = listen("routine-suggestion", () => void refresh()).catch(
      () => () => {},
    );
    return () => {
      void unlisten.then((stop) => stop());
    };
  }, [refresh]);

  async function save(suggestion: RoutineSuggestion) {
    const fingerprint = suggestion.definitionFingerprint;
    setBusy(fingerprint);
    try {
      await approveRoutineSuggestion(
        fingerprint,
        names[fingerprint]?.trim() || suggestion.suggestedName,
      );
      forget(fingerprint);
      await refresh();
    } catch (e) {
      toastError(translate("Couldn't save the routine: {value1}", { value1: String(e) }));
    } finally {
      setBusy(null);
    }
  }

  async function dismiss(fingerprint: string) {
    setBusy(fingerprint);
    try {
      await dismissRoutineSuggestion(fingerprint);
      forget(fingerprint);
      await refresh();
    } catch (e) {
      toastError(
        translate("Couldn't dismiss the suggestion: {value1}", { value1: String(e) }),
      );
    } finally {
      setBusy(null);
    }
  }

  if (status === "loading") return null;
  if (suggestions.length === 0) {
    // A successful empty read, or a queue we just drained ourselves, hides the section.
    // Only a load that never succeeded has something to report.
    if (status !== "error") return null;
    return (
      <div className="flex items-center gap-2 rounded-lg border border-border/60 bg-muted/20 p-3 text-xs text-muted-foreground">
        <Braces className="size-3.5 shrink-0 text-warning" />
        <span>{t("Couldn't load suggested routines.")}</span>
        <Button
          size="sm"
          variant="ghost"
          className="ml-auto gap-1.5"
          disabled={refreshing}
          onClick={() => void refresh()}
        >
          <RefreshCw className="size-3.5" /> {t("Retry")}{" "}
        </Button>
      </div>
    );
  }
  return (
    <div className="rounded-lg border border-border/60 bg-muted/20 p-3">
      <div className="flex items-center gap-2 text-xs">
        <Braces className="size-3.5 shrink-0 text-warning" />
        <span className="font-medium">{t("Suggested routines")}</span>
        <span className="rounded-full bg-muted px-1.5 py-0.5 text-muted-foreground">
          {suggestions.length}
        </span>
        <span className="ml-auto text-muted-foreground">
          {" "}
          {t(
            "repeated patterns Toolport verified; saving advertises them to every client",
          )}{" "}
        </span>
      </div>
      {status === "stale" && (
        <div className="mt-2 flex items-center gap-2 text-xs text-muted-foreground">
          <span>
            {" "}
            {t(
              "Couldn't refresh suggested routines; showing the last loaded queue.",
            )}{" "}
          </span>
          <Button
            size="sm"
            variant="ghost"
            className="ml-auto gap-1.5"
            disabled={refreshing}
            onClick={() => void refresh()}
          >
            <RefreshCw className="size-3.5" /> {t("Retry")}{" "}
          </Button>
        </div>
      )}
      <ul className="mt-2 space-y-2">
        {suggestions.map((suggestion) => {
          const fingerprint = suggestion.definitionFingerprint;
          const synthesized =
            suggestion.evidence.provenance === "synthesized_from_observed_calls";
          return (
            <li
              key={fingerprint}
              className="space-y-2 rounded-md border border-border/60 bg-background/60 p-2.5 text-sm"
            >
              <input
                aria-label={t("Routine name")}
                className="w-full rounded border border-border/60 bg-background px-2 py-1 text-sm font-medium"
                value={names[fingerprint] ?? suggestion.suggestedName}
                onChange={(e) =>
                  setNames((prev) => ({ ...prev, [fingerprint]: e.target.value }))
                }
              />
              {synthesized && (
                <div className="rounded border border-warning/40 bg-warning/10 px-2 py-1 text-xs leading-relaxed text-warning">
                  {" "}
                  {t(
                    "Synthesized by Toolport from observed direct calls: every listed call really ran, but the surrounding script was generated and statically validated, not yet executed.",
                  )}{" "}
                </div>
              )}
              <div className="flex flex-wrap gap-x-3 gap-y-1 text-xs text-muted-foreground">
                <span>
                  {t("Risk:")} {suggestion.evidence.riskClass}
                </span>
                <span>
                  {t("Calls:")} {suggestion.evidence.calls}
                </span>
                <span>
                  {" "}
                  {t("Dependencies:")}{" "}
                  {suggestion.evidence.observedDependencies
                    .map((dependency) => dependency.name)
                    .join(", ")}
                </span>
                <span>
                  ~{Math.max(1, Math.round(suggestion.intermediateBytes / 1024))}{" "}
                  {t("KB/run not returned by Toolport")}{" "}
                </span>
              </div>
              <details>
                <summary className="cursor-pointer text-xs font-medium text-muted-foreground hover:text-foreground">
                  {" "}
                  {t("Source and schema")}{" "}
                </summary>
                <pre className="mt-2 max-h-36 overflow-auto rounded border border-border/60 bg-background/60 p-2 font-mono text-[0.7rem] leading-relaxed">
                  {suggestion.source}
                  {"\n\n"}
                  {JSON.stringify(suggestion.inputSchema, null, 2)}
                </pre>
              </details>
              <div className="flex justify-end gap-2">
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={busy === fingerprint}
                  onClick={() => void dismiss(fingerprint)}
                >
                  {" "}
                  {t("Dismiss")}{" "}
                </Button>
                <Button
                  size="sm"
                  disabled={busy === fingerprint}
                  onClick={() => void save(suggestion)}
                >
                  {" "}
                  {t("Save routine")}{" "}
                </Button>
              </div>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

/** The set of tools pinned as lazy-discovery prerequisites, with one-click unpin.
 * Pinning happens contextually (a tool's card in Playground); this is where you see
 * and manage the whole set. Reads `registry.pinnedTools` (server id -> tool names). */
function PinnedPrerequisites({
  registry,
  onRegistryChange,
}: {
  registry: Registry | null;
  onRegistryChange: (registry: Registry) => void;
}) {
  const { t } = useI18n();
  const [busy, setBusy] = useState<string | null>(null);
  const nameOf = useMemo(() => {
    const m = new Map<string, string>();
    for (const s of registry?.servers ?? []) m.set(s.id, s.name);
    return m;
  }, [registry?.servers]);
  const pins = useMemo(() => {
    const out: { serverId: string; server: string; tool: string }[] = [];
    for (const [serverId, tools] of Object.entries(registry?.pinnedTools ?? {})) {
      for (const tool of tools) {
        out.push({ serverId, server: nameOf.get(serverId) ?? serverId, tool });
      }
    }
    return out.sort((a, b) =>
      `${a.server}${a.tool}`.localeCompare(`${b.server}${b.tool}`),
    );
  }, [registry?.pinnedTools, nameOf]);

  async function unpin(serverId: string, tool: string) {
    setBusy(`${serverId}:${tool}`);
    try {
      onRegistryChange(await setToolPinned(serverId, tool, false));
    } catch (e) {
      toastError(translate("Couldn't unpin the tool: {value1}", { value1: String(e) }));
    } finally {
      setBusy(null);
    }
  }

  return (
    <div className="rounded-lg border border-border/60 bg-muted/20 p-3">
      <div className="flex items-center gap-2 text-xs">
        <Pin className="size-3.5 shrink-0 text-info" />
        <span className="font-medium">{t("Pinned prerequisites")}</span>
        <span className="rounded-full bg-muted px-1.5 py-0.5 text-muted-foreground">
          {pins.length}
        </span>
        <span className="ml-auto text-muted-foreground">
          {" "}
          {t("always surfaced in search, with full schema")}{" "}
        </span>
      </div>
      {pins.length === 0 ? (
        <p className="mt-2 max-w-2xl text-xs text-muted-foreground">
          {" "}
          {t(
            "None yet. Pin a load-bearing tool (auth, list-before-act, or one whose description doesn't match your keywords) from its card in Playground, so lazy discovery never hides it.",
          )}{" "}
        </p>
      ) : (
        <ul className="mt-2 space-y-1">
          {pins.map((p) => (
            <li
              key={`${p.serverId}:${p.tool}`}
              className="flex items-center gap-2 text-xs"
            >
              <code className="rounded bg-muted px-1.5 py-0.5 font-mono text-foreground/90">
                {p.tool}
              </code>
              <span className="text-muted-foreground">{p.server}</span>
              <button
                onClick={() => unpin(p.serverId, p.tool)}
                disabled={busy === `${p.serverId}:${p.tool}`}
                aria-label={t("Unpin {value1}", { value1: p.tool })}
                className="ml-auto rounded p-0.5 text-muted-foreground/60 transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-border disabled:opacity-50"
              >
                <X className="size-3.5" />
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

/** Folder -> profile auto-routing (SOU-188): map a project folder to a profile so a client
 * opened in that folder auto-scopes to it, no manual profile switch. Reads/writes
 * `registry.folderProfiles`; the longest matching path wins at match time. stdio clients. */
function FolderRouting({
  registry,
  onRegistryChange,
}: {
  registry: Registry | null;
  onRegistryChange: (registry: Registry) => void;
}) {
  const { t } = useI18n();
  const mappings = registry?.folderProfiles ?? [];
  const profiles = registry?.profiles ?? [];
  const [path, setPath] = useState("");
  const [profile, setProfile] = useState("");
  const [busy, setBusy] = useState(false);

  async function save(next: FolderProfile[]) {
    setBusy(true);
    try {
      onRegistryChange(await setFolderProfiles(next));
    } catch (e) {
      toastError(
        translate("Couldn't save folder routing: {value1}", { value1: String(e) }),
      );
    } finally {
      setBusy(false);
    }
  }

  async function browse() {
    try {
      const picked = await openFolderDialog({
        directory: true,
        multiple: false,
        title: translate("Choose a project folder"),
      });
      if (typeof picked === "string") setPath(picked);
    } catch (e) {
      toastError(
        translate("Couldn't open the folder picker: {value1}", { value1: String(e) }),
      );
    }
  }

  async function add() {
    const p = path.trim();
    if (!p || !profile) return;
    // Replace any existing mapping for the same path rather than duplicating it.
    const next = [...mappings.filter((m) => m.path.trim() !== p), { path: p, profile }];
    await save(next);
    setPath("");
    setProfile("");
  }

  // A mapping's profile is stored as an id or name; show the profile's display name.
  const label = (ref: string) =>
    profiles.find((p) => p.id === ref || p.name === ref)?.name ?? ref;

  return (
    <div className="rounded-lg border border-border/60 bg-muted/20 p-3">
      <div className="flex items-center gap-2 text-xs">
        <FolderTree className="size-3.5 shrink-0 text-info" />
        <span className="font-medium">{t("Project folder routing")}</span>
        <span className="rounded-full bg-muted px-1.5 py-0.5 text-muted-foreground">
          {mappings.length}
        </span>
        <span className="ml-auto text-muted-foreground">
          {" "}
          {t("auto-scope a client by the folder it opens in")}{" "}
        </span>
      </div>
      {mappings.length > 0 && (
        <ul className="mt-2 space-y-1">
          {mappings.map((m) => (
            <li key={m.path} className="flex items-center gap-2 text-xs">
              <code className="truncate rounded bg-muted px-1.5 py-0.5 font-mono text-foreground/90">
                {m.path}
              </code>
              <span className="shrink-0 text-muted-foreground">&rarr;</span>
              <span className="shrink-0 text-foreground/90">{label(m.profile)}</span>
              <button
                onClick={() => save(mappings.filter((x) => x.path !== m.path))}
                disabled={busy}
                aria-label={t("Remove routing for {value1}", { value1: m.path })}
                className="ml-auto rounded p-0.5 text-muted-foreground/60 transition-colors hover:bg-muted hover:text-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-border disabled:opacity-50"
              >
                <X className="size-3.5" />
              </button>
            </li>
          ))}
        </ul>
      )}
      {profiles.length === 0 ? (
        <p className="mt-2 text-xs text-muted-foreground">
          {" "}
          {t("Create a profile first, then map a folder to it here.")}{" "}
        </p>
      ) : (
        <div className="mt-2 flex items-center gap-2">
          <input
            value={path}
            onChange={(e) => setPath(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") void add();
            }}
            placeholder="/path/to/project"
            className="min-w-0 flex-1 rounded border bg-background px-2 py-1 font-mono text-xs focus-visible:ring-1 focus-visible:ring-border focus-visible:outline-none"
          />
          <button
            type="button"
            onClick={() => void browse()}
            disabled={busy}
            aria-label={t("Browse for a project folder")}
            title={t("Browse…")}
            className="flex shrink-0 items-center gap-1 rounded border px-2 py-1 text-xs font-medium hover:bg-muted disabled:opacity-50"
          >
            <FolderOpen className="size-3.5" /> {t("Browse")}{" "}
          </button>
          <Select value={profile} onValueChange={setProfile}>
            <SelectTrigger className="h-7 w-32 text-xs">
              <SelectValue placeholder={t("Profile")} />
            </SelectTrigger>
            <SelectContent>
              {profiles.map((p) => (
                <SelectItem key={p.id} value={p.id}>
                  {p.name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <button
            onClick={() => void add()}
            disabled={busy || !path.trim() || !profile}
            className="shrink-0 rounded border px-2 py-1 text-xs font-medium hover:bg-muted disabled:opacity-50"
          >
            {" "}
            {t("Add")}{" "}
          </button>
        </div>
      )}
    </div>
  );
}

interface Props {
  registry: Registry | null;
  onRegistryChange: (registry: Registry) => void;
}

type SettingKey =
  | "autostart"
  | "lazy-discovery"
  | "code-mode"
  | "allow-routine-writes"
  | "deny-destructive"
  | "confirm-destructive"
  | "human-approval"
  | "quarantine-on-drift"
  | "block-on-injection"
  | "pii-redaction"
  | "allow-agent-control"
  | "live-inspect";

type RegistrySettingKey = Exclude<SettingKey, "autostart">;

const REGISTRY_FIELD_BY_SETTING = {
  "lazy-discovery": "lazyDiscovery",
  "code-mode": "codeMode",
  "allow-routine-writes": "allowRoutineWrites",
  "deny-destructive": "denyDestructive",
  "confirm-destructive": "confirmDestructive",
  "human-approval": "humanApproval",
  "quarantine-on-drift": "quarantineOnDrift",
  "block-on-injection": "blockOnInjection",
  "pii-redaction": "piiRedaction",
  "allow-agent-control": "allowAgentControl",
  "live-inspect": "liveInspect",
} as const satisfies Record<RegistrySettingKey, keyof Registry>;

/** A one-line security posture readout at the top of the Security section, so the user can
 * tell at a glance whether they're protected instead of mentally AND-ing every toggle. */
function PostureSummary({
  denyDestructive,
  confirmDestructive,
  humanApproval,
  quarantineOnDrift,
  blockOnInjection,
}: {
  denyDestructive: boolean;
  confirmDestructive: boolean;
  humanApproval: boolean;
  quarantineOnDrift: boolean;
  blockOnInjection: boolean;
}) {
  const { t } = useI18n();
  const active = [
    humanApproval && t("human approval on"),
    denyDestructive && t("destructive tools denied"),
    confirmDestructive && t("destructive calls ask first"),
    quarantineOnDrift && t("changed tools paused"),
    blockOnInjection && t("injection-like output blocked"),
  ].filter(Boolean) as string[];
  // A hard gate (block or human-approval) = guarded; softer measures alone = partial.
  const gated = humanApproval || denyDestructive || blockOnInjection;
  const state = gated ? "guarded" : active.length > 0 ? "partial" : "open";
  const meta = {
    guarded: {
      Icon: ShieldCheck,
      ring: "border-success/30 bg-success/5",
      tint: "text-success",
      label: t("Guardrails active"),
    },
    partial: {
      Icon: ShieldAlert,
      ring: "border-warning/35 bg-warning/5",
      tint: "text-warning",
      label: t("Some guardrails active"),
    },
    open: {
      Icon: ShieldAlert,
      ring: "border-warning/35 bg-warning/5",
      tint: "text-warning",
      label: t("Approval gates are off"),
    },
  }[state];
  const { Icon } = meta;
  return (
    <div className={`flex items-start gap-3 rounded-lg border p-3 ${meta.ring}`}>
      <Icon className={`mt-0.5 size-4 shrink-0 ${meta.tint}`} />
      <p className="text-sm">
        <span className={`font-medium ${meta.tint}`}>{t(meta.label)}.</span>{" "}
        <span className="text-muted-foreground">
          {state === "open"
            ? t("Tool calls run without a Toolport approval or blocking gate.")
            : t("Active: {value1}.", { value1: active.join(", ") })}
        </span>
      </p>
    </div>
  );
}

/** Tool-granular scope for one profile (SOU-189): per enabled server, expand to pick exactly
 * which tools the profile exposes. All-checked = the whole server (no narrowing); unchecking
 * writes an allow-list that tools/list, search, and the call guard all honor. Tools load
 * lazily on expand. stdio clients (the per-profile router); the HTTP bridge is a follow-up. */
function ProfileToolScope({
  profile,
  registry,
  onRegistryChange,
}: {
  profile: Profile;
  registry: Registry;
  onRegistryChange: (r: Registry) => void;
}) {
  const { t } = useI18n();
  const serverName = useMemo(() => {
    const m = new Map<string, string>();
    // Toolport's own gateway is infrastructure, not a scopable server, so keep it out.
    for (const s of registry.servers) if (!isGatewayServer(s)) m.set(s.id, s.name);
    return m;
  }, [registry.servers]);
  const [expanded, setExpanded] = useState<string | null>(null);
  const [toolsByServer, setToolsByServer] = useState<Record<string, string[]>>({});
  const [errorByServer, setErrorByServer] = useState<Record<string, boolean>>({});
  const [loadingByServer, setLoadingByServer] = useState<Record<string, boolean>>({});
  const requestIdByServer = useRef<Record<string, number>>({});
  const [busy, setBusy] = useState(false);
  const scope = profile.toolScope ?? {};

  async function loadTools(serverId: string) {
    const requestId = (requestIdByServer.current[serverId] ?? 0) + 1;
    requestIdByServer.current[serverId] = requestId;
    setLoadingByServer((m) => ({
      ...m,
      [serverId]: true,
    }));
    try {
      const tools = await listServerTools(serverId);
      if (requestIdByServer.current[serverId] !== requestId) {
        return;
      }
      setToolsByServer((m) => ({ ...m, [serverId]: tools.map((t) => t.name) }));
      setErrorByServer((m) => {
        const next = { ...m };
        delete next[serverId];
        return next;
      });
    } catch (e) {
      // Ignore stale failures (a newer load for this server is in flight or done).
      if (requestIdByServer.current[serverId] !== requestId) {
        return;
      }
      toastError(
        translate("Couldn't load {value1} tools: {value2}", {
          value1: serverName.get(serverId) ?? serverId,
          value2: String(e),
        }),
      );
      setErrorByServer((m) => ({ ...m, [serverId]: true }));
    } finally {
      if (requestIdByServer.current[serverId] === requestId) {
        setLoadingByServer((m) => ({
          ...m,
          [serverId]: false,
        }));
      }
    }
  }

  async function expand(serverId: string) {
    if (expanded === serverId) {
      setExpanded(null);
      return;
    }
    setExpanded(serverId);
    if (!toolsByServer[serverId]) {
      await loadTools(serverId);
    }
  }

  async function toggleTool(serverId: string, tool: string, allTools: string[]) {
    // Current in-scope set is the allow-list if present, else every tool.
    const current = new Set(scope[serverId] ?? allTools);
    if (current.has(tool)) current.delete(tool);
    else current.add(tool);
    // Every real tool selected -> clear the scope (whole server). Otherwise persist the
    // checked subset (which may be empty = expose no tools). `.every` (not a size compare)
    // so a stale entry from a since-removed tool can't fake an "all selected" state.
    const next = allTools.every((t) => current.has(t))
      ? null
      : allTools.filter((t) => current.has(t));
    setBusy(true);
    try {
      onRegistryChange(await setProfileServerTools(profile.id, serverId, next));
    } catch (e) {
      toastError(
        translate("Couldn't update tool scope: {value1}", { value1: String(e) }),
      );
    } finally {
      setBusy(false);
    }
  }

  const servers = profile.enabledServerIds.filter((id) => serverName.has(id));
  if (servers.length === 0) return null;

  return (
    <div className="mt-1.5 flex flex-col gap-1">
      {servers.map((serverId) => {
        const allTools = toolsByServer[serverId];
        const scoped = scope[serverId];
        const badge = scoped
          ? allTools
            ? t("{value1} of {value2} tools", {
                value1: scoped.length,
                value2: allTools.length,
              })
            : t("{value1} tools", { value1: scoped.length })
          : t("all tools");
        const open = expanded === serverId;
        return (
          <div key={serverId} className="rounded border border-border/50 bg-muted/10">
            <button
              type="button"
              onClick={() => expand(serverId)}
              aria-expanded={open}
              className="flex w-full items-center gap-2 px-2 py-1.5 text-xs hover:bg-muted/30"
            >
              <ChevronRight
                className={`size-3.5 shrink-0 transition-transform ${open ? "rotate-90" : ""}`}
              />
              <span className="font-medium">{serverName.get(serverId)}</span>
              <span
                className={`ml-auto ${scoped ? "text-info" : "text-muted-foreground"}`}
              >
                {badge}
              </span>
            </button>
            {open && (
              <div className="border-t border-border/40 px-2 py-1.5">
                {loadingByServer[serverId] ? (
                  <p className="text-xs text-muted-foreground">{t("Loading tools…")}</p>
                ) : errorByServer[serverId] ? (
                  <div
                    role="status"
                    aria-live="polite"
                    className="flex flex-col gap-2 rounded-md border border-border p-3"
                  >
                    <p className="text-xs text-muted-foreground">
                      {t("Couldn't load tools")}
                    </p>
                    <Button
                      variant="outline"
                      size="xs"
                      className="w-fit"
                      onClick={() => loadTools(serverId)}
                    >
                      {" "}
                      {t("Retry")}{" "}
                    </Button>
                  </div>
                ) : allTools && allTools.length > 0 ? (
                  <div className="flex flex-col gap-1">
                    {allTools.map((tool) => (
                      <label key={tool} className="flex items-center gap-2 text-xs">
                        <input
                          type="checkbox"
                          checked={scoped ? scoped.includes(tool) : true}
                          disabled={busy}
                          onChange={() => toggleTool(serverId, tool, allTools)}
                          className="size-3.5"
                        />
                        <code className="font-mono text-foreground/90">{tool}</code>
                      </label>
                    ))}
                  </div>
                ) : (
                  <p className="text-xs text-muted-foreground">
                    {" "}
                    {t("This server exposes no tools.")}{" "}
                  </p>
                )}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}

/** Global discovery + security policy. These apply to every client uniformly, so
 * they live here rather than in the per-server Playground. */
export function SettingsView({ registry, onRegistryChange }: Props) {
  const {
    t,
    language,
    setLanguage,
    loading: languageLoading,
    error: languageError,
  } = useI18n();
  const { theme, setTheme } = useTheme();
  const lazyDiscovery = registry?.lazyDiscovery ?? true;
  // On by default (SOU-397); only treat explicit false as off when the field is missing
  // during a partial load, match the registry serde default.
  const codeMode = registry?.codeMode ?? true;
  const allowRoutineWrites = registry?.allowRoutineWrites ?? false;
  const denyDestructive = registry?.denyDestructive ?? false;
  const confirmDestructive = registry?.confirmDestructive ?? false;
  const humanApproval = registry?.humanApproval ?? false;
  const allowAgentControl = registry?.allowAgentControl ?? false;
  const quarantineOnDrift = registry?.quarantineOnDrift ?? false;
  const blockOnInjection = registry?.blockOnInjection ?? false;
  const piiRedaction = registry?.piiRedaction ?? false;
  const liveInspect = registry?.liveInspect ?? false;
  const [busySettings, setBusySettings] = useState<ReadonlySet<SettingKey>>(
    () => new Set(),
  );
  const latestRegistry = useRef(registry);
  useEffect(() => {
    latestRegistry.current = registry;
  }, [registry]);
  // Profile cards collapse so a big Default profile doesn't dump every server (and its
  // per-server tool rows) onto the page. Collapsed by default; the comma summary still shows.
  const [openProfiles, setOpenProfiles] = useState<Set<string>>(new Set());
  const [quarantined, setQuarantined] = useState<QuarantinedTool[]>([]);
  const [quarantineError, setQuarantineError] = useState(false);
  const [allowedTools, setAllowedTools] = useState<AllowedTool[]>([]);
  const [allowedError, setAllowedError] = useState(false);
  const [bridge, setBridge] = useState<HttpBridgeStatus | null>(null);
  const [bridgeError, setBridgeError] = useState(false);
  // Mask the bridge bearer token by default (grants any local process access to all
  // the user's tools); reveal on demand.
  const [showToken, setShowToken] = useState(false);
  const [bridgeBusy, setBridgeBusy] = useState(false);
  const [copied, setCopied] = useState<string | null>(null);
  const [newLabel, setNewLabel] = useState("");
  const [newProfile, setNewProfile] = useState("");
  const [newToken, setNewToken] = useState<string | null>(null);
  const [clientBusy, setClientBusy] = useState(false);
  const [reapBusy, setReapBusy] = useState(false);
  const [reapOutcome, setReapOutcome] = useState<ReapOutcome | null>(null);
  // Apps still launching an obsolete gateway. Seeded from stored state because the
  // launch reaper already found them before this view existed; refreshed from the
  // button's own outcome, which merges rather than replaces (SOU-435).
  const [needsRestart, setNeedsRestart] = useState<ClientNeedingRestart[]>([]);
  const [restartCheckFailed, setRestartCheckFailed] = useState(false);
  const loadNeedsRestart = useCallback(async () => {
    try {
      setNeedsRestart(await clientsNeedingRestart());
      setRestartCheckFailed(false);
    } catch {
      setRestartCheckFailed(true);
      toastError(translate("Couldn't check for apps using an old gateway"));
    }
  }, []);
  useEffect(() => {
    void loadNeedsRestart();
  }, [loadNeedsRestart]);
  // Launch-at-login is OS-level state owned by the autostart plugin. Model
  // loading/ready/error explicitly so the switch is never presented as a
  // verified Off while the real OS value is unknown or unreadable (#694).
  const [autostart, setAutostart] = useState<
    { status: "loading" } | { status: "error" } | { status: "ready"; enabled: boolean }
  >({ status: "loading" });

  const httpClients = registry?.httpClients ?? [];
  const profiles = registry?.profiles ?? [];
  const serverName = useMemo(() => {
    const m = new Map<string, string>();
    // Exclude Toolport's own gateway; it's infrastructure, not a profile-scopable server.
    for (const s of registry?.servers ?? []) if (!isGatewayServer(s)) m.set(s.id, s.name);
    return m;
  }, [registry?.servers]);

  // Launch-at-login is OS-level (managed by the autostart plugin), not registry state.
  // A failed read must not be rendered as "Off": keep the switch disabled and offer
  // a retry until the OS value is actually known.
  const loadAutostart = useCallback(async () => {
    setAutostart({ status: "loading" });
    try {
      setAutostart({ status: "ready", enabled: await isAutostartEnabled() });
    } catch {
      setAutostart({ status: "error" });
    }
  }, []);

  useEffect(() => {
    void loadAutostart();
  }, [loadAutostart]);

  const toggleAutostart = async (on: boolean) => {
    setBusySettings((current) => new Set(current).add("autostart"));
    try {
      if (on) await enableAutostart();
      else await disableAutostart();
      setAutostart({ status: "ready", enabled: on });
    } catch (e) {
      toastError(
        on
          ? t("Couldn't enable launch at login: {value2}", { value2: String(e) })
          : t("Couldn't disable launch at login: {value2}", { value2: String(e) }),
      );
    } finally {
      setBusySettings((current) => {
        const next = new Set(current);
        next.delete("autostart");
        return next;
      });
    }
  };

  async function addClient() {
    if (!newLabel.trim()) return;
    setClientBusy(true);
    try {
      const res = await addHttpClient(newLabel.trim(), newProfile || undefined);
      onRegistryChange(res.registry);
      setNewToken(res.token);
      setNewLabel("");
    } catch (e) {
      toastError(translate("Couldn't add client: {value1}", { value1: String(e) }));
    } finally {
      setClientBusy(false);
    }
  }

  async function removeClient(id: string) {
    try {
      onRegistryChange(await removeHttpClient(id));
    } catch (e) {
      toastError(translate("Couldn't remove client: {value1}", { value1: String(e) }));
    }
  }

  // One-shot (not polled): the status only changes when the user toggles it here.
  // Exposed as a callback so a failed initial read can be retried explicitly, rather
  // than leaving the panel wedged until an app restart.
  const loadBridge = useCallback(() => {
    httpBridgeStatus()
      .then((s) => {
        setBridge(s);
        setBridgeError(false);
      })
      .catch(() => setBridgeError(true));
  }, []);
  useEffect(() => {
    loadBridge();
  }, [loadBridge]);

  useEffect(() => {
    const load = () =>
      listQuarantined()
        .then((q) => {
          setQuarantined(q);
          setQuarantineError(false);
        })
        // A failed poll must not read as "nothing quarantined": keep any prior
        // list and flag the error so the panel can say the status is stale.
        .catch(() => setQuarantineError(true));
    load();
    // Quarantine happens asynchronously in the gateway, so poll to keep the list fresh.
    const id = setInterval(load, 15000);
    return () => clearInterval(id);
  }, []);

  const reapprove = async (q: QuarantinedTool) => {
    try {
      await releaseQuarantine(q.profile, q.tool);
      setQuarantined(await listQuarantined());
    } catch (e) {
      toastError(t("Couldn't re-approve: {value1}", { value1: String(e) }));
    }
  };

  // Tools the user allowed to skip human approval. Polled because "always allow" is chosen
  // from the approval overlay (a different component), so this keeps the list in sync.
  useEffect(() => {
    const load = () =>
      listAllowedTools()
        .then((t) => {
          setAllowedTools(t);
          setAllowedError(false);
        })
        .catch(() => setAllowedError(true));
    load();
    const id = setInterval(load, 10000);
    return () => clearInterval(id);
  }, []);

  const revokeAllowed = async (key: string) => {
    try {
      await revokeAllowedTool(key);
      setAllowedTools(await listAllowedTools());
    } catch (e) {
      toastError(t("Couldn't revoke: {value1}", { value1: String(e) }));
    }
  };

  const toggleBridge = async (on: boolean) => {
    setBridgeBusy(true);
    try {
      setBridge(on ? await startHttpBridge() : await stopHttpBridge());
      setBridgeError(false);
    } catch (e) {
      toastError(
        on
          ? t("Couldn't start the HTTP endpoint: {value2}", { value2: String(e) })
          : t("Couldn't stop the HTTP endpoint: {value2}", { value2: String(e) }),
      );
    } finally {
      setBridgeBusy(false);
    }
  };

  const copy = (text: string, which: string) => {
    navigator.clipboard.writeText(text).then(
      () => {
        setCopied(which);
        setTimeout(() => setCopied(null), 1500);
      },
      () => {},
    );
  };

  const publishRegistrySetting = (key: RegistrySettingKey, updated: Registry) => {
    const field = REGISTRY_FIELD_BY_SETTING[key];
    const reconciled: Registry = latestRegistry.current
      ? { ...latestRegistry.current, [field]: updated[field] }
      : updated;
    latestRegistry.current = reconciled;
    onRegistryChange(reconciled);
  };

  const apply =
    (key: RegistrySettingKey, fn: (v: boolean) => Promise<Registry>) =>
    async (v: boolean) => {
      setBusySettings((current) => new Set(current).add(key));
      try {
        publishRegistrySetting(key, await fn(v));
      } catch (e) {
        toastError(t("Couldn't update the setting: {value1}", { value1: String(e) }));
      } finally {
        setBusySettings((current) => {
          const next = new Set(current);
          next.delete(key);
          return next;
        });
      }
    };

  // Live inspection needs a step the plain `apply` helper doesn't: when turned OFF,
  // clear the ephemeral capture ring so nothing lingers on disk.
  const applyLiveInspect = async (on: boolean) => {
    setBusySettings((current) => new Set(current).add("live-inspect"));
    try {
      const reg = await setLiveInspect(on);
      if (!on) await clearInspectLog();
      publishRegistrySetting("live-inspect", reg);
    } catch (e) {
      toastError(t("Couldn't update the setting: {value1}", { value1: String(e) }));
    } finally {
      setBusySettings((current) => {
        const next = new Set(current);
        next.delete("live-inspect");
        return next;
      });
    }
  };

  const toggle = (
    Icon: typeof Layers,
    on: boolean,
    accent: string,
    title: string,
    desc: string,
    onChange: (v: boolean) => void,
    settingKey: SettingKey,
    extra?: {
      switchDisabled?: boolean;
      hint?: string;
      hintTone?: "muted" | "destructive";
      onRetry?: () => void;
    },
  ) => (
    <label className="flex items-center gap-2.5 rounded-md border px-3 py-2.5 text-sm">
      <Icon className={`size-4 shrink-0 ${on ? accent : "text-muted-foreground"}`} />
      <span className="flex min-w-0 flex-1 flex-col leading-tight">
        <span className="font-medium">{title}</span>
        <span className="text-xs text-muted-foreground">{desc}</span>
        {extra?.hint && (
          <span
            className={
              extra.hintTone === "destructive"
                ? "text-xs text-destructive"
                : "text-xs text-muted-foreground"
            }
          >
            {extra.hint}
          </span>
        )}
      </span>
      <Switch
        checked={on}
        onCheckedChange={onChange}
        disabled={busySettings.has(settingKey) || extra?.switchDisabled}
      />
      {extra?.onRetry && (
        <Button size="sm" variant="ghost" className="gap-1.5" onClick={extra.onRetry}>
          <RefreshCw className="size-3.5" /> {t("Retry")}{" "}
        </Button>
      )}
    </label>
  );

  // 実行結果を保持し、言語切替では清掃を再実行せず表示だけを再生成する。
  const reapResultParts: string[] = [];
  if (reapOutcome?.killed.length) {
    reapResultParts.push(
      t("Stopped {value1}: {value2}", {
        value1: reapOutcome.killed.length,
        value2: reapOutcome.killed.join("; "),
      }),
    );
  }
  if (reapOutcome?.failed.length) {
    reapResultParts.push(
      t("Could not stop {value1}: {value2}", {
        value1: reapOutcome.failed.length,
        value2: reapOutcome.failed.join("; "),
      }),
    );
  }
  const reapResult = !reapOutcome
    ? null
    : reapResultParts.length > 0
      ? reapResultParts.join(". ")
      : reapOutcome.needsRestart.length > 0
        ? // プロセスが今なくても、次のツール呼び出しで旧版を起動するアプリは残る。
          t(
            "Nothing to stop: these apps spawn the old gateway on their next tool call, so there is no process running between calls.",
          )
        : t("No old gateway processes found.");

  return (
    <div className="mx-auto flex max-w-2xl flex-col gap-6">
      <section className="flex flex-col gap-2">
        <h2 className="text-xs font-medium tracking-wide text-muted-foreground uppercase">
          {" "}
          {t("General")}{" "}
        </h2>
        <div className="flex items-center gap-2.5 rounded-md border px-3 py-2.5 text-sm">
          <Globe className="size-4 shrink-0 text-info" />
          <label
            htmlFor="interface-language"
            className="flex min-w-0 flex-1 flex-col leading-tight"
          >
            <span className="font-medium">{t("Language")}</span>
            <span className="text-xs text-muted-foreground">
              {t("Choose the language used in Toolport.")}
            </span>
          </label>
          <select
            id="interface-language"
            value={language}
            onChange={(event) => void setLanguage(event.target.value as LanguageChoice)}
            aria-busy={languageLoading}
            className="max-w-48 rounded-md border bg-background px-2 py-1.5 text-sm"
          >
            <option value="system">{t("System language")}</option>
            {LANGUAGES.map(({ code, label }) => (
              <option key={code} value={code}>
                {label}
              </option>
            ))}
          </select>
        </div>
        {languageLoading && (
          <p role="status" className="text-xs text-muted-foreground">
            {t("Loading language…")}
          </p>
        )}
        {languageError && (
          <p role="alert" className="text-xs text-destructive">
            {t("Couldn't load this language. Reload Toolport to retry.")}
          </p>
        )}
        {toggle(
          Power,
          autostart.status === "ready" && autostart.enabled,
          "text-info",
          t("Launch at login"),
          t(
            "Start Toolport in the tray when you sign in, so it can hold tool calls for approval even before you open it",
          ),
          toggleAutostart,
          "autostart",
          {
            switchDisabled: autostart.status !== "ready",
            hint:
              autostart.status === "loading"
                ? t("Checking the current OS setting…")
                : autostart.status === "error"
                  ? t("Couldn't read the OS setting — launch-at-login state is unknown.")
                  : undefined,
            hintTone: autostart.status === "error" ? "destructive" : "muted",
            onRetry:
              autostart.status === "error" ? () => void loadAutostart() : undefined,
          },
        )}
        <div className="flex items-center gap-2.5 rounded-md border px-3 py-2.5 text-sm">
          <Sun className="size-4 shrink-0 text-info" />
          <span className="flex min-w-0 flex-1 flex-col leading-tight">
            <span className="font-medium">{t("Appearance")}</span>
            <span className="text-xs text-muted-foreground">
              {" "}
              {t("Light, dark, or follow your system setting.")}{" "}
            </span>
          </span>
          <div className="flex shrink-0 gap-0.5 rounded-md border bg-background p-0.5">
            {(
              [
                ["light", Sun, t("Light")],
                ["system", Monitor, t("System")],
                ["dark", Moon, t("Dark")],
              ] as [Theme, typeof Sun, string][]
            ).map(([value, Icon, label]) => (
              <button
                key={value}
                onClick={() => setTheme(value)}
                aria-pressed={theme === value}
                title={t(label)}
                className={`flex items-center gap-1 rounded px-2 py-1 text-xs transition-colors ${
                  theme === value
                    ? "bg-muted font-medium text-foreground"
                    : "text-muted-foreground hover:text-foreground"
                }`}
              >
                <Icon className="size-3.5" />
                {t(label)}
              </button>
            ))}
          </div>
        </div>
      </section>
      {profiles.length > 0 && (
        <section className="flex flex-col gap-2">
          <h2 className="text-xs font-medium tracking-wide text-muted-foreground uppercase">
            {" "}
            {t("Profiles")}{" "}
          </h2>
          <p className="text-xs text-muted-foreground">
            {" "}
            {t(
              'A profile is a named set of servers you can scope a client to. Expand a server to narrow it to specific tools (a "FeatureSet"); leaving every tool checked exposes the whole server. Tool narrowing applies to stdio clients (Claude Desktop, Cursor, VS Code, and the like); HTTP-bridge clients still see the whole server for now.',
            )}{" "}
          </p>
          <div className="flex flex-col divide-y rounded-lg border">
            {profiles.map((p) => {
              const names = p.enabledServerIds
                .map((id) => serverName.get(id))
                .filter((n): n is string => !!n)
                .sort((a, b) => a.localeCompare(b));
              const active = p.id === registry?.activeProfileId;
              const isOpen = openProfiles.has(p.id);
              const toggle = () =>
                setOpenProfiles((prev) => {
                  const next = new Set(prev);
                  if (next.has(p.id)) next.delete(p.id);
                  else next.add(p.id);
                  return next;
                });
              return (
                <div key={p.id} className="flex flex-col gap-1 px-3 py-2.5">
                  <button
                    type="button"
                    onClick={toggle}
                    aria-expanded={isOpen}
                    disabled={names.length === 0}
                    className="flex items-center gap-2 text-left focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-border disabled:cursor-default"
                  >
                    <ChevronRight
                      className={`size-3.5 shrink-0 text-muted-foreground transition-transform ${isOpen ? "rotate-90" : ""} ${names.length === 0 ? "invisible" : ""}`}
                    />
                    <span className="text-sm font-medium">{p.name}</span>
                    {active && (
                      <span className="rounded-full bg-info/15 px-1.5 py-0.5 text-[10px] font-medium text-info">
                        {" "}
                        {t("active")}{" "}
                      </span>
                    )}
                    <span className="ml-auto text-xs text-muted-foreground">
                      {t(names.length === 1 ? "{count} server" : "{count} servers", {
                        count: names.length,
                      })}
                    </span>
                  </button>
                  {names.length === 0 ? (
                    <p className="pl-5 text-xs text-muted-foreground italic">
                      {" "}
                      {t("No servers in this profile.")}{" "}
                    </p>
                  ) : isOpen ? (
                    registry && (
                      <ProfileToolScope
                        profile={p}
                        registry={registry}
                        onRegistryChange={onRegistryChange}
                      />
                    )
                  ) : (
                    <p className="truncate pl-5 text-xs text-muted-foreground">
                      {names.join(", ")}
                    </p>
                  )}
                </div>
              );
            })}
          </div>
          <FolderRouting registry={registry} onRegistryChange={onRegistryChange} />
        </section>
      )}
      <section className="flex flex-col gap-2">
        <h2 className="text-xs font-medium tracking-wide text-muted-foreground uppercase">
          {" "}
          {t("Discovery")}{" "}
        </h2>
        {toggle(
          Layers,
          lazyDiscovery,
          "text-info",
          t("Lazy discovery"),
          t("Expose 4 meta-tools, not the full catalog (all clients)"),
          apply("lazy-discovery", setLazyDiscovery),
          "lazy-discovery",
        )}
        {/* Pinned prerequisites is a refinement of lazy discovery (the tools it must never
            hide), not a peer feature, so nest it under the Lazy discovery toggle with an
            indent + left rail. It has no meaning when lazy discovery is off, so it collapses
            away entirely then. Code mode below is an independent capability and stays a
            full-width sibling. */}
        {lazyDiscovery ? (
          <div className="ml-4 border-l-2 border-border/50 pl-3">
            <PinnedPrerequisites
              registry={registry}
              onRegistryChange={onRegistryChange}
            />
          </div>
        ) : null}
        {toggle(
          Braces,
          codeMode,
          "text-info",
          t("Code mode"),
          t(
            "On by default: agents can run one server-side script that calls many tools in a single round-trip. Sandboxed JS; each call still respects profile scope and human approval. Not a security boundary; turn off to hide toolport_run_script.",
          ),
          apply("code-mode", setCodeMode),
          "code-mode",
        )}
        {codeMode
          ? toggle(
              Braces,
              allowRoutineWrites,
              "text-warning",
              t("Allow routine writes"),
              t(
                "Allow agents to request saving persistent routines. Every save still requires your approval.",
              ),
              apply("allow-routine-writes", setAllowRoutineWrites),
              "allow-routine-writes",
            )
          : null}
        {/* Rendered independently of the writes toggle: a suggestion queued while
            writes were on stays actionable (the user is the authority here). A
            successful empty read hides the section; a failed load is a visible
            error, not an empty queue (SBS-879). */}
        <RoutineSuggestions />
      </section>
      <section className="flex flex-col gap-2">
        <h2 className="text-xs font-medium tracking-wide text-muted-foreground uppercase">
          {" "}
          {t("Security")}{" "}
        </h2>
        <PostureSummary
          denyDestructive={denyDestructive}
          confirmDestructive={confirmDestructive}
          humanApproval={humanApproval}
          quarantineOnDrift={quarantineOnDrift}
          blockOnInjection={blockOnInjection}
        />
        {toggle(
          ShieldAlert,
          denyDestructive,
          "text-warning",
          t("Block destructive tools"),
          t(
            "Hide any tool the server marks as able to delete or change data, from every client",
          ),
          apply("deny-destructive", setDenyDestructive),
          "deny-destructive",
        )}
        {toggle(
          ShieldCheck,
          confirmDestructive,
          "text-info",
          t("Confirm destructive tools"),
          t("Hold each destructive call for the agent to confirm before it runs"),
          apply("confirm-destructive", setConfirmDestructive),
          "confirm-destructive",
        )}
        {toggle(
          UserCheck,
          humanApproval,
          "text-info",
          t("Require human approval"),
          t(
            "Hold destructive or untrusted-server calls until you approve them in the app",
          ),
          apply("human-approval", setHumanApproval),
          "human-approval",
        )}
        {toggle(
          ShieldX,
          quarantineOnDrift,
          "text-destructive",
          t("Quarantine changed high-risk tools"),
          t(
            "Block a destructive or poisoned tool that changes from its approved version, until you re-approve it",
          ),
          apply("quarantine-on-drift", setQuarantineOnDrift),
          "quarantine-on-drift",
        )}
        {toggle(
          ShieldAlert,
          blockOnInjection,
          "text-destructive",
          t("Block high-confidence injection"),
          t(
            "Fail a tool call when content defense finds a high-confidence prompt-injection hit, instead of only labeling the text. Off by default; medium-confidence hits still label only",
          ),
          apply("block-on-injection", setBlockOnInjection),
          "block-on-injection",
        )}
        {toggle(
          EyeOff,
          piiRedaction,
          "text-info",
          t("Hide personal data from the model"),
          t(
            "Replace emails, phone numbers, card numbers and API keys in tool results with placeholders before the model sees them, then put the real values back when it calls a tool. A value only goes back to the server it came from, so a call that would send one server's data to another is refused. Real data stays on this machine and is forgotten when the conversation ends. Off by default; a value no detector recognises still passes through, so this reduces what reaches the model rather than guaranteeing it",
          ),
          apply("pii-redaction", setPiiRedaction),
          "pii-redaction",
        )}
        {toggle(
          Bot,
          allowAgentControl,
          "text-success",
          t("Allow agent control"),
          t(
            "Let an agent turn servers on/off; your destructive-tool block always stays yours",
          ),
          apply("allow-agent-control", setAllowAgentControl),
          "allow-agent-control",
        )}
        {toggle(
          Activity,
          liveInspect,
          "text-info",
          t("Live request/response inspection"),
          t(
            "Off by default. While on, Toolport captures each tool call's arguments and results to a small local, ephemeral buffer (the last 50 calls) so you can inspect them in Activity. This is separate from the audit log, never leaves your machine, and is cleared when you turn it off or restart the gateway.",
          ),
          applyLiveInspect,
          "live-inspect",
        )}
        {quarantined.length === 0 && quarantineError && (
          <div className="flex items-center gap-2 rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2 text-xs text-muted-foreground">
            <ShieldX className="size-4 shrink-0 text-destructive" />
            <span>{t("Couldn't read quarantine status. Retrying every 15s.")}</span>
          </div>
        )}
        {quarantined.length > 0 && (
          <div className="flex flex-col gap-2 rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2.5">
            <div className="flex items-center gap-2">
              <ShieldX className="size-4 shrink-0 text-destructive" />
              <span className="text-sm font-medium">{t("Quarantined tools")}</span>
              <span className="text-xs text-muted-foreground">
                {quarantineError
                  ? t("status may be stale")
                  : t("blocked until you re-approve")}
              </span>
            </div>
            <ul className="flex flex-col gap-1.5">
              {quarantined.map((q) => (
                <li
                  key={`${q.profile}:${q.tool}`}
                  className="flex items-center gap-2 text-xs"
                >
                  <span className="min-w-0 truncate font-mono">{q.tool}</span>
                  <span
                    className="min-w-0 truncate text-muted-foreground"
                    title={q.detail || q.reason}
                  >
                    {q.detail ? q.detail : q.reason}
                  </span>
                  <button
                    type="button"
                    onClick={() => reapprove(q)}
                    className="ml-auto shrink-0 rounded-md border bg-background px-2 py-0.5 text-[11px] font-medium hover:bg-accent"
                  >
                    {" "}
                    {t("Re-approve")}{" "}
                  </button>
                </li>
              ))}
            </ul>
          </div>
        )}
        {allowedTools.length === 0 && allowedError && (
          <div className="flex items-center gap-2 rounded-md border border-border bg-muted/20 px-3 py-2 text-xs text-muted-foreground">
            <UserCheck className="size-4 shrink-0 text-info" />
            <span>{t("Couldn't read the allowed-tools list. Retrying every 10s.")}</span>
          </div>
        )}
        {allowedTools.length > 0 && (
          <div className="flex flex-col gap-2 rounded-md border border-border bg-muted/20 px-3 py-2.5">
            <div className="flex items-center gap-2">
              <UserCheck className="size-4 shrink-0 text-info" />
              <span className="text-sm font-medium">{t("Allowed tools")}</span>
              <span className="text-xs text-muted-foreground">
                {allowedError ? t("list may be stale") : t("skip human approval")}
              </span>
            </div>
            <ul className="flex flex-col gap-1.5">
              {allowedTools.map((tool) => (
                <li key={tool.key} className="flex items-center gap-2 text-xs">
                  <span className="min-w-0 truncate font-mono">
                    {tool.server}/{tool.tool}
                  </span>
                  <span className="shrink-0 text-muted-foreground">
                    {tool.persistent ? t("Always") : t("this session")}
                  </span>
                  <button
                    type="button"
                    onClick={() => void revokeAllowed(tool.key)}
                    className="ml-auto shrink-0 rounded-md border bg-background px-2 py-0.5 text-[11px] font-medium hover:bg-accent"
                  >
                    {" "}
                    {t("Revoke")}{" "}
                  </button>
                </li>
              ))}
            </ul>
          </div>
        )}
      </section>
      <section className="flex flex-col gap-2">
        <h2 className="text-xs font-medium tracking-wide text-muted-foreground uppercase">
          {" "}
          {t("Integrations")}{" "}
        </h2>
        <div className="flex flex-col gap-2 rounded-md border px-3 py-2.5">
          <label className="flex items-center gap-2.5 text-sm">
            <Globe
              className={`size-4 shrink-0 ${bridge?.running ? "text-success" : "text-muted-foreground"}`}
            />
            <span className="flex min-w-0 flex-1 flex-col leading-tight">
              <span className="font-medium">{t("Open WebUI / HTTP endpoint")}</span>
              <span className="text-xs text-muted-foreground">
                {" "}
                {t(
                  "Serve your tools over HTTP/OpenAPI for Open WebUI and any OpenAPI client",
                )}{" "}
              </span>
            </span>
            <Switch
              checked={!!bridge?.running}
              onCheckedChange={toggleBridge}
              disabled={bridgeBusy}
            />
          </label>
          {bridgeError && bridge === null && (
            <p className="flex items-center gap-1.5 text-xs text-muted-foreground">
              <span>
                {" "}
                {t(
                  "Couldn't read the HTTP endpoint status. The gateway may be starting up.",
                )}{" "}
              </span>
              <button
                type="button"
                onClick={loadBridge}
                className="shrink-0 font-medium text-foreground underline underline-offset-2 hover:text-primary"
              >
                {" "}
                {t("Retry")}{" "}
              </button>
            </p>
          )}
          {bridge?.running && bridge.url && (
            <>
              <div className="flex items-center gap-2 rounded border bg-muted/40 px-2 py-1.5">
                <span className="shrink-0 text-[11px] font-medium text-muted-foreground">
                  {" "}
                  {"URL"}{" "}
                </span>
                <code className="min-w-0 flex-1 truncate text-xs">{bridge.url}</code>
                <button
                  type="button"
                  onClick={() => copy(bridge.url!, "url")}
                  title={t("Copy URL")}
                  className="shrink-0 rounded p-1 text-muted-foreground hover:bg-muted hover:text-foreground"
                >
                  {copied === "url" ? (
                    <Check className="size-3.5 text-success" />
                  ) : (
                    <Copy className="size-3.5" />
                  )}
                </button>
              </div>
              {bridge.token && (
                <div className="flex items-center gap-2 rounded border bg-muted/40 px-2 py-1.5">
                  <span className="shrink-0 text-[11px] font-medium text-muted-foreground">
                    {" "}
                    {t("Token")}{" "}
                  </span>
                  <code className="min-w-0 flex-1 truncate text-xs">
                    {showToken ? bridge.token : "•".repeat(24)}
                  </code>
                  <button
                    type="button"
                    onClick={() => setShowToken((s) => !s)}
                    title={showToken ? t("Hide token") : t("Reveal token")}
                    aria-label={showToken ? t("Hide token") : t("Reveal token")}
                    className="shrink-0 rounded p-1 text-muted-foreground hover:bg-muted hover:text-foreground"
                  >
                    {showToken ? (
                      <EyeOff className="size-3.5" />
                    ) : (
                      <Eye className="size-3.5" />
                    )}
                  </button>
                  <button
                    type="button"
                    onClick={() => copy(bridge.token!, "token")}
                    title={t("Copy token")}
                    className="shrink-0 rounded p-1 text-muted-foreground hover:bg-muted hover:text-foreground"
                  >
                    {copied === "token" ? (
                      <Check className="size-3.5 text-success" />
                    ) : (
                      <Copy className="size-3.5" />
                    )}
                  </button>
                </div>
              )}
              <p className="text-xs text-muted-foreground">
                {" "}
                {t(
                  "In Open WebUI: Settings → Tools → add the URL as an OpenAPI server and paste the token as its API key (Bearer auth), then set Function Calling to Native (per chat). The token stops other local apps from calling your tools.",
                )}{" "}
              </p>

              <div className="mt-1 flex flex-col gap-2 rounded border bg-muted/20 p-2.5">
                <div className="flex items-baseline justify-between gap-2">
                  <span className="text-[11px] font-medium text-muted-foreground">
                    {" "}
                    {t("Scoped clients")}{" "}
                  </span>
                  <span className="text-[11px] text-muted-foreground/70">
                    {" "}
                    {t("each gets its own token and server set")}{" "}
                  </span>
                </div>

                {httpClients.length > 0 && (
                  <ul className="flex flex-col gap-1">
                    {httpClients.map((c) => (
                      <li key={c.id} className="flex items-center gap-2 text-xs">
                        <span className="truncate font-medium">
                          {c.label || t("(unnamed)")}
                        </span>
                        <span className="shrink-0 rounded bg-muted px-1.5 py-0.5 text-[11px] text-muted-foreground">
                          {c.profile || t("all servers")}
                        </span>
                        <button
                          type="button"
                          onClick={() => removeClient(c.id)}
                          aria-label={t("Revoke {value1}", { value1: c.label })}
                          className="ml-auto shrink-0 rounded p-1 text-muted-foreground/60 hover:bg-destructive/10 hover:text-destructive"
                        >
                          <Trash2 className="size-3.5" />
                        </button>
                      </li>
                    ))}
                  </ul>
                )}

                {newToken && (
                  <>
                    <div className="flex items-center gap-2 rounded border border-success/30 bg-success/5 px-2 py-1.5">
                      <span className="shrink-0 text-[11px] font-medium text-success">
                        {" "}
                        {t("New token")}{" "}
                      </span>
                      <code className="min-w-0 flex-1 truncate text-xs">{newToken}</code>
                      <button
                        type="button"
                        onClick={() => copy(newToken, "newtoken")}
                        title={t("Copy token")}
                        className="shrink-0 rounded p-1 text-muted-foreground hover:bg-muted hover:text-foreground"
                      >
                        {copied === "newtoken" ? (
                          <Check className="size-3.5 text-success" />
                        ) : (
                          <Copy className="size-3.5" />
                        )}
                      </button>
                      <button
                        type="button"
                        onClick={() => setNewToken(null)}
                        aria-label={t("Dismiss")}
                        className="shrink-0 rounded p-1 text-muted-foreground/60 hover:text-foreground"
                      >
                        <X className="size-3.5" />
                      </button>
                    </div>
                    <p className="text-[11px] text-warning">
                      {" "}
                      {t("Copy this token now, it won't be shown again.")}{" "}
                    </p>
                  </>
                )}

                <div className="flex items-center gap-2">
                  <input
                    value={newLabel}
                    onChange={(e) => setNewLabel(e.target.value)}
                    placeholder={t("Client name (e.g. Open WebUI)")}
                    className="h-8 min-w-0 flex-1 rounded-md border border-input bg-transparent px-2 text-xs focus-visible:ring-1 focus-visible:ring-ring focus-visible:outline-none"
                  />
                  {profiles.length > 0 && (
                    <Select
                      value={newProfile || "__all__"}
                      onValueChange={(v) => setNewProfile(v === "__all__" ? "" : v)}
                    >
                      <SelectTrigger
                        size="sm"
                        aria-label={t("Scope")}
                        className="h-8 w-32 shrink-0 text-xs"
                      >
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value="__all__">{t("All servers")}</SelectItem>
                        {profiles.map((p) => (
                          <SelectItem key={p.id} value={p.name}>
                            {p.name}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  )}
                  <button
                    type="button"
                    onClick={addClient}
                    disabled={clientBusy || !newLabel.trim()}
                    className="h-8 shrink-0 rounded-md border bg-background px-2.5 text-xs font-medium hover:bg-accent disabled:opacity-50"
                  >
                    {" "}
                    {t("Add")}{" "}
                  </button>
                </div>
              </div>
            </>
          )}
        </div>
        <div className="flex flex-col gap-2 rounded-md border px-3 py-2.5">
          <div className="flex items-center gap-2.5 text-sm">
            <RefreshCw
              className={`size-4 shrink-0 text-muted-foreground ${reapBusy ? "animate-spin" : ""}`}
            />
            <span className="flex min-w-0 flex-1 flex-col leading-tight">
              <span className="font-medium">{t("Stop old gateways")}</span>
              <span className="text-xs text-muted-foreground">
                {" "}
                {t(
                  "End leftover gateway processes from earlier installs. Where the gateway path stays the same across upgrades, a host that respawns MCP picks up the new binary on its own. Where the filename carries its version, or the old one sits at a path an upgrade never touches, an app that was already running keeps launching it until you restart that app.",
                )}{" "}
              </span>
            </span>
            <button
              type="button"
              disabled={reapBusy}
              onClick={async () => {
                setReapBusy(true);
                setReapOutcome(null);
                try {
                  const outcome = await stopStaleGateways();
                  // Always from the outcome: it carries the merged advice, so
                  // clicking Run before a client has respawned cannot blank the
                  // panel below (SOU-435).
                  setNeedsRestart(outcome.needsRestart);
                  // This outcome IS a fresh answer to "which apps need a
                  // restart", so a stale "couldn't check" panel must go with it.
                  // Leaving it up would state the check failed directly above
                  // the check's own result, which is the same contradiction the
                  // panel exists to prevent (#730).
                  setRestartCheckFailed(false);
                  setReapOutcome(outcome);
                } catch (e) {
                  toastError(
                    t("Couldn't stop old gateways: {value1}", { value1: String(e) }),
                  );
                } finally {
                  setReapBusy(false);
                }
              }}
              className="h-8 shrink-0 rounded-md border bg-background px-2.5 text-xs font-medium hover:bg-accent disabled:opacity-50"
            >
              {reapBusy ? t("Working…") : t("Run")}
            </button>
          </div>
          {reapResult && <p className="text-xs text-muted-foreground">{reapResult}</p>}
          {restartCheckFailed && (
            <p className="flex items-center gap-2 rounded-md border border-destructive/30 bg-destructive/5 px-3 py-2 text-xs text-muted-foreground">
              {" "}
              {t("Couldn't check for apps using an old gateway.")}{" "}
              <button
                type="button"
                aria-label={t("Retry checking for old gateway")}
                onClick={() => void loadNeedsRestart()}
                className="shrink-0 font-medium text-foreground underline underline-offset-2 hover:text-primary"
              >
                {" "}
                {t("Retry")}{" "}
              </button>
            </p>
          )}
          {needsRestart.length > 0 && (
            <div className="rounded-md border border-warning/40 bg-warning/5 p-3">
              <p className="text-xs font-medium text-warning">
                {needsRestart.length === 1
                  ? t("1 app is still launching an old gateway")
                  : t("{value1} apps are still launching an old gateway", {
                      value1: needsRestart.length,
                    })}
              </p>
              <ul className="mt-1.5 space-y-1">
                {needsRestart.map((c) => (
                  <li key={c.clientPid} className="text-xs text-muted-foreground">
                    <span className="font-medium text-foreground">{c.client}</span>
                    {/* The pid is the only thing separating two rows for the same app.
                        Without it, several copies of one editor render as identical
                        lines that read as a duplicate-row bug, and "restart each one"
                        cannot be followed when a dozen are running. */}
                    <span className="ml-1 font-mono text-muted-foreground/70">
                      {" "}
                      {t("(pid")} {c.clientPid})
                    </span>{" "}
                    {t("keeps starting")} <code className="font-mono">{c.gateway}</code>
                  </li>
                ))}
              </ul>
              <p className="mt-2 text-xs text-muted-foreground">
                {" "}
                {t(
                  "These apps cached the old gateway path when they started, so stopping the process only makes them launch it again — and they only launch it when they next use a tool, which is why none may be running this moment. Restart each one to pick up the current gateway. This list clears itself as you do.",
                )}{" "}
              </p>
            </div>
          )}
        </div>
      </section>
    </div>
  );
}
