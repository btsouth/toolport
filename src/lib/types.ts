export type Transport = "stdio" | "http" | "sse" | "unknown";

/** The main content views, selected from the sidebar. */
export type View = "servers" | "clients" | "activity" | "catalog" | "teams" | "settings";

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
  env: { key: string; value: string | null; secret?: boolean }[];
}

/** Ownership of the gateway entry under our name in a client config (SOU-406). */
export type GatewayEntryState = "managed" | "customized" | "absent";

export interface DetectedClient {
  id: string;
  name: string;
  usesConnectors: boolean;
  /** Backend capability evidence; missing or unknown native search resolves to lazy. */
  discovery?: {
    nativeToolSearch: boolean | null;
    toolsListChanged: boolean | null;
    coldFullListWaitMs?: number;
    evidence: string;
  };
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
  warnings?: string[];
  path: string;
  backup: string | null;
  /** Servers Disconnect put back after an earlier "Move into gateway". */
  restored?: string[];
}

export interface ClientSetupReview {
  configPath: string;
  backupDir: string;
  revision: string;
  items: ImportItem[];
}

export interface MigrateResult {
  backupDate?: number | null;
  registry: Registry;
  imported: number;
  servers: { name: string; toolCount: number; credentialState: string }[];
  moved: string[];
  tools: { name: string; description?: string }[];
  outcome: WriteOutcome;
}

