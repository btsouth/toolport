import { invoke } from "@tauri-apps/api/core";
import type {
  AuditEntry,
  AuditStats,
  AuthInfo,
  CatalogEntry,
  DetectedClient,
  FolderProfile,
  HookEvent,
  HooksPreview,
  HooksView as HooksViewData,
  ImportItem,
  InspectEntry,
  InstructionsStatusView,
  McpPrompt,
  McpResource,
  McpTool,
  MigrateResult,
  ParsedSnippetServer,
  AllowedTool,
  PendingApproval,
  ProbeResult,
  Registry,
  RoutineSuggestion,
  RulesImportCandidate,
  RulesImportedFile,
  RulesPreview,
  RulesView,
  SavingsSummary,
  SearchTrace,
  ToolIdentity,
  ServerEntry,
  ToolCallResult,
  Stack,
  WriteOutcome,
  PermissionRule,
  PermissionsPreview,
  PermissionsView,
} from "./types";

/** The hand-verified popular catalog (offline, instant). */
export function popularCatalog(): Promise<CatalogEntry[]> {
  return invoke<CatalogEntry[]>("popular_catalog");
}

/** Curated stacks: role-based server bundles for one-flow setup (offline). */
export function listStacks(): Promise<Stack[]> {
  return invoke<Stack[]>("list_stacks");
}

/** Search the catalog (your picks + curated, then the MCP Registry). */
export function searchCatalog(query: string): Promise<CatalogEntry[]> {
  return invoke<CatalogEntry[]>("search_catalog", { query });
}

/** Recent tool-call audit entries (newest first). */
export function getAuditLog(limit = 200): Promise<AuditEntry[]> {
  return invoke<AuditEntry[]>("get_audit_log", { limit });
}

/** Aggregated per-server stats (calls, error rate, latency) from the audit log. */
export function getAuditStats(): Promise<AuditStats> {
  return invoke<AuditStats>("audit_stats", {});
}

/** A tool-definition integrity event: a previously-approved tool changed
 * (rug-pull signal) or a known server added a tool. */
export interface SecurityEvent {
  ts: number;
  /** "tool_drift" (definition changed/added) or "tool_poison_flag" (injection in a definition). */
  type: string;
  /** Absent for events not tied to a specific tool (e.g. pins_load_failed). */
  server?: string;
  tool?: string;
  change: string;
  /** For tool_poison_flag: which heuristic signatures matched. */
  signatures?: string[];
  /** For tool_poison_flag: a short de-obfuscated excerpt of the matched text, so the
   * flag is verifiable instead of an opaque label. Absent when no direct phrase matched
   * (e.g. an encoded payload) or on events written before evidence was captured. */
  evidence?: string;
  /** "high" = loud/actionable (poison, destructive-tool change, safety-annotation
   * downgrade); "info" = benign non-destructive schema churn for the quiet history.
   * Absent on events written before severity tiering; classified by type on read. */
  severity?: "high" | "info";
}

/** Recent tool-definition integrity events (newest first). */
export function getSecurityEvents(limit = 100): Promise<SecurityEvent[]> {
  return invoke<SecurityEvent[]>("get_security_events", { limit });
}

/** Cumulative tokens lazy discovery has kept out of client context. */
export function getSavingsSummary(): Promise<SavingsSummary> {
  return invoke<SavingsSummary>("savings_summary");
}

/** A shareable diagnostics blob (version, registry summary, gateway log tail) for bug reports. */
export function gatherDiagnostics(): Promise<string> {
  return invoke<string>("gather_diagnostics");
}

/** Connect to each enabled server and report health + tool count. */
export function probeServers(): Promise<ProbeResult[]> {
  return invoke<ProbeResult[]>("probe_servers");
}

/** Connect to a (possibly unsaved) server entry to verify it works before
 * saving. Typed secret values ride in on `entry.env`; nothing is persisted. */
export function testServer(entry: ServerEntry): Promise<ProbeResult> {
  return invoke<ProbeResult>("test_server", { entry });
}

/** Result of registering an HTTP-bridge client: the updated registry plus the
 * plaintext bearer token, shown once and never returned again. */
export interface AddedHttpClient {
  registry: Registry;
  token: string;
}

/** Register an HTTP-bridge client scoped to a profile (empty = all servers).
 * Returns the one-time plaintext token to paste into the client. */
export function addHttpClient(label: string, profile?: string): Promise<AddedHttpClient> {
  return invoke<AddedHttpClient>("add_http_client", { label, profile });
}

/** Revoke a registered HTTP-bridge client by id. */
export function removeHttpClient(id: string): Promise<Registry> {
  return invoke<Registry>("remove_http_client", { id });
}

/** List the tools one server exposes (connects on demand). Playground picker. */
export function listServerTools(serverId: string): Promise<McpTool[]> {
  return invoke<McpTool[]>("list_server_tools", { serverId });
}

