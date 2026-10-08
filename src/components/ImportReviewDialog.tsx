import { useState } from "react";
import { Check, Loader2, ShieldAlert } from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import type { ImportItem, ServerEntry } from "@/lib/types";

interface Props {
  open: boolean;
  items: ImportItem[];
  busy?: boolean;
  title?: string;
  description?: string;
  confirmLabel?: string;
  allowEmpty?: boolean;
  details?: string;
  error?: string;
  onOpenChange: (open: boolean) => void;
  onConfirm: (
    keys: string[],
    secretChoices?: Record<string, Record<string, boolean>>,
  ) => void;
}

/** Review and choose detected client servers before adding them to Toolport. */
export function ImportReviewDialog({
  open,
  items,
  busy = false,
  title = "Review servers to import",
  description,
  confirmLabel,
  allowEmpty = false,
  details,
  error,
  onOpenChange,
  onConfirm,
}: Props) {
  if (!open) return null;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <ImportReviewContent
        items={items}
        busy={busy}
        title={title}
        description={description}
        confirmLabel={confirmLabel}
        allowEmpty={allowEmpty}
        details={details}
        error={error}
        onOpenChange={onOpenChange}
        onConfirm={onConfirm}
      />
    </Dialog>
  );
}

function ImportReviewContent({
  items,
  busy = false,
  title = "Review servers to import",
  description,
  confirmLabel,
  allowEmpty = false,
  details,
  error,
  onOpenChange,
  onConfirm,
}: Omit<Props, "open">) {
  const keyedItems = items.map((item, index) => ({
    item,
    key: item.key ?? `${item.name}-${index}`,
  }));
  const [selected, setSelected] = useState<Set<string>>(
    () =>
      new Set(keyedItems.filter(({ item }) => !item.unsupported).map(({ key }) => key)),
  );

  const [secretChoices, setSecretChoices] = useState<
    Record<string, Record<string, boolean>>
  >(() =>
    Object.fromEntries(
      items.map((item) => [
        item.name,
        Object.fromEntries((item.credentials ?? []).map((env) => [env.key, env.secret])),
      ]),
    ),
  );
  const selectedCount = selected.size;
  return (
    <DialogContent className="sm:max-w-lg">
      <DialogHeader>
        <DialogTitle>{title}</DialogTitle>
      </DialogHeader>
      <div className="flex flex-col gap-4 py-1">
        <p className="text-xs text-muted-foreground">
          {description ??
            "Review the commands and URLs before adding them. Leave any server unchecked to keep it as it is."}
        </p>
        {details && (
          <details>
            <summary className="text-xs">Details</summary>
            <p className="mt-2 whitespace-pre-wrap break-all text-xs text-muted-foreground">
              {details}
            </p>
          </details>
        )}
        {error && (
          <p role="alert" className="text-sm text-warning">
            {error}
          </p>
        )}
        <div className="flex max-h-72 flex-col gap-2 overflow-y-auto">
          {keyedItems.map(({ item, key }) => {
            const isSelected = selected.has(key);
            return (
              <div key={key}>
                <button
                  type="button"
                  aria-pressed={isSelected}
                  disabled={busy || !!item.unsupported}
                  data-failed={!!error?.includes(item.name)}
                  className={`w-full rounded-md text-left transition-colors ${
                    error?.includes(item.name)
                      ? "ring-1 ring-warning"
                      : isSelected
                        ? "ring-1 ring-success/60"
                        : "opacity-60"
                  }`}
                  onClick={() =>
                    setSelected((previous) => {
                      const next = new Set(previous);
                      if (isSelected) next.delete(key);
                      else next.add(key);
                      return next;
                    })
                  }
                >
                  <ImportRow item={item} selected={isSelected} />
                  {busy && isSelected && (
                    <p role="status" className="flex gap-2 px-3 pb-2 text-xs">
                      <Loader2 className="size-3 animate-spin" />
                      Checking {item.name}...
                    </p>
                  )}
                </button>
                {!!item.credentials?.length && (
                  <div className="flex flex-col gap-2 px-3 pb-3 text-xs">
                    {item.credentials.map((env) => (
                      <label key={env.key} className="flex items-center gap-2">
                        <input
                          type="checkbox"
                          disabled={busy || !!item.unsupported}
                          checked={secretChoices[item.name]?.[env.key] ?? env.secret}
                          onChange={(e) =>
                            setSecretChoices((previous) => ({
                              ...previous,
                              [item.name]: {
                                ...previous[item.name],
                                [env.key]: e.target.checked,
                              },
                            }))
                          }
                        />
                        Keep {env.key} in keychain{" "}
                        <span className="ml-auto text-muted-foreground">
                          {env.present ? "Found" : "Missing"}
                        </span>
                      </label>
                    ))}
                  </div>
                )}
                {item.unsupported && (
                  <p className="px-3 pb-2 text-xs text-warning">
                    Unsupported: {item.unsupported}
                  </p>
                )}
              </div>
            );
          })}
        </div>
      </div>
      <DialogFooter className="justify-between">
        <Button variant="ghost" onClick={() => onOpenChange(false)} disabled={busy}>
          Cancel
        </Button>
        <Button
          onClick={() => {
            if (items.some((item) => item.credentials?.length))
              onConfirm(Array.from(selected), secretChoices);
            else onConfirm(Array.from(selected));
          }}
          disabled={busy || (!allowEmpty && selectedCount === 0)}
        >
          <Check className="size-4" />
          {confirmLabel ??
            (selectedCount === 0
              ? "Select a server"
              : `Import ${selectedCount} server${selectedCount === 1 ? "" : "s"}`)}
        </Button>
      </DialogFooter>
    </DialogContent>
  );
}

