import { useI18n, Trans } from "@/lib/i18n";
import { useRef, useState, type ReactNode } from "react";
import {
  AlertTriangle,
  CheckCircle2,
  ChevronDown,
  ChevronRight,
  ClipboardPaste,
  Loader2,
  Plus,
  X,
} from "lucide-react";
import { toast } from "sonner";
import { toastError } from "@/lib/toast";
import {
  addServer,
  parseServerSnippet,
  setSecret,
  setLaunchSecret,
  testServer,
  updateServer,
} from "@/lib/api";
import { formatArgs, parseArgs } from "@/lib/args";
import { isDownloadLauncher } from "@/lib/launcher";
import type { LaunchConfig, Registry, ServerEntry, Transport } from "@/lib/types";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

interface Props {
  /** The control that opens the dialog. Omit when opening it with `autoOpen` (a
   * keyboard shortcut or a catalog flow), where there is no button to render. */
  trigger?: ReactNode;
  onSaved: (registry: Registry) => void;
  /** When set, the dialog edits this server (vs. adding a new one). */
  editId?: string;
  /** Pre-fill values (for edit, or duplicating an existing server). */
  initial?: ServerEntry;
  /** Names of servers that already exist, to warn on a duplicate name. */
  existingNames?: string[];
  /** Open the dialog automatically on mount (used by catalog configure-add). */
  autoOpen?: boolean;
  /** Called when the dialog closes without saving (dismiss/cancel). */
  onClose?: () => void;
  /** Placeholder + helper text for the URL field when the server is self-hosted
   * (e.g. n8n, Langfuse). Shown as input placeholder and as an explanatory note
   * below the field. */
  urlHint?: string;
}

type TestState =
  | { status: "idle"; message: "" }
  | { status: "testing"; message: ""; requestId: number; stale: boolean }
  | {
      status: "ok" | "fail";
      message: string;
      values?: Record<string, string | number>;
      authFallback?: boolean;
      localized?: boolean;
    };

type TestResult = Extract<TestState, { status: "ok" | "fail" }>;

const IDLE_TEST: TestState = { status: "idle", message: "" };

