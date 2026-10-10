import type { Registry } from "./types";
export interface AccountStatus {
  personalSync?: boolean;
  plan: string;
  trialActive: boolean;
  trialEndsAt: number | null;
  freeSyncGraceEndsAt: number | null;
  deviceId: string;
  canReceiveConfig: boolean;
  reason: string | null;
}
export interface PersonalSyncState {
  lastSyncedAt?: number | null;
  error?: string | null;
  pending?: Record<string, { localId: string; after: unknown }>;
  conflicts?: Record<string, unknown>;
  conflictVersions?: Record<string, string>;
  publishErrors?: Record<string, string>;
  warnings?: Record<string, string>;
  signInRequired?: boolean;
  chooseLocalServers?: boolean;
}
export function isPersonalSync(registry: Registry | null | undefined): boolean {
  return (
    registry?.team?.role === "admin" && registry.team.accountStatus?.personalSync === true
  );
}
export function accountStatusText(status: AccountStatus, now = Date.now()): string[] {
  const lines = [
    status.plan === "pro"
      ? "Pro · unlimited devices"
      : status.plan === "free"
        ? "Free · 1 person, 1 device"
        : status.plan,
  ];
  if (status.trialActive && status.trialEndsAt != null)
    lines.push(
      `${Math.max(0, Math.ceil((status.trialEndsAt - now) / 86400000))} trial days left`,
    );
  if (status.freeSyncGraceEndsAt != null)
    lines.push(
      status.freeSyncGraceEndsAt > now
        ? `Every device keeps syncing until ${new Date(status.freeSyncGraceEndsAt).toLocaleString()}`
        : "Sync grace period ended.",
    );
  if (!status.canReceiveConfig)
    lines.push(
      status.reason ||
        "This device cannot receive your setup. Choose your active device in Your account.",
    );
  return lines;
}
export function planName(plan: string | null | undefined): string {
  const names: Record<string, string> = { pro: "Pro", team: "Team", free: "Free" };
  return names[plan?.toLowerCase() ?? ""] ?? (plan || "unknown");
}
export function syncSignInUrl(origin: string): string {
  const url = new URL(origin.trim());
  url.search = "intent=pro&from=app-sync";
  url.hash = "";
  return url.toString();
}
export const SYNC_SIGN_IN_URL = "https://teams.toolport.app/?intent=pro&from=app-sync";

// Mirrors risky_sync_env and looks_machine_local in personal_sync.rs. A Rust
// test checks that these lists match.
const RISKY_SYNC_ENV_PREFIXES = [
  "GIT_CONFIG_",
  "DYLD_",
  "NPM_CONFIG_",
  "UV_INDEX",
  "UV_PYTHON",
];
const RISKY_SYNC_ENV = new Set([
  "PATH",
  "LD_PRELOAD",
  "LD_LIBRARY_PATH",
  "LD_AUDIT",
  "NODE_OPTIONS",
  "NODE_PATH",
  "PYTHONPATH",
  "PYTHONSTARTUP",
  "JAVA_TOOL_OPTIONS",
  "_JAVA_OPTIONS",
  "JDK_JAVA_OPTIONS",
  "PERL5OPT",
  "PERL5LIB",
  "RUBYOPT",
  "RUBYLIB",
  "PYTHONHOME",
  "UV_INDEX",
  "PIP_CONFIG_FILE",
  "NODE_EXTRA_CA_CERTS",
  "LD_DEBUG",
  "LD_PROFILE",
  "GIT_CONFIG_SYSTEM",
  "GIT_CONFIG_GLOBAL",
  "GIT_CONFIG_COUNT",
  "GIT_EXEC_PATH",
  "PYTHONUSERBASE",
  "PYTHONINSPECT",
  "PIP_INDEX_URL",
  "PIP_EXTRA_INDEX_URL",
  "UV_INDEX_URL",
  "UV_EXTRA_INDEX_URL",
  "UV_DEFAULT_INDEX",
  "GIT_SSH",
  "GIT_SSH_COMMAND",
  "GIT_ASKPASS",
  "GIT_PROXY_COMMAND",
  "SSH_ASKPASS",
  "SSH_ASKPASS_REQUIRE",
  "PIP_FIND_LINKS",
  "PIP_TRUSTED_HOST",
  "UV_FIND_LINKS",
  "UV_INSECURE_HOST",
  "DOCKER_HOST",
  "RUSTC_WRAPPER",
  "BASH_ENV",
  "ENV",
  "ZDOTDIR",
  "GCONV_PATH",
]);
const CREDENTIAL_QUERY_KEYS = new Set([
  "token",
  "access_token",
  "api_key",
  "apikey",
  "api-key",
  "password",
  "secret",
  "key",
  "auth",
  "authorization",
  "signature",
]);
/** A variable that changes how programs run never syncs. */
export function riskySyncEnv(key: string): boolean {
  const name = key.trim().toUpperCase();
  return (
    RISKY_SYNC_ENV_PREFIXES.some((p) => name.startsWith(p)) || RISKY_SYNC_ENV.has(name)
  );
}
function credentialUrl(text: string): boolean {
  let url: URL;
  try {
    url = new URL(text);
  } catch {
    return false;
  }
  if (url.username || url.password) return true;
  return [...url.searchParams.keys()].some((k) =>
    CREDENTIAL_QUERY_KEYS.has(k.toLowerCase()),
  );
}
/**
 * Whether a new plain value syncs when nobody chose. Paths, token-like text,
 * credential URLs and risky variables stay on this machine.
 */
export function syncsByDefault(key: string, value: string): boolean {
  const v = value.trim();
  const path =
    v.startsWith("/") ||
    v.startsWith("~") ||
    v.startsWith("./") ||
    v.startsWith("..") ||
    v.startsWith("\\") ||
    v.slice(1, 3) === ":\\" ||
    v.slice(1, 3) === ":/";
  const tokenLike =
    v.length >= 20 &&
    !/\s/.test(v) &&
    /[0-9]/.test(v) &&
    /[A-Za-z]/.test(v) &&
    !v.includes("://");
  return !(path || tokenLike || credentialUrl(v) || riskySyncEnv(key));
}
