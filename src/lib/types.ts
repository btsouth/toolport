export type Transport = "stdio" | "http" | "sse" | "unknown";

/** The main content views, selected from the sidebar. */
export type View =
  | "servers"
  | "clients"
  | "activity"
  | "catalog"
  | "playground"
  | "rules"
  | "hooks"
  | "permissions"
  | "teams"
  | "settings";

export interface McpServer {
  name: string;
  transport: Transport;
  command: string | null;
  args: string[];
  /** Env-variable names only. Values are never sent from the backend. */
  envKeys: string[];
  url: string | null;
}

/** A server parsed from a pasted config snippet. Includes env-var values. */
export interface ParsedSnippetServer {
  name: string;
  transport: Transport;
  command: string | null;
  args: string[];
  url: string | null;
  env: { key: string; value: string | null }[];
}

/** Ownership of the gateway entry under our name in a client config (SOU-406). */
export type GatewayEntryState = "managed" | "customized" | "absent";

export interface DetectedClient {
  id: string;
  name: string;
  usesConnectors: boolean;
  configPath: string;
  configExists: boolean;
  /** Whether the client app appears installed (its data dir exists), even if it
   * has no MCP config yet. Distinguishes "installed, no servers" from "not here". */
  appPresent: boolean;
  servers: McpServer[];
  /** Servers found outside the config file (e.g. Cursor plugins); read-only. */
  pluginServers: McpServer[];
  gatewayInstalled: boolean;
  /** First-class ownership: managed by us, hand-customized, or absent (SOU-406). */
  entryState: GatewayEntryState;
  error: string | null;
}

export interface WriteOutcome {
  path: string;
  backup: string | null;
}

export interface MigrateResult {
  registry: Registry;
  imported: number;
  moved: string[];
}

export interface AuditEntry {
  ts: number;
  server: string;
  tool: string;
  ok: boolean;
  /** How long the call took, ms. Absent for records logged before timing. */
  durationMs?: number;
  /** How long a gated call waited for a human approval decision, ms. Present on
   * `kind:"approval"` records instead of durationMs (which is downstream exec time). */
  heldMs?: number;
  /** Short failure message for a failed call (never args or result data). */
  error?: string;
  /** A destructive call held for confirmation (not a success and not an error). */
  held?: boolean;
  /** The registered HTTP client that made the call, when known. Absent for the
   * local desktop client and legacy/open tokens. */
  client?: string;
  /** Human-readable name of the registered HTTP client, when known. */
  clientName?: string;
  /** How many values this call's result had pseudonymized. Absent when PII redaction was
   * off for the call — which is deliberately distinct from `0` ("it ran, found nothing").
   * A count only; the values themselves never enter the audit log. */
  piiReplaced?: number;
  /** Present (and always `true`) when the pass left values in the clear: the session map
   * hit its cap, or the result exceeded the scan cap. Pseudonymization fails OPEN by
   * design, so this is the case the row most needs to show. */
  piiIncomplete?: boolean;
}

/** One live-inspection capture: a tool call's request args and response, plus timing.
 * Only present while live inspection is on. `request`/`response` are the raw captured
 * bodies (or a "<truncated N bytes>" marker string when the body exceeded the size cap). */
export interface InspectEntry {
  ts: number;
  client?: string;
  clientName?: string;
  server: string;
  tool: string;
  request: unknown;
  response: unknown;
  ok: boolean;
  durationMs?: number;
}

/** One lazy-discovery search: what the model searched for and what came back, with
 * the ground-truth token cost of the results vs. loading the whole catalog. */
export interface SearchTrace {
  ts: number;
  client?: string;
  query: string;
  server?: string;
  top: string;
  names: string[];
  returned: number;
  total: number;
  /** Full count of appended recovery candidates. Absent on older traces. */
  fallbacks?: number;
  /** Tool-definition tokens the returned schemas cost this turn (≈). */
  returnedTokens: number;
  /** Tool-definition tokens advertising the whole (scoped) catalog would cost (≈). */
  flatTokens: number;
  /** flatTokens - returnedTokens: the context kept out of the model this turn. */
  savedTokens: number;
  /** The loop-breaker fired: repeated searches kept landing on the same top tool. */
  escalated: boolean;
  /** Ranker used: keyword-only (`lexical`) or semantic re-rank. Absent on older traces. */
  mode?: "lexical" | "semantic";
  /** Per-result explanation, in result order: why each tool surfaced. Absent on older
   * traces (fall back to `names`). */
  ranking?: SearchTraceRank[];
}

