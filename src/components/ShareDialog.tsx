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
import { open as openFile, save } from "@tauri-apps/plugin-dialog";
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
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [exported, setExported] = useState("");
  const [debouncedName, setDebouncedName] = useState(name);
  const [debouncedDescription, setDebouncedDescription] = useState(description);
  const [paste, setPaste] = useState("");
  const [copied, setCopied] = useState(false);
  const [busyAction, setBusyAction] = useState<
    "preview-paste" | "preview-file" | "import" | null
  >(null);
  // When set, the dialog shows the review-and-confirm view for `pendingJson`.
  const [preview, setPreview] = useState<ImportItem[] | null>(null);
  const [pendingJson, setPendingJson] = useState("");
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
        toast.success("Share link created and copied");
      } catch {
        if (request !== linkRequest.current) return;
        toast.success("Share link created");
        toastError("Couldn't copy automatically. Select the link and copy it.");
      }
    } catch (e) {
      if (request !== linkRequest.current) return;
      toastError(`Couldn't create a link: ${e}`);
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
      toastError("Couldn't copy automatically. Select the text and copy it.");
    }
  }

  async function copyLink() {
    try {
      await navigator.clipboard.writeText(shareLink);
      setLinkCopied(true);
      setTimeout(() => setLinkCopied(false), 1500);
    } catch {
      toastError("Couldn't copy automatically. Select the link and copy it.");
    }
  }

  async function saveToFile() {
    setSaving(true);
    try {
      const path = await save({
        title: "Save Toolport setup",
        defaultPath: `${slug(name) || "toolport-setup"}.json`,
        filters: [{ name: "Toolport setup", extensions: ["json"] }],
      });
      if (!path) return;
      await exportConfigToPath(path, name, description, shareFilter);
      toast.success("Saved setup to file");
    } catch (e) {
      toastError(`Couldn't save: ${e}`);
    } finally {
      setSaving(false);
    }
  }

  // Parse + preview a candidate setup (paste or file) before importing anything.
  async function startPreview(
    json: string,
    source: "preview-paste" | "preview-file",
    existingRequest?: number,
  ) {
    const request = existingRequest ?? ++previewRequest.current;
    setBusyAction(source);
    try {
      const items = await previewImport(json);
      if (request !== previewRequest.current) return;
      setPendingJson(json);
      setPreview(items);
    } catch (e) {
      if (request !== previewRequest.current) return;
      toastError(`Couldn't read that setup: ${e}`);
    } finally {
      if (request === previewRequest.current) setBusyAction(null);
    }
  }

  async function loadFromFile() {
    const request = ++previewRequest.current;
    setBusyAction("preview-file");
    try {
      const path = await openFile({
        title: "Open a Toolport setup",
        multiple: false,
        directory: false,
        filters: [{ name: "Toolport setup", extensions: ["json"] }],
      });
      if (request !== previewRequest.current || !path || typeof path !== "string") return;
      const json = await readSetupFile(path);
      if (request !== previewRequest.current) return;
      await startPreview(json, "preview-file", request);
    } catch (e) {
      if (request !== previewRequest.current) return;
      toastError(`Couldn't open that file: ${e}`);
    } finally {
      if (request === previewRequest.current) setBusyAction(null);
    }
  }

  async function confirmImport() {
    const request = previewRequest.current;
    const json = pendingJson;
    setBusyAction("import");
    try {
      const imported = await importConfig(json);
      // The write already happened. Always update the parent registry.
      onImported(imported);
      if (request !== previewRequest.current) {
        toast.success("Imported the setup you already confirmed");
        return;
      }
      toast.success("Imported shared setup", {
        description: "Add any API keys each server needs, then enable them.",
      });
      onOpenChange(false);
    } catch (e) {
      if (request !== previewRequest.current) return;
      toastError(`Couldn't import: ${e}`);
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
        toastError(`Couldn't open that shared stack: ${e}`);
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
  }, [onOpenChange]);

  const newCount = preview?.filter((i) => i.isNew).length ?? 0;
  const hasSelection = selected.size > 0 || servers.length === 0;
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
          <DialogTitle>{preview ? "Review this setup" : "Share setup"}</DialogTitle>
        </DialogHeader>

        {preview ? (
          <div className="flex flex-col gap-4 py-1">
            <p className="text-xs text-muted-foreground">
              These servers come from a shared file. Each runs the command shown when you
              enable it, so review them before importing. You'll add your own keys after.
            </p>
            <div className="flex max-h-72 flex-col gap-2 overflow-y-auto">
              {preview.map((item, i) => (
                <ImportRow key={`${item.name}-${i}`} item={item} />
              ))}
            </div>
            <div className="flex items-center justify-between gap-2 border-t pt-3">
              <Button
                variant="ghost"
                onClick={() => setPreview(null)}
                disabled={busyAction !== null}
              >
                <ArrowLeft className="size-4" />
                Back
              </Button>
              <Button
                onClick={confirmImport}
                disabled={busyAction !== null || newCount === 0}
              >
                {busyAction === "import" ? (
                  <>
                    <Loader2 className="size-4 animate-spin" />
                    Importing…
                  </>
                ) : (
                  <>
                    <Check className="size-4" />
                    {newCount === 0
                      ? "Nothing new to import"
                      : `Import ${newCount} server${newCount === 1 ? "" : "s"}`}
                  </>
                )}
              </Button>
            </div>
          </div>
        ) : (
          <div className="flex flex-col gap-5 py-1">
            <div className="flex flex-col gap-2">
              <Label className="text-sm">Your setup</Label>
              <p className="text-xs text-muted-foreground">
                Send this to a teammate to share your server set. Secrets are never
                included, each person adds their own keys after importing.
              </p>
              <div className="grid grid-cols-2 gap-2">
                <Input
                  value={name}
                  onChange={(e) => {
                    invalidateShareOutput();
                    setName(e.target.value);
                  }}
                  placeholder="Name (optional)"
                  className="h-8 text-sm"
                />
                <Input
                  value={description}
                  onChange={(e) => {
                    invalidateShareOutput();
                    setDescription(e.target.value);
                  }}
                  placeholder="Description (optional)"
                  className="h-8 text-sm"
                />
              </div>

              {servers.length > 0 && (
                <div className="flex flex-col gap-1.5">
                  <div className="flex items-center justify-between">
                    <span className="text-xs text-muted-foreground">
                      Servers to include ({selected.size}/{servers.length})
                    </span>
                    <div className="flex gap-2 text-[11px]">
                      <button
                        type="button"
                        className="text-muted-foreground hover:text-foreground"
                        onClick={() => {
                          invalidateShareOutput();
                          setSelected(new Set(servers.map((s) => s.id)));
                        }}
                      >
                        All
                      </button>
                      <button
                        type="button"
                        className="text-muted-foreground hover:text-foreground"
                        onClick={() => {
                          invalidateShareOutput();
                          setSelected(new Set());
                        }}
                      >
                        None
                      </button>
                    </div>
                  </div>
                  <div className="flex flex-wrap gap-1.5">
                    {servers.map((s) => {
                      const on = selected.has(s.id);
                      return (
                        <button
                          key={s.id}
                          type="button"
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
                          {s.name}
                        </button>
                      );
                    })}
                  </div>
                </div>
              )}

              {registryState === "loading" && (
                <p role="status" className="text-xs text-muted-foreground">
                  Loading servers…
                </p>
              )}
              {registryState === "error" && (
                <div
                  role="alert"
                  className="flex items-center justify-between gap-2 rounded-md border border-destructive/30 bg-destructive/5 px-2.5 py-2 text-xs text-destructive"
                >
                  <span>Couldn&apos;t load your servers: {registryError}</span>
                  <Button size="sm" variant="outline" onClick={() => void loadServers()}>
                    Retry
                  </Button>
                </div>
              )}
              {registryState === "ready" && exportState === "loading" && (
                <p role="status" className="text-xs text-muted-foreground">
                  Updating setup…
                </p>
              )}
              {registryState === "ready" && exportState === "error" && (
                <div
                  role="alert"
                  className="flex items-center justify-between gap-2 rounded-md border border-destructive/30 bg-destructive/5 px-2.5 py-2 text-xs text-destructive"
                >
                  <span>Couldn&apos;t build this setup: {exportError}</span>
                  <Button
                    size="sm"
                    variant="outline"
                    onClick={() => {
                      invalidateShareOutput();
                      setExportRetry((value) => value + 1);
                    }}
                  >
                    Retry
                  </Button>
                </div>
              )}

              <Textarea
                readOnly
                aria-label="Exported setup"
                value={exported}
                rows={5}
                className="resize-none font-mono text-xs"
              />
              <div className="flex flex-wrap gap-2">
                <Button
                  size="sm"
                  className="h-8"
                  onClick={createLink}
                  disabled={shareActionDisabled}
                >
                  {linking ? (
                    <>
                      <Loader2 className="size-3.5 animate-spin" /> Creating link…
                    </>
                  ) : (
                    <>
                      <Link2 className="size-3.5" /> Create share link
                    </>
                  )}
                </Button>
                <Button
                  size="sm"
                  variant="outline"
                  className="h-8"
                  onClick={copy}
                  disabled={shareActionDisabled}
                >
                  {copied ? (
                    <>
                      <Check className="size-3.5" /> Copied
                    </>
                  ) : (
                    <>
                      <Copy className="size-3.5" /> Copy
                    </>
                  )}
                </Button>
                <Button
                  size="sm"
                  variant="outline"
                  className="h-8"
                  onClick={saveToFile}
                  disabled={shareActionDisabled}
                >
                  {saving ? (
                    <>
                      <Loader2 className="size-3.5 animate-spin" /> Saving…
                    </>
                  ) : (
                    <>
                      <FileDown className="size-3.5" /> Save to file
                    </>
                  )}
                </Button>
              </div>
              {shareLink && (
                <div className="flex items-center gap-2 rounded-md border bg-muted/40 px-2.5 py-1.5">
                  <Link2 className="size-3.5 shrink-0 text-success" />
                  <code className="min-w-0 flex-1 truncate text-xs">{shareLink}</code>
                  <button
                    type="button"
                    title={linkCopied ? "Copied" : "Copy link"}
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
              <Label className="text-sm">Import a setup</Label>
              <Textarea
                placeholder="Paste a shared setup here"
                aria-label="Paste a shared setup"
                value={paste}
                onChange={(e) => setPaste(e.target.value)}
                rows={5}
                className="resize-none font-mono text-xs"
              />
              <div className="flex flex-wrap gap-2">
                <Button
                  onClick={() => startPreview(paste, "preview-paste")}
                  disabled={busyAction !== null || !paste.trim()}
                >
                  {busyAction === "preview-paste" ? (
                    <>
                      <Loader2 className="size-4 animate-spin" />
                      Reviewing…
                    </>
                  ) : (
                    <>
                      <Upload className="size-4" />
                      Review and import
                    </>
                  )}
                </Button>
                <Button
                  variant="outline"
                  onClick={loadFromFile}
                  disabled={busyAction !== null}
                >
                  {busyAction === "preview-file" ? (
                    <>
                      <Loader2 className="size-4 animate-spin" />
                      Loading…
                    </>
                  ) : (
                    <>
                      <FileUp className="size-4" />
                      Load from file
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

/** A filesystem-safe slug for the default filename. */
function slug(s: string): string {
  return s
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
}
