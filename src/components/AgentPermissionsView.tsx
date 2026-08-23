import { useEffect, useState } from "react";
import { Eye, Plus, ShieldCheck, Trash2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Callout } from "@/components/Callout";
import { Input } from "@/components/ui/input";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  agentPermissionsPreview,
  agentPermissionsSetEnabled,
  agentPermissionsSetRules,
  agentPermissionsView,
} from "@/lib/api";
import type {
  PermissionAction,
  PermissionRule,
  PermissionsPreview,
  PermissionsView as PermissionsViewData,
} from "@/lib/types";

const ACTION_LABEL: Record<PermissionAction, string> = {
  deny: "Never",
  ask: "Ask first",
  allow: "Always allow",
};

/** What each per-profile state means, in the row. */
const STATE_LABEL: Record<string, { label: string; className: string }> = {
  applied: { label: "Applied", className: "text-success" },
  stale: { label: "Not applied yet", className: "text-warning" },
  off: { label: "Off", className: "text-muted-foreground" },
  error: { label: "Error", className: "text-destructive" },
};

/**
 * Native permission policy for Claude Code (SBS-1058): rules in Claude Code's own syntax that
 * Toolport writes into every profile's `settings.json` so Claude Code itself refuses, or asks
 * before, a matching native tool call. No hook, no Toolport process in the loop: Claude Code
 * enforces its `permissions` lists on every call, deny first, whatever any hook says.
 *
 * Off by default and empty by default. Nothing is written until the switch is on; presets are
 * one-click adds, never pre-applied. Only Toolport's own rule strings are ever added or
 * removed: a rule you already had in the file is left exactly where it is.
 */