/** Why one tool surfaced in a lazy-discovery search. */
export interface SearchTraceRank {
  name: string;
  /** 1-based position in the returned results. */
  rank: number;
  /** Query terms this tool matched, e.g. "products (name)". Empty when it surfaced
   * without a keyword hit (a semantic match or a pinned prerequisite). */
  matched: string[];
  /** A pinned prerequisite prepended ahead of the ranked matches, not a query hit. */
  pinned: boolean;
  /** A zero-score recovery candidate appended because the direct search was weak. */
  fallback?: boolean;
}

/** One exposed tool's verifiable identity: the model-visible alias joined back to its
 * source server + profiles, with the integrity fingerprint and first-seen/last-changed. */
export interface ToolIdentity {
  alias: string;
  serverId: string;
  serverName: string;
  profiles: string[];
  upstream: string;
  fingerprint: string;
  firstSeen: number;
  lastChanged: number;
  quarantined: boolean;
}

export interface ProbeResult {
  serverId: string;
  ok: boolean;
  toolCount: number;
  error: string | null;
  /** Failure looks like missing credentials (remote 401/403, or unvaulted secret). */
  authRequired: boolean;
}

/** A tool as advertised by a downstream MCP server (raw `tools/list` entry). */
export interface McpTool {
  name: string;
  description?: string;
  inputSchema?: {
    type?: string;
    properties?: Record<string, JsonSchemaProp>;
    required?: string[];
  };
  /** MCP tool annotations. `destructiveHint` marks a tool that deletes/writes;
   * some servers also emit it at the top level, so both are tolerated. */
  annotations?: { destructiveHint?: boolean; [k: string]: unknown };
  destructiveHint?: boolean;
  /** Server-declared icons (SEP-973). Already transit the gateway untouched. Only
   * `data:` sources are ever rendered — see `pickIconSrc` for why a remote URL is a
   * request rather than a picture. */
  icons?: { src: string; mimeType?: string; sizes?: string }[];
}

/** A resource as advertised by a downstream server (raw `resources/list` entry). */
export interface McpResource {
  uri: string;
  name?: string;
  title?: string;
  description?: string;
  mimeType?: string;
}

/** A prompt as advertised by a downstream server (raw `prompts/list` entry). */
export interface McpPrompt {
  name: string;
  title?: string;
  description?: string;
  arguments?: Array<{ name: string; description?: string; required?: boolean }>;
}

/** The subset of JSON Schema the playground form renders per argument. */
export interface JsonSchemaProp {
  type?: string | string[];
  description?: string;
  enum?: unknown[];
  default?: unknown;
  items?: JsonSchemaProp;
}

/** Raw MCP `tools/call` result: content blocks plus an error flag. */
export interface ToolCallResult {
  content?: Array<{ type: string; text?: string; [k: string]: unknown }>;
  isError?: boolean;
  [k: string]: unknown;
}

/** Per-tool aggregate within a server (calls, error rate, latency). */
export interface ToolStat {
  tool: string;
  calls: number;
  errors: number;
  errorRate: number;
  avgMs: number | null;
  p95Ms: number | null;
  lastTs: number;
}

/** Per-server aggregate from the audit log (calls, error rate, latency). */
export interface ServerStat {
  server: string;
  calls: number;
  errors: number;
  errorRate: number;
  avgMs: number | null;
  p95Ms: number | null;
  lastTs: number;
  /** Per-tool breakdown, busiest first. */
  tools: ToolStat[];
}

export interface AuditStats {
  total: number;
  errors: number;
  errorRate: number;
  servers: ServerStat[];
}

