import { useState } from "react";
import { Check, ShieldAlert } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { TransportPill } from "@/components/TransportPill";
import type { ImportItem } from "@/lib/types";

interface Props {
  open: boolean;
  items: ImportItem[];
  busy?: boolean;
  title?: string;
  onOpenChange: (open: boolean) => void;
  onConfirm: (keys: string[]) => void;
}

/** Review and choose detected client servers before adding them to Toolport. */
export function ImportReviewDialog({
  open,
  items,
  busy = false,
  title = "Review servers to import",
  onOpenChange,
  onConfirm,
}: Props) {
  if (!open) return null;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-lg">
        <ImportReviewContent
          items={items}
          busy={busy}
          title={title}
          onOpenChange={onOpenChange}
          onConfirm={onConfirm}
        />
      </DialogContent>
    </Dialog>
  );
}

function ImportReviewContent({
  items,
  busy = false,
  title = "Review servers to import",
  onOpenChange,
  onConfirm,
}: Omit<Props, "open">) {
  const keyedItems = items.map((item, index) => ({
    item,
    key: item.key ?? `${item.name}-${index}`,
  }));
  const [selected, setSelected] = useState<Set<string>>(
    () => new Set(keyedItems.map(({ key }) => key)),
  );

  const selectedCount = selected.size;
  return (
    <>
      <DialogHeader>
        <DialogTitle>{title}</DialogTitle>
      </DialogHeader>
      <div className="flex flex-col gap-4 py-1">
        <p className="text-xs text-muted-foreground">
          Review the commands and URLs before adding them. You can leave any server
          unchecked and import only the ones you want.
        </p>
        <div className="flex max-h-72 flex-col gap-2 overflow-y-auto">
          {keyedItems.map(({ item, key }) => {
            const isSelected = selected.has(key);
            return (
              <button
                key={key}
                type="button"
                className={`rounded-md text-left transition-colors ${
                  isSelected ? "ring-1 ring-success/60" : "opacity-60"
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
              </button>
            );
          })}
        </div>
        <div className="flex items-center justify-between gap-2 border-t pt-3">
          <Button variant="ghost" onClick={() => onOpenChange(false)} disabled={busy}>
            Cancel
          </Button>
          <Button
            onClick={() => onConfirm(Array.from(selected))}
            disabled={busy || selectedCount === 0}
          >
            <Check className="size-4" />
            {selectedCount === 0
              ? "Select a server"
              : `Import ${selectedCount} server${selectedCount === 1 ? "" : "s"}`}
          </Button>
        </div>
      </div>
    </>
  );
}

/** One reviewable server: name, what it runs, and the relevant safety flags. */
export function ImportRow({ item, selected }: { item: ImportItem; selected?: boolean }) {
  const runs =
    item.command != null ? [item.command, ...item.args].join(" ") : (item.url ?? "");
  const shell = runsShell(item.command);
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
        <TransportPill transport={item.transport} />
        {!item.isNew && (
          <span className="ml-auto shrink-0 text-xs text-muted-foreground">
            already added
          </span>
        )}
      </div>
      {runs && (
        <p className="mt-1 font-mono text-xs break-all text-muted-foreground">{runs}</p>
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
export function runsShell(command: string | null): boolean {
  if (!command) return false;
  const base = command
    .replace(/\\/g, "/")
    .split("/")
    .pop()!
    .toLowerCase()
    .replace(/\.exe$/, "");
  return ["cmd", "sh", "bash", "zsh", "powershell", "pwsh"].includes(base);
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