export function AgentPermissionsView() {
  const [data, setData] = useState<PermissionsViewData | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [preview, setPreview] = useState<PermissionsPreview[] | null>(null);
  const [pattern, setPattern] = useState("");
  const [action, setAction] = useState<PermissionAction>("deny");

  useEffect(() => {
    let cancelled = false;
    agentPermissionsView()
      .then((v) => {
        if (!cancelled) setData(v);
      })
      .catch((e) => {
        if (!cancelled) setError(String(e));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  /**
   * Run a mutating call and return the view it left behind: the call's own result, or,
   * when it failed, the backend's current state (a failed profile write still saves the
   * policy, and the rows must show what the files really hold). `null` only when even that
   * could not be read.
   */
  async function act(
    fn: () => Promise<PermissionsViewData>,
  ): Promise<PermissionsViewData | null> {
    setBusy(true);
    setError(null);
    try {
      const next = await fn();
      setData(next);
      return next;
    } catch (e) {
      setError(String(e));
      try {
        const next = await agentPermissionsView();
        setData(next);
        return next;
      } catch {
        return null; // the first error is the one worth showing
      }
    } finally {
      setBusy(false);
    }
  }

  function withRules(rules: PermissionRule[]) {
    return act(() => agentPermissionsSetRules(rules));
  }

  async function addRule() {
    if (!data) return;
    const p = pattern.trim();
    if (!p) return;
    // Clear the input once the rule is in the list - including when a profile write failed
    // after the policy saved - and keep a refused pattern put to be fixed.
    const wasThere = data.rules.some((r) => r.pattern === p);
    const next = await withRules([...data.rules, { pattern: p, action }]);
    const isThere =
      next?.rules.some((r) => r.pattern === p && r.action === action) ?? false;
    if (isThere && !wasThere) setPattern("");
  }

  async function openPreview() {
    setBusy(true);
    setError(null);
    try {
      setPreview(await agentPermissionsPreview());
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  if (loading) return <p className="text-sm text-muted-foreground">Loading…</p>;
  if (!data) {
    return (
      <Callout variant="danger" role="alert">
        {error ?? "Could not load the permission policy."}
      </Callout>
    );
  }

  const rulesForAction = (a: PermissionAction) =>
    data.rules.filter((r) => r.action === a);

  return (
    <div className="grid gap-4">
      {error && (
        <Callout variant="danger" role="alert">
          {error}
        </Callout>
      )}

      <div className="rounded-xl border bg-card p-5">
        <label className="flex items-start gap-3">
          <input
            type="checkbox"
            className="mt-1"
            checked={data.enabled}
            disabled={busy || preview !== null}
            aria-label="Enforce my permission rules in Claude Code"
            onChange={(e) => {
              const next = e.target.checked;
              void act(() => agentPermissionsSetEnabled(next));
            }}
          />
          <span className="min-w-0">
            <span className="text-sm font-medium">
              Enforce my permission rules in Claude Code
            </span>
            <span className="mt-1 block text-xs text-muted-foreground">
              Toolport writes the rules below into every Claude Code profile&rsquo;s{" "}
              <span className="font-mono">settings.json</span>, under{" "}
              <span className="font-mono">permissions</span>. Claude Code itself then
              refuses, or asks before, a matching native tool call &mdash; shell commands,
              file reads and edits, web fetches, MCP tools &mdash; on every call, whatever
              any hook says. Turning this off removes only what Toolport added; a rule you
              already had stays.
            </span>
          </span>
        </label>
        <div className="mt-3 flex flex-wrap items-center gap-2 border-t pt-3">
          <Button variant="outline" size="sm" disabled={busy} onClick={openPreview}>
            <Eye className="size-3.5" />
            Preview what would be written
          </Button>
        </div>
      </div>

      <div className="rounded-xl border bg-card p-5">
        <div className="mb-1 flex items-center gap-2">
          <ShieldCheck className="size-4 text-muted-foreground" />
          <h2 className="text-sm font-medium">Rules</h2>
        </div>
        <p className="mb-3 text-xs text-muted-foreground">
          Claude Code&rsquo;s own rule syntax: a tool name, optionally with a pattern in
          parentheses &mdash; <span className="font-mono">Bash(rm -rf *)</span>,{" "}
          <span className="font-mono">Read(./.env)</span>,{" "}
          <span className="font-mono">WebFetch(domain:example.com)</span>,{" "}
          <span className="font-mono">mcp__github__create_issue</span>. A bare tool name
          covers every use of it. &ldquo;Never&rdquo; beats &ldquo;Ask first&rdquo; beats
          &ldquo;Always allow&rdquo; when more than one matches.
        </p>

        {data.rules.length === 0 ? (
          <p className="mb-3 text-sm text-muted-foreground">
            No rules yet. Add one below, or start from a preset.
          </p>
        ) : (
          <ul className="mb-3 grid gap-1.5">
            {(["deny", "ask", "allow"] as PermissionAction[]).flatMap((a) =>
              rulesForAction(a).map((r) => (
                <li
                  key={`${a}:${r.pattern}`}
                  className="flex items-center justify-between gap-3 rounded-md border px-2 py-1 text-sm"
                >
                  <span className="min-w-0 truncate">
                    <span className="mr-2 rounded bg-muted px-1.5 py-0.5 text-xs text-muted-foreground">
                      {ACTION_LABEL[a]}
                    </span>
                    <span className="font-mono text-xs">{r.pattern}</span>
                  </span>
                  <button
                    type="button"
                    disabled={busy}
                    aria-label={`Remove rule ${r.pattern}`}
                    title="Remove this rule"
                    onClick={() =>
                      void withRules(
                        data.rules.filter(
                          (x) => !(x.pattern === r.pattern && x.action === a),
                        ),
                      )
                    }
                    className="rounded p-0.5 text-muted-foreground hover:bg-accent hover:text-destructive disabled:opacity-50"
                  >
                    <Trash2 className="size-3.5" />
                  </button>
                </li>
              )),
            )}
          </ul>
        )}

        <form
          className="flex flex-wrap items-center gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            void addRule();
          }}
        >
          <select
            aria-label="Action"
            value={action}
            disabled={busy}
            onChange={(e) => setAction(e.target.value as PermissionAction)}
            className="h-9 rounded-md border bg-background px-2 text-sm"
          >
            <option value="deny">Never</option>
            <option value="ask">Ask first</option>
            <option value="allow">Always allow</option>
          </select>
          <Input
            value={pattern}
            disabled={busy}
            aria-label="Rule pattern"
            placeholder="Bash(rm -rf *)"
            onChange={(e) => setPattern(e.target.value)}
            className="h-9 w-72 font-mono text-xs"
          />
          <Button type="submit" size="sm" disabled={busy || !pattern.trim()}>
            <Plus className="size-3.5" />
            Add rule
          </Button>
        </form>

        <div className="mt-3 border-t pt-3">
          <p className="mb-2 text-xs text-muted-foreground">
            Presets (added to the list, nothing written until the switch is on):
          </p>
          <div className="flex flex-wrap gap-1.5">
            {data.presets.map((p) => {
              // A pattern already in the list is never overridden by a preset (your "never" is
              // not downgraded to a preset's "ask"), so the preset has nothing to add exactly
              // when every one of its patterns is already present, whatever the action.
              const already = p.rules.every((r) =>
                data.rules.some((x) => x.pattern === r.pattern),
              );
              return (
                <Button
                  key={p.label}
                  variant="outline"
                  size="sm"
                  disabled={busy || already}
                  title={
                    already
                      ? "Every pattern in this preset is already in the list"
                      : p.rules
                          .map((r) => `${ACTION_LABEL[r.action]}: ${r.pattern}`)
                          .join("\n")
                  }
                  onClick={() => {
                    const merged = [...data.rules];
                    for (const r of p.rules) {
                      if (!merged.some((x) => x.pattern === r.pattern)) merged.push(r);
                    }
                    void withRules(merged);
                  }}
                >
                  {p.label}
                </Button>
              );
            })}
          </div>
        </div>
      </div>

      <div className="rounded-xl border bg-card p-5">
        <h2 className="mb-1 text-sm font-medium">Claude Code profiles</h2>
        <p className="mb-3 text-xs text-muted-foreground">
          Every profile on this machine is covered, including ones selected with{" "}
          <span className="font-mono">CLAUDE_CONFIG_DIR</span>; a rule in only one of them
          would quietly not apply in the others.
        </p>
        {data.profiles.length === 0 ? (
          <p className="text-sm text-muted-foreground">No Claude Code profile found.</p>
        ) : (
          <ul className="grid gap-1.5 text-sm">
            {data.profiles.map((p) => {
              const meta = STATE_LABEL[p.state] ?? STATE_LABEL.error;
              return (
                <li key={p.path} className="flex items-center justify-between gap-3">
                  <span className="min-w-0 truncate font-mono text-xs" title={p.path}>
                    {p.path}
                  </span>
                  <span
                    className={`flex shrink-0 items-center gap-2 text-xs ${meta.className}`}
                    title={p.error}
                  >
                    {meta.label}
                    {p.state === "applied" &&
                      p.added < data.rules.length &&
                      data.rules.length > 0 && (
                        <span className="text-muted-foreground">
                          ({data.rules.length - p.added} already yours)
                        </span>
                      )}
                  </span>
                </li>
              );
            })}
          </ul>
        )}
      </div>

      <div className="rounded-xl border bg-card p-5">
        <h2 className="mb-1 text-sm font-medium">Which agents this reaches</h2>
        <ul className="grid gap-1 text-xs text-muted-foreground">
          <li>
            <span className="text-foreground">Claude Code</span> &mdash; enforced through
            its own <span className="font-mono">permissions</span> settings, on every
            native tool call.
          </li>
          <li>
            <span className="text-foreground">Cursor, Codex, Gemini CLI</span> &mdash; not
            yet. Cursor has hooks but no settings-level rule list; Codex uses approval and
            sandbox settings of a different shape; Gemini CLI is mixed. Toolport says so
            rather than pretending. The gateway&rsquo;s own approval and destructive-call
            gates still cover every MCP call from every client.
          </li>
        </ul>
      </div>

      <PreviewDialog previews={preview} onClose={() => setPreview(null)} />
    </div>
  );
}

function PreviewDialog({
  previews,
  onClose,
}: {
  previews: PermissionsPreview[] | null;
  onClose: () => void;
}) {
  return (
    <Dialog open={previews !== null} onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="sm:max-w-3xl">
        <DialogHeader>
          <DialogTitle>What would be written</DialogTitle>
        </DialogHeader>
        <div className="grid gap-4 overflow-auto">
          {previews?.length === 0 && (
            <p className="text-sm text-muted-foreground">
              No Claude Code profile was found, so there is nothing to write.
            </p>
          )}
          {previews?.map((p) => (
            <div key={p.path} className="grid gap-1">
              <p className="font-mono text-xs text-muted-foreground">{p.path}</p>
              {p.error ? (
                <p className="rounded-md bg-warning/10 px-3 py-2 text-xs text-warning">
                  No preview for this profile: {p.error}
                </p>
              ) : (
                <>
                  <pre className="max-h-64 overflow-auto rounded-md border bg-muted/40 p-3 text-xs whitespace-pre-wrap">
                    {p.after}
                  </pre>
                  {p.before === "" && (
                    <p className="text-xs text-muted-foreground">
                      This file does not exist yet and would be created.
                    </p>
                  )}
                </>
              )}
            </div>
          ))}
        </div>
        <DialogFooter className="text-xs text-muted-foreground sm:justify-start">
          Nothing has been written. Only the{" "}
          <span className="font-mono">permissions</span> key changes; everything else in
          the file, including your comments, is left exactly as it is.
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