/** Invoke one tool on a server and return its raw MCP result. */
export function callTool(
  serverId: string,
  tool: string,
  args: Record<string, unknown>,
): Promise<ToolCallResult> {
  return invoke<ToolCallResult>("call_tool", { serverId, tool, arguments: args });
}

/** List the resources one server advertises (connects on demand). Playground. */
export function listServerResources(serverId: string): Promise<McpResource[]> {
  return invoke<McpResource[]>("list_server_resources", { serverId });
}

/** List the prompts one server advertises (connects on demand). Playground. */
export function listServerPrompts(serverId: string): Promise<McpPrompt[]> {
  return invoke<McpPrompt[]>("list_server_prompts", { serverId });
}

/** Read one resource by uri; returns the raw MCP result (`{ contents }`). */
export function readResource(serverId: string, uri: string): Promise<unknown> {
  return invoke("read_resource", { serverId, uri });
}

/** Get one prompt by name with arguments; returns the raw MCP result. */
export function getPrompt(
  serverId: string,
  name: string,
  args: Record<string, unknown>,
): Promise<unknown> {
  return invoke("get_prompt", { serverId, name, arguments: args });
}

/** Enable/disable one tool on a server (gateway hides+blocks disabled tools). */
export function setToolEnabled(
  serverId: string,
  tool: string,
  enabled: boolean,
): Promise<Registry> {
  return invoke<Registry>("set_tool_enabled", { serverId, tool, enabled });
}

/** Pin/unpin a tool as a lazy-discovery prerequisite (search always surfaces it). */
export function setToolPinned(
  serverId: string,
  tool: string,
  pinned: boolean,
): Promise<Registry> {
  return invoke<Registry>("set_tool_pinned", { serverId, tool, pinned });
}

/** Toggle the global destructive-tool deny switch. */
export function setDenyDestructive(deny: boolean): Promise<Registry> {
  return invoke<Registry>("set_deny_destructive", { deny });
}

/** Toggle per-call confirmation for destructive tools (intercept + preview + token). */
export function setConfirmDestructive(confirm: boolean): Promise<Registry> {
  return invoke<Registry>("set_confirm_destructive", { confirm });
}

/** Toggle human-in-the-loop approval: hold a gated tool call (destructive, or from an
 * untrusted-provenance server) until a person approves or denies it in the app. */
export function setHumanApproval(on: boolean): Promise<Registry> {
  return invoke<Registry>("set_human_approval", { on });
}

/** Tool calls currently held awaiting a human decision (the approval queue). */
export function listPendingApprovals(): Promise<PendingApproval[]> {
  return invoke<PendingApproval[]>("list_pending_approvals");
}

/** How long an approval sticks: `once` (this call only), `session` (until the app
 * restarts), or `always` (persisted, skips the prompt for this tool from now on). */
export type ApprovalScope = "once" | "session" | "always";

/** Approve or deny a held tool call by id; the parked gateway call then runs or is refused.
 * On approve, `scope` controls whether future calls to the same tool skip the prompt. */
export function decideApproval(
  id: string,
  approved: boolean,
  scope: ApprovalScope = "once",
): Promise<void> {
  return invoke<void>("decide_approval", { id, approved, scope });
}

/** Strong routine candidates the gateway queued for the passive save area. */
export function listRoutineSuggestions(): Promise<RoutineSuggestion[]> {
  return invoke<RoutineSuggestion[]>("list_routine_suggestions");
}

/** Persist a queued suggestion. The click is the persistence authorization: the card
 * showed the same disclosure the approval prompt would, so no second prompt fires. */
export function approveRoutineSuggestion(
  fingerprint: string,
  name: string,
  description?: string,
): Promise<unknown> {
  return invoke<unknown>("approve_routine_suggestion", {
    fingerprint,
    name,
    description,
  });
}

/** Drop a queued suggestion and keep the same definition out for this app run. */
export function dismissRoutineSuggestion(fingerprint: string): Promise<void> {
  return invoke<void>("dismiss_routine_suggestion", { fingerprint });
}

/** Tools currently allowed to skip human approval (persistent "always" + this session). */
export function listAllowedTools(): Promise<AllowedTool[]> {
  return invoke<AllowedTool[]>("list_allowed_tools");
}

/** Revoke an allowed tool so it requires approval again. */
export function revokeAllowedTool(key: string): Promise<void> {
  return invoke<void>("revoke_allowed_tool", { key });
}

/** Set (or clear) a per-tool exposure override, keyed by `(server, original tool)`:
 * rename and/or replace the description clients see. Blank name + description clears it.
 * The call still routes to the original downstream tool. */
export function setToolOverride(
  server: string,
  tool: string,
  name: string | null,
  description: string | null,
): Promise<Registry> {
  return invoke<Registry>("set_tool_override", { server, tool, name, description });
}