/** One reviewable server: name, what it runs, and the relevant safety flags. */
export function ImportRow({ item, selected }: { item: ImportItem; selected?: boolean }) {
  const runs =
    item.command != null ? [item.command, ...item.args].join(" ") : (item.url ?? "");
  const shell = runsShell(item.command, item.args);
  const privateHost = isPrivateHostUrl(item.url);
  return (
    <div className="rounded-md border px-3 py-2">
      <div className="flex items-center gap-2">
        {selected !== undefined && (
          <span
            aria-hidden="true"
            className={`size-3 rounded-sm border ${
              selected ? "border-success bg-success" : "border-muted-foreground"
            }`}
          />
        )}
        <span className="truncate text-sm font-medium">{item.name}</span>
        {
          <span className="ml-auto shrink-0 text-xs text-muted-foreground">
            {item.isNew ? "New" : "In Toolport"}
          </span>
        }
      </div>
      {runs && (
        <p
          title={runs}
          aria-label={runs}
          className="mt-1 flex min-w-0 font-mono text-xs text-muted-foreground"
        >
          {runs.length > 72 ? (
            <>
              <span className="min-w-0 truncate">{runs.slice(0, -28)}</span>
              <span className="shrink-0">{runs.slice(-28)}</span>
            </>
          ) : (
            runs
          )}
        </p>
      )}
      {!!item.envKeys?.length && !item.credentials?.length && (
        <p className="mt-1 text-xs text-warning">
          Credentials: {item.envKeys.join(", ")}. Review their status before connecting.
        </p>
      )}
      {shell && (
        <p className="mt-1.5 flex items-center gap-1.5 text-xs text-warning">
          <ShieldAlert className="size-3.5 shrink-0" />
          Runs a shell command. Only import setups you trust.
        </p>
      )}
      {privateHost && (
        <p className="mt-1.5 flex items-center gap-1.5 text-xs text-warning">
          <ShieldAlert className="size-3.5 shrink-0" />
          Connects to a private or internal address. Only import setups you trust.
        </p>
      )}
    </div>
  );
}

