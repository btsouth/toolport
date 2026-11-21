import { useI18n, Trans } from "@/lib/i18n";
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import {
  ArrowLeft,
  Check,
  Copy,
  FileDown,
  FileUp,
  Link2,
  Loader2,
  Upload,
} from "lucide-react";
import { toast } from "sonner";
import { toastError } from "@/lib/toast";
import {
  exportConfig,
  exportConfigToPath,
  fetchSharedSetup,
  getRegistry,
  importConfig,
  previewImport,
  readSetupFile,
  shareStack,
  takePendingShared,
} from "@/lib/api";
import { listen } from "@tauri-apps/api/event";
import { isGatewayServer, type ImportItem, type Registry } from "@/lib/types";
import { SkeletonRows } from "@/components/ui/skeleton";
import { ErrorCallout } from "@/components/ui/status-card";
import { EmptyState } from "@/components/ui/empty-state";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { Label } from "@/components/ui/label";
import { ImportRow } from "@/components/ImportReviewDialog";

interface Props {
  trigger: ReactNode;
  onImported: (registry: Registry) => void;
}

type LoadState = "idle" | "loading" | "ready" | "error";

/** Share a curated server set with a teammate (and import theirs). Secret values
 * are never included - each person vaults their own keys after importing. This is
 * the no-backend version of "push a setup to your team".
 *
 * A shared setup is untrusted input: each server carries a command that runs when
 * the server is enabled. So importing is two steps - preview exactly what would be
 * added (command/args/url), then confirm - rather than applying a pasted blob blind. */