/** Remove a tool's exposure override, restoring the server's own name + description. */
export function clearToolOverride(server: string, tool: string): Promise<Registry> {
  return invoke<Registry>("clear_tool_override", { server, tool });
}

/** Toggle live request/response inspection (opt-in, off by default). When on, the
 * gateway captures each tool call's args + result into a small ephemeral local ring. */
export function setLiveInspect(enabled: boolean): Promise<Registry> {
  return invoke<Registry>("set_live_inspect", { enabled });
}

/** Recent live-inspection captures (newest first): each call's args + result. Empty
 * unless live inspection has been on. */
export function getInspectLog(limit = 50): Promise<InspectEntry[]> {
  return invoke<InspectEntry[]>("get_inspect_log", { limit });
}

/** Clear the live-inspection ring so no captured args/results linger. */
export function clearInspectLog(): Promise<void> {
  return invoke<void>("clear_inspect_log");
}

/** Recent lazy-discovery search traces (newest first): what the model searched for,
 * which tools matched, and the tool-definition tokens the results cost vs. loading the
 * whole catalog. Empty until something has searched. */
export function getSearchTraces(limit = 100): Promise<SearchTrace[]> {
  return invoke<SearchTrace[]>("get_search_traces", { limit });
}

/** Clear the search-trace log. */
export function clearSearchTraces(): Promise<void> {
  return invoke<void>("clear_search_traces");
}

/** Clear all retained local activity at once: audit log, discovery traces,
 * live-inspection captures, and the savings tally (incl. its carry-forward total).
 * Local, irreversible deletes; each log re-creates itself on the next event. */
export function clearActivityLogs(): Promise<void> {
  return invoke<void>("clear_activity_logs");
}

/** Every pinned tool's verifiable identity (alias -> server/profiles + fingerprint +
 * first-seen/last-changed) for the active profile. Empty until a baseline is pinned. */
export function getToolIdentities(): Promise<ToolIdentity[]> {
  return invoke<ToolIdentity[]>("list_tool_identities");
}

/** Toggle quarantine-on-drift: block a high-risk tool that drifted until re-approved. */
export function setQuarantineOnDrift(on: boolean): Promise<Registry> {
  return invoke<Registry>("set_quarantine_on_drift", { on });
}

/** Toggle opt-in block-on-injection: fail high-confidence injection hits instead of only labeling. */
export function setBlockOnInjection(on: boolean): Promise<Registry> {
  return invoke<Registry>("set_block_on_injection", { on });
}

/** Toggle PII pseudonymization: replace emails, cards and keys in tool results with
 * stable pseudonyms before the model sees them, re-hydrating them on the way out.
 * Rehydration is scoped to the server that produced the value, and a call carrying
 * another server's token is refused (SBS-605). Off by default. A value no detector
 * recognises passes through, so this reduces what reaches the model rather than
 * guaranteeing anything. */
export function setPiiRedaction(on: boolean): Promise<Registry> {
  return invoke<Registry>("set_pii_redaction", { on });
}

/** A tool blocked after high-risk drift or loss of the integrity baseline,
 * awaiting re-approval. */
export interface QuarantinedTool {
  server: string;
  tool: string;
  reason: string;
  ts: number;
  profile: string;
  /** Concrete annotation delta when known, e.g. `readOnlyHint: true → false` (SOU-305). */
  detail?: string | null;
  prev_ro?: boolean | null;
  new_ro?: boolean | null;
  prev_dh?: boolean | null;
  new_dh?: boolean | null;
}

/** Tools currently quarantined after high-risk drift or baseline loss,
 * across profiles. */
export function listQuarantined(): Promise<QuarantinedTool[]> {
  return invoke<QuarantinedTool[]>("list_quarantined");
}

/** Re-approve a quarantined tool so the gateway re-exposes it on its next rebuild. */
export function releaseQuarantine(profile: string, tool: string): Promise<void> {
  return invoke<void>("release_quarantine", { profile, tool });
}

/** Outcome of a bulk re-approval. `skipped` names tools that stay blocked. */
export type ReleaseAllOutcome = {
  released: number;
  skipped: string[];
};

/**
 * Re-approve every blocked tool for a profile in one pass. A lost integrity
 * baseline blocks the whole catalog at once, which is far too many to clear one
 * at a time. Tools whose captured definition could not be read stay blocked and
 * are returned in `skipped`.
 */
export function releaseAllQuarantine(profile: string): Promise<ReleaseAllOutcome> {
  return invoke<ReleaseAllOutcome>("release_all_quarantine", { profile });
}

/** Toggle global lazy discovery (meta-tools vs full catalog) for all clients. */
export function setLazyDiscovery(lazy: boolean): Promise<Registry> {
  return invoke<Registry>("set_lazy_discovery", { lazy });
}