// Exported for unit tests: security-relevant classifier behind the
// "Runs a shell command" warning above.
export function runsShell(command: string | null, args: string[] = []): boolean {
  if (!command) return false;
  let cmd = command;
  let argv = args;
  // Backend `normalize_invocation`: a packed `"bash -c '…'"` with empty args is
  // split, but only when the first token is a bare program name (no path) — the
  // same rule `isDownloadLauncher` mirrors, so a path with spaces stays intact.
  if (args.length === 0) {
    const parts = command.split(/\s+/).filter(Boolean);
    const first = parts[0] ?? "";
    if (parts.length > 1 && !first.includes("/") && !first.includes("\\")) {
      cmd = first;
      argv = parts.slice(1);
    }
  }
  const base = cmd
    .replace(/\\/g, "/")
    .split("/")
    .pop()!
    .toLowerCase()
    .replace(/\.(exe|cmd|bat)$/, "");
  // `env` just launches a command; classify that instead. GNU env options that
  // take a separate operand (-u NAME, -C DIR) must be skipped with their value,
  // or `env -u FOO bash -c '…'` would classify FOO and miss the shell. Unknown
  // options (notably -S, which repacks a whole command line) classify as shell
  // conservatively rather than silently dropping the warning.
  if (base === "env") {
    for (let i = 0; i < argv.length; i++) {
      const a = argv[i];
      if (a.startsWith("--")) {
        const name = a.split("=")[0];
        if (name === "--ignore-environment") continue;
        if (name === "--unset" || name === "--chdir") {
          if (!a.includes("=")) i++;
          continue;
        }
        return true;
      }
      if (a.startsWith("-")) {
        if (a === "-" || a === "-i") continue; // both mean ignore-environment
        if (a === "-u" || a === "-C") {
          i++;
          continue;
        }
        return true;
      }
      if (a.includes("=")) continue; // NAME=VALUE assignment
      return runsShell(a, argv.slice(i + 1));
    }
    return false;
  }
  return ["cmd", "sh", "bash", "zsh", "fish", "powershell", "pwsh"].includes(base);
}

// Exported for unit tests: security-relevant classifier behind the
// "Connects to a private or internal address" warning above.
export function isPrivateHostUrl(url: string | null | undefined): boolean {
  if (!url) return false;
  let host: string;
  try {
    // WHATWG keeps a trailing dot on named hosts (localhost.) but strips it on
    // IPv4 literals — strip for both so loopback warnings stay consistent.
    host = new URL(url).hostname
      .toLowerCase()
      .replace(/^\[|\]$/g, "")
      .replace(/\.$/, "");
  } catch {
    return false;
  }
  if (
    host === "localhost" ||
    host.endsWith(".localhost") ||
    host === "::1" ||
    host === "0:0:0:0:0:0:0:1"
  ) {
    return true;
  }
  // IPv4-mapped IPv6 — WHATWG may emit dotted or hex form (::ffff:127.0.0.1 / ::ffff:7f00:1)
  const v4MappedDotted = host.match(/^::ffff:(\d{1,3}(?:\.\d{1,3}){3})$/i);
  if (v4MappedDotted) {
    return isPrivateIpv4(v4MappedDotted[1]);
  }
  const v4MappedHex = host.match(/^::ffff:([0-9a-f]{1,4}):([0-9a-f]{1,4})$/i);
  if (v4MappedHex) {
    const hi = parseInt(v4MappedHex[1], 16);
    const lo = parseInt(v4MappedHex[2], 16);
    const dotted = `${(hi >> 8) & 0xff}.${hi & 0xff}.${(lo >> 8) & 0xff}.${lo & 0xff}`;
    return isPrivateIpv4(dotted);
  }
  // IPv6: loopback, unspecified, link-local fe80::/10, ULA fc00::/7
  if (host.includes(":")) {
    if (host === "::" || host === "0:0:0:0:0:0:0:0") return true;
    const first = parseInt(host.split(":")[0] || "0", 16);
    if (Number.isNaN(first)) return false;
    if ((first & 0xffc0) === 0xfe80) return true; // link-local
    if ((first & 0xfe00) === 0xfc00) return true; // unique-local
    return false;
  }
  return isPrivateIpv4(host);
}