/** Cumulative tool-definition tokens lazy discovery kept out of client context. */
export interface SavingsSummary {
  tokensSaved: number;
  listLoads: number;
  peakCatalog: number;
  sinceTs: number;
  /** Downstream tool round-trips collapsed into single code-mode run_script calls.
   * Absent in older savings logs written before code mode. */
  roundTripsSaved?: number;
}

export interface AuthInfo {
  kind: "none" | "oauth" | "token" | "unknown";
  vendor: string | null;
  tokenUrl: string | null;
  instructions: string | null;
}

/** One server a shared setup would add, shown for review before importing. */
export interface ImportItem {
  /** Opaque key used to confirm a detected-client import. Absent for shared setups. */
  key?: string;
  name: string;
  transport: Transport;
  command: string | null;
  args: string[];
  url: string | null;
  /** False if a server with this name is already present (import skips it). */
  isNew: boolean;
}

/** An addable server from the catalog (curated seed or the live MCP Registry). */
export interface CatalogEntry {
  name: string;
  description: string;
  transport: Transport;
  command: string | null;
  args: string[];
  url: string | null;
  envKeys: string[];
  source: "curated" | "registry" | "user";
  homepage: string | null;
  /** Publishing namespace from the registry (who published it), if known. */
  publisher?: string | null;
  /** Curated browse-view grouping (e.g. "Databases"); absent for registry/user. */
  category?: string;
  /** Direct link to create this server's credential (provider token page). */
  credentialsUrl?: string;
  /** One-line hint on what credential to create (scopes, what to paste). */
  setupHint?: string;
  /** Placeholder for URL field when self-hosted (opens dialog on add). */
  urlHint?: string;
}

/** A curated "stack": a role-based bundle of catalog servers for guided setup. */
export interface Stack {
  id: string;
  name: string;
  description: string;
  /** The stack's servers, resolved to full catalog entries (with cred hints). */
  servers: CatalogEntry[];
}

// --- Toolport registry (source of truth) ---

export interface EnvVar {
  key: string;
  value: string | null;
  secret: boolean;
}

export interface ServerEntry {
  id: string;
  name: string;
  transport: Transport;
  command: string | null;
  args: string[];
  env: EnvVar[];
  url: string | null;
  source: string | null;
  /** Original tool names switched off; hidden from clients by the gateway. */
  disabledTools?: string[];
  /** Working directory for a stdio server. Unset = inherit the gateway's cwd.
   * `~` and `${VAR}` are expanded. Lets a server run in a project dir (#239). */
  cwd?: string | null;
  /** Headless outbound OAuth (SBS-524). Present = this server uses the
   * client-credentials flow instead of the interactive browser one. */
  clientCredentials?: ClientCredentials | null;
  /** Total deadline for each HTTP request, in milliseconds.
   * Valid values are 1 ms through 24 hours; unset preserves the 30-second default. */
  requestTimeoutMs?: number | null;
}

/** Non-secret client-credentials config. The client SECRET is never here: it
 * lives in the OS keychain, because this object is written to registry.json and
 * included in config backups and exports. */
export interface ClientCredentials {
  clientId: string;
  /** `client_secret_basic` | `client_secret_post` | `private_key_jwt`.
   * Unset = negotiate from what the authorization server advertises. */
  tokenEndpointAuthMethod?: string | null;
  /** Space-delimited scopes. Unset = use what discovery advertises. */
  scope?: string | null;
}

export interface Profile {
  id: string;
  name: string;
  enabledServerIds: string[];
  /** Tool-granular scope ("FeatureSet"): server id -> the only tool names this profile
   * exposes on that server. A server absent = all its tools; empty/absent = server-granular
   * only. Enforced in tools/list, search, and the call guard. */
  toolScope?: Record<string, string[]>;
}

/** A folder -> profile auto-routing mapping (SOU-188): a client whose reported project
 * root is `path` or a descendant auto-scopes to `profile` (a profile id or name), the
 * longest matching path wins. Empty list = no folder routing. */
export interface FolderProfile {
  path: string;
  profile: string;
}