/** Toggle server-side code mode (the toolport_run_script meta-tool) for all clients. */
export function setCodeMode(enabled: boolean): Promise<Registry> {
  return invoke<Registry>("set_code_mode", { enabled });
}

/** Opt into agent-requested Routine writes. Each save still requires human approval. */
export function setAllowRoutineWrites(allow: boolean): Promise<Registry> {
  return invoke<Registry>("set_allow_routine_writes", { allow });
}

/** Override one client's discovery mode ("full" | "lazy" | "grouped"), or clear it
 * (`null`) so the client inherits the global mode. Applies live via the gateway's
 * per-client resolution, no reconnect needed. */
export function setClientDiscovery(
  clientId: string,
  mode: string | null,
): Promise<Registry> {
  return invoke<Registry>("set_client_discovery", { clientId, mode });
}

/** Opt into agent control: let an agent enable/disable servers via the gateway. */
export function setAllowAgentControl(allow: boolean): Promise<Registry> {
  return invoke<Registry>("set_allow_agent_control", { allow });
}

export interface HttpBridgeStatus {
  running: boolean;
  port: number | null;
  url: string | null;
  token: string | null;
}

/** Start the supervised toolport-gateway HTTP/OpenAPI server (Open WebUI etc.). */
export function startHttpBridge(port?: number): Promise<HttpBridgeStatus> {
  return invoke<HttpBridgeStatus>("start_http_bridge", { port: port ?? null });
}

/** Stop the supervised HTTP/OpenAPI server. */
export function stopHttpBridge(): Promise<HttpBridgeStatus> {
  return invoke<HttpBridgeStatus>("stop_http_bridge");
}

/** Current HTTP/OpenAPI bridge status (reaps the child if it exited). */
export function httpBridgeStatus(): Promise<HttpBridgeStatus> {
  return invoke<HttpBridgeStatus>("http_bridge_status");
}

/**
 * An application that keeps relaunching an obsolete gateway, so only restarting
 * that application delivers the new gateway code (SOU-435).
 */
export type ClientNeedingRestart = {
  /** Application basename, e.g. `claude.exe`. */
  client: string;
  /** Its pid. The entry disappears once that process is gone. */
  clientPid: number;
  /** The obsolete gateway image it relaunched. */
  gateway: string;
};

/** What a reaper run did, and what the user still has to do about it. */
export type ReapOutcome = {
  /** Human-readable labels of processes that were stopped. */
  killed: string[];
  /** Matched but could not be stopped. Distinct from "nothing was stale". */
  failed: string[];
  /** Apps needing a manual restart, accumulated across passes this session. */
  needsRestart: ClientNeedingRestart[];
};

/**
 * Stop obsolete Toolport gateway processes (older versions / stale paths).
 * Keeps the current resolved binary and the supervised HTTP bridge when they
 * match.
 *
 * Reaping alone does not always deliver new gateway code: a client caches its
 * spawn command at its own startup, so one pinned to a path an upgrade never
 * rewrites relaunches the same obsolete binary. Those apps come back in
 * `needsRestart`, which is why this returns an outcome rather than a killed list.
 */
export function stopStaleGateways(): Promise<ReapOutcome> {
  return invoke<ReapOutcome>("stop_stale_gateways");
}

/**
 * Apps needing a restart, without running a reaper pass.
 *
 * Read from stored state rather than recomputed: the advice is only visible in a
 * pre-kill process table, so asking after a reap would always come back emptier
 * than the truth.
 */
export function clientsNeedingRestart(): Promise<ClientNeedingRestart[]> {
  return invoke<ClientNeedingRestart[]>("clients_needing_restart");
}

/**
 * Result of {@link teamConnect} / {@link teamJoinPoll}. `status` is:
 * - `connected` — joined; `registry` is the fresh merged state.
 * - `pending` — the link requires admin approval; poll `requestToken` via {@link teamJoinPoll}.
 * - `denied` — an admin declined the request.
 * - `unknown` — the request expired or is invalid; start over.
 */
export interface TeamConnectResult {
  status: "connected" | "pending" | "denied" | "unknown";
  registry?: Registry;
  requestToken?: string;
}

/** Join a Toolport Teams server with an invite or join-link code; merges the team's servers in. */
export function teamConnect(
  serverUrl: string,
  inviteCode: string,
  memberName?: string,
): Promise<TeamConnectResult> {
  return invoke<TeamConnectResult>("team_connect", {
    serverUrl,
    inviteCode,
    memberName: memberName ?? null,
  });
}

/**
 * Poll a pending, approval-gated join. Call on an interval after {@link teamConnect} returns
 * `status: "pending"`, passing back the `requestToken` and the same `memberName`. Resolves to
 * `connected` once an admin approves, or `pending` / `denied` / `unknown`.
 */