/** Team-synced local commands and LAN URLs stay off until the member confirms.
 * Mirrors `ServerEntry::needs_team_enable_review` so the Servers Switch and
 * Enable all cannot skip the Teams review dialog.
 *
 * Deliberately an AFFORDANCE, not the security boundary. It cannot match the Rust
 * check exactly: `host_is_private` resolves named hosts (and treats an unresolvable
 * one as private), and a renderer cannot do DNS, so `http://internal-service/mcp`
 * pointing at 10.0.0.5 reads as public here. `set_server_enabled` decides for real
 * and refuses without an explicit reviewed flag, so a miss here costs a clear error
 * rather than an unreviewed enable. */
export function needsTeamEnableReview(
  server: Pick<ServerEntry, "source" | "transport" | "command" | "url">,
): boolean {
  if (!server.source?.startsWith("team:")) return false;
  if (server.transport === "stdio" || !!server.command) return true;
  // Anything that is not a plain https:// URL to a dotted public name is treated as
  // needing review: a bare hostname is an intranet name far more often than not, and
  // over-prompting is the safe direction for a dialog whose whole job is consent.
  return isPrivateHostUrl(server.url) || !isPublicLookingHttpsUrl(server.url);
}

/** True only for `https://` URLs whose host is a dotted name or a public literal IP.
 * A bare hostname (`https://internal-service/mcp`) and any plaintext `http://` fall
 * to false, so [`needsTeamEnableReview`] asks. */
function isPublicLookingHttpsUrl(url: string | null | undefined): boolean {
  if (!url) return false;
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    return false;
  }
  if (parsed.protocol !== "https:") return false;
  const host = parsed.hostname.replace(/^\[|\]$/g, "");
  if (isPrivateHostUrl(url)) return false;
  return host.includes(".") && !host.endsWith(".local") && !host.endsWith(".internal");
}

/** The fields a member actually reviews before enabling a team server. `args` is
 * optional so an entry that omits it compares equal to one with an empty list. */
export type ReviewedFields = {
  transport: ServerEntry["transport"];
  command: ServerEntry["command"];
  args?: ServerEntry["args"];
  url: ServerEntry["url"];
};

/** Compared on confirm so a team push landing mid-dialog cannot swap the definition
 * underneath the member (the confirmation carries only the server id). */
export function sameReviewedDefinition(a: ReviewedFields, b: ReviewedFields): boolean {
  return (
    a.transport === b.transport &&
    (a.command ?? "") === (b.command ?? "") &&
    (a.url ?? "") === (b.url ?? "") &&
    JSON.stringify(a.args ?? []) === JSON.stringify(b.args ?? [])
  );
}

/** Mirror src-tauri/src/oauth.rs ip_is_private for IPv4. */
function isPrivateIpv4(host: string): boolean {
  const match = host.match(/^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/);
  if (!match) return false;
  const a = Number(match[1]);
  const b = Number(match[2]);
  const c = Number(match[3]);
  const d = Number(match[4]);
  if ([a, b, c, d].some((n) => n > 255)) return false;
  return (
    a === 127 || // loopback
    a === 10 || // RFC1918
    // 0.0.0.0/8 "this network". Deliberately broader than Rust's is_unspecified(),
    // which is only 0.0.0.0. Warning on the whole block is the safe direction here.
    a === 0 ||
    (a === 192 && b === 168) ||
    (a === 172 && b >= 16 && b <= 31) ||
    (a === 169 && b === 254) || // link-local
    (a === 100 && (b & 0xc0) === 64) || // CGNAT 100.64/10
    (a === 255 && b === 255 && c === 255 && d === 255) // broadcast
  );
}