/** A tool call held awaiting a human decision (the HITL approval queue). */
export interface PendingApproval {
  id: string;
  client: string | null;
  server: string;
  tool: string;
  toolFingerprint?: string | null;
  reason:
    | "destructive"
    | "untrusted_source"
    | "destructive_and_untrusted"
    | "persistent_code_write"
    | "pii_cross_server";
  arguments: unknown;
  /** A screened URL-mode elicitation brokered by the desktop because the MCP host
   * did not declare URL elicitation support. */
  urlElicitation?: {
    url: string;
    origin: string;
    message: string;
  } | null;
  /** Pseudonymized values this call would send to a server that never produced them.
   * Present only for `reason: "pii_cross_server"`.
   *
   * `value` is REAL, un-pseudonymized PII. It reaches this window and nowhere else —
   * a person cannot judge the release without seeing what is being released. It must
   * never be logged, persisted, or echoed anywhere the model can read. */
  piiRelease?: {
    server: string;
    values: { token: string; value: string; origins: string[] }[];
  } | null;
  /** Wall-clock epoch-ms when this call auto-denies; the overlay counts down to it. */
  deadlineMs: number;
}

/** A strong, repeated orchestration pattern the gateway queued for the passive save
 * area in Settings. Self-contained: approving persists from this payload alone. */
export interface RoutineSuggestion {
  suggestedName: string;
  source: string;
  inputSchema: unknown;
  limits: Record<string, unknown>;
  definitionFingerprint: string;
  evidence: {
    sourceRunId: string;
    executedAtMs: number;
    calls: number;
    observedDependencies: { name: string; toolFingerprint?: string | null }[];
    validationVersion: number;
    riskClass: string;
    /** Absent = immutable_run (the source really executed). */
    provenance?: "immutable_run" | "synthesized_from_observed_calls";
  };
  intermediateBytes: number;
}

/** A tool the user allowed to skip human approval (Settings "Allowed tools" list). */
export interface AllowedTool {
  key: string;
  server: string;
  tool: string;
  /** true = persisted ("always"); false = only for this app session. */
  persistent: boolean;
}

/** A per-tool exposure override, keyed in `Registry.toolOverrides` by server id then
 * original tool name. Rename and/or replace the description clients see; the call still
 * routes to the original downstream tool. */
export interface ToolOverride {
  name?: string;
  description?: string;
}