export function teamJoinPoll(
  serverUrl: string,
  requestToken: string,
  memberName?: string,
): Promise<TeamConnectResult> {
  return invoke<TeamConnectResult>("team_join_poll", {
    serverUrl,
    requestToken,
    memberName: memberName ?? null,
  });
}

/** Pull the latest team config and re-merge it (no-op if unchanged). */
export function teamSync(): Promise<Registry> {
  return invoke<Registry>("team_sync");
}

/**
 * Long-polling sync: parks on the server for up to `waitSecs` and returns the instant the
 * team config view changes (or the wait elapses), so a dashboard policy edit enforces in
 * ~1s. Drive it in a loop; it returns like {@link teamSync}.
 */
export function teamSyncWait(waitSecs: number): Promise<Registry> {
  return invoke<Registry>("team_sync_wait", { waitSecs });
}

/**
 * Whether the main window is currently shown (vs hidden to the tray). Used to seed the
 * team-sync loop's visibility gate on mount, for the case where the app launched straight to
 * the tray; live changes arrive via the `team-window-visible` event. Defaults to visible on
 * any error so sync never wedges off.
 */
export function mainWindowVisible(): Promise<boolean> {
  return invoke<boolean>("main_window_visible");
}

/**
 * The member-facing Team Instructions status on this machine: the org content, its version, and
 * each installed client's on-disk state. `null` when the team has no active instructions.
 */
export function teamInstructionsStatus(): Promise<InstructionsStatusView | null> {
  return invoke<InstructionsStatusView | null>("team_instructions_status");
}

/** Leave the team: remove its merged servers and clear the saved token. */
export function teamDisconnect(): Promise<Registry> {
  return invoke<Registry>("team_disconnect");
}

// ---- Personal agent rules (SBS-821) ----
//
// Every mutating call returns the refreshed view, so the tab never has to re-fetch to stay
// honest about what is on disk. All of these work with no MCP server configured.

/** The user's rule sets, which one is active, and each installed client's on-disk state. */
export function rulesView(): Promise<RulesView> {
  return invoke<RulesView>("rules_view");
}

/** Create (`id` omitted) or update a rule set, then apply it to every opted-in client. */
export function rulesSaveSet(
  name: string,
  content: string,
  id?: string,
): Promise<RulesView> {
  return invoke<RulesView>("rules_save_set", { id: id ?? null, name, content });
}

/** Delete a rule set. Deleting the active one also removes the files Toolport wrote. */
export function rulesDeleteSet(id: string): Promise<RulesView> {
  return invoke<RulesView>("rules_delete_set", { id });
}

/** Switch the active set, or pass nothing to clear it and remove our files everywhere. */
export function rulesSetActive(id?: string): Promise<RulesView> {
  return invoke<RulesView>("rules_set_active", { id: id ?? null });
}

/** Opt one client in or out. Opting out removes that client's rules file. */
export function rulesSetClientEnabled(
  clientId: string,
  enabled: boolean,
): Promise<RulesView> {
  return invoke<RulesView>("rules_set_client_enabled", { clientId, enabled });
}

/**
 * Dry-run one client's write: the exact before/after bytes, without touching disk. `null` when
 * the client has no rules file we manage, or no set is active.
 *
 * Pass `content` to preview unsaved editor text. Do NOT save first to get an accurate preview: a
 * save applies to every opted-in client, which would turn the dry run into a write.
 */
export function rulesPreview(
  clientId: string,
  content?: string,
): Promise<RulesPreview | null> {
  return invoke<RulesPreview | null>("rules_preview", {
    clientId,
    content: content ?? null,
  });
}

/** Re-apply the active set to every opted-in client. */
export function rulesApply(): Promise<RulesView> {
  return invoke<RulesView>("rules_apply");
}

/** Overwrite ONE client's file from the set (its drift card's action); everything else reconciles. */
export function rulesApplyClient(clientId: string): Promise<RulesView> {
  return invoke<RulesView>("rules_apply_client", { clientId });
}

// Project-level rules (SBS-1037). Registered folders only; written only by `rulesProjectApply`.
export function rulesProjectAdd(path: string): Promise<RulesView> {
  return invoke<RulesView>("rules_project_add", { path });
}
export function rulesProjectRemove(id: string): Promise<RulesView> {
  return invoke<RulesView>("rules_project_remove", { id });
}
export function rulesProjectSetSet(id: string, setId?: string): Promise<RulesView> {
  return invoke<RulesView>("rules_project_set_set", { id, setId: setId ?? null });
}
export function rulesProjectSetFileEnabled(
  id: string,
  key: string,
  enabled: boolean,
): Promise<RulesView> {
  return invoke<RulesView>("rules_project_set_file_enabled", { id, key, enabled });
}
export function rulesProjectApply(id: string): Promise<RulesView> {
  return invoke<RulesView>("rules_project_apply", { id });
}
export function rulesProjectPreview(
  id: string,
  key: string,
): Promise<RulesPreview | null> {
  return invoke<RulesPreview | null>("rules_project_preview", { id, key });
}