export function ServerDialog({
  trigger,
  onSaved,
  editId,
  initial,
  existingNames,
  autoOpen,
  onClose,
  urlHint,
}: Props) {
  const { t } = useI18n();
  const [open, setOpen] = useState(autoOpen ?? false);
  const [form, setForm] = useState({
    name: initial?.name ?? "",
    transport: (initial?.transport ?? "stdio") as Transport,
    command: initial?.command ?? "",
    args: formatArgs(initial?.args ?? []),
    url: initial?.url ?? "",
    cwd: initial?.cwd ?? "",
    initializeTimeoutSeconds:
      initial?.initializeTimeoutMs == null
        ? ""
        : String(initial.initializeTimeoutMs / 1000),
  });
  // Env vars (API keys etc.). Values are vaulted in the OS keychain, never stored
  // in the registry, so existing secrets show as declared keys with empty values.
  const [envRows, setEnvRows] = useState<{ key: string; value: string }[]>(
    initial?.env.map((e) => ({ key: e.key, value: "" })) ?? [],
  );
  const [launch, setLaunch] = useState<LaunchConfig | null>(initial?.launch ?? null);
  const [launchValues, setLaunchValues] = useState<Record<string, string>>(
    Object.fromEntries(
      initial?.launch?.inputs.map((input) => [
        input.key,
        input.secret ? "" : (input.value ?? ""),
      ]) ?? [],
    ),
  );
  const [bindingCleared, setBindingCleared] = useState(false);
  const [busy, setBusy] = useState(false);
  const [test, setTest] = useState<TestState>(IDLE_TEST);
  const testRequestId = useRef(0);
  const [partialEdit, setPartialEdit] = useState<{ id: string; name: string } | null>(
    null,
  );
  const currentEditId = editId ?? partialEdit?.id;
  const isStdio = form.transport === "stdio";
  const initialUsesLocalCommand = initial?.transport === "stdio" || !!initial?.command;
  const editing = currentEditId !== undefined;

  // Paste-from-config state.
  const [showPaste, setShowPaste] = useState(false);
  const [pasteText, setPasteText] = useState("");
  const [parsing, setParsing] = useState(false);

  // A completed result clears immediately when the connection details change. An in-flight
  // test stays visibly busy until it settles, but its result is then discarded as stale.
  function clearTest() {
    setTest((current) => {
      if (current.status === "idle") return current;
      if (current.status === "testing") {
        return current.stale ? current : { ...current, stale: true };
      }
      return IDLE_TEST;
    });
  }

  // The dialog instance is mounted persistently (e.g. the header "Add server"
  // button), so reset the form each time it opens - otherwise it keeps the last
  // entry's values instead of starting blank (or re-deriving from `initial`).
  function onOpenChange(next: boolean) {
    if (!next) setTest(IDLE_TEST);
    if (!next && onClose) {
      onClose();
      return;
    }
    if (next) {
      setPartialEdit(null);
      setForm({
        name: initial?.name ?? "",
        transport: (initial?.transport ?? "stdio") as Transport,
        command: initial?.command ?? "",
        args: formatArgs(initial?.args ?? []),
        url: initial?.url ?? "",
        cwd: initial?.cwd ?? "",
        initializeTimeoutSeconds:
          initial?.initializeTimeoutMs == null
            ? ""
            : String(initial.initializeTimeoutMs / 1000),
      });
      setEnvRows(initial?.env.map((e) => ({ key: e.key, value: "" })) ?? []);
      setLaunch(initial?.launch ?? null);
      setLaunchValues(
        Object.fromEntries(
          initial?.launch?.inputs.map((input) => [
            input.key,
            input.secret ? "" : (input.value ?? ""),
          ]) ?? [],
        ),
      );
      setBindingCleared(false);
      setTest(IDLE_TEST);
      setShowPaste(false);
      setPasteText("");
    }
    setOpen(next);
  }

  function set<K extends keyof typeof form>(key: K, value: (typeof form)[K]) {
    if ((key === "args" || key === "command") && value !== form[key] && launch) {
      setLaunch(null);
      setBindingCleared(true);
    }
    setForm((f) => ({ ...f, [key]: value }));
    clearTest();
  }

  function setEnvRow(i: number, field: "key" | "value", value: string) {
    setEnvRows((rows) => rows.map((r, j) => (j === i ? { ...r, [field]: value } : r)));
    clearTest();
  }
  function addEnvRow() {
    setEnvRows((rows) => [...rows, { key: "", value: "" }]);
    clearTest();
  }
  function removeEnvRow(i: number) {
    setEnvRows((rows) => rows.filter((_, j) => j !== i));
    clearTest();
  }

  async function handleParse() {
    if (!pasteText.trim()) return;
    setParsing(true);
    try {
      const servers = await parseServerSnippet(pasteText);
      if (servers.length === 0) {
        toast.error(t("No servers found in the pasted config"));
        return;
      }
      const s = servers[0];
      setForm({
        name: s.name || "",
        transport: (s.transport === "unknown" ? "stdio" : s.transport) as Transport,
        command: s.command ?? "",
        args: formatArgs(s.args),
        url: s.url ?? "",
        cwd: "",
        initializeTimeoutSeconds: "",
      });
      setEnvRows(
        s.env.map((e) => ({
          key: e.key,
          value: e.value ?? "",
        })),
      );
      setLaunch(null);
      setLaunchValues({});
      setBindingCleared(false);
      clearTest();
      if (servers.length > 1) {
        toast.info(
          t('Found {value1} servers, filled "{value2}". Add the rest separately.', {
            value1: servers.length,
            value2: s.name,
          }),
        );
      } else {
        toast.success(t('Parsed "{value1}" from config', { value1: s.name }));
      }
      setShowPaste(false);
      setPasteText("");
    } catch (e) {
      toastError(t("Couldn't parse: {value1}", { value1: String(e) }));
    } finally {
      setParsing(false);
    }
  }

  // Build the entry from the form. For a real save the secret values are vaulted
  // separately (never written to the registry); for a connection test they ride
  // along on `env` so the probe can actually launch/authenticate the server.
  function buildEntry(withSecretValues: boolean): ServerEntry {
    const declared = envRows.filter((r) => r.key.trim());
    return {
      id: currentEditId ?? "",
      name: form.name.trim(),
      transport: form.transport,
      command: isStdio ? form.command.trim() || null : null,
      args: isStdio ? parseArgs(form.args) : [],
      launch:
        isStdio && launch
          ? {
              ...launch,
              inputs: launch.inputs.map((input) => ({
                ...input,
                value: input.secret
                  ? withSecretValues
                    ? launchValues[input.key] || null
                    : null
                  : launchValues[input.key] || null,
              })),
            }
          : null,
      env: declared.map((r) => ({
        key: r.key.trim(),
        value: withSecretValues && r.value ? r.value : null,
        secret: true,
      })),
      url: isStdio ? null : form.url.trim() || null,
      source: bindingCleared ? "manual" : (initial?.source ?? "manual"),
      cwd: isStdio ? form.cwd.trim() || null : null,
      requestTimeoutMs:
        isStdio || initialUsesLocalCommand ? null : initial?.requestTimeoutMs,
      initializeTimeoutMs: form.initializeTimeoutSeconds.trim()
        ? Math.round(Number(form.initializeTimeoutSeconds) * 1000)
        : null,
    };
  }

  // Per-transport validation. `errors` block Save; the duplicate-name case is a
  // soft warning, since duplicating a server per account is a real workflow.
  const nameTrim = form.name.trim();
  const urlTrim = form.url.trim();
  const cmdTrim = form.command.trim();
  const errors: string[] = [];
  if (!nameTrim) errors.push(t("Give the server a name."));
  if (isStdio) {
    if (!cmdTrim) errors.push(t("Enter the command to run (e.g. npx)."));
    if (bindingCleared && parseArgs(form.args).includes("<launch-input>")) {
      errors.push(t("Replace <launch-input> with a literal argument before saving."));
    }
  } else if (!urlTrim) {
    errors.push(t("Enter the server URL."));
  } else if (!/^https?:\/\//i.test(urlTrim)) {
    errors.push(t("The URL must start with http:// or https://."));
  }
  if (form.initializeTimeoutSeconds.trim()) {
    const seconds = Number(form.initializeTimeoutSeconds);
    const milliseconds = Math.round(seconds * 1000);
    if (!Number.isFinite(seconds) || milliseconds < 1 || milliseconds > 86_400_000) {
      errors.push(
        t("Startup timeout must be greater than 0 and at most 86,400 seconds."),
      );
    }
  }
  const ownName = editing
    ? (initial?.name ?? partialEdit?.name)?.trim().toLowerCase()
    : undefined;
  const duplicateName =
    !!nameTrim &&
    nameTrim.toLowerCase() !== ownName &&
    (existingNames ?? []).some((n) => n.trim().toLowerCase() === nameTrim.toLowerCase());
  const canSave = errors.length === 0 && !busy && test.status !== "testing";

  function finishTest(requestId: number, result: TestResult) {
    setTest((current) => {
      if (current.status !== "testing" || current.requestId !== requestId) return current;
      return current.stale ? IDLE_TEST : result;
    });
  }

  async function handleTest() {
    const requestId = ++testRequestId.current;
    setTest({ status: "testing", message: "", requestId, stale: false });
    try {
      const r = await testServer(buildEntry(true));
      if (r.ok) {
        finishTest(requestId, {
          status: "ok",
          message:
            r.toolCount === 1
              ? "Connected. Found {value1} tool."
              : "Connected. Found {value1} tools.",
          values: { value1: r.toolCount },
          localized: true,
        });
      } else {
        finishTest(requestId, {
          status: "fail",
          message: r.authRequired
            ? "Reachable, but needs credentials: {value1}"
            : (r.error ?? "Couldn't connect."),
          values: r.authRequired ? { value1: r.error ?? "" } : undefined,
          authFallback: r.authRequired && !r.error,
          localized: r.authRequired || !r.error,
        });
      }
    } catch (e) {
      finishTest(requestId, { status: "fail", message: String(e) });
    }
  }

  async function handleSave() {
    if (errors.length > 0) return;
    const entry = buildEntry(false);
    const declared = envRows.filter((r) => r.key.trim());
    const wasEditing = editing;
    setBusy(true);
    try {
      let result = wasEditing ? await updateServer(entry) : await addServer(entry);
      // Vault any values the user entered now. setSecret keys by server id. For a
      // new server, add_server appends it, so it's the last entry - resolving by
      // name would pick the wrong one if two servers share a name.
      const id = wasEditing
        ? currentEditId
        : result.servers[result.servers.length - 1]?.id;
      const failedKeys: string[] = [];
      if (id) {
        for (const r of declared) {
          if (!r.value) continue;
          const key = r.key.trim();
          try {
            result = await setSecret(id, key, r.value);
          } catch {
            failedKeys.push(key);
          }
        }
        for (const input of launch?.inputs ?? []) {
          if (!input.secret || !launchValues[input.key]) continue;
          try {
            result = await setLaunchSecret(id, input.key, launchValues[input.key]);
          } catch {
            failedKeys.push(input.label);
          }
        }
      }
      onSaved(result);
      if (failedKeys.length > 0) {
        if (!wasEditing && id) setPartialEdit({ id, name: entry.name });
        toast.warning(
          t(
            wasEditing
              ? "Saved {value2}, but couldn't save: {value3}"
              : "Added {value2}, but couldn't save: {value3}",
            { value2: entry.name, value3: failedKeys.join(", ") },
          ),
        );
        return;
      }
      toast.success(
        wasEditing
          ? t("Saved {value1}", { value1: entry.name })
          : t("Added {value1}", { value1: entry.name }),
      );
      onOpenChange(false);
    } catch (e) {
      toastError(
        t("Couldn't save {value1}: {value2}", { value1: entry.name, value2: String(e) }),
      );
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      {trigger && <DialogTrigger asChild>{trigger}</DialogTrigger>}
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{editing ? t("Edit server") : t("Add MCP server")}</DialogTitle>
        </DialogHeader>

        {/* Paste-from-config: collapsible textarea that auto-detects format.
            Only shown when adding a new server, not when editing. */}
        {!editing && (
          <div className="rounded-md border">
            <button
              type="button"
              className="flex w-full items-center gap-2 px-3 py-2 text-sm font-medium text-muted-foreground transition hover:text-foreground"
              onClick={() => setShowPaste((v) => !v)}
            >
              <Trans
                message={"{value1}{value2}Paste from client config"}
                values={{
                  value1: showPaste ? (
                    <ChevronDown className="size-4" />
                  ) : (
                    <ChevronRight className="size-4" />
                  ),
                  value2: <ClipboardPaste className="size-4" />,
                }}
              />
            </button>
            {showPaste && (
              <div className="flex flex-col gap-2 border-t px-3 pb-3 pt-2">
                <textarea
                  className="min-h-[100px] w-full resize-y rounded-md bg-muted/50 p-2 font-mono text-xs"
                  placeholder={t(
                    'Paste a config snippet from any client:\n\n• Claude Code: claude mcp add-json ...\n• Cursor/Devin/Antigravity: {"mcpServers": ...}\n• VS Code: {"servers": ...}\n• Codex: [mcp_servers.name]\n• Zed: {"context_servers": ...}',
                  )}
                  value={pasteText}
                  onChange={(e) => setPasteText(e.target.value)}
                />
                <Button
                  variant="secondary"
                  size="sm"
                  className="self-end"
                  disabled={!pasteText.trim() || parsing}
                  onClick={handleParse}
                >
                  {parsing ? t("Parsing…") : t("Parse & fill")}
                </Button>
              </div>
            )}
          </div>
        )}

        <div className="flex flex-col gap-4 py-2">
          <div className="flex flex-col gap-2">
            <Label htmlFor="srv-name">{t("Name")}</Label>
            <Input
              id="srv-name"
              autoFocus
              placeholder={t("revenuecat (work)")}
              value={form.name}
              onChange={(e) => set("name", e.target.value)}
            />
          </div>

          <div className="flex flex-col gap-2">
            <Label>{t("Transport")}</Label>
            <Select
              value={form.transport}
              onValueChange={(v) => set("transport", v as Transport)}
            >
              <SelectTrigger aria-label={t("Transport")}>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="stdio">{t("stdio (local command)")}</SelectItem>
                <SelectItem value="http">{t("http (remote)")}</SelectItem>
                <SelectItem value="sse">{t("sse (remote)")}</SelectItem>
              </SelectContent>
            </Select>

            <p className="text-xs text-muted-foreground">
              {t("Use stdio for a local command, or http/sse for a remote URL. ")}
            </p>
          </div>

          {isStdio ? (
            <>
              <div className="flex flex-col gap-2">
                <Label htmlFor="srv-cmd">{t("Command")}</Label>
                <Input
                  id="srv-cmd"
                  placeholder="npx"
                  value={form.command}
                  onChange={(e) => set("command", e.target.value)}
                />
              </div>
              <div className="flex flex-col gap-2">
                <Label htmlFor="srv-args">{t("Arguments")}</Label>
                <Input
                  id="srv-args"
                  placeholder={t(
                    '-y @scope/package  (quote paths with spaces, e.g. "/Applications/My App.app/tool")',
                  )}
                  value={form.args}
                  onChange={(e) => set("args", e.target.value)}
                />
                {bindingCleared && (
                  <p className="text-xs text-warning">
                    {t(
                      "Catalog launch setup was removed when you edited the command or arguments. Add any required values to the new command or restore the catalog preset. ",
                    )}
                  </p>
                )}
              </div>
              {!!launch?.inputs.length && (
                <div className="flex flex-col gap-2 rounded-md border p-3">
                  <Label>{t("Launch setup")}</Label>
                  {launch.inputs.map((input) => (
                    <div className="flex flex-col gap-1" key={input.key}>
                      <Label htmlFor={`launch-${input.key}`}>
                        {input.label}
                        {input.required ? " *" : ""}
                      </Label>
                      <Input
                        id={`launch-${input.key}`}
                        type={input.secret ? "password" : "text"}
                        value={launchValues[input.key] ?? ""}
                        placeholder={
                          input.secret && editing
                            ? t("Leave blank to keep vaulted value")
                            : input.label
                        }
                        onChange={(event) => {
                          setLaunchValues((values) => ({
                            ...values,
                            [input.key]: event.target.value,
                          }));
                          clearTest();
                        }}
                      />
                    </div>
                  ))}
                  <p className="text-xs text-muted-foreground">
                    {t(
                      "Secret inputs are saved in Toolport's vault. This server stays disabled until required inputs are configured. ",
                    )}
                  </p>
                </div>
              )}
              <div className="flex flex-col gap-2">
                <Label htmlFor="srv-cwd">{t("Working directory (optional)")}</Label>
                <Input
                  id="srv-cwd"
                  placeholder="~/projects/my-app"
                  value={form.cwd}
                  onChange={(e) => set("cwd", e.target.value)}
                />
                <p className="text-xs text-muted-foreground">
                  <Trans
                    message={
                      "Where this server runs. Leave blank to inherit Toolport's directory. Useful for tools that work on a project (a filesystem or code-search server). {value1} and{value2}{value3} are expanded."
                    }
                    values={{
                      value1: <code className="font-mono">~</code>,
                      value2: " ",
                      value3: <code className="font-mono">{"${VAR}"}</code>,
                    }}
                  />
                </p>
              </div>
            </>
          ) : (
            <div className="flex flex-col gap-2">
              <Label htmlFor="srv-url">{t("URL")}</Label>
              <Input
                id="srv-url"
                placeholder={urlHint ?? "https://mcp.example.com/mcp"}
                value={form.url}
                onChange={(e) => set("url", e.target.value)}
              />
              {urlHint && (
                <p className="text-xs text-muted-foreground">
                  <Trans
                    message={
                      "Enter the URL of your self-hosted instance. For example:{value1}{value2}"
                    }
                    values={{
                      value1: " ",
                      value2: <code className="font-mono">{urlHint}</code>,
                    }}
                  />
                </p>
              )}
            </div>
          )}

          <div className="flex flex-col gap-2">
            <Label htmlFor="srv-initialize-timeout">
              {t("Startup timeout (optional)")}
            </Label>
            <Input
              id="srv-initialize-timeout"
              type="number"
              min="0.001"
              max="86400"
              step="0.001"
              placeholder={
                isStdio
                  ? isDownloadLauncher(form.command, parseArgs(form.args))
                    ? "120"
                    : "10"
                  : String(
                      (initialUsesLocalCommand
                        ? 30_000
                        : (initial?.requestTimeoutMs ?? 30_000)) / 1000,
                    )
              }
              value={form.initializeTimeoutSeconds}
              onChange={(e) => set("initializeTimeoutSeconds", e.target.value)}
            />
            <p className="text-xs text-muted-foreground">
              {t(
                "Seconds to wait for this server to initialize. Raise this for a slow first start, such as a model download or index build. ",
              )}
            </p>
          </div>

          <div className="flex flex-col gap-2">
            <Label>{t("Environment variables")}</Label>
            <p className="-mt-1 text-xs text-muted-foreground">
              <Trans
                message={
                  "API keys and other secrets the server needs (e.g.{value1}{value2}). Values are stored in your OS keychain, never in the config."
                }
                values={{
                  value1: " ",
                  value2: <code className="font-mono">RESEND_API_KEY</code>,
                }}
              />
            </p>
            {envRows.map((row, i) => (
              <div key={i} className="flex items-center gap-2">
                <Input
                  placeholder="ENV_NAME"
                  className="font-mono"
                  value={row.key}
                  onChange={(e) => setEnvRow(i, "key", e.target.value)}
                />
                <Input
                  type="password"
                  placeholder={
                    initial?.env.some((e) => e.key === row.key)
                      ? t("•••• (saved)")
                      : t("value")
                  }
                  value={row.value}
                  onChange={(e) => setEnvRow(i, "value", e.target.value)}
                />
                <Button
                  size="icon"
                  variant="ghost"
                  className="size-8 shrink-0 text-muted-foreground hover:text-destructive"
                  aria-label={t("Remove variable")}
                  onClick={() => removeEnvRow(i)}
                >
                  <X className="size-4" />
                </Button>
              </div>
            ))}
            <Button
              variant="outline"
              size="sm"
              className="self-start"
              onClick={addEnvRow}
            >
              <Trans
                message={"{value1}Add variable"}
                values={{ value1: <Plus className="size-4" /> }}
              />
            </Button>
          </div>

          {(errors.length > 0 ||
            duplicateName ||
            test.status === "ok" ||
            test.status === "fail") && (
            <div className="flex flex-col gap-1.5 text-xs">
              {errors.map((msg) => (
                <p key={msg} className="text-destructive">
                  {msg}
                </p>
              ))}
              {duplicateName && (
                <p className="flex items-start gap-1.5 text-warning">
                  <AlertTriangle className="mt-0.5 size-3.5 shrink-0" />
                  <span>
                    <Trans
                      message={
                        "Another server is already named \"{value1}\". That's fine for multiple accounts; it'll be saved as a separate entry."
                      }
                      values={{ value1: nameTrim }}
                    />
                  </span>
                </p>
              )}
              {test.status === "ok" && (
                <p className="flex items-start gap-1.5 text-success">
                  <CheckCircle2 className="mt-0.5 size-3.5 shrink-0" />
                  <span>
                    {test.localized
                      ? t(
                          test.message,
                          test.authFallback
                            ? { value1: t("Authentication required") }
                            : test.values,
                        )
                      : test.message}
                  </span>
                </p>
              )}
              {test.status === "fail" && (
                <p className="flex items-start gap-1.5 text-destructive">
                  <AlertTriangle className="mt-0.5 size-3.5 shrink-0" />
                  <span>
                    {test.localized
                      ? t(
                          test.message,
                          test.authFallback
                            ? { value1: t("Authentication required") }
                            : test.values,
                        )
                      : test.message}
                  </span>
                </p>
              )}
            </div>
          )}
        </div>
        <DialogFooter className="sm:justify-between">
          <Button
            variant="ghost"
            onClick={handleTest}
            disabled={busy || errors.length > 0 || test.status === "testing"}
          >
            {test.status === "testing" ? (
              <>
                <Loader2 className="size-4 animate-spin" />
                {t("Testing… ")}
              </>
            ) : (
              t("Test connection")
            )}
          </Button>
          <div className="flex gap-2">
            <Button variant="outline" onClick={() => onOpenChange(false)} disabled={busy}>
              {t("Cancel ")}
            </Button>
            <Button onClick={handleSave} disabled={!canSave}>
              {busy ? (
                <>
                  <Loader2 className="size-4 animate-spin" />
                  {editing ? t("Saving…") : t("Adding…")}
                </>
              ) : editing ? (
                t("Save")
              ) : (
                t("Add")
              )}
            </Button>
          </div>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
