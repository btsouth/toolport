import { useEffect, useRef, useState } from "react";
import { Eye, FileText, Plus, RefreshCw, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Callout } from "@/components/Callout";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { RuleStateBadge } from "@/components/RuleStateBadge";
import {
  rulesApply,
  rulesDeleteSet,
  rulesPreview,
  rulesSaveSet,
  rulesSetActive,
  rulesSetClientEnabled,
  rulesView,
} from "@/lib/api";
import { forgetRulesDraft, getRulesDraft, setRulesDraft } from "@/lib/rulesDraftCache";
import type {
  InstructionsApplyState,
  RulesPreview,
  RulesView as RulesViewData,
} from "@/lib/types";

/**
 * States where `write_target` refuses the write outright, so the previewed bytes describe what
 * WOULD land rather than what will. `too_long` is a refusal, not a truncation: Toolport never
 * writes a file it knows the client will silently cut short.
 */
const PREVIEW_BLOCKED_REASON: Partial<Record<InstructionsApplyState, string>> = {
  blocked_override:
    "a local override file makes this client ignore it, so writing would be invisible.",
  too_long:
    "the finished file would exceed this client's hard limit, and a truncated rules file is worse than none.",
  error: "the file could not be read or written, so it was left untouched.",
};
const BLOCKING_STATES = new Set(Object.keys(PREVIEW_BLOCKED_REASON));
const RESERVED_MARKERS = [
  "<!-- toolport:rules:start",
  "<!-- toolport:rules:end -->",
  "<!-- toolport:team-instructions:start",
  "<!-- toolport:team-instructions:end -->",
  "<!-- Managed by Toolport",
  "<!-- Toolport personal rules",
];

/**
 * Agent rules: write your own instructions once and have Toolport put them in every AI client's
 * rules file (CLAUDE.md, AGENTS.md, GEMINI.md, and the rest) instead of editing each by hand.
 *
 * Two things this screen is careful about, because they involve writing into files the user owns:
 *
 *   * A client receives nothing until it is switched on here.
 *   * "Preview" shows the exact bytes that would land, before anything is written.
 *
 * Needs no MCP server and no gateway, so it works on a fresh install.
 */