export interface Registry {
  version: number;
  servers: ServerEntry[];
  profiles: Profile[];
  activeProfileId: string | null;
  /** Folder -> profile auto-routing mappings. Absent/empty = no folder routing. */
  folderProfiles?: FolderProfile[];
  /** Per-tool exposure overrides (rename / re-describe), keyed by server id then original tool name. */
  toolOverrides?: Record<string, Record<string, ToolOverride>>;
  /** Tools pinned as lazy-discovery prerequisites, keyed by server id -> original tool names. */
  pinnedTools?: Record<string, string[]>;
  /** Global switch: hide and block every destructive-hinted tool. */
  denyDestructive?: boolean;
  /** Per-call confirmation: intercept destructive tools with a preview + token. */
  confirmDestructive?: boolean;
  /** Human-in-the-loop: hold a gated tool call until a person approves it in the app. */
  humanApproval?: boolean;
  /** Live request/response inspection: capture each tool call's args + result into a
   * small, separate, ephemeral local ring (last 50 calls) for the Activity inspector.
   * Off by default; never touches the audit log. */
  liveInspect?: boolean;
  /** Quarantine-on-drift: block a high-risk tool that changed until re-approved. */
  quarantineOnDrift?: boolean;
  /** Opt-in fail-closed content defense: block high-confidence injection hits (SOU-345). */
  blockOnInjection?: boolean;
  /** Replace PII in tool results with stable pseudonyms before the model sees them,
   * re-hydrating them on the way back out (SBS-346), but only for the server that
   * produced the value (SBS-605). Off by default. */
  piiRedaction?: boolean;
  /** Server ids exempt from block-on-injection (label only). */
  injectionBlockExempt?: Record<string, boolean>;
  /** Global switch: expose 4 meta-tools instead of the full catalog. */
  lazyDiscovery?: boolean;
  /** Global discovery mode ("full" | "lazy" | "grouped"). Takes precedence over
   * `lazyDiscovery`; absent = fall back to the `lazyDiscovery` bool. */
  discoveryMode?: string | null;
  /** Code mode: advertise `toolport_run_script` so agents can orchestrate many tool
   * calls in one server-side script. On by default (SOU-397); Settings is the kill switch. */
  codeMode?: boolean;
  /** Opt-in: let agents request saving immutable Code Mode routines. Every save still
   * requires a separate human approval. */
  allowRoutineWrites?: boolean;
  /** Per-client discovery-mode override, keyed by client id (e.g. "cursor" ->
   * "grouped"). Absent = that client inherits the global mode. */
  clientDiscovery?: Record<string, string>;
  /** Opt-in: let an agent enable/disable servers via the gateway's control tools. */
  allowAgentControl?: boolean;
  /** Connection to a Toolport Teams server, if joined. Token lives in the keychain. */
  team?: TeamConnection | null;
  /** Per-server result-shaping budgets in bytes, keyed by server id. Absent =
   * global default; 0 = never shape (full fidelity); n = cap that server at n bytes. */
  resultBudgets?: Record<string, number>;
  /** Which profile each client was connected with, keyed by client id (e.g.
   * "cursor" -> "Billing"). Absent = that client follows the active profile. */
  clientScopes?: Record<string, string>;
  /** What Toolport last wrote into each client config as its gateway entry
   * (SOU-406 ownership record). Absent key = pre-ownership install. */
  clientManagedEntries?: Record<string, ManagedEntry>;
  /** Consumers registered to reach the gateway over the HTTP/OpenAPI bridge,
   * each with its own hashed token and scope (multi-tenant bridge). */
  httpClients?: HttpClient[];
  /** Whether the supervised HTTP endpoint should return after an app restart. */
  httpBridgeEnabled?: boolean;
  /** Last port selected for the supervised HTTP endpoint. */
  httpBridgePort?: number | null;
}

/** Snapshot of the gateway entry Toolport last wrote (SOU-406/407). */
export interface ManagedEntry {
  command: string;
  args: string[];
  env: Record<string, string>;
  /** `"stdio"` (default) or `"sharedHttp"`. */
  transport?: string;
  /** Shared-HTTP MCP URL when transport is sharedHttp. */
  url?: string | null;
  updatedAt: number;
}

/** A consumer registered to reach the HTTP/OpenAPI bridge with its own token and
 * scope. The plaintext token is shown once at creation, never stored. */
export interface HttpClient {
  id: string;
  label: string;
  /** SHA-256 of the bearer token (the plaintext is never returned again). */
  tokenSha256: string;
  /** Profile this client is scoped to; empty = the full connected set. */
  profile: string;
}

/** A joined Toolport Teams server (the shared config-sync layer). */
export interface TeamConnection {
  serverUrl: string;
  teamId: string;
  /** "admin" | "member" */
  role: string;
  memberName?: string | null;
  /** Last team config version pulled. */
  lastVersion?: number;
}

/** Per-client on-disk state of the org Team Instructions (spec W4/W5). */
export type InstructionsApplyState =
  | "applied"
  | "stale"
  | "blocked_override"
  | "too_long"
  | "unsupported"
  | "error"
  /** Toolport wrote this block for the current set revision and it was edited on disk since (personal rules only). */
  | "drifted";

export interface InstructionsClientStatus {
  id: string;
  name: string;
  state: InstructionsApplyState;
}

/** The member-facing view of the org instructions on this machine (`team_instructions_status`). */
export interface InstructionsStatusView {
  content: string;
  version: number;
  clients: InstructionsClientStatus[];
}

/**
 * One of the user's own named rule sets (SBS-821). `revision` moves only when `content` changes,
 * because it rides in the marker written into each client's file.
 */
export interface RuleSet {
  id: string;
  name: string;
  content: string;
  revision: number;
}

/**
 * One client's row in the Rules tab. Reuses {@link InstructionsApplyState}: personal rules and
 * team instructions run through the same writer, so the states (and their badges) are identical.
 */