/** Rules files the detected clients already have, for "Start from a file". Read-only. */
export function rulesImportCandidates(): Promise<RulesImportCandidate[]> {
  return invoke<RulesImportCandidate[]>("rules_import_candidates");
}

/**
 * Read one file as the seed for a new set. Read-only: nothing is saved and the file is left as it
 * was; the caller puts the text in the editor for the user to review and save.
 */
export function rulesImportFile(
  path: string,
  clientName?: string,
): Promise<RulesImportedFile> {
  return invoke<RulesImportedFile>("rules_import_file", {
    path,
    clientName: clientName ?? null,
  });
}

export interface TeamPushPreview {
  baseVersion: number;
  localFingerprint: string;
  added: string[];
  changed: string[];
  removed: string[];
}

/** Admin: compare the local server export with the team's current shared server list. */
export function teamPushPreview(): Promise<TeamPushPreview> {
  return invoke<TeamPushPreview>("team_push_preview");
}

/** Admin: apply an explicitly previewed shared-server replacement; returns version. */
export function teamPush(preview: TeamPushPreview): Promise<number> {
  return invoke<number>("team_push", {
    baseVersion: preview.baseVersion,
    localFingerprint: preview.localFingerprint,
  });
}

/** Probe every supported MCP client and read its current server configuration. */
export function detectClients(): Promise<DetectedClient[]> {
  return invoke<DetectedClient[]>("detect_clients");
}

/** Install the Toolport gateway into a client's config, optionally scoped to a
 * profile (by name). Omit profile to expose all enabled servers.
 * Pass `force: true` after the user confirms overwriting a custom entry (SOU-406).
 * `transport` is `"stdio"` (default) or `"sharedHttp"` (SOU-407).
 * Callers that already know the live transport (Apply scope) must pass it —
 * omitting defaults to stdio and would silently downgrade Shared HTTP (WS3-2). */
export function installGateway(
  clientId: string,
  profile?: string,
  force?: boolean,
  transport?: "stdio" | "sharedHttp",
): Promise<WriteOutcome> {
  return invoke<WriteOutcome>("install_gateway", {
    clientId,
    profile: profile ?? null,
    force: force ?? false,
    transport: transport ?? "stdio",
  });
}

/** Remove the Toolport gateway from a client's config. */
export function uninstallGateway(clientId: string): Promise<WriteOutcome> {
  return invoke<WriteOutcome>("uninstall_gateway", { clientId });
}

/** Import a client's servers into Toolport, then leave the client with only the
 * Toolport gateway (optionally scoped to a profile). Backs up the config first.
 * Pass `force: true` after the user confirms overwriting a custom entry (SOU-406).
 * Pass `transport` to preserve Shared HTTP on migrate (WS3-2). */
export function migrateClient(
  clientId: string,
  profile?: string,
  force?: boolean,
  transport?: "stdio" | "sharedHttp",
): Promise<MigrateResult> {
  return invoke<MigrateResult>("migrate_client", {
    clientId,
    profile: profile ?? null,
    force: force ?? false,
    transport: transport ?? "stdio",
  });
}

/** Store a secret env value in the OS keychain. */
export function setSecret(
  serverId: string,
  key: string,
  value: string,
): Promise<Registry> {
  return invoke<Registry>("set_secret", { serverId, key, value });
}

/** Remove a secret from the keychain and the server entry. */
export function deleteSecret(serverId: string, key: string): Promise<Registry> {
  return invoke<Registry>("delete_secret", { serverId, key });
}

/** For each env key, whether a value is currently vaulted. */
export function secretStatus(
  serverId: string,
  keys: string[],
): Promise<[string, boolean][]> {
  return invoke<[string, boolean][]>("secret_status", { serverId, keys });
}

/** Store a bearer token for a remote (http) server. */
export function setAuthToken(serverId: string, token: string): Promise<void> {
  return invoke<void>("set_auth_token", { serverId, token });
}

export function clearAuthToken(serverId: string): Promise<void> {
  return invoke<void>("clear_auth_token", { serverId });
}

export function hasAuthToken(serverId: string): Promise<boolean> {
  return invoke<boolean>("has_auth_token", { serverId });
}

/** Run the OAuth 2.1 browser flow for a remote server; vaults the access token. */
export function authenticateOauth(serverId: string, url: string): Promise<void> {
  return invoke<void>("authenticate_oauth", { serverId, url });
}

/** Configure the headless OAuth client-credentials flow for an http server.
 *
 * The secret goes straight to the OS keychain; only the client id, auth method
 * and scopes are written to the registry. Pass an empty `clientSecret` to keep
 * the stored one, so editing scopes does not require re-entering it. */