export function RulesView() {
  const [data, setData] = useState<RulesViewData | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // The editor is a local draft so typing never round-trips to disk. Every view refresh goes
  // through `adopt`, which reseats the draft on the active set, so one set's text can never be
  // carried into another and saved over it.
  const [draft, setDraft] = useState("");
  const [draftName, setDraftName] = useState("");

  const [preview, setPreview] = useState<RulesPreview | null>(null);
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);

  /**
   * The preview card renders after the clients list, so on a window too short to reach it the
   * Preview button looks like it does nothing at all - the worst failure available to the one
   * control whose entire job is to show you something. Bring the card into view when it opens.
   *
   * A dialog would also solve this, and the Agent activity tab uses one, but not here: a modal
   * makes the page behind it inert, and this card is deliberately a live panel that clears itself
   * when the editor or the view moves under it. Scrolling keeps that.
   *
   * Both `scrollIntoView` and `matchMedia` are optional calls: jsdom ships neither, and
   * `src/test/setup.ts` only stubs them for suites that opt in.
   */
  const previewRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    if (!preview) return;
    // The `scroll-behavior: auto !important` reduced-motion rule in index.css does NOT cover
    // this. An explicit `behavior` in the options dict beats the CSS property, so the preference
    // has to be read here as well. Getting the card on screen is the fix; the smoothness is a
    // nicety, and the first thing to drop for anyone who asked for less motion.
    const reduceMotion =
      window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false;
    previewRef.current?.scrollIntoView?.({
      behavior: reduceMotion ? "auto" : "smooth",
      block: "start",
    });
  }, [preview]);

  const active = data?.sets.find((s) => s.id === data.activeSetId) ?? null;
  const dirty =
    active !== null && (draft !== active.content || draftName !== active.name);

  /**
   * Reseat the editor on the refreshed view. A save just landed, so the cached draft for that set
   * is spent: drop it, or the next mount would resurrect text the user already committed and show
   * it as unsaved.
   */
  function adopt(next: RulesViewData) {
    setData(next);
    const set = next.sets.find((s) => s.id === next.activeSetId) ?? null;
    if (set) forgetRulesDraft(set.id);
    setDraft(set?.content ?? "");
    setDraftName(set?.name ?? "");
    // The preview card describes one client's file under whichever set was active when it was
    // opened. Anything that reseats the editor can invalidate it, and a preview showing a path
    // and bytes that no longer match what is on screen is worse than showing no preview at all.
    setPreview(null);
  }

  /** Reseat a reloaded view without discarding an unsaved draft held across an unmount. */
  function adoptPreservingDraft(next: RulesViewData) {
    const set = next.sets.find((s) => s.id === next.activeSetId);
    const held = set ? getRulesDraft(set.id) : undefined;
    adopt(next);
    if (held) {
      setDraft(held.content);
      setDraftName(held.name);
      setRulesDraft(set!.id, held);
    }
  }

  useEffect(() => {
    let cancelled = false;
    rulesView()
      .then((v) => {
        if (cancelled) return;
        // Read the held draft BEFORE `adopt`, which clears the cache entry for the set it seats.
        const set = v.sets.find((s) => s.id === v.activeSetId);
        const held = set ? getRulesDraft(set.id) : undefined;
        adopt(v);
        // Restore text typed before the tab was switched away, onto the set it was typed for.
        if (held) {
          setDraft(held.content);
          setDraftName(held.name);
          setRulesDraft(set!.id, held);
        }
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

  /** Run a mutating call, adopt the refreshed view, and surface any failure in place. */
  async function run(fn: () => Promise<RulesViewData>, preserveDraft = false) {
    setBusy(true);
    setError(null);
    try {
      const next = await fn();
      if (preserveDraft) adoptPreservingDraft(next);
      else adopt(next);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  /**
   * Apply an edit to the editor, and hold the result against an unmount.
   *
   * Editing also invalidates any open preview: that card is rendered from the set as it stood when
   * it was opened, so leaving it up would show bytes that quietly stop matching as you type.
   */
  function editDraft(next: { name?: string; content?: string }) {
    const name = next.name ?? draftName;
    const content = next.content ?? draft;
    setDraftName(name);
    setDraft(content);
    setPreview(null);
    if (!active) return;
    if (content === active.content && name === active.name) {
      forgetRulesDraft(active.id); // typed back to the saved text: nothing to hold
    } else {
      setRulesDraft(active.id, { name, content });
    }
  }

  function assertDraftHasNoMarkers() {
    if (RESERVED_MARKERS.some((marker) => draft.includes(marker))) {
      throw new Error(
        "Remove Toolport's generated markers before saving. Paste only your rules, not the preview wrapper.",
      );
    }
  }

  async function saveDraft() {
    assertDraftHasNoMarkers();
    return rulesSaveSet(draftName, draft, active!.id);
  }

  /** Persist an unsaved draft, adopting the refreshed view. No-op when nothing changed. */
  async function flushDraft() {
    if (dirty && active) adopt(await saveDraft());
  }

  /**
   * Every action EXCEPT Save and Delete goes through here.
   *
   * `adopt` reseats the editor from the SAVED set, so any action that refreshes the view would
   * otherwise discard whatever the user had typed and put the old text back. That is not an edge
   * case: "type your rules, then switch a client on" is the first thing anyone does. Flushing
   * first turns that discard into a save.
   *
   * Three deliberate exceptions. Save IS the flush. Delete would save into a set that is about to
   * be thrown away. Preview promises to write nothing, so it sends the draft as an argument
   * instead (see `openPreview`).
   */
  async function act(fn: () => Promise<RulesViewData>) {
    await run(async () => {
      await flushDraft();
      return fn();
    });
  }

  /** Switch the edited set. */
  async function selectSet(id: string) {
    await act(() => rulesSetActive(id));
  }

  /**
   * Create a set and switch to it. The backend only auto-activates a new set when nothing else
   * is active (so a background create can never hijack the user's current rules), which means
   * this button has to select it explicitly: someone who asks for a new set means to edit it.
   */
  async function newSet() {
    const known = new Set((data?.sets ?? []).map((s) => s.id));
    await act(async () => {
      const created = await rulesSaveSet("New rules", "");
      const fresh = created.sets.find((s) => !known.has(s.id));
      return fresh ? rulesSetActive(fresh.id) : created;
    });
  }

  /**
   * Open the dry run for one client.
   *
   * Sends the DRAFT to the backend rather than saving it first. Saving applies to every opted-in
   * client, so a save-then-preview would have this button write the files it claims to be a dry
   * run of. It does not go through `act` for the same reason.
   */
  async function openPreview(clientId: string) {
    setBusy(true);
    setError(null);
    // Drop any previous preview up front. If this one fails, a card left over from ANOTHER client
    // would sit there under the error looking like it belongs to the client just clicked.
    setPreview(null);
    try {
      setPreview(await rulesPreview(clientId, dirty ? draft : undefined));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  if (loading) {
    return <p className="text-sm text-muted-foreground">Loading…</p>;
  }

  // A failed load has no data, and every empty-state line below reads off `data`. Rendering them
  // would tell the user their machine has no clients and no rule sets, which is a claim we have no
  // basis for: we never found out. Show the failure and a way to retry instead.
  if (!data) {
    return (
      <div className="flex flex-col gap-3">
        <Callout variant="danger" role="alert">
          {error ?? "Could not load your agent rules."}
        </Callout>
        <Button
          size="sm"
          variant="outline"
          className="self-start"
          disabled={busy}
          onClick={() => run(rulesView, true)}
        >
          <RefreshCw className="size-3.5" />
          Try again
        </Button>
      </div>
    );
  }

  const clients = data?.clients ?? [];
  const supported = clients.filter((c) => c.path);
  const unsupported = clients.filter((c) => !c.path);
  const onCount = supported.filter((c) => c.enabled).length;
  // Name the client in the card header. The path alone is ambiguous wherever two clients share a
  // file (Claude Code / VS Code, Gemini CLI / Antigravity).
  const previewClientName = preview
    ? (clients.find((c) => c.id === preview.clientId)?.name ?? null)
    : null;

  return (
    <div className="flex flex-col gap-5">
      {error && (
        <Callout variant="danger" role="alert">
          {error}
        </Callout>
      )}

      <div className="rounded-xl border bg-card p-5">
        <div className="mb-1 flex items-center gap-2">
          <FileText className="size-4 text-muted-foreground" />
          <h2 className="text-sm font-medium">Your agent rules</h2>
        </div>
        <p className="mb-4 text-xs text-muted-foreground">
          Written into each client's own rules file next to, never over, anything you
          already have there. Turning a client off removes what Toolport put in it.
        </p>

        <div className="mb-3 flex flex-wrap items-center gap-1.5">
          {(data?.sets ?? []).map((s) => (
            // Each set carries its own delete. Offering it only for the ACTIVE set meant deleting
            // any other one required selecting it first, which applies it: every opted-in client's
            // file would be rewritten to the set being thrown away, then emptied again.
            <span
              key={s.id}
              className={`inline-flex items-center gap-1 rounded-md border pr-1 pl-2.5 text-xs transition-colors ${
                s.id === data?.activeSetId
                  ? "border-primary bg-primary/10 text-foreground"
                  : "text-muted-foreground"
              }`}
            >
              <button
                type="button"
                disabled={busy}
                onClick={() => selectSet(s.id)}
                aria-current={s.id === data?.activeSetId}
                className="py-1 hover:text-foreground"
              >
                {s.name}
              </button>
              <button
                type="button"
                disabled={busy}
                aria-label={`Delete ${s.name}`}
                title={`Delete ${s.name}`}
                onClick={() => setConfirmDelete(s.id)}
                className="rounded p-0.5 text-muted-foreground hover:bg-accent hover:text-destructive"
              >
                <X className="size-3" />
              </button>
            </span>
          ))}
          <Button variant="outline" size="sm" disabled={busy} onClick={newSet}>
            <Plus className="size-3.5" />
            New set
          </Button>
        </div>

        {active ? (
          <>
            <Input
              value={draftName}
              disabled={busy}
              aria-label="Rule set name"
              onChange={(e) => editDraft({ name: e.target.value })}
              className="mb-2 h-9"
            />
            <textarea
              value={draft}
              disabled={busy}
              aria-label="Rules"
              onChange={(e) => editDraft({ content: e.target.value })}
              rows={12}
              spellCheck={false}
              placeholder="Always run the tests before you say you're done."
              className="mb-3 w-full resize-y rounded-lg border bg-background p-3 font-mono text-xs text-foreground"
            />
            <div className="flex flex-wrap items-center gap-2">
              <Button size="sm" disabled={busy || !dirty} onClick={() => run(saveDraft)}>
                {dirty ? "Save and apply" : "Saved"}
              </Button>
              <Button
                variant="outline"
                size="sm"
                disabled={busy}
                onClick={() => act(rulesApply)}
              >
                <RefreshCw className="size-3.5" />
                Re-apply
              </Button>
            </div>
          </>
        ) : (data?.sets.length ?? 0) > 0 ? (
          // Deleting the active set clears the selection rather than promoting a sibling, so the
          // other sets are still right there. Saying "no rule set yet" would be a plain lie about
          // what is on screen.
          <p className="text-sm text-muted-foreground">
            No set is applied right now. Pick one above to edit and apply it.
          </p>
        ) : (
          <p className="text-sm text-muted-foreground">
            No rule set yet. Create one and it applies to the clients you switch on below.
          </p>
        )}
      </div>

      <div className="rounded-xl border bg-card p-5">
        <h2 className="mb-1 text-sm font-medium">Clients</h2>
        <p className="mb-3 text-xs text-muted-foreground">
          {supported.length === 0
            ? "No AI clients with a rules file Toolport can manage were found on this machine."
            : `${onCount} of ${supported.length} switched on. Nothing is written to a client until you turn it on.`}
        </p>

        <ul className="grid gap-1.5">
          {supported.map((c) => (
            <li key={c.id} className="flex items-center justify-between gap-3 text-sm">
              <label className="flex min-w-0 items-center gap-2">
                <input
                  type="checkbox"
                  checked={c.enabled}
                  disabled={busy}
                  aria-label={c.name}
                  onChange={(e) => {
                    // Read the new value NOW. `act` awaits a possible draft save first, and by
                    // the time the inner callback runs React has re-rendered this controlled
                    // input back to `c.enabled`, so a lazy `e.target.checked` reads the OLD
                    // value and the toggle silently does nothing.
                    const next = e.target.checked;
                    act(() => rulesSetClientEnabled(c.id, next));
                  }}
                />
                <span className="truncate" title={c.path}>
                  {c.name}
                </span>
              </label>
              <span className="flex shrink-0 items-center gap-2">
                {/* With no active set, Applied means correctly empty and is hidden. Refusal/error
                    states still matter because they mean cleanup left rules on disk. */}
                {c.enabled && (active || c.state !== "applied") && (
                  <RuleStateBadge state={c.state} />
                )}
                <button
                  type="button"
                  disabled={busy || !active}
                  title={
                    active
                      ? "See exactly what would be written"
                      : data.sets.length > 0
                        ? "Pick a rule set first"
                        : "Create a rule set first"
                  }
                  onClick={() => openPreview(c.id)}
                  className="inline-flex items-center gap-1 rounded-md border px-2 py-0.5 text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-foreground disabled:opacity-50"
                >
                  <Eye className="size-3" />
                  Preview
                </button>
              </span>
            </li>
          ))}
        </ul>

        {unsupported.length > 0 && (
          <p className="mt-3 text-xs text-muted-foreground">
            No rules file Toolport can write for{" "}
            {unsupported.map((c) => c.name).join(", ")}. Paste your rules in by hand.
          </p>
        )}
      </div>

      {preview && (
        <div ref={previewRef} className="scroll-mt-4 rounded-xl border bg-card p-5">
          <div className="mb-1 flex items-center justify-between gap-3">
            <h2 className="text-sm font-medium">
              Preview{previewClientName ? `: ${previewClientName}` : ""}
            </h2>
            <button
              type="button"
              onClick={() => setPreview(null)}
              className="text-xs text-muted-foreground hover:text-foreground"
            >
              Close
            </button>
          </div>
          <p className="mb-3 font-mono text-xs break-all text-muted-foreground">
            {preview.path}
          </p>
          <p className="mb-2 text-xs text-muted-foreground">
            {preview.strategy === "ownedFile"
              ? "Toolport owns this whole file. Nothing of yours lives in it."
              : "Toolport owns only the marked block. Every other byte in this file is left exactly as it is."}
          </p>
          {/* The bytes below are what an unblocked write would produce. When this client refuses
              the write, saying so here matters more than the bytes do: a preview that looks like
              a normal write, followed by nothing landing, reads as a bug rather than a rule. */}
          {BLOCKING_STATES.has(preview.state) && (
            <Callout variant="warning" className="mb-2 text-xs">
              This is what Toolport would write, but it will not be written to this
              client: {PREVIEW_BLOCKED_REASON[preview.state]}
            </Callout>
          )}
          <pre className="max-h-64 overflow-auto rounded-lg border bg-muted/20 p-3 font-mono text-xs whitespace-pre-wrap text-foreground">
            {preview.after}
          </pre>
        </div>
      )}

      <ConfirmDialog
        open={confirmDelete !== null}
        onOpenChange={(o) => !o && setConfirmDelete(null)}
        title="Delete this rule set?"
        description={
          confirmDelete === data.activeSetId
            ? "Toolport removes what it wrote from every client. Your own content in those files is left alone."
            : "This unused rule set is deleted. The active set and client files stay unchanged."
        }
        confirmLabel="Delete"
        destructive
        onConfirm={() => {
          const id = confirmDelete;
          setConfirmDelete(null);
          if (id) {
            void (id === data.activeSetId
              ? run(() => rulesDeleteSet(id))
              : act(() => rulesDeleteSet(id)));
          }
        }}
      />
    </div>
  );
}