export interface RulesClientStatus {
  id: string;
  name: string;
  /** User opt-in. A client is off until turned on; nothing is written to it until then. */
  enabled: boolean;
  /** Absent when this client has no global-rules file Toolport can manage (Cursor, Warp). */
  path?: string;
  /**
   * No global rules file, but the client reads one of the files the Projects section writes
   * (Cursor, GitHub Copilot CLI), so the UI points at Projects instead of "unsupported".
   */
  projectCovered?: boolean;
  state: InstructionsApplyState;
  /** When `state` is `drifted`: the block's body as it is on disk, for the diff and Pull into set. */
  onDisk?: string;
}

/** One file Toolport can write in a registered project folder, and its state there (SBS-1037). */
export interface RulesProjectFileStatus {
  key: string;
  relPath: string;
  path: string;
  /** Display names of the detected clients that read this file in a project. */
  clients: string[];
  enabled: boolean;
  state: InstructionsApplyState;
  onDisk?: string;
}

/** One registered project folder for project-level rules (SBS-1037). */
export interface RulesProjectStatus {
  id: string;
  path: string;
  name: string;
  setId?: string;
  files: RulesProjectFileStatus[];
}

/** Everything the Rules tab renders, from one `rules_view` call. */
export interface RulesView {
  sets: RuleSet[];
  activeSetId?: string;
  clients: RulesClientStatus[];
  projects: RulesProjectStatus[];
}

/** A dry run of one client's write, shown before the first apply. Nothing is written to get it. */
export interface RulesPreview {
  clientId: string;
  path: string;
  /** `ownedFile` = Toolport owns the file; `sentinelBlock` = it owns only the marked span. */
  strategy: "ownedFile" | "sentinelBlock";
  before: string;
  after: string;
  state: InstructionsApplyState;
}

/** A rules file already on this machine that a new set can start from (SBS-1035). */
export interface RulesImportCandidate {
  clientId: string;
  clientName: string;
  path: string;
  bytes: number;
}

/**
 * What importing a file yields: the user's own text with anything Toolport wrote removed. Nothing
 * is saved by the import and the source file is not touched; the UI seeds a draft with it.
 */
export interface RulesImportedFile {
  path: string;
  name: string;
  content: string;
  strippedOurs: boolean;
}

export function activeProfile(registry: Registry): Profile | undefined {
  return (
    registry.profiles.find((p) => p.id === registry.activeProfileId) ??
    registry.profiles[0]
  );
}

export function isEnabled(registry: Registry, serverId: string): boolean {
  return activeProfile(registry)?.enabledServerIds.includes(serverId) ?? false;
}

/** Whether a registry entry is Toolport's own gateway. It's infrastructure, not a
 * proxied server, so it shouldn't appear as a manageable server in the UI.
 * Mirrors `is_gateway_server` in the Rust backend. */
function isGatewayIdentity(id: string, name: string, command: string | null): boolean {
  const normalizedId = id.toLowerCase();
  const normalizedName = name.toLowerCase();
  const normalizedCommand = command?.toLowerCase() ?? "";
  return (
    normalizedId === "conduit" ||
    normalizedId === "toolport" ||
    normalizedName === "conduit" ||
    normalizedName === "toolport" ||
    // Current binary name and the pre-rename one, so an entry written by an older
    // Toolport is still recognized as the gateway.
    normalizedCommand.includes("toolport-gateway") ||
    normalizedCommand.includes("conduit-gateway")
  );
}

export function isGatewayServer(server: ServerEntry): boolean {
  return isGatewayIdentity(server.id, server.name, server.command);
}

/** Whether a server read from a client's own config (a detected `McpServer`, which
 * has no registry id) is Toolport's own gateway entry. Recognizes the pre-rename
 * `conduit` name too. Mirrors `detected_is_gateway` in the Rust backend. */
export function isGatewayDetected(server: McpServer): boolean {
  return isGatewayIdentity(server.name, server.name, server.command);
}

