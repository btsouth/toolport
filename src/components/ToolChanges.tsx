import { msg, translate, useI18n } from "@/lib/i18n";
import { useMemo, useState } from "react";
import { ChevronRight, ShieldCheck } from "lucide-react";
import { releaseQuarantine, type SecurityEvent } from "@/lib/api";
import { securityKey } from "@/lib/securityReview";
import type { Registry } from "@/lib/types";
import { toastError } from "@/lib/toast";

// Rust sanitize_segment maps Unicode scalar values, rather than UTF-16 code units.
// eslint-disable-next-line react-refresh/only-export-components -- Review tests share this classifier; keep the pending drift fix easy to rebase.
export function sanitizeSegment(value: string) {
  return Array.from(value, (character) =>
    /^[a-zA-Z0-9_]$/.test(character) ? character : "_",
  ).join("");
}

// eslint-disable-next-line react-refresh/only-export-components -- Review tests share this classifier; keep the pending drift fix easy to rebase.
export function groupToolChanges(events: SecurityEvent[]) {
  const groups: {
    server: string;
    profile: string;
    ts: number;
    tools: SecurityEvent[];
  }[] = [];
  for (const event of [...events].sort((a, b) => b.ts - a.ts)) {
    const server = event.server || "";
    const profile = event.profile || "";
    const group = groups.find(
      (group) =>
        group.server === server &&
        group.profile === profile &&
        group.ts - event.ts <= 60_000,
    );
    if (group) {
      if (
        !group.tools.some(
          (tool) =>
            tool.tool === event.tool &&
            tool.severity === event.severity &&
            tool.change === event.change &&
            tool.new_fp === event.new_fp &&
            tool.ts === event.ts,
        )
      )
        group.tools.push(event);
    } else groups.push({ server, profile, ts: event.ts, tools: [event] });
  }
  return groups;
}

function summary(tools: SecurityEvent[]) {
  const singular = tools.length === 1;
  if (tools.every((tool) => tool.change === "added"))
    return translate(singular ? msg("{count} new tool") : msg("{count} new tools"), {
      count: tools.length,
    });
  if (tools.every((tool) => tool.changed_fields?.includes("input_schema")))
    return translate(
      singular
        ? msg("{count} tool changed its inputs")
        : msg("{count} tools changed their inputs"),
      { count: tools.length },
    );
  if (tools.every((tool) => tool.changed_fields?.join() === "description"))
    return translate(
      singular
        ? msg("{count} tool changed its description")
        : msg("{count} tools changed their descriptions"),
      { count: tools.length },
    );
  return translate(
    singular ? msg("{count} tool changed") : msg("{count} tools changed"),
    { count: tools.length },
  );
}

function ago(ts: number) {
  const minutes = Math.max(0, Math.floor((Date.now() - ts) / 60_000));
  if (minutes < 1) return translate("just now");
  if (minutes < 60) return translate("{value0}m ago", { value0: minutes });
  if (minutes < 1440)
    return translate("{value0}h ago", { value0: Math.floor(minutes / 60) });
  return translate("{value0}d ago", { value0: Math.floor(minutes / 1440) });
}

function toolName(event: SecurityEvent) {
  const name = (event.tool || translate("Unknown tool"))
    .replace(`${event.server}__`, "")
    .replace(/_/g, " ");
  return name.charAt(0).toUpperCase() + name.slice(1);
}

