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
export function syncSignInUrl(origin: string): string {
  const url = new URL(origin.trim());
  url.search = "intent=pro&from=app-sync";
  url.hash = "";
  return url.toString();
}
export const SYNC_SIGN_IN_URL = "https://teams.toolport.app/?intent=pro&from=app-sync";