/** Servers a client has (config + plugins) that Toolport doesn't manage yet.
 * These are the only client-side entries worth surfacing - they're import
 * candidates. Toolport's own gateway entry is never importable. */
export function importableServers(
  client: DetectedClient,
  registry: Registry | null,
): McpServer[] {
  const have = new Set((registry?.servers ?? []).map((s) => s.name.toLowerCase()));
  return [...client.servers, ...client.pluginServers].filter(
    (server) =>
      !isGatewayIdentity(server.name, server.name, server.command) &&
      !have.has(server.name.toLowerCase()),
  );
}

/**
 * One Claude Code profile's `settings.json` and whether the hook sensor is in it (SBS-822).
 *
 * A machine routinely has more than one: `CLAUDE_CONFIG_DIR` picks a profile per shell, so
 * `~/.claude` and `~/.claude-work` are both real and both need the sensor, or the one Toolport
 * missed reports nothing.
 */
export interface HookProfileStatus {
  path: string;
  installed: boolean;
  /** Why this profile could not be read or written. A profile that is simply not installed has
   *  no error; an unreadable one says so instead of quietly looking "off". */
  error?: string;
}

/** Everything the Hooks tab renders, from one `hooks_view` call. */
export interface HooksView {
  enabled: boolean;
  /** The harness lifecycle events the sensor registers, so the UI can name them exactly. */
  events: string[];
  profiles: HookProfileStatus[];
  /** Absent when no gateway binary has been published yet, which is the one thing that stops
   *  the sensor being installable. */
  binary?: string;
}

// ---- Native permission policy for Claude Code (SBS-1058) ----

export type PermissionAction = "allow" | "ask" | "deny";

/** One rule in Claude Code's own syntax (`Bash(rm -rf *)`, `Read(./.env)`, `mcp__server__tool`). */
export interface PermissionRule {
  pattern: string;
  action: PermissionAction;
}

export interface PermissionProfileStatus {
  path: string;
  /** applied | stale | off | error */
  state: string;
  /** How many of the policy's rules Toolport itself added to this file. */
  added: number;
  error?: string;
}

export interface PermissionPreset {
  label: string;
  rules: PermissionRule[];
}

export interface PermissionsView {
  enabled: boolean;
  rules: PermissionRule[];
  profiles: PermissionProfileStatus[];
  presets: PermissionPreset[];
}

export interface PermissionsPreview {
  path: string;
  before: string;
  after: string;
  error?: string;
}

/** A dry run of one profile's write. Nothing is written to produce it. */
export interface HooksPreview {
  path: string;
  /** The file as it is now; empty when the profile has no settings file yet. */
  before: string;
  after: string;
  /**
   * Why this profile has no dry run, when that is the case. Absent on a healthy one.
   * Mirrors `ProfileStatus.error`: one profile the backend cannot parse must not hide
   * the preview for the profiles it can.
   */
  error?: string;
}

/** One recorded sensor row. Deliberately loose: SBS-823 owns the shape, this tab only counts. */
export interface HookEvent {
  ts?: number;
  event?: string;
  agent?: string;
  tool?: string;
  sessionId?: string;
  /** The folder the agent was working in. A path, never its contents. */
  cwd?: string;
  /** Canonical fingerprint of the tool input. Cannot be turned back into the input. */
  argsHash?: string;
  /** Absent means UNKNOWN, never success. */
  ok?: boolean;
  malformed?: boolean;
  /** Guard rows (SBS-1059): the Cursor event, what the hook answered, and the rule that decided. */
  hookEvent?: string;
  decision?: string;
  rule?: string | null;
  mode?: string;
  /** In observe mode: what the answer WOULD have been. */
  wouldBe?: string | null;
}

// ---- Guard hook for Cursor (SBS-1059) ----

export type GuardMode = "off" | "observe" | "enforce";

export interface GuardProfile {
  path: string;
  installed: boolean;
  error?: string;
}

export interface GuardView {
  cursorMode: GuardMode;
  cursor?: GuardProfile;
  events: string[];
  binary?: string;
}

export interface GuardPreview {
  path: string;
  before: string;
  after: string;
  error?: string;
}
