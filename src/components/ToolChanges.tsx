import { useState } from "react";
import { ChevronRight, ShieldCheck } from "lucide-react";
import { releaseQuarantine, type SecurityEvent } from "@/lib/api";
import type { Registry } from "@/lib/types";
import { toastError } from "@/lib/toast";

export function groupToolChanges(events: SecurityEvent[]) {
  const groups: {
    server: string;
    profile: string;
    ts: number;
    tools: SecurityEvent[];
  }[] = [];
  for (const event of [...events].sort((a, b) => b.ts - a.ts)) {
    const server = event.server || "Unknown server";
    const profile = event.profile || "";
    const group = groups.find(
      (group) =>
        group.server === server &&
        group.profile === profile &&
        group.ts - event.ts <= 60_000,
    );
    if (group) {
      if (!group.tools.some((tool) => tool.tool === event.tool)) group.tools.push(event);
    } else groups.push({ server, profile, ts: event.ts, tools: [event] });
  }
  return groups;
}

function summary(tools: SecurityEvent[]) {
  const noun = tools.length === 1 ? "tool" : "tools";
  if (tools.every((tool) => tool.change === "added")) return `${tools.length} new tools`;
  if (tools.every((tool) => tool.changed_fields?.includes("input_schema")))
    return `${tools.length} ${noun} changed their inputs`;
  if (tools.every((tool) => tool.changed_fields?.join() === "description"))
    return `${tools.length} ${noun} changed their descriptions`;
  return `${tools.length} ${noun} changed`;
}

function ago(ts: number) {
  const minutes = Math.max(0, Math.floor((Date.now() - ts) / 60_000));
  if (minutes < 1) return "just now";
  if (minutes < 60) return `${minutes}m ago`;
  if (minutes < 1440) return `${Math.floor(minutes / 60)}h ago`;
  return `${Math.floor(minutes / 1440)}d ago`;
}

function toolName(event: SecurityEvent) {
  const name = (event.tool || "Unknown tool")
    .replace(`${event.server}__`, "")
    .replace(/_/g, " ");
  return name.charAt(0).toUpperCase() + name.slice(1);
}

export function ToolChanges({
  events,
  registry,
  onAccept,
}: {
  events: SecurityEvent[];
  registry: Registry | null;
  onAccept: (events: SecurityEvent[]) => void;
}) {
  const [busy, setBusy] = useState(false);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  async function accept(events: SecurityEvent[]) {
    setBusy(true);
    try {
      for (const event of events) {
        for (const profile of event.blocked_profiles || []) {
          if (event.tool) await releaseQuarantine(profile, event.tool, event.new_fp);
        }
      }
      onAccept(events);
    } catch (error) {
      toastError(`Couldn't accept the changes: ${error}`);
    } finally {
      setBusy(false);
    }
  }
  return (
    <section
      aria-label="Tool changes"
      className="mb-4 rounded-lg border border-border bg-muted/20 p-4"
    >
      <h3 className="mb-3 flex items-center gap-2 text-sm font-medium">
        <ShieldCheck className="size-4 text-muted-foreground" />
        Tool changes
      </h3>
      <div className="space-y-3">
        {groupToolChanges(events).map((group) => {
          const blocked = group.tools.filter((tool) => tool.blocked === true).length;
          const unknown = group.tools.some((tool) => tool.blocked == null);
          const server =
            registry?.servers.find(
              (server) =>
                server.id.replace(/[^a-zA-Z0-9_]/g, "_") === group.server ||
                server.id === group.server,
            )?.name || group.server;
          const canAccept =
            !unknown &&
            !group.tools.some((tool) => tool.signatures !== undefined) &&
            group.tools.every(
              (tool) => !tool.blocked || (tool.blocked_profiles?.length && tool.new_fp),
            );
          const key = `${group.profile}:${group.server}:${group.ts}`;
          const open = expanded.has(key);
          return (
            <div
              key={key}
              className={`rounded-md border p-3 ${blocked ? "border-destructive/40 bg-destructive/5" : "border-border"}`}
            >
              <button
                type="button"
                aria-expanded={open}
                onClick={() =>
                  setExpanded((prev) => {
                    const next = new Set(prev);
                    if (open) next.delete(key);
                    else next.add(key);
                    return next;
                  })
                }
                className="flex w-full items-center gap-2 text-left text-sm"
              >
                <ChevronRight
                  className={`size-4 shrink-0 transition-transform ${open ? "rotate-90" : ""}`}
                />
                <span>
                  {server}: {summary(group.tools)}
                </span>
                <span className="ml-auto shrink-0 text-xs text-muted-foreground">
                  {ago(group.ts)}
                </span>
              </button>
              <div className="mt-2 flex flex-wrap items-center gap-2 text-xs">
                <p className={blocked ? "text-destructive" : "text-muted-foreground"}>
                  {blocked
                    ? `${blocked} ${blocked === 1 ? "tool is" : "tools are"} blocked. Review the changes or accept them.`
                    : unknown
                      ? "Blocking status unavailable. Refresh to check."
                      : "Not blocked. Review the changes or accept them."}
                </p>
                <button
                  disabled={busy || !canAccept}
                  onClick={() => void accept(group.tools)}
                  className="ml-auto rounded border border-border px-2 py-1 text-foreground disabled:opacity-50"
                >
                  Accept all for this server
                </button>
              </div>
              {group.tools.some((tool) => tool.signatures !== undefined) && (
                <p className="mt-2 text-xs text-destructive">
                  Suspicious content found. Review and accept each tool separately.
                </p>
              )}
              {open && (
                <div className="mt-3 space-y-2">
                  {group.tools.map((event) => (
                    <details
                      key={event.tool}
                      className="rounded border border-border p-2 text-xs"
                    >
                      <summary className="cursor-pointer font-medium">
                        {toolName(event)}
                      </summary>
                      <div className="mt-2 space-y-1 text-muted-foreground">
                        {event.changed_fields?.map((field) => (
                          <p key={field}>
                            {(
                              {
                                description: "Description changed.",
                                input_schema: "Inputs changed.",
                                output_schema: "Output format changed.",
                                annotations: "Safety hints changed.",
                              } as Record<string, string>
                            )[field] || "Definition changed."}
                          </p>
                        ))}
                        {event.parameters
                          ? (["added", "removed", "changed"] as const).map(
                              (kind) =>
                                event.parameters![kind].length > 0 && (
                                  <p key={kind}>
                                    {kind.charAt(0).toUpperCase() + kind.slice(1)}{" "}
                                    parameters: {event.parameters![kind].join(", ")}
                                  </p>
                                ),
                            )
                          : event.change === "changed" && (
                              <p>
                                Parameter details were not saved for this older change.
                              </p>
                            )}
                        <p>
                          {event.blocked === true
                            ? "Blocked until you accept the changes."
                            : event.blocked === false
                              ? "Not blocked."
                              : "Blocking status unavailable. Refresh to check."}
                        </p>
                        {event.signatures !== undefined && (
                          <p className="text-destructive">
                            Suspicious content. Matched signals:{" "}
                            {event.signatures.join(", ") || "details unavailable"}.
                          </p>
                        )}
                        <button
                          disabled={
                            busy ||
                            event.blocked == null ||
                            (event.blocked &&
                              (!event.blocked_profiles?.length || !event.new_fp))
                          }
                          onClick={() => void accept([event])}
                          className="mt-1 rounded border border-border px-2 py-1 text-foreground disabled:opacity-50"
                        >
                          Accept this tool
                        </button>
                      </div>
                    </details>
                  ))}
                </div>
              )}
            </div>
          );
        })}
      </div>
    </section>
  );
}