export function setClientCredentials(
  serverId: string,
  clientId: string,
  clientSecret: string,
  tokenEndpointAuthMethod: string | null,
  scope: string | null,
): Promise<Registry> {
  return invoke<Registry>("set_client_credentials", {
    serverId,
    clientId,
    clientSecret,
    tokenEndpointAuthMethod,
    scope,
  });
}

/** Remove client-credentials auth: vaulted secret, minted token, and config. */
export function clearClientCredentials(serverId: string): Promise<Registry> {
  return invoke<Registry>("clear_client_credentials", { serverId });
}

/** Whether a client secret is vaulted, so the UI can show "configured" without
 * ever reading the value back. */
export function hasClientSecret(serverId: string): Promise<boolean> {
  return invoke<boolean>("has_client_secret", { serverId });
}

/** Detect what a remote server needs to connect (none/oauth/token) + guidance. */
export function probeAuth(url: string): Promise<AuthInfo> {
  return invoke<AuthInfo>("probe_auth", { url });
}

/** Open Toolport's data directory (registry, logs, audit) in the OS file manager. */
export function openDataDir(): Promise<void> {
  return invoke<void>("open_data_dir");
}

/** Serialize the user's servers into a shareable setup (no secret values),
 * optionally labelled with a name + description. */
export function exportConfig(
  name: string | undefined,
  description: string | undefined,
  // Required snapshot. `[]` means share nothing; never omit this or default it
  // to empty, or a caller would silently export zero servers.
  serverIds: string[],
): Promise<string> {
  return invoke<string>("export_config", {
    name: name ?? null,
    description: description ?? null,
    serverIds,
  });
}

/** Write the shareable setup to a file on disk (path from a save dialog). */
export function exportConfigToPath(
  path: string,
  name: string | undefined,
  description: string | undefined,
  serverIds: string[],
): Promise<void> {
  return invoke<void>("export_config_to_path", {
    path,
    name: name ?? null,
    description: description ?? null,
    serverIds,
  });
}

/** Export the audit/activity log to a file (path from a save dialog). */
export function exportAuditToPath(path: string, format: "csv" | "json"): Promise<void> {
  return invoke<void>("export_audit_to_path", { path, format });
}

/** Turn a shareable setup (from exportConfig) into a toolport.app/s/<id> link. */
export function shareStack(setupJson: string): Promise<string> {
  return invoke<string>("share_stack", { setupJson });
}

/** Fetch a shared setup's JSON by id (resolving a toolport://import?s=<id> link). */
export function fetchSharedSetup(id: string): Promise<string> {
  return invoke<string>("fetch_shared_setup", { id });
}

/** Claim a share id captured from a deep link before the UI was listening. */
export function takePendingShared(): Promise<string | null> {
  return invoke<string | null>("take_pending_shared");
}

/** Claim a tray approvals request captured before the frontend was listening. */
export function takePendingTrayApprovals(): Promise<boolean> {
  return invoke<boolean>("take_pending_tray_approvals");
}

/** Import a shared setup, adding servers not already present. */
export function importConfig(json: string): Promise<Registry> {
  return invoke<Registry>("import_config", { json });
}

/** Read a shared-setup file from disk (path from an open dialog), size-capped. */
export function readSetupFile(path: string): Promise<string> {
  return invoke<string>("read_setup_file", { path });
}

/** Parse a shared setup and report what it would add, without importing. */
export function previewImport(json: string): Promise<ImportItem[]> {
  return invoke<ImportItem[]>("preview_import", { json });
}

/** Enable or disable every server in a profile at once. */
export function setAllEnabled(profileId: string, enabled: boolean): Promise<Registry> {
  return invoke<Registry>("set_all_enabled", { profileId, enabled });
}

/** Load Toolport's registry (servers + profiles). */
export function getRegistry(): Promise<Registry> {
  return invoke<Registry>("get_registry");
}

/** One-time notice after the registry was recovered from `.bak` on launch. */
export interface RegistryRecoveryNotice {
  recoveredAtMs: number;
  reason: string;
  quarantinePath?: string | null;
}

export function takeRegistryRecoveryNotice(): Promise<RegistryRecoveryNotice | null> {
  return invoke<RegistryRecoveryNotice | null>("take_registry_recovery_notice");
}

/** Pull reviewed servers from every detected client into the registry. */
export function importServers(selected?: string[]): Promise<Registry> {
  return invoke<Registry>("import_servers", { selected });
}

/** Preview every detected-client server the bulk import would add. */
export function previewImportServers(): Promise<ImportItem[]> {
  return invoke<ImportItem[]>("preview_import_servers");
}

/** Parse a pasted config snippet (JSON/TOML/YAML/CLI), auto-detecting format. */
export function parseServerSnippet(text: string): Promise<ParsedSnippetServer[]> {
  return invoke<ParsedSnippetServer[]>("parse_server_snippet", { text });
}