/** `history` renders records only: no status, accept or review actions. */
export function ToolChanges({
  events,
  registry,
  onAccept,
  onReview,
  history = false,
}: {
  events: SecurityEvent[];
  registry: Registry | null;
  onAccept: (events: SecurityEvent[]) => void;
  onReview?: () => void;
  history?: boolean;
}) {
  const { t } = useI18n();
  const groups = useMemo(() => groupToolChanges(events), [events]);
  const [busy, setBusy] = useState(false);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  async function accept(events: SecurityEvent[]) {
    if (events.some((event) => event.review_required)) return;
    setBusy(true);
    try {
      for (const event of events) {
        for (const profile of event.blocked_profiles || []) {
          if (event.tool) await releaseQuarantine(profile, event.tool, event.new_fp);
        }
      }
      onAccept(events);
    } catch (error) {
      toastError(
        translate("Couldn't accept the changes: {value0}", { value0: String(error) }),
      );
    } finally {
      setBusy(false);
    }
  }
  return (
    <section
      aria-label={t("Tool changes")}
      className="mb-4 rounded-lg border border-border bg-muted/20 p-4"
    >
      {!history && (
        <h3 className="mb-3 flex items-center gap-2 text-sm font-medium">
          <ShieldCheck className="size-4 text-muted-foreground" />
          {t("Tool changes")}
        </h3>
      )}
      <div className="space-y-3">
        {groups.map((group) => {
          const blocked = group.tools.filter((tool) => tool.blocked === true).length;
          const reviewRequired = group.tools.some((tool) => tool.review_required);
          // Capture status belongs to each tool: any captured review keeps the
          // Settings route; the group waits only when every review is being captured.
          const capturing =
            reviewRequired &&
            group.tools.every((tool) => !tool.review_required || tool.pending_capture);
          const unknown = group.tools.some((tool) => tool.blocked == null);
          const server =
            registry?.servers.find(
              (server) =>
                sanitizeSegment(server.id) === group.server || server.id === group.server,
            )?.name ||
            group.server ||
            translate("Unknown server");
          const canAccept =
            !unknown &&
            !reviewRequired &&
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
                <span className="ml-auto shrink-0 text-meta text-muted-foreground">
                  {ago(group.ts)}
                </span>
              </button>
              {!history && (
                <div className="mt-2 flex flex-wrap items-center gap-2 text-meta">
                  <p className={blocked ? "text-destructive" : "text-muted-foreground"}>
                    {/* Unknown wins over blocked: a group with any unknown status
                        cannot be accepted here, so say where to look instead. */}
                    {capturing
                      ? translate(
                          "Toolport is capturing this tool's new definition. It stays blocked until then.",
                        )
                      : reviewRequired
                        ? translate(
                            "Review the captured definition in Settings before accepting it.",
                          )
                        : unknown
                          ? translate(
                              "Blocking status unavailable. Review in Settings > Quarantined tools.",
                            )
                          : blocked
                            ? translate(
                                blocked === 1
                                  ? msg(
                                      "{count} tool is blocked. Review the changes or accept them.",
                                    )
                                  : msg(
                                      "{count} tools are blocked. Review the changes or accept them.",
                                    ),
                                { count: blocked },
                              )
                            : translate(
                                "Not blocked. Review the changes or accept them.",
                              )}
                  </p>
                  {reviewRequired && !capturing && (
                    <button
                      onClick={onReview}
                      className="rounded border border-border px-2 py-1 text-foreground"
                    >
                      {translate("Review in Settings")}
                    </button>
                  )}
                  {/* Shown only when it can be used; the sentence beside it or the
                      suspicious-content line says what to do otherwise. */}
                  {canAccept && (
                    <button
                      disabled={busy}
                      onClick={() => void accept(group.tools)}
                      className="ml-auto rounded border border-border px-2 py-1 text-foreground disabled:opacity-50"
                    >
                      {translate("Accept all for this server")}
                    </button>
                  )}
                </div>
              )}
              {!history && group.tools.some((tool) => tool.signatures !== undefined) && (
                <p className="mt-2 text-meta text-destructive">
                  {translate(
                    "Suspicious content found. Review and accept each tool separately.",
                  )}
                </p>
              )}
              {open && (
                <div className="mt-3 space-y-2">
                  {group.tools.map((event) => (
                    <details
                      key={`${securityKey(event)}:${event.new_fp}:${event.ts}`}
                      className="rounded border border-border p-2 text-meta"
                    >
                      <summary className="cursor-pointer font-medium">
                        {toolName(event)}
                      </summary>
                      <div className="mt-2 space-y-1 text-muted-foreground">
                        {event.changed_fields?.map((field) => (
                          <p key={field}>
                            {(
                              {
                                description: translate("Description changed."),
                                input_schema: translate("Inputs changed."),
                                output_schema: translate("Output format changed."),
                                annotations: translate("Safety hints changed."),
                              } as Record<string, string>
                            )[field] || translate("Definition changed.")}
                          </p>
                        ))}
                        {event.parameters
                          ? (["added", "removed", "changed"] as const).map(
                              (kind) =>
                                event.parameters![kind].length > 0 && (
                                  <p key={kind}>
                                    {translate(
                                      {
                                        added: msg("Added parameters: {parameters}"),
                                        removed: msg("Removed parameters: {parameters}"),
                                        changed: msg("Changed parameters: {parameters}"),
                                      }[kind],
                                      { parameters: event.parameters![kind].join(", ") },
                                    )}
                                  </p>
                                ),
                            )
                          : event.change === "changed" && (
                              <p>
                                {translate(
                                  "Parameter details were not saved for this older change.",
                                )}
                              </p>
                            )}
                        {/* A tool still being captured has nothing to accept yet. */}
                        {!(history && event.blocked == null) &&
                          !event.pending_capture && (
                            <p>
                              {event.blocked === true
                                ? translate("Blocked until you accept the changes.")
                                : event.blocked === false
                                  ? translate("Not blocked.")
                                  : translate(
                                      "Blocking status unavailable. Review in Settings > Quarantined tools.",
                                    )}
                            </p>
                          )}
                        {event.pending_capture && (
                          <p>
                            {translate(
                              "Toolport is capturing this tool's new definition. It stays blocked until then.",
                            )}
                          </p>
                        )}
                        {event.signatures !== undefined && (
                          <p className="text-destructive">
                            {translate(
                              "Suspicious content. Matched signals: {signals}.",
                              {
                                signals:
                                  event.signatures.join(", ") ||
                                  translate("details unavailable"),
                              },
                            )}
                          </p>
                        )}
                        {history ||
                        event.pending_capture ? null : event.review_required ? (
                          <button
                            onClick={onReview}
                            className="mt-1 rounded border border-border px-2 py-1 text-foreground"
                          >
                            {translate("Review in Settings")}
                          </button>
                        ) : (
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
                            {translate("Accept this tool")}
                          </button>
                        )}
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