export function ShareDialog({ trigger, onImported }: Props) {
  const { t } = useI18n();
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [exported, setExported] = useState("");
  const [debouncedName, setDebouncedName] = useState(name);
  const [debouncedDescription, setDebouncedDescription] = useState(description);
  const [paste, setPaste] = useState("");
  // A paste that could not be read: shown under the field, as in GTK.
  const [pasteError, setPasteError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [busyAction, setBusyAction] = useState<
    "preview-paste" | "preview-file" | "import" | null
  >(null);
  // When set, the dialog shows the review-and-confirm view for `pendingJson`.
  const [preview, setPreview] = useState<ImportItem[] | null>(null);
  const [pendingJson, setPendingJson] = useState("");
  // The new servers in the review that will be imported (default all, by name).
  const [importSelection, setImportSelection] = useState<Set<string>>(new Set());
  // The user's servers and which to include in the shared stack (default all).
  const [servers, setServers] = useState<
    { id: string; name: string; transport: string }[]
  >([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [registryState, setRegistryState] = useState<LoadState>("idle");
  const [registryError, setRegistryError] = useState("");
  const [exportState, setExportState] = useState<LoadState>("idle");
  const [exportError, setExportError] = useState("");
  const [exportRetry, setExportRetry] = useState(0);
  // A generated share link (toolport.app/s/...), cleared when the export changes.
  const [shareLink, setShareLink] = useState("");
  const [linkCopied, setLinkCopied] = useState(false);
  const [linking, setLinking] = useState(false);
  const [saving, setSaving] = useState(false);
  // Request generations make close/reopen, retry, and out-of-order completions inert.
  const registryRequest = useRef(0);
  const exportRequest = useRef(0);
  const linkRequest = useRef(0);
  const previewRequest = useRef(0);

  const invalidateShareOutput = useCallback((nextState: LoadState = "loading") => {
    exportRequest.current += 1;
    linkRequest.current += 1;
    setExported("");
    setExportState(nextState);
    setExportError("");
    setShareLink("");
    setLinkCopied(false);
    setLinking(false);
    setCopied(false);
  }, []);

  const loadServers = useCallback(async () => {
    const request = ++registryRequest.current;
    invalidateShareOutput("idle");
    setRegistryState("loading");
    setRegistryError("");
    setServers([]);
    setSelected(new Set());
    try {
      const reg = await getRegistry();
      if (request !== registryRequest.current) return;
      const list = reg.servers
        .filter((s) => !isGatewayServer(s))
        .map((s) => ({ id: s.id, name: s.name, transport: s.transport }));
      invalidateShareOutput();
      setServers(list);
      setSelected(new Set(list.map((s) => s.id)));
      setRegistryState("ready");
    } catch (e) {
      if (request !== registryRequest.current) return;
      setRegistryState("error");
      setRegistryError(String(e));
    }
  }, [invalidateShareOutput]);

  // Always pass the exact stable-ID snapshot, including for All and verified empty.
  // This prevents duplicate names or a server added after load from widening a share.
  const shareFilter = useMemo(() => Array.from(selected), [selected]);

  useEffect(() => {
    const timer = setTimeout(() => {
      setDebouncedName(name);
      setDebouncedDescription(description);
    }, 250);

    return () => clearTimeout(timer);
  }, [name, description]);

  useEffect(() => {
    if (!open || registryState !== "ready") return;
    const request = ++exportRequest.current;
    linkRequest.current += 1;
    exportConfig(debouncedName, debouncedDescription, shareFilter)
      .then((v) => {
        if (request !== exportRequest.current) return;
        setExported(v);
        setExportState("ready");
      })
      .catch((e) => {
        if (request !== exportRequest.current) return;
        setExported("");
        setExportState("error");
        setExportError(String(e));
      });
    return () => {
      if (request === exportRequest.current) exportRequest.current += 1;
    };
  }, [
    debouncedDescription,
    debouncedName,
    exportRetry,
    open,
    registryState,
    shareFilter,
  ]);

  const onOpenChange = useCallback(
    (next: boolean) => {
      setOpen(next);
      previewRequest.current += 1;
      setBusyAction(null);
      if (next) {
        setPaste("");
        setPasteError(null);
        setCopied(false);
        setPreview(null);
        setPendingJson("");
        void loadServers();
      } else {
        registryRequest.current += 1;
        setRegistryState("idle");
        setRegistryError("");
        invalidateShareOutput("idle");
      }
    },
    [invalidateShareOutput, loadServers],
  );

  async function createLink() {
    const request = ++linkRequest.current;
    const setup = exported;
    setLinking(true);
    try {
      const url = await shareStack(setup);
      if (request !== linkRequest.current) return;
      setShareLink(url);
      try {
        await navigator.clipboard.writeText(url);
        if (request !== linkRequest.current) return;
        toast.success(t("Share link created and copied"));
      } catch {
        if (request !== linkRequest.current) return;
        toast.success(t("Share link created"));
        toastError(t("Couldn't copy automatically. Select the link and copy it."));
      }
    } catch (e) {
      if (request !== linkRequest.current) return;
      toastError(t("Couldn't create a link: {value1}", { value1: String(e) }));
    } finally {
      if (request === linkRequest.current) setLinking(false);
    }
  }

  async function copy() {
    try {
      await navigator.clipboard.writeText(exported);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      toastError(t("Couldn't copy automatically. Select the text and copy it."));
    }
  }

  async function copyLink() {
    try {
      await navigator.clipboard.writeText(shareLink);
      setLinkCopied(true);
      setTimeout(() => setLinkCopied(false), 1500);
    } catch {
      toastError(t("Couldn't copy automatically. Select the link and copy it."));
    }
  }

  async function saveToFile() {
    setSaving(true);
    try {
      const file = `${slug(name) || "toolport-setup"}.json`;
      const { save } = await import("@tauri-apps/plugin-dialog");
      const path = await save({
        title: t("Save Toolport setup"),
        defaultPath: await setupSavePath(file),
        filters: setupFilters(t),
      });
      if (!path) return;
      await exportConfigToPath(path, name, description, shareFilter);
      toast.success(t("Saved setup to file"));
    } catch (e) {
      toastError(t("Couldn't save: {value1}", { value1: String(e) }));
    } finally {
      setSaving(false);
    }
  }

  // Parse + preview a candidate setup (paste or file) before importing anything.
  const startPreview = useCallback(
    async (
      json: string,
      source: "preview-paste" | "preview-file",
      existingRequest?: number,
      pasted = false,
    ) => {
      const request = existingRequest ?? ++previewRequest.current;
      setBusyAction(source);
      try {
        const items = await previewImport(json);
        if (request !== previewRequest.current) return;
        setPendingJson(json);
        setPreview(items);
        setImportSelection(new Set(items.filter((i) => i.isNew).map((i) => i.name)));
      } catch (e) {
        if (request !== previewRequest.current) return;
        // A failed paste is an error on its field: what to do first, then the
        // parser detail, under the field as in GTK.
        if (pasted)
          setPasteError(
            `${t(
              "Couldn't read the pasted setup. Check that you copied all of it, then try again.",
            )}\n\n${String(e)}`,
          );
        else toastError(t("Couldn't read that setup: {value1}", { value1: String(e) }));
      } finally {
        if (request === previewRequest.current) setBusyAction(null);
      }
    },
    [t],
  );

  async function loadFromFile() {
    const request = ++previewRequest.current;
    setBusyAction("preview-file");
    try {
      const { open: openFile } = await import("@tauri-apps/plugin-dialog");
      const path = await openFile({
        title: t("Open a Toolport setup"),
        defaultPath: await setupFolder(),
        multiple: false,
        directory: false,
        filters: setupFilters(t),
      });
      if (request !== previewRequest.current || !path || typeof path !== "string") return;
      const json = await readSetupFile(path);
      if (request !== previewRequest.current) return;
      await startPreview(json, "preview-file", request);
    } catch (e) {
      if (request !== previewRequest.current) return;
      toastError(t("Couldn't open that file: {value1}", { value1: String(e) }));
    } finally {
      if (request === previewRequest.current) setBusyAction(null);
    }
  }

  async function confirmImport() {
    const request = previewRequest.current;
    const json = pendingJson;
    const chosen = Array.from(importSelection);
    setBusyAction("import");
    try {
      const imported = await importConfig(json, chosen);
      // The write already happened. Always update the parent registry.
      onImported(imported);
      if (request !== previewRequest.current) {
        toast.success(t("Imported the setup you already confirmed"));
        return;
      }
      toast.success(t("Imported shared setup"), {
        description: t(
          "Imported servers stay off. Turn each one on in Servers when you're ready.",
        ),
      });
      onOpenChange(false);
    } catch (e) {
      if (request !== previewRequest.current) return;
      toastError(t("Couldn't import: {value1}", { value1: String(e) }));
    } finally {
      if (request === previewRequest.current) setBusyAction(null);
    }
  }

  // Open the import review when a toolport://import?s=<id> deep link arrives (the
  // share page's "Open in Toolport" button), including one captured before mount.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    async function openShared(id: string) {
      // Immediately invalidate and hide any older command-bearing review. The
      // incoming setup is a new review session even while its payload is fetching.
      onOpenChange(true);
      const request = ++previewRequest.current;
      setBusyAction("preview-paste");
      try {
        const json = await fetchSharedSetup(id);
        if (cancelled || request !== previewRequest.current) return;
        await startPreview(json, "preview-paste", request);
      } catch (e) {
        if (cancelled || request !== previewRequest.current) return;
        toastError(t("Couldn't open that shared stack: {value1}", { value1: String(e) }));
      } finally {
        if (!cancelled && request === previewRequest.current) setBusyAction(null);
      }
    }
    takePendingShared()
      .then((id) => {
        if (id && !cancelled) openShared(id);
      })
      .catch(() => {});
    listen<string>("import-shared", (event) => {
      if (event.payload) openShared(event.payload);
    })
      .then((un) => {
        if (cancelled) un();
        else unlisten = un;
      })
      .catch(() => {});
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [onOpenChange, startPreview, t]);

  const newCount = preview?.filter((i) => i.isNew).length ?? 0;
  const importCount =
    preview?.filter((i) => i.isNew && importSelection.has(i.name)).length ?? 0;
  const shareLabels = shareCheckLabels(servers);
  const hasSelection = selected.size > 0 || servers.length === 0;
  const noShareReason =
    selected.size === 0 && servers.length > 0
      ? t("Select at least one server to share.")
      : undefined;
  const shareReady =
    registryState === "ready" &&
    exportState === "ready" &&
    exported.length > 0 &&
    hasSelection;
  const shareActionDisabled = !shareReady || linking || saving;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogTrigger asChild>{trigger}</DialogTrigger>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{preview ? t("Review this setup") : t("Share setup")}</DialogTitle>
        </DialogHeader>

        {preview ? (
          <div className="flex flex-col gap-4 py-1">
            <p className="text-meta text-muted-foreground">
              {t(
                "These servers come from a shared file. Each runs the command shown when you enable it, so review them before importing. You'll add your own keys after. ",
              )}
            </p>
            {newCount === 0 && (
              <p id="share-import-reason" className="text-meta text-muted-foreground">
                {t(
                  "Every server in this setup is already present, so there is nothing to import.",
                )}
              </p>
            )}
            <div className="flex max-h-72 flex-col gap-2 overflow-y-auto">
              {preview.map((item, i) => {
                const on = item.isNew && importSelection.has(item.name);
                return (
                  <button
                    key={`${item.name}-${i}`}
                    type="button"
                    aria-pressed={on}
                    disabled={busyAction !== null || !item.isNew}
                    className={`w-full rounded-md text-left transition-colors outline-none hover:bg-muted/60 focus-visible:ring-3 focus-visible:ring-ring/50 disabled:hover:bg-transparent ${
                      on ? "ring-1 ring-success/60" : ""
                    }`}
                    onClick={() =>
                      setImportSelection((previous) => {
                        const next = new Set(previous);
                        if (on) next.delete(item.name);
                        else next.add(item.name);
                        return next;
                      })
                    }
                  >
                    <ImportRow item={item} selected={on} />
                  </button>
                );
              })}
            </div>
            <div className="flex items-center justify-between gap-2 border-t pt-3">
              <Button
                variant="ghost"
                onClick={() => setPreview(null)}
                disabled={busyAction !== null}
              >
                <Trans
                  message={"{value1}Back"}
                  values={{ value1: <ArrowLeft className="size-4" /> }}
                />
              </Button>
              <Button
                onClick={confirmImport}
                disabled={busyAction !== null || importCount === 0}
                aria-describedby={newCount === 0 ? "share-import-reason" : undefined}
              >
                {busyAction === "import" ? (
                  <>
                    <Loader2 className="size-4 animate-spin" />
                    {t("Importing… ")}
                  </>
                ) : (
                  <>
                    <Check className="size-4" />
                    {newCount === 0
                      ? t("Nothing new to import")
                      : importCount === 0
                        ? t("Select a server")
                        : t(
                            importCount === 1
                              ? "Import {count} server"
                              : "Import {count} servers",
                            { count: importCount },
                          )}
                  </>
                )}
              </Button>
            </div>
          </div>
        ) : (
          <div className="flex flex-col gap-5 py-1">
            <div className="flex flex-col gap-2">
              <Label className="text-sm">{t("Your setup")}</Label>
              <p className="text-meta text-muted-foreground">
                {t(
                  "Send this to a teammate to share your server set. Secrets are never included, each person adds their own keys after importing. ",
                )}
              </p>
              <div className="grid grid-cols-2 gap-2">
                <Input
                  value={name}
                  onChange={(e) => {
                    invalidateShareOutput();
                    setName(e.target.value);
                  }}
                  placeholder={t("Name (optional)")}
                  className="h-8 text-sm"
                />
                <Input
                  value={description}
                  onChange={(e) => {
                    invalidateShareOutput();
                    setDescription(e.target.value);
                  }}
                  placeholder={t("Description (optional)")}
                  className="h-8 text-sm"
                />
              </div>

              {servers.length > 0 && (
                <div className="flex flex-col gap-1.5">
                  <div className="flex items-center justify-between">
                    <span className="text-meta text-muted-foreground">
                      <Trans
                        message={"Servers to include ({value1}/{value2})"}
                        values={{ value1: selected.size, value2: servers.length }}
                      />
                    </span>
                    <div className="flex gap-2 text-meta">
                      <button
                        type="button"
                        className="text-muted-foreground hover:text-foreground"
                        onClick={() => {
                          invalidateShareOutput();
                          setSelected(new Set(servers.map((s) => s.id)));
                        }}
                      >
                        {t("All ")}
                      </button>
                      <button
                        type="button"
                        className="text-muted-foreground hover:text-foreground"
                        onClick={() => {
                          invalidateShareOutput();
                          setSelected(new Set());
                        }}
                      >
                        {t("None ")}
                      </button>
                    </div>
                  </div>
                  <div className="flex flex-wrap gap-1.5">
                    {servers.map((s, index) => {
                      const on = selected.has(s.id);
                      return (
                        <button
                          key={s.id}
                          type="button"
                          aria-pressed={on}
                          onClick={() => {
                            invalidateShareOutput();
                            setSelected((prev) => {
                              const next = new Set(prev);
                              if (on) next.delete(s.id);
                              else next.add(s.id);
                              return next;
                            });
                          }}
                          className={`rounded-full border px-2.5 py-1 text-xs transition-colors ${
                            on
                              ? "border-success/50 bg-success/10 text-success"
                              : "text-muted-foreground hover:bg-accent"
                          }`}
                        >
                          {shareLabels[index]}
                        </button>
                      );
                    })}
                  </div>
                </div>
              )}

              {registryState === "loading" && (
                <SkeletonRows label={t("Loading servers… ")} />
              )}
              {registryState === "error" && (
                <ErrorCallout
                  title={t("Share setup")}
                  cause={registryError}
                  onRetry={() => void loadServers()}
                />
              )}
              {registryState === "ready" && servers.length === 0 && (
                <EmptyState title={t("No servers yet")} />
              )}
              {registryState === "ready" && exportState === "loading" && (
                <SkeletonRows label={t("Updating setup… ")} />
              )}
              {registryState === "ready" && exportState === "error" && (
                <ErrorCallout
                  title={t("Share setup")}
                  cause={exportError}
                  onRetry={() => {
                    invalidateShareOutput();
                    setExportRetry((value) => value + 1);
                  }}
                />
              )}

              <Textarea
                readOnly
                aria-label={t("Exported setup")}
                value={exported}
                rows={5}
                className="resize-none font-mono text-meta"
              />
              {/* One right-aligned row as in GTK's footer: the exports, then the one
                  primary. Disabled buttons take no pointer events, so the row carries
                  the reason too, and it shows as a line by the buttons. */}
              <div className="flex flex-wrap justify-end gap-2" title={noShareReason}>
                <Button
                  size="sm"
                  variant="outline"
                  className="h-8"
                  onClick={copy}
                  disabled={shareActionDisabled}
                  title={noShareReason}
                >
                  {copied ? (
                    <>
                      <Check className="size-3.5" />
                      {t(" Copied ")}
                    </>
                  ) : (
                    <>
                      <Copy className="size-3.5" />
                      {t(" Copy ")}
                    </>
                  )}
                </Button>
                <Button
                  size="sm"
                  variant="outline"
                  className="h-8"
                  onClick={saveToFile}
                  disabled={shareActionDisabled}
                  title={noShareReason}
                >
                  {saving ? (
                    <>
                      <Loader2 className="size-3.5 animate-spin" />
                      {t(" Saving… ")}
                    </>
                  ) : (
                    <>
                      <FileDown className="size-3.5" />
                      {t(" Save to file ")}
                    </>
                  )}
                </Button>
                <Button
                  size="sm"
                  className="h-8"
                  onClick={createLink}
                  disabled={shareActionDisabled}
                  title={noShareReason}
                >
                  {linking ? (
                    <>
                      <Loader2 className="size-3.5 animate-spin" />
                      {t(" Creating link… ")}
                    </>
                  ) : (
                    <>
                      <Link2 className="size-3.5" />
                      {t(" Create share link ")}
                    </>
                  )}
                </Button>
              </div>
              {noShareReason && (
                <p className="text-right text-[13px] text-muted-foreground">
                  {noShareReason}
                </p>
              )}
              {shareLink && (
                <div className="flex items-center gap-2 rounded-md border bg-muted/40 px-2.5 py-1.5">
                  <Link2 className="size-3.5 shrink-0 text-success" />
                  <code className="min-w-0 flex-1 truncate text-meta">{shareLink}</code>
                  <button
                    type="button"
                    title={linkCopied ? t("Copied") : t("Copy link")}
                    onClick={copyLink}
                    className="shrink-0 rounded p-1 text-muted-foreground hover:bg-muted hover:text-foreground"
                  >
                    {linkCopied ? (
                      <Check className="size-3.5 text-success" />
                    ) : (
                      <Copy className="size-3.5" />
                    )}
                  </button>
                </div>
              )}
            </div>

            <div className="flex flex-col gap-2 border-t pt-4">
              <Label className="text-sm">{t("Import a setup")}</Label>
              <Textarea
                placeholder={t("Paste a shared setup here")}
                aria-label={t("Paste a shared setup")}
                value={paste}
                onChange={(e) => {
                  setPaste(e.target.value);
                  setPasteError(null);
                }}
                aria-invalid={pasteError ? true : undefined}
                aria-describedby={pasteError ? "share-paste-error" : undefined}
                rows={5}
                className="resize-none font-mono text-meta"
              />
              {pasteError && (
                <p
                  id="share-paste-error"
                  role="alert"
                  className="whitespace-pre-wrap break-words text-[13px] text-destructive"
                >
                  {pasteError}
                </p>
              )}
              <div className="flex flex-wrap justify-end gap-2">
                <Button
                  variant="outline"
                  onClick={loadFromFile}
                  disabled={busyAction !== null}
                >
                  {busyAction === "preview-file" ? (
                    <>
                      <Loader2 className="size-4 animate-spin" />
                      {t("Loading… ")}
                    </>
                  ) : (
                    <>
                      <FileUp className="size-4" />
                      {t("Load from file ")}
                    </>
                  )}
                </Button>
                <Button
                  onClick={() => startPreview(paste, "preview-paste", undefined, true)}
                  disabled={busyAction !== null || !paste.trim()}
                >
                  {busyAction === "preview-paste" ? (
                    <>
                      <Loader2 className="size-4 animate-spin" />
                      {t("Reviewing… ")}
                    </>
                  ) : (
                    <>
                      <Upload className="size-4" />
                      {t("Review and import ")}
                    </>
                  )}
                </Button>
              </div>
            </div>
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}

/** Setup files first, every file one choice away, as in GTK. */
function setupFilters(t: (message: string) => string) {
  return [
    { name: t("Toolport setup"), extensions: ["json"] },
    { name: t("All files"), extensions: ["*"] },
  ];
}

/** Setup choosers start where exports land: Downloads, else Home, as in GTK. */
async function setupFolder(): Promise<string | undefined> {
  const { downloadDir, homeDir } = await import("@tauri-apps/api/path");
  try {
    return await downloadDir();
  } catch {
    return homeDir().catch(() => undefined);
  }
}

async function setupSavePath(file: string): Promise<string> {
  const folder = await setupFolder();
  if (!folder) return file;
  const { join } = await import("@tauri-apps/api/path");
  return join(folder, file);
}

/** Chip labels: the name, plus the id when another server has the same name. */
function shareCheckLabels(servers: { id: string; name: string }[]): string[] {
  return servers.map((s) =>
    servers.filter((other) => other.name === s.name).length > 1
      ? `${s.name} (${s.id})`
      : s.name,
  );
}

/** A filesystem-safe slug for the default filename. */
function slug(s: string): string {
  return s
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
}