export function addServer(entry: ServerEntry): Promise<Registry> {
  return invoke<Registry>("add_server", { entry });
}

/** Add a catalog entry as a registry server (the user vaults any keys after). */
export function addCatalogServer(entry: CatalogEntry): Promise<Registry> {
  const server: ServerEntry = {
    id: "",
    name: entry.name,
    transport: entry.transport,
    command: entry.command,
    args: entry.args,
    env: entry.envKeys.map((key) => ({ key, value: null, secret: true })),
    url: entry.url,
    source: `catalog:${entry.source}`,
  };
  return addServer(server);
}

export function updateServer(entry: ServerEntry): Promise<Registry> {
  return invoke<Registry>("update_server", { entry });
}

export function removeServer(id: string): Promise<Registry> {
  return invoke<Registry>("remove_server", { id });
}

/// `reviewed` asserts the member saw the Teams review dialog for the definition
/// being enabled. The backend refuses to enable a team server that needs review
/// without it, so pass it ONLY from that dialog's confirm handler.
export function setServerEnabled(
  profileId: string,
  serverId: string,
  enabled: boolean,
  reviewed = false,
): Promise<Registry> {
  return invoke<Registry>("set_server_enabled", {
    profileId,
    serverId,
    enabled,
    reviewed,
  });
}

export function createProfile(name: string): Promise<Registry> {
  return invoke<Registry>("create_profile", { name });
}

export function deleteProfile(id: string): Promise<Registry> {
  return invoke<Registry>("delete_profile", { id });
}

export function setActiveProfile(id: string): Promise<Registry> {
  return invoke<Registry>("set_active_profile", { id });
}

/** Set (or clear with `null`) a profile's tool-granular scope for one server (SOU-189):
 * the only original tool names that profile exposes on that server. `null`/empty = all. */
export function setProfileServerTools(
  profileId: string,
  serverId: string,
  tools: string[] | null,
): Promise<Registry> {
  return invoke<Registry>("set_profile_server_tools", {
    profileId,
    serverId,
    tools,
  });
}

/** Replace the folder -> profile auto-routing mappings (SOU-188). */
export function setFolderProfiles(mappings: FolderProfile[]): Promise<Registry> {
  return invoke<Registry>("set_folder_profiles", { mappings });
}

/** OS launch-at-login. Linux AppImage sessions write `$APPIMAGE`, not the FUSE mount. */
export function isAutostartEnabled(): Promise<boolean> {
  return invoke<boolean>("is_launch_at_login_enabled");
}

export function enableAutostart(): Promise<void> {
  return invoke<void>("enable_launch_at_login");
}

export function disableAutostart(): Promise<void> {
  return invoke<void>("disable_launch_at_login");
}

// ---- Native-agent hook sensor (SBS-822) ----
//
// Records what an agent does OUTSIDE the gateway (Bash, Edit, Read). Off until the user turns it
// on. Every mutating call returns the refreshed view, so the tab never re-fetches to stay honest.

/** Current state: the opt-in, the events registered, and every Claude Code profile found. */
// ---- Native permission policy for Claude Code (SBS-1058) ----

export function agentPermissionsView(): Promise<PermissionsView> {
  return invoke<PermissionsView>("agent_permissions_view");
}
export function agentPermissionsSetEnabled(enabled: boolean): Promise<PermissionsView> {
  return invoke<PermissionsView>("agent_permissions_set_enabled", { enabled });
}
export function agentPermissionsSetRules(
  rules: PermissionRule[],
): Promise<PermissionsView> {
  return invoke<PermissionsView>("agent_permissions_set_rules", { rules });
}
/** Dry run with the given rules (an unsaved policy) or, when omitted, the saved one. */
export function agentPermissionsPreview(
  rules?: PermissionRule[],
): Promise<PermissionsPreview[]> {
  return invoke<PermissionsPreview[]>("agent_permissions_preview", {
    rules: rules ?? null,
  });
}

export function hooksView(): Promise<HooksViewData> {
  return invoke<HooksViewData>("hooks_view");
}

/** Turn the sensor on or off. Turning it off removes it from every profile Toolport wrote. */
export function hooksSetEnabled(enabled: boolean): Promise<HooksViewData> {
  return invoke<HooksViewData>("hooks_set_enabled", { enabled });
}

/**
 * Dry-run the write for every profile: the exact before/after bytes, writing nothing anywhere.
 *
 * The `after` text comes from the same renderer the real write uses, so comments and formatting
 * shown here are what actually survives.
 */
export function hooksPreview(): Promise<HooksPreview[]> {
  return invoke<HooksPreview[]>("hooks_preview");
}

/** The most recent sensor rows, newest first. */
export function hooksRecent(limit: number): Promise<HookEvent[]> {
  return invoke<HookEvent[]>("hooks_recent", { limit });
}