export interface AuditEntry {
  /** Approval outcomes are Activity events, never dispatched calls. */
  kind?: string;
  decision?: string;
  reason?: string;
  ts: number;
  server: string;
  /** Canonical routed ID, separate from the Activity display prefix. */
  serverId?: string;
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
  /** Untrusted client-reported name/version, for display only. */
  clientLabel?: string;
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

/** One lazy-discovery search. Byte fields are exact UTF-8 payload measurements;
 * token fields are legacy/reference estimates, never provider usage. */
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
  /** Legacy compatibility estimate of matched-schema bytes / 4; excludes lead text. */
  returnedTokens: number;
  /** Legacy compatibility estimate of searchable catalog schema bytes / 4. */
  flatTokens: number;
  /** Schema-only estimate difference, not provider tokens saved. */
  savedTokens: number;
  responseContentBytes?: number;
  matchedSchemaBytes?: number;
  catalogSchemaBytes?: number;
  estimatedResponseTokens?: number;
  estimateMethod?: "utf8_bytes_div_4";
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
  /** Retained quarantine state for the default access, supplied by Toolport. */
  toolportQuarantine?: "quarantined" | "clear" | "unknown";
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

/** The subset of JSON Schema the tool arguments form renders per argument. */
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

export interface TelemetryHealth {
  queueDropped: number;
  writeFailedRecords: number;
  writeFailures: number;
  incompleteFlushes: number;
  unavailable?: boolean;
  retainedDropped?: number;
}

export interface AuditStats {
  telemetry?: TelemetryHealth;
  gatewayNotes?: string[];
  total: number;
  errors: number;
  errorRate: number;
  servers: ServerStat[];
}

/** Cumulative catalog exposure measurements plus the legacy estimate. */
export interface SavingsSummary {
  /** Signed cl100k_base catalog delta minus discovery responses; excludes historical estimates. */
  tokensSaved: number;
  tokenizedLoads?: number;
  catalogTokenDelta?: number;
  discoveryTokens?: number;
  tokenizer?: "cl100k_base";
  listLoads: number;
  peakCatalog: number;
  sinceTs: number;
  legacyEstimatedTokensAvoided?: number;
  measuredLoads?: number;
  latestCatalogTs?: number;
  latestFullToolCount?: number;
  latestExposedToolCount?: number;
  latestFullSurfaceBytes?: number;
  latestExposedSurfaceBytes?: number;
  fullSurfaceBytes?: number;
  exposedSurfaceBytes?: number;
  avoidedSurfaceBytes?: number;
  extraExposedSurfaceBytes?: number;
  surfaceDeltaBytes?: number;
  estimatedTokensAvoided?: number;
  estimateMethod?: "utf8_bytes_div_4";
  discoveryCount?: number;
  discoveryResponseBytes?: number;
  matchedSchemaBytes?: number;
  estimatedDiscoveryTokens?: number;
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
  credentials?: { key: string; secret: boolean; present: boolean; required: boolean }[];
  unsupported?: string | null;
  envKeys?: string[];
  /** Opaque key used to confirm a detected-client import. Absent for shared setups. */
  key?: string;
  name: string;
  transport: Transport;
  command: string | null;
  args: string[];
  url: string | null;
  /** False if a server with this name is already present (import skips it). */
  isNew: boolean;
  updates?: string[];
}

export interface CatalogSearch {
  entries: CatalogEntry[];
  registryStatus: "notQueried" | "available" | "unavailable" | "timedOut";
}

/** An addable server from the catalog (curated seed or the live MCP Registry). */
export interface CatalogEntry {
  name: string;
  description: string;
  transport: Transport;
  command: string | null;
  args: string[];
  launch?: LaunchConfig | null;
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

// --- Toolport registry (source of truth) ---

export interface EnvVar {
  key: string;
  value: string | null;
  secret: boolean;
}

export interface LaunchInput {
  key: string;
  label: string;
  secret: boolean;
  required: boolean;
  /** Present only for nonsecret inputs in saved configuration. */
  value?: string | null;
}

export type ArgPart = { kind: "literal"; value: string } | { kind: "input"; key: string };
export interface ArgBinding {
  index: number;
  parts: ArgPart[];
}
export interface LaunchConfig {
  inputs: LaunchInput[];
  bindings: ArgBinding[];
  requiredEnv?: string[];
  template?: string | null;
  revision?: number | null;
}

export interface ServerEntry {
  enabled?: boolean;
  id: string;
  name: string;
  transport: Transport;
  command: string | null;
  args: string[];
  launch?: LaunchConfig | null;
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
  /** Deadline for the initial MCP initialize request, in milliseconds.
   * Unset keeps the launcher-aware transport default. */
  initializeTimeoutMs?: number | null;
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
  /** Server instructions sent to a connection scoped to this profile. Absent = inherit
   * `gatewayInstructions`, then the built-in text; empty = send none. */
  instructions?: string;
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
  clientName?: string;
  client: string | null;
  /** Untrusted initialize clientInfo label. */
  clientLabel?: string | null;
  server: string;
  tool: string;
  toolFingerprint?: string | null;
  reason:
    "destructive" | "untrusted_source" | "destructive_and_untrusted" | "pii_cross_server";
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
  defaultAccessProfileId?: string | null;
  defaultAccessContextId?: string | null;
  defaultAccessLegacyPolicy?: boolean;
  accessUpgradeNoticeDismissed?: boolean;
  /** Folder -> profile auto-routing mappings. Absent/empty = no folder routing. */
  folderProfiles?: FolderProfile[];
  /** Per-tool exposure overrides (rename / re-describe), keyed by server id then original tool name. */
  toolOverrides?: Record<string, Record<string, ToolOverride>>;
  /** Tools pinned as lazy-discovery prerequisites, keyed by server id -> original tool names. */
  pinnedTools?: Record<string, string[]>;
  /** Member safety choice. Absent values derive from retained legacy fields. */
  safetyLevel?: "off" | "ask" | "strict";
  teamMinSafetyLevel?: "off" | "ask" | "strict";
  teamForcedHumanApproval?: boolean;
  teamForcedDenyDestructive?: boolean;
  teamForcedQuarantineOnDrift?: boolean;
  teamForcedBlockOnInjection?: boolean;
  /** Retained legacy switch for registries without a safety level. */
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
  /** Replacement for the gateway's built-in server instructions, for profiles that set
   * none of their own. Absent = built-in text; empty = send none. */
  gatewayInstructions?: string;
  /** Code mode: advertise `toolport_run_script` so agents can orchestrate many tool
   * calls in one server-side script. Off by default; opt in under Advanced. */
  codeMode?: boolean;
  /** Per-client discovery-mode override, keyed by client id (e.g. "cursor" ->
   * "grouped"). Absent = Auto from the client capability table. */
  clientDiscovery?: Record<string, string>;
  /** Connection to a Toolport Teams server, if joined. Token lives in the keychain. */
  team?: TeamConnection | null;
  /** Per-server result-shaping budgets in bytes, keyed by server id. Absent =
   * global default; 0 = never shape (full fidelity); n = cap that server at n bytes. */
  resultBudgets?: Record<string, number>;
  /** Which profile each client was connected with, keyed by client id (e.g.
   * "cursor" -> "Billing"). Absent = that client uses the default access. */
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
  managedServerIds?: Record<string, string>;
  serverUrl: string;
  teamId: string;
  teamName?: string | null;
  accountLinked?: boolean | null;
  /** "admin" | "member" */
  role: string;
  memberName?: string | null;
  /** Last team config version pulled. */
  lastVersion?: number;
}

/** Per-client on-disk state of the org Team Instructions (spec W4/W5). */
export type InstructionsApplyState =
  "applied" | "stale" | "blocked_override" | "too_long" | "unsupported" | "error";

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

export function activeProfile(registry: Registry): Profile | undefined {
  return (
    registry.profiles.find((p) => p.id === registry.activeProfileId) ??
    registry.profiles[0]
  );
}

export function isEnabled(registry: Registry, serverId: string): boolean {
  if (registry.version < 3)
    return activeProfile(registry)?.enabledServerIds.includes(serverId) ?? false;
  return registry.servers.find((s) => s.id === serverId)?.enabled ?? false;
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

/** Existing HTTP shims are preserved until the owner confirms migration. */
export function hasLegacyBearerArgv(server: McpServer): boolean {
  return (
    isGatewayDetected(server) &&
    [server.command ?? "", ...server.args].some((part) => part.includes("mcp-remote")) &&
    server.args.some(
      (arg) => /authorization:\s*bearer\s+\S+/i.test(arg) && !arg.includes("${"),
    )
  );
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
