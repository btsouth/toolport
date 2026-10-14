import { useI18n, Trans } from "@/lib/i18n";
import { useEffect, useRef, useState } from "react";
import { Eye, FileText, FileUp, FolderPlus, Plus, RefreshCw, X } from "lucide-react";
import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Callout } from "@/components/Callout";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { RuleStateBadge } from "@/components/RuleStateBadge";
import {
  rulesApply,
  rulesApplyClient,
  rulesDeleteSet,
  rulesImportCandidates,
  rulesImportFile,
  rulesPreview,
  rulesProjectAdd,
  rulesProjectApply,
  rulesProjectPreview,
  rulesProjectRemove,
  rulesProjectSetFileEnabled,
  rulesProjectSetSet,
  rulesSaveSet,
  rulesSetActive,
  rulesSetClientEnabled,
  rulesView,
} from "@/lib/api";
import { forgetRulesDraft, getRulesDraft, setRulesDraft } from "@/lib/rulesDraftCache";
import { lineDiff } from "@/lib/lineDiff";
import type {
  InstructionsApplyState,
  RulesImportCandidate,
  RulesPreview,
  RulesProjectFileStatus,
  RulesProjectStatus,
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
function formatBytes(n: number): string {
  return n < 1024 ? `${n} B` : `${(n / 1024).toFixed(1)} KB`;
}

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
  const { t: tr } = useI18n();
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
  // Set alongside `preview` when it is a PROJECT file's dry run, which has no client to name.
  const [previewTitle, setPreviewTitle] = useState<string | null>(null);
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);
  // Pull into set would replace whatever is typed in the editor; with unsaved edits, ask first.
  const [confirmPull, setConfirmPull] = useState<string | null>(null);
  const [confirmRemoveProject, setConfirmRemoveProject] = useState<string | null>(null);
  // The diff card for a client whose block was edited on disk (SBS-1036).
  const [drift, setDrift] = useState<{
    clientId: string;
    clientName: string;
    onDisk: string;
  } | null>(null);

  // "Start from a file" (SBS-1035). `null` = panel closed; `candidates: null` = still looking.
  const [importing, setImporting] = useState<{
    candidates: RulesImportCandidate[] | null;
  } | null>(null);
  const [importNote, setImportNote] = useState<string | null>(null);

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
    // Same for a drift card: the refreshed view carries the current on-disk body.
    setDrift(null);
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

  /**
   * Run a mutating call, adopt the refreshed view, and surface any failure in place.
   *
   * On failure the view is refreshed anyway, best-effort and draft-preserving: an action can do
   * part of its work and then report a problem (a project Apply that wrote two files and was
   * refused on the third, a remove that could not clean one path), and leaving the rows as they
   * were would show a state the disk and the registry no longer have.
   */
  async function run(fn: () => Promise<RulesViewData>, preserveDraft = false) {
    setBusy(true);
    setError(null);
    try {
      const next = await fn();
      if (preserveDraft) adoptPreservingDraft(next);
      else adopt(next);
    } catch (e) {
      setError(String(e));
      try {
        adoptPreservingDraft(await rulesView());
      } catch {
        // The error above is the one worth showing; a failed refresh adds nothing.
      }
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
        tr(
          "Remove Toolport's generated markers before saving. Paste only your rules, not the preview wrapper.",
        ),
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
      const created = await rulesSaveSet(tr("New rules"), "");
      const fresh = created.sets.find((s) => !known.has(s.id));
      return fresh ? rulesSetActive(fresh.id) : created;
    });
  }

  /**
   * Show how a client's on-disk block differs from the saved set, with the two ways out: take the
   * edit into the set (a draft, nothing written) or put the set back over it (the explicit
   * overwrite, which is what Re-apply does). Until one of those happens every automatic apply
   * leaves the edited block alone.
   */
  function openDrift(clientId: string, clientName: string, onDisk: string) {
    setPreview(null);
    setDrift({ clientId, clientName, onDisk });
  }

  // ---- Project-level rules (SBS-1037) ----
  //
  // Every project action goes through `run(..., true)`: it refreshes the view but must not touch
  // the GLOBAL editor's unsaved draft, which is about a different set entirely.

  async function addProject() {
    try {
      const picked = await openFileDialog({
        directory: true,
        multiple: false,
        title: tr("Choose a project folder"),
      });
      if (typeof picked === "string") await run(() => rulesProjectAdd(picked), true);
    } catch (e) {
      setError(tr("Couldn't open the folder picker: {value1}", { value1: String(e) }));
    }
  }

  async function openProjectPreview(p: RulesProjectStatus, f: RulesProjectFileStatus) {
    setBusy(true);
    setError(null);
    setPreview(null);
    try {
      setPreview(await rulesProjectPreview(p.id, f.key));
      setPreviewTitle(`${p.name} - ${f.relPath}`);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  /**
   * Start a new set from a rules file already on disk (SBS-1035). Opens a panel listing the
   * files the detected clients already read - the user's own text, which is almost always where
   * their rules live before Toolport - plus a picker for anything else.
   */
  async function openImport() {
    setError(null);
    setImportNote(null);
    setImporting({ candidates: null });
    try {
      setImporting({ candidates: await rulesImportCandidates() });
    } catch (e) {
      setImporting(null);
      setError(String(e));
    }
  }

  /**
   * Import one file as a NEW set. The file is read and never written. The set is created with
   * the file's text and is NOT selected: selecting a set applies it to every switched-on
   * client, and that is the user's call to make with the new chip in front of them, not a side
   * effect of importing. (Creating it empty and selecting it, to land the text as a draft,
   * would have applied an EMPTY set first and wiped those clients' files.) The backend selects
   * the new set only when nothing else was selected, the same rule New set follows.
   */
  async function importFrom(path: string, clientName?: string) {
    setImporting(null);
    let note: string | null = null;
    const known = new Set((data?.sets ?? []).map((s) => s.id));
    await run(async () => {
      await flushDraft();
      const imported = await rulesImportFile(path, clientName);
      const created = await rulesSaveSet(imported.name, imported.content);
      // A set id can be reused after a delete ("Imported from Codex" again), and a draft held
      // for the deleted one would otherwise be restored over the imported text.
      const fresh = created.sets.find((s) => !known.has(s.id));
      if (fresh) forgetRulesDraft(fresh.id);
      const nowActive = created.activeSetId !== data?.activeSetId;
      note =
        (imported.strippedOurs
          ? tr(
              'Created "{value1}" from your own text in {value2}; the block Toolport had written there was left out. ',
              { value1: imported.name, value2: imported.path },
            )
          : tr('Created "{value1}" from {value2}. ', {
              value1: imported.name,
              value2: imported.path,
            })) +
        (nowActive
          ? tr("It is now your applied set. The file itself was not changed.")
          : tr("Pick it above to edit and apply it. The file itself was not changed."));
      return created;
    }, true);
    setImportNote(note);
  }

  async function importFromPicker() {
    try {
      const picked = await openFileDialog({
        multiple: false,
        directory: false,
        title: tr("Choose a rules file"),
      });
      if (typeof picked === "string") await importFrom(picked);
    } catch (e) {
      setError(tr("Couldn't open the file picker: {value1}", { value1: String(e) }));
    }
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
    setPreviewTitle(null);
    try {
      setPreview(await rulesPreview(clientId, dirty ? draft : undefined));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  if (loading) {
    return <p className="text-sm text-muted-foreground">{tr("Loading…")}</p>;
  }

  // A failed load has no data, and every empty-state line below reads off `data`. Rendering them
  // would tell the user their machine has no clients and no rule sets, which is a claim we have no
  // basis for: we never found out. Show the failure and a way to retry instead.
  if (!data) {
    return (
      <div className="flex flex-col gap-3">
        <Callout variant="danger" role="alert">
          {error ?? tr("Could not load your agent rules.")}
        </Callout>
        <Button
          size="sm"
          variant="outline"
          className="self-start"
          disabled={busy}
          onClick={() => run(rulesView, true)}
        >
          <Trans
            message="{value1} Try again"
            values={{ value1: <RefreshCw className="size-3.5" /> }}
          />
        </Button>
      </div>
    );
  }

  const clients = data?.clients ?? [];
  const supported = clients.filter((c) => c.path);
  const unsupported = clients.filter((c) => !c.path);
  // Three different truths hide behind "no global rules file", and lumping them into one
  // "unsupported" sentence overstates it: Cursor and Copilot CLI are reached per project,
  // and Claude Desktop is the chat app while Claude Code inside it is already covered.
  const projectOnly = unsupported.filter((c) => c.projectCovered);
  const chatDesktop = unsupported.filter((c) => c.id === "claude-desktop");
  const manualOnly = unsupported.filter(
    (c) => !c.projectCovered && c.id !== "claude-desktop",
  );
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
          <h2 className="text-sm font-medium">{tr("Your agent rules")}</h2>
        </div>
        <p className="mb-4 text-xs text-muted-foreground">
          {tr(
            "Written into each client's own rules file next to, never over, anything you already have there. Turning a client off removes what Toolport put in it.",
          )}
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
                aria-label={tr("Delete {value1}", { value1: s.name })}
                title={tr("Delete {value1}", { value1: s.name })}
                onClick={() => setConfirmDelete(s.id)}
                className="rounded p-0.5 text-muted-foreground hover:bg-accent hover:text-destructive"
              >
                <X className="size-3" />
              </button>
            </span>
          ))}
          <Button variant="outline" size="sm" disabled={busy} onClick={newSet}>
            <Trans
              message="{value1} New set"
              values={{ value1: <Plus className="size-3.5" /> }}
            />
          </Button>
          <Button variant="outline" size="sm" disabled={busy} onClick={openImport}>
            <Trans
              message="{value1} Start from a file"
              values={{ value1: <FileUp className="size-3.5" /> }}
            />
          </Button>
        </div>

        {importing && (
          <div className="mb-3 rounded-lg border bg-muted/20 p-3 text-xs">
            <div className="mb-2 flex items-center justify-between gap-3">
              <span className="font-medium text-foreground">
                {tr("Start a new set from a file")}
              </span>
              <button
                type="button"
                className="text-muted-foreground hover:text-foreground"
                onClick={() => setImporting(null)}
              >
                {tr("Close")}
              </button>
            </div>
            <p className="mb-2 text-muted-foreground">
              {tr(
                "Toolport reads the file and creates a new set from your text. Anything Toolport itself wrote into the file is left out, and the file is not changed. Nothing is applied until you pick the new set.",
              )}
            </p>
            {importing.candidates === null ? (
              <p className="mb-2 text-muted-foreground">
                {tr("Looking for rules files…")}
              </p>
            ) : importing.candidates.length === 0 ? (
              <p className="mb-2 text-muted-foreground">
                {tr("No rules files found for the detected clients.")}
              </p>
            ) : (
              <ul className="mb-2 grid gap-1">
                {importing.candidates.map((c) => (
                  <li key={c.path}>
                    <button
                      type="button"
                      disabled={busy}
                      onClick={() => importFrom(c.path, c.clientName)}
                      className="flex w-full items-center justify-between gap-3 rounded-md border px-2 py-1 text-left transition-colors hover:bg-accent disabled:opacity-50"
                    >
                      <span className="truncate">
                        <span className="text-foreground">{c.clientName}</span>{" "}
                        <span className="font-mono text-muted-foreground">{c.path}</span>
                      </span>
                      <span className="shrink-0 text-muted-foreground">
                        {formatBytes(c.bytes)}
                      </span>
                    </button>
                  </li>
                ))}
              </ul>
            )}
            <Button
              variant="outline"
              size="sm"
              disabled={busy}
              onClick={importFromPicker}
            >
              {tr("Choose a file…")}
            </Button>
          </div>
        )}

        {importNote && (
          <Callout variant="info" className="mb-3 text-xs">
            {importNote}
          </Callout>
        )}

        {active ? (
          <>
            <Input
              value={draftName}
              disabled={busy}
              aria-label={tr("Rule set name")}
              onChange={(e) => editDraft({ name: e.target.value })}
              className="mb-2 h-9"
            />
            <textarea
              value={draft}
              disabled={busy}
              aria-label={tr("Rules")}
              onChange={(e) => editDraft({ content: e.target.value })}
              rows={12}
              spellCheck={false}
              placeholder={tr("Always run the tests before you say you're done.")}
              className="mb-3 w-full resize-y rounded-lg border bg-background p-3 font-mono text-xs text-foreground"
            />
            <div className="flex flex-wrap items-center gap-2">
              <Button size="sm" disabled={busy || !dirty} onClick={() => run(saveDraft)}>
                {dirty ? tr("Save and apply") : tr("Saved")}
              </Button>
              <Button
                variant="outline"
                size="sm"
                disabled={busy}
                title={tr(
                  "Rewrite every switched-on client's file from this set, including a block you edited on disk",
                )}
                onClick={() => act(rulesApply)}
              >
                <Trans
                  message="{value1} Re-apply"
                  values={{ value1: <RefreshCw className="size-3.5" /> }}
                />
              </Button>
            </div>
          </>
        ) : (data?.sets.length ?? 0) > 0 ? (
          // Deleting the active set clears the selection rather than promoting a sibling, so the
          // other sets are still right there. Saying "no rule set yet" would be a plain lie about
          // what is on screen.
          <p className="text-sm text-muted-foreground">
            {tr("No set is applied right now. Pick one above to edit and apply it.")}
          </p>
        ) : (
          <p className="text-sm text-muted-foreground">
            {tr(
              "No rule set yet. Create one, or start from a rules file you already have, and it applies to the clients you switch on below.",
            )}
          </p>
        )}
      </div>

      <div className="rounded-xl border bg-card p-5">
        <h2 className="mb-1 text-sm font-medium">{tr("Clients")}</h2>
        <p className="mb-3 text-xs text-muted-foreground">
          {supported.length === 0
            ? tr(
                "No AI clients with a rules file Toolport can manage were found on this machine.",
              )
            : tr(
                "{value1} of {value2} switched on. Nothing is written to a client until you turn it on.",
                { value1: onCount, value2: supported.length },
              )}
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
                {c.state === "drifted" && c.onDisk !== undefined && (
                  <button
                    type="button"
                    disabled={busy}
                    title={tr("See how the file differs from this set")}
                    onClick={() => openDrift(c.id, c.name, c.onDisk ?? "")}
                    className="inline-flex items-center gap-1 rounded-md border px-2 py-0.5 text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-foreground disabled:opacity-50"
                  >
                    {tr("View diff")}
                  </button>
                )}
                <button
                  type="button"
                  disabled={busy || !active}
                  title={
                    active
                      ? tr("See exactly what would be written")
                      : data.sets.length > 0
                        ? tr("Pick a rule set first")
                        : tr("Create a rule set first")
                  }
                  onClick={() => openPreview(c.id)}
                  className="inline-flex items-center gap-1 rounded-md border px-2 py-0.5 text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-foreground disabled:opacity-50"
                >
                  <Trans
                    message="{value1} Preview"
                    values={{ value1: <Eye className="size-3" /> }}
                  />
                </button>
              </span>
            </li>
          ))}
        </ul>

        {unsupported.length > 0 && (
          <div className="mt-3 space-y-1 text-xs text-muted-foreground">
            {projectOnly.length > 0 && (
              <p>
                {projectOnly.length === 1
                  ? tr(
                      "No global rules file for {value1}, but project rules reach it: add a folder under Projects below.",
                      { value1: projectOnly.map((c) => c.name).join(", ") },
                    )
                  : tr(
                      "No global rules file for {value1}, but project rules reach them: add a folder under Projects below.",
                      { value1: projectOnly.map((c) => c.name).join(", ") },
                    )}
              </p>
            )}
            {chatDesktop.length > 0 && (
              <p>
                {tr(
                  "Claude Desktop is the chat app and has no rules file; Claude Code inside it is covered by the Claude Code row above.",
                )}
              </p>
            )}
            {manualOnly.length > 0 && (
              <p>
                {tr(
                  "No rules file Toolport can write for  {value1}. Paste your rules in by hand.",
                  { value1: manualOnly.map((c) => c.name).join(", ") },
                )}
              </p>
            )}
          </div>
        )}
      </div>

      <div className="rounded-xl border bg-card p-5">
        <div className="mb-1 flex items-center justify-between gap-3">
          <h2 className="text-sm font-medium">{tr("Projects")}</h2>
          <Button variant="outline" size="sm" disabled={busy} onClick={addProject}>
            <Trans
              message="{value1} Add a project folder"
              values={{ value1: <FolderPlus className="size-3.5" /> }}
            />
          </Button>
        </div>
        <p className="mb-3 text-xs text-muted-foreground">
          <Trans
            message="Project rules go into the folder itself, beside your repo&rsquo;s own files, and will show up in {value1}. Toolport writes only its own marked block (or its own file) and only when you press Apply for that project, never at startup. Each file names the clients that read it there."
            values={{ value1: <span className="font-mono">{"git status"}</span> }}
          />
        </p>
        {data.projects.length === 0 ? (
          <p className="text-sm text-muted-foreground">
            {tr("No project folders registered. Add one to apply a rule set inside it.")}
          </p>
        ) : (
          <ul className="grid gap-3">
            {data.projects.map((p) => {
              const set = data.sets.find((s) => s.id === p.setId) ?? null;
              const anyOn = p.files.some((f) => f.enabled);
              return (
                <li key={p.id} className="rounded-lg border p-3">
                  <div className="mb-2 flex items-start justify-between gap-3">
                    <div className="min-w-0">
                      <div className="text-sm font-medium">{p.name}</div>
                      <div
                        className="truncate font-mono text-xs text-muted-foreground"
                        title={p.path}
                      >
                        {p.path}
                      </div>
                    </div>
                    <button
                      type="button"
                      disabled={busy}
                      aria-label={tr("Remove project {value1}", { value1: p.name })}
                      title={tr("Remove this project and what Toolport wrote in it")}
                      onClick={() => setConfirmRemoveProject(p.id)}
                      className="rounded p-0.5 text-muted-foreground hover:bg-accent hover:text-destructive"
                    >
                      <X className="size-3.5" />
                    </button>
                  </div>
                  <label className="mb-2 flex items-center gap-2 text-xs">
                    <span className="text-muted-foreground">{tr("Rule set")}</span>
                    <select
                      aria-label={tr("Rule set for {value1}", { value1: p.name })}
                      value={p.setId ?? ""}
                      disabled={busy}
                      onChange={(e) => {
                        const next = e.target.value || undefined;
                        void run(() => rulesProjectSetSet(p.id, next), true);
                      }}
                      className="rounded-md border bg-background px-2 py-1 text-xs"
                    >
                      <option value="">{tr("None")}</option>
                      {data.sets.map((s) => (
                        <option key={s.id} value={s.id}>
                          {s.name}
                        </option>
                      ))}
                    </select>
                  </label>
                  <ul className="mb-2 grid gap-1.5">
                    {p.files.map((f) => (
                      <li
                        key={f.key}
                        className="flex items-center justify-between gap-3 text-sm"
                      >
                        <label className="flex min-w-0 items-center gap-2">
                          <input
                            type="checkbox"
                            checked={f.enabled}
                            disabled={busy}
                            aria-label={`${f.relPath} in ${p.name}`}
                            onChange={(e) => {
                              const next = e.target.checked;
                              void run(
                                () => rulesProjectSetFileEnabled(p.id, f.key, next),
                                true,
                              );
                            }}
                          />
                          <span className="min-w-0">
                            <span className="font-mono text-xs">{f.relPath}</span>{" "}
                            <span className="text-xs text-muted-foreground">
                              {tr("for {value1}", { value1: f.clients.join(", ") })}
                            </span>
                          </span>
                        </label>
                        <span className="flex shrink-0 items-center gap-2">
                          {f.enabled && (set || f.state !== "applied") && (
                            <RuleStateBadge state={f.state} />
                          )}
                          <button
                            type="button"
                            disabled={busy || !set}
                            title={
                              set
                                ? tr("See exactly what would be written")
                                : tr("Pick a rule set first")
                            }
                            onClick={() => openProjectPreview(p, f)}
                            className="inline-flex items-center gap-1 rounded-md border px-2 py-0.5 text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-foreground disabled:opacity-50"
                          >
                            <Trans
                              message="{value1} Preview"
                              values={{ value1: <Eye className="size-3" /> }}
                            />
                          </button>
                        </span>
                      </li>
                    ))}
                  </ul>
                  <Button
                    size="sm"
                    disabled={busy || !set || !anyOn}
                    title={
                      !set
                        ? tr("Pick a rule set first")
                        : !anyOn
                          ? tr("Switch on a file first")
                          : tr("Write the switched-on files from this set")
                    }
                    onClick={() => run(() => rulesProjectApply(p.id), true)}
                  >
                    {tr("Apply to {value1}", { value1: p.name })}
                  </Button>
                </li>
              );
            })}
          </ul>
        )}
      </div>

      {preview && (
        <div ref={previewRef} className="scroll-mt-4 rounded-xl border bg-card p-5">
          <div className="mb-1 flex items-center justify-between gap-3">
            <h2 className="text-sm font-medium">
              {tr("Preview{value1}", {
                value1: previewTitle
                  ? `: ${previewTitle}`
                  : previewClientName
                    ? `: ${previewClientName}`
                    : "",
              })}
            </h2>
            <button
              type="button"
              onClick={() => setPreview(null)}
              className="text-xs text-muted-foreground hover:text-foreground"
            >
              {tr("Close")}
            </button>
          </div>
          <p className="mb-3 font-mono text-xs break-all text-muted-foreground">
            {preview.path}
          </p>
          <p className="mb-2 text-xs text-muted-foreground">
            {preview.strategy === "ownedFile"
              ? tr("Toolport owns this whole file. Nothing of yours lives in it.")
              : tr(
                  "Toolport owns only the marked block. Every other byte in this file is left exactly as it is.",
                )}
          </p>
          {/* The bytes below are what an unblocked write would produce. When this client refuses
              the write, saying so here matters more than the bytes do: a preview that looks like
              a normal write, followed by nothing landing, reads as a bug rather than a rule. */}
          {BLOCKING_STATES.has(preview.state) && (
            <Callout variant="warning" className="mb-2 text-xs">
              {tr(
                "This is what Toolport would write, but it will not be written to this client: {value1}",
                { value1: tr(PREVIEW_BLOCKED_REASON[preview.state] ?? "") },
              )}
            </Callout>
          )}
          <pre className="max-h-64 overflow-auto rounded-lg border bg-muted/20 p-3 font-mono text-xs whitespace-pre-wrap text-foreground">
            {preview.after}
          </pre>
        </div>
      )}

      {drift && active && (
        <div className="rounded-xl border bg-card p-5">
          <div className="mb-1 flex items-center justify-between gap-3">
            <h2 className="text-sm font-medium">
              {tr("Edited on disk: {value1}", { value1: drift.clientName })}
            </h2>
            <button
              type="button"
              onClick={() => setDrift(null)}
              className="text-xs text-muted-foreground hover:text-foreground"
            >
              {tr("Close")}
            </button>
          </div>
          <p className="mb-3 text-xs text-muted-foreground">
            {tr(
              "Toolport wrote this block from the set “{value1}” and it has been changed in the file since. Nothing is written here until you choose: pull the file&rsquo;s version into the set (it becomes an unsaved draft you can still edit), or overwrite the file with the set. Lines starting with &minus; are the set as saved; lines starting with + are what the file has now.",
              { value1: active.name },
            )}
          </p>
          <pre className="mb-3 max-h-64 overflow-auto rounded-lg border bg-muted/20 p-3 font-mono text-xs whitespace-pre-wrap">
            {lineDiff(active.content, drift.onDisk).map((line, i) => (
              <div
                key={i}
                className={
                  line.kind === "add"
                    ? "bg-success/10 text-foreground"
                    : line.kind === "del"
                      ? "bg-destructive/10 text-muted-foreground line-through"
                      : "text-muted-foreground"
                }
              >
                {line.kind === "add" ? "+ " : line.kind === "del" ? "\u2212 " : "  "}
                {line.text}
              </div>
            ))}
          </pre>
          <div className="flex flex-wrap items-center gap-2">
            <Button
              size="sm"
              disabled={busy}
              onClick={() => {
                const onDisk = drift.onDisk;
                if (dirty) {
                  setConfirmPull(onDisk);
                  return;
                }
                setDrift(null);
                editDraft({ content: onDisk });
              }}
            >
              {tr("Pull into set")}
            </Button>
            <Button
              variant="outline"
              size="sm"
              disabled={busy}
              title={tr(
                "Rewrite only this client's file from the set as saved; other clients and your unsaved edits are left as they are",
              )}
              onClick={() => {
                // Not through `act`: that would save the draft first, and a saved content
                // change reconcile-writes a new revision to EVERY client, which is the
                // opposite of what "this file" promises. The overwrite uses the set as saved;
                // the draft stays in the editor.
                const id = drift.clientId;
                void run(() => rulesApplyClient(id), true);
              }}
            >
              <Trans
                message="{value1} Overwrite this file"
                values={{ value1: <RefreshCw className="size-3.5" /> }}
              />
            </Button>
          </div>
        </div>
      )}

      <ConfirmDialog
        open={confirmPull !== null}
        onOpenChange={(o) => !o && setConfirmPull(null)}
        title={tr("Replace your unsaved edits?")}
        description={tr(
          "The editor has changes you have not saved. Pulling the file's version in replaces them. Nothing is written to disk either way.",
        )}
        confirmLabel={tr("Replace")}
        destructive
        onConfirm={() => {
          const onDisk = confirmPull;
          setConfirmPull(null);
          setDrift(null);
          if (onDisk !== null) editDraft({ content: onDisk });
        }}
      />

      <ConfirmDialog
        open={confirmRemoveProject !== null}
        onOpenChange={(o) => !o && setConfirmRemoveProject(null)}
        title={tr("Remove this project?")}
        description={tr(
          "Toolport removes what it wrote in the folder. Your repo's own files are left alone.",
        )}
        confirmLabel={tr("Remove")}
        destructive
        onConfirm={() => {
          const id = confirmRemoveProject;
          setConfirmRemoveProject(null);
          if (id) void run(() => rulesProjectRemove(id), true);
        }}
      />

      <ConfirmDialog
        open={confirmDelete !== null}
        onOpenChange={(o) => !o && setConfirmDelete(null)}
        title={tr("Delete this rule set?")}
        description={(() => {
          // A set can be in use by projects even when it is not the global active set;
          // deleting it cleans Toolport's files out of those folders too, so say so.
          const projects = data.projects.filter((p) => p.setId === confirmDelete);
          const projectNote =
            projects.length > 0
              ? tr(
                  " It is also applied to {value1} {value2}: Toolport removes what it wrote in {value3}. Your repo's own files are left alone.",
                  {
                    value1: projects.length === 1 ? tr("the project") : tr("projects"),
                    value2: projects.map((p) => `\u201C${p.name}\u201D`).join(", "),
                    value3:
                      projects.length === 1 ? tr("that folder") : tr("those folders"),
                  },
                )
              : "";
          return confirmDelete === data.activeSetId
            ? tr(
                "Toolport removes what it wrote from every client. Your own content in those files is left alone.{value1}",
                { value1: projectNote },
              )
            : projects.length > 0
              ? tr(
                  "This rule set is deleted. The active set and client files stay unchanged.{value1}",
                  { value1: projectNote },
                )
              : tr(
                  "This unused rule set is deleted. The active set and client files stay unchanged.",
                );
        })()}
        confirmLabel={tr("Delete")}
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
