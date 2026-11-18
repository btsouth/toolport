import { msg, Trans, translate, useI18n } from "@/lib/i18n";
import { useEffect, useId, useMemo, useRef, useState } from "react";
import {
  Check,
  CheckCircle2,
  ChevronRight,
  Copy,
  FileText,
  FlaskConical,
  Loader2,
  MessageSquare,
  Pencil,
  Pin,
  Play,
  Search,
  ShieldAlert,
  X,
  XCircle,
} from "lucide-react";
import { toastError } from "@/lib/toast";
import {
  callTool,
  cancelToolCall,
  clearToolOverride,
  getPrompt,
  listServerPrompts,
  listServerResources,
  listServerTools,
  onToolRunProgress,
  readResource,
  setToolEnabled,
  setToolPinned,
  setToolOverride,
} from "@/lib/api";
import type {
  JsonSchemaProp,
  McpPrompt,
  McpResource,
  McpTool,
  Registry,
  ToolCallResult,
} from "@/lib/types";
import { Badge } from "@/components/ui/badge";
import { pickIconSrc } from "@/lib/icons";
import { SkeletonRows } from "@/components/ui/skeleton";
import { EmptyState } from "@/components/ui/empty-state";
import { ErrorCallout } from "@/components/ui/status-card";
import { openGatewayLog } from "@/lib/api";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { Textarea } from "@/components/ui/textarea";
import { Callout } from "@/components/Callout";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

/** A tool is destructive if it carries the MCP `destructiveHint` annotation. */
function isDestructive(tool: McpTool): boolean {
  return tool.annotations?.destructiveHint === true || tool.destructiveHint === true;
}

/** Editor to rename / re-describe a tool as clients see it (the security lever: locally
 * neutralize a misleading or injection-laden description). Collapsed by default; the call
 * still routes to the original downstream tool. Overrides are keyed by (server, original
 * tool name), so they're stable across renames and collision suffixes. */
function ToolOverrideEditor({
  serverId,
  tool,
  registry,
  onRegistryChange,
}: {
  serverId: string;
  tool: McpTool;
  registry: Registry | null;
  onRegistryChange: (r: Registry) => void;
}) {
  const { t } = useI18n();
  const current = registry?.toolOverrides?.[serverId]?.[tool.name];
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [busy, setBusy] = useState(false);

  // Re-sync the fields (and collapse) whenever the selected tool changes.
  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- Reset editable tool fields only when the selected tool changes.
    setName(current?.name ?? "");
    setDescription(current?.description ?? "");
    setOpen(false);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [serverId, tool.name]);

  const hasOverride = !!current;

  const save = async () => {
    setBusy(true);
    try {
      onRegistryChange(
        await setToolOverride(serverId, tool.name, name || null, description || null),
      );
    } catch (e) {
      toastError(translate("Couldn't save override: {value0}", { value0: String(e) }));
    } finally {
      setBusy(false);
    }
  };
  const reset = async () => {
    setBusy(true);
    try {
      onRegistryChange(await clearToolOverride(serverId, tool.name));
      setName("");
      setDescription("");
    } catch (e) {
      toastError(translate("Couldn't clear override: {value0}", { value0: String(e) }));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="rounded-md border border-border/60 bg-muted/20">
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
        className="flex w-full items-center gap-2 px-3 py-2 text-meta text-muted-foreground"
      >
        <Pencil className="size-3.5 shrink-0" />
        <span className="font-medium text-muted-foreground">
          {t("Override how clients see this tool")}
        </span>
        {hasOverride && (
          <span className="rounded-full bg-info/15 px-1.5 py-0.5 text-xs font-medium text-info">
            {t("active")}
          </span>
        )}
        <ChevronRight
          className={`ml-auto size-3.5 transition-transform ${open ? "rotate-90" : ""}`}
        />
      </button>
      {open && (
        <div className="flex flex-col gap-2.5 border-t border-border/60 px-3 py-3">
          <p className="text-meta text-muted-foreground">
            {t(
              "Rename the tool or replace its description as every client sees it, e.g. to neutralize a misleading or injection-laden description. The call still runs the original server tool.",
            )}
          </p>
          <label className="flex flex-col gap-1 text-meta">
            <span className="text-muted-foreground">
              {t("Name")}{" "}
              <span className="text-muted-foreground">
                <Trans
                  message="(blank keeps {name})"
                  values={{ name: <span className="font-mono">{tool.name}</span> }}
                />
              </span>
            </span>
            <input
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder={tool.name}
              className="rounded-md border border-input bg-transparent px-2.5 py-1.5 font-mono text-meta shadow-sm focus-visible:ring-3 focus-visible:ring-ring/50 focus-visible:outline-none"
            />
          </label>
          <label className="flex flex-col gap-1 text-meta">
            <span className="text-muted-foreground">
              {t("Description (blank keeps the server's)")}
            </span>
            <textarea
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              rows={3}
              placeholder={tool.description ?? t("(server's description)")}
              className="rounded-md border border-input bg-transparent px-2.5 py-1.5 text-meta shadow-sm focus-visible:ring-3 focus-visible:ring-ring/50 focus-visible:outline-none"
            />
          </label>
          <div className="flex gap-2">
            <Button size="sm" onClick={() => void save()} disabled={busy}>
              {t("Save override")}
            </Button>
            {hasOverride && (
              <Button
                size="sm"
                variant="outline"
                onClick={() => void reset()}
                disabled={busy}
              >
                {t("Reset to original")}
              </Button>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

/** First declared type of a JSON-schema property (schemas may list several). */
function primaryType(schema: JsonSchemaProp): string {
  return Array.isArray(schema.type)
    ? (schema.type[0] ?? "string")
    : (schema.type ?? "string");
}

/** Turn a form field's raw value into the JSON type the schema expects. */
function coerce(v: unknown, schema: JsonSchemaProp): unknown {
  if (typeof v !== "string") return v; // booleans arrive already typed
  const t = primaryType(schema);
  if (t === "number" || t === "integer") {
    const n = Number(v);
    return Number.isNaN(n) ? v : n;
  }
  if (t === "boolean") return v === "true";
  if (t === "object" || t === "array") {
    try {
      return JSON.parse(v);
    } catch {
      return v; // let the server reject it rather than swallowing the input
    }
  }
  return v;
}

/** Render the result content blocks as readable text. */
const PROVENANCE_KEY = "app.toolport/provenance";

/** Toolport's provenance label, removed from every `_meta`; the server's own
 * `_meta` fields stay. GTK's result view does the same. */
function withoutProvenance(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(withoutProvenance);
  if (value === null || typeof value !== "object") return value;
  const out: Record<string, unknown> = {};
  for (const [key, item] of Object.entries(value)) {
    if (
      key === "_meta" &&
      item !== null &&
      typeof item === "object" &&
      !Array.isArray(item)
    ) {
      const meta = Object.fromEntries(
        Object.entries(item).filter(([name]) => name !== PROVENANCE_KEY),
      );
      if (Object.keys(meta).length > 0) out[key] = withoutProvenance(meta);
    } else {
      out[key] = withoutProvenance(item);
    }
  }
  return out;
}

function renderResult(result: ToolCallResult): string {
  const blocks = result.content ?? [];
  if (blocks.length === 0) return JSON.stringify(withoutProvenance(result), null, 2);
  return blocks
    .map((b) =>
      b.type === "text" && b.text != null
        ? b.text
        : JSON.stringify(withoutProvenance(b), null, 2),
    )
    .join("\n");
}

interface FieldProps {
  name: string;
  schema: JsonSchemaProp;
  required: boolean;
  value: unknown;
  onChange: (v: unknown) => void;
}

/** One argument input, shaped by the property's JSON-schema type. */
function ArgField({ name, schema, required, value, onChange }: FieldProps) {
  const { t } = useI18n();
  const schemaType = primaryType(schema);
  const id = useId();
  const enumVals = schema.enum;

  let control;
  if (enumVals && enumVals.length > 0) {
    control = (
      <Select value={value === undefined ? "" : String(value)} onValueChange={onChange}>
        <SelectTrigger id={id} className="h-9">
          <SelectValue placeholder={t("Select…")} />
        </SelectTrigger>
        <SelectContent>
          {enumVals.map((e) => (
            <SelectItem key={String(e)} value={String(e)}>
              {String(e)}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    );
  } else if (schemaType === "boolean") {
    control = (
      <Switch
        id={id}
        checked={value === true || value === "true"}
        onCheckedChange={(c) => onChange(c)}
      />
    );
  } else if (schemaType === "object" || schemaType === "array") {
    control = (
      <Textarea
        id={id}
        value={typeof value === "string" ? value : ""}
        onChange={(e) => onChange(e.target.value)}
        placeholder={schemaType === "array" ? "[ … ]" : "{ … }"}
        rows={3}
        className="font-mono text-meta"
      />
    );
  } else {
    control = (
      <Input
        id={id}
        type={schemaType === "number" || schemaType === "integer" ? "number" : "text"}
        value={value === undefined ? "" : String(value)}
        onChange={(e) => onChange(e.target.value)}
        placeholder={
          schema.default != null
            ? t("default: {value0}", { value0: String(schema.default) })
            : ""
        }
        className="h-9"
      />
    );
  }

  return (
    <div className="flex flex-col gap-1.5">
      <Label htmlFor={id} className="flex items-center gap-1.5">
        <span className="font-mono text-meta">{name}</span>
        {required && <span className="text-destructive">*</span>}
        <span className="text-meta font-normal text-muted-foreground">{schemaType}</span>
      </Label>
      {schema.description && (
        <p className="text-meta text-muted-foreground">{schema.description}</p>
      )}
      {control}
    </div>
  );
}

/** Heuristic: does a tool-list failure look like the server needing authentication
 * (missing/expired credentials, 401/403) rather than a generic transport error? If so
 * we point the user at the server's Secrets dialog instead of showing a raw backend string. */
function looksLikeAuthError(message: string): boolean {
  return /\b(401|403|unauthori[sz]ed|forbidden|authenticat\w*|invalid.{0,15}(token|api.?key|credentials?)|missing.{0,15}(token|api.?key|credentials?)|expired.{0,15}(token|credentials?)|not.{0,15}logged.?in|access.{0,15}denied)\b/i.test(
    message,
  );
}

/** Inline "connecting…" line shared by the resource/prompt panels. */
function Connecting({ label }: { label?: string }) {
  const { t } = useI18n();
  return <SkeletonRows label={label ?? t("Connecting to server…")} />;
}

/** Copy-to-clipboard button with a brief confirmation, for raw result / JSON blocks. */
function CopyButton({ text }: { text: string }) {
  const { t } = useI18n();
  const [copied, setCopied] = useState(false);
  return (
    <button
      type="button"
      aria-label={t("Copy to clipboard")}
      onClick={async () => {
        try {
          await navigator.clipboard.writeText(text);
          setCopied(true);
          setTimeout(() => setCopied(false), 1200);
        } catch {
          /* clipboard unavailable; no-op */
        }
      }}
      className="ml-auto inline-flex items-center gap-1 rounded-md px-1.5 py-0.5 text-xs font-medium text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
    >
      {copied ? <Check className="size-3" /> : <Copy className="size-3" />}
      {copied ? t("Copied") : t("Copy")}
    </button>
  );
}

/** A raw MCP result rendered as pretty JSON (or text), with a copy button. */
function ResultBlock({ title, value }: { title: string; value: unknown }) {
  const text =
    typeof value === "string" ? value : JSON.stringify(withoutProvenance(value), null, 2);
  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center gap-2">
        <div className="text-sm font-medium">{title}</div>
        <CopyButton text={text} />
      </div>
      <pre className="max-h-96 overflow-auto rounded-lg border bg-muted/40 p-3 font-mono text-meta whitespace-pre-wrap">
        {text}
      </pre>
    </div>
  );
}

/** Resources tab: list what the server advertises and read one on click. */
function ResourcesPanel({ serverId }: { serverId: string }) {
  const { t } = useI18n();
  const [resources, setResources] = useState<McpResource[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [reading, setReading] = useState(false);
  const [result, setResult] = useState<unknown>(null);
  const [readError, setReadError] = useState<string | null>(null);
  const [reloadKey, setReloadKey] = useState(0);
  // Id of the read that owns the result area. Picking another resource or unmounting
  // bumps it, so a late read can't replace the current one or clear its busy state.
  const readSeq = useRef(0);

  useEffect(() => {
    const seq = readSeq;
    return () => {
      seq.current += 1;
    };
  }, []);

  useEffect(() => {
    let alive = true;
    // eslint-disable-next-line react-hooks/set-state-in-effect -- Clear the previous server resources before starting its replacement request.
    setLoading(true);
    setError(null);
    setResources(null);
    setSelected(null);
    setResult(null);
    setReadError(null);
    listServerResources(serverId)
      .then((r) => alive && setResources(r))
      .catch((e) => alive && setError(String(e)))
      .finally(() => alive && setLoading(false));
    return () => {
      alive = false;
    };
  }, [serverId, reloadKey]);

  function retryList() {
    setReloadKey((n) => n + 1);
  }

  async function read(uri: string) {
    const myId = ++readSeq.current;
    setSelected(uri);
    setReading(true);
    setResult(null);
    setReadError(null);
    try {
      const contents = await readResource(serverId, uri);
      if (readSeq.current === myId) setResult(contents);
    } catch (e) {
      if (readSeq.current === myId) setReadError(String(e));
    } finally {
      if (readSeq.current === myId) setReading(false);
    }
  }

  if (loading) return <Connecting />;
  if (error)
    return <ErrorCallout title={t("Resources")} cause={error} onRetry={retryList} />;
  if (!resources || resources.length === 0) {
    return <EmptyState title={t("This server advertises no resources.")} />;
  }

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-col gap-1.5">
        <Label className="text-meta text-muted-foreground">
          {t("Resources ({count})", { count: resources.length })}
        </Label>
        <div className="flex flex-col divide-y rounded-lg border">
          {resources.map((r) => (
            <button
              key={r.uri}
              onClick={() => read(r.uri)}
              aria-pressed={selected === r.uri}
              className={`flex flex-col items-start gap-0.5 px-3 py-2 text-left ${
                selected === r.uri ? "bg-accent" : "hover:bg-muted/40"
              }`}
            >
              <span className="flex w-full min-w-0 flex-wrap items-center gap-2">
                <span className="truncate font-mono text-sm">
                  {r.name ?? r.title ?? r.uri}
                </span>
                {r.mimeType && (
                  <Badge variant="outline" className="text-muted-foreground">
                    {r.mimeType}
                  </Badge>
                )}
              </span>
              <span className="truncate font-mono text-meta text-muted-foreground">
                {r.uri}
              </span>
              {r.description && (
                <span className="line-clamp-1 text-meta text-muted-foreground">
                  {r.description}
                </span>
              )}
            </button>
          ))}
        </div>
      </div>
      {reading && <Connecting label={t("Reading resource…")} />}
      {readError && <Callout variant="danger">{readError}</Callout>}
      {result != null && <ResultBlock title={t("Resource contents")} value={result} />}
    </div>
  );
}

/** Prompts tab: list prompts, fill any arguments, and render one. */
function PromptsPanel({ serverId }: { serverId: string }) {
  const { t } = useI18n();
  const [prompts, setPrompts] = useState<McpPrompt[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [args, setArgs] = useState<Record<string, string>>({});
  const [running, setRunning] = useState(false);
  const [result, setResult] = useState<unknown>(null);
  const [runError, setRunError] = useState<string | null>(null);
  const [reloadKey, setReloadKey] = useState(0);
  // Id of the prompt request that owns the result area. Changing the selection or
  // unmounting bumps it, so a late response can't replace the current one or clear its
  // busy state.
  const runSeq = useRef(0);

  useEffect(() => {
    const seq = runSeq;
    return () => {
      seq.current += 1;
    };
  }, []);

  useEffect(() => {
    let alive = true;
    // eslint-disable-next-line react-hooks/set-state-in-effect -- Clear the previous server prompts before starting its replacement request.
    setLoading(true);
    setError(null);
    setPrompts(null);
    setSelected(null);
    setResult(null);
    setRunError(null);
    listServerPrompts(serverId)
      .then((p) => alive && setPrompts(p))
      .catch((e) => alive && setError(String(e)))
      .finally(() => alive && setLoading(false));
    return () => {
      alive = false;
    };
  }, [serverId, reloadKey]);

  function retryList() {
    setReloadKey((n) => n + 1);
  }

  const prompt = prompts?.find((p) => p.name === selected) ?? null;

  useEffect(() => {
    // A new selection takes over the result area, so drop any request still in flight.
    runSeq.current += 1;
    // eslint-disable-next-line react-hooks/set-state-in-effect -- Reset prompt arguments and results when the selection changes.
    setArgs({});
    setResult(null);
    setRunError(null);
    setRunning(false);
  }, [selected]);

  async function run() {
    if (!prompt) return;
    const missing = (prompt.arguments ?? [])
      .filter((a) => a.required && !args[a.name]?.trim())
      .map((a) => a.name);
    if (missing.length > 0) {
      setRunError(
        translate(
          missing.length === 1
            ? msg("Fill in required argument: {names}")
            : msg("Fill in required arguments: {names}"),
          { names: missing.join(", ") },
        ),
      );
      return;
    }
    const myId = ++runSeq.current;
    setRunning(true);
    setResult(null);
    setRunError(null);
    try {
      const messages = await getPrompt(serverId, prompt.name, args);
      if (runSeq.current === myId) setResult(messages);
    } catch (e) {
      if (runSeq.current === myId) setRunError(String(e));
    } finally {
      if (runSeq.current === myId) setRunning(false);
    }
  }

  if (loading) return <Connecting />;
  if (error)
    return <ErrorCallout title={t("Prompts")} cause={error} onRetry={retryList} />;
  if (!prompts || prompts.length === 0) {
    return <EmptyState title={t("This server advertises no prompts.")} />;
  }

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-col gap-1.5">
        <Label className="text-meta text-muted-foreground">
          {t("Prompts ({count})", { count: prompts.length })}
        </Label>
        <div className="flex flex-col divide-y rounded-lg border">
          {prompts.map((p) => (
            <button
              key={p.name}
              onClick={() => setSelected(p.name)}
              aria-pressed={p.name === selected}
              className={`flex flex-col items-start gap-0.5 px-3 py-2 text-left ${
                p.name === selected ? "bg-accent" : "hover:bg-muted/40"
              }`}
            >
              <span className="truncate font-mono text-sm">{p.title ?? p.name}</span>
              {p.description && (
                <span className="line-clamp-1 text-meta text-muted-foreground">
                  {p.description}
                </span>
              )}
            </button>
          ))}
        </div>
      </div>

      {prompt && (
        <div className="flex flex-col gap-4 rounded-lg border bg-card p-4">
          {prompt.description && (
            <p className="text-sm text-muted-foreground">{prompt.description}</p>
          )}
          {(prompt.arguments ?? []).length === 0 ? (
            <p className="text-sm text-muted-foreground">
              {t("This prompt takes no arguments.")}
            </p>
          ) : (
            <div className="flex flex-col gap-4">
              {(prompt.arguments ?? []).map((a) => (
                <div key={a.name} className="flex flex-col gap-1.5">
                  <Label
                    htmlFor={`prompt-arg-${a.name}`}
                    className="flex items-center gap-1.5"
                  >
                    <span className="font-mono text-meta">{a.name}</span>
                    {a.required && <span className="text-destructive">*</span>}
                  </Label>
                  {a.description && (
                    <p className="text-meta text-muted-foreground">{a.description}</p>
                  )}
                  <Input
                    id={`prompt-arg-${a.name}`}
                    value={args[a.name] ?? ""}
                    onChange={(e) =>
                      setArgs((prev) => ({ ...prev, [a.name]: e.target.value }))
                    }
                    className="h-9"
                  />
                </div>
              ))}
            </div>
          )}
          <div>
            <Button onClick={run} disabled={running} size="sm">
              {running ? (
                <Loader2 className="size-4 animate-spin" />
              ) : (
                <Play className="size-4" />
              )}
              {running ? t("Getting…") : t("Get prompt")}
            </Button>
          </div>
        </div>
      )}

      {runError && <Callout variant="danger">{runError}</Callout>}
      {result != null && <ResultBlock title={t("Prompt messages")} value={result} />}
    </div>
  );
}

interface ServerToolsProps {
  serverId: string;
  registry: Registry | null;
  onRegistryChange: (registry: Registry) => void;
}

export function ServerToolsPanel({
  serverId,
  registry,
  onRegistryChange,
}: ServerToolsProps) {
  const { t } = useI18n();
  const servers = useMemo(() => registry?.servers ?? [], [registry?.servers]);
  const denyDestructive = registry?.denyDestructive ?? false;
  const defaultAccessKey = `${registry?.defaultAccessProfileId}:${registry?.defaultAccessContextId}:${registry?.defaultAccessLegacyPolicy}`;
  const overrideVersion = JSON.stringify(registry?.toolOverrides?.[serverId] ?? {});

  const [tab, setTab] = useState<"tools" | "resources" | "prompts">("tools");
  const [policyBusy, setPolicyBusy] = useState(false);
  const [tools, setTools] = useState<McpTool[] | null>(null);
  const [loadingTools, setLoadingTools] = useState(false);
  const [toolsReload, setToolsReload] = useState(0);
  const [toolsError, setToolsError] = useState<string | null>(null);
  const [selectedTool, setSelectedTool] = useState<string | null>(null);
  const [toolFilter, setToolFilter] = useState("");

  const [args, setArgs] = useState<Record<string, unknown>>({});
  const [rawMode, setRawMode] = useState(false);
  const [rawJson, setRawJson] = useState("{}");

  const [calling, setCalling] = useState(false);
  const [result, setResult] = useState<ToolCallResult | null>(null);
  const [callError, setCallError] = useState<string | null>(null);
  // Seconds elapsed while a call is in flight, and a monotonically-increasing id used
  // to invalidate a call the user cancelled or superseded, so its late result is
  // ignored even after the gateway was told to cancel it.
  const [elapsed, setElapsed] = useState(0);
  const callSeq = useRef(0);
  const tickerRef = useRef<ReturnType<typeof setInterval> | null>(null);
  // The invoke panel sits ABOVE the tool list; on a long list, scroll it into view when a
  // tool is picked so the arguments form is never buried out of sight.
  const invokeRef = useRef<HTMLDivElement | null>(null);
  // The Run in flight, so Cancel and the backend's "starting" phase reach it. The
  // backend bounds the Run itself (server start, approval and the call's own limit).
  const runIdRef = useRef<string | null>(null);
  const [starting, setStarting] = useState(false);

  /** Cancel the Run in flight, if any, in the gateway as well as here. */
  function abandonRun() {
    const runId = runIdRef.current;
    runIdRef.current = null;
    setStarting(false);
    if (runId) void cancelToolCall(runId).catch(() => {});
  }

  useEffect(() => {
    const unlisten = onToolRunProgress(({ runId, phase }) => {
      if (runId === runIdRef.current) setStarting(phase === "starting");
    });
    // Stop the elapsed-time ticker and the Run if the view unmounts mid-call.
    return () => {
      void unlisten.then((stop) => stop());
      if (tickerRef.current) clearInterval(tickerRef.current);
      const runId = runIdRef.current;
      runIdRef.current = null;
      if (runId) void cancelToolCall(runId).catch(() => {});
    };
  }, []);

  // Bring the invoke panel into view when a tool is selected (it renders above the list).
  useEffect(() => {
    if (selectedTool && invokeRef.current) {
      invokeRef.current.scrollIntoView({ behavior: "smooth", block: "start" });
    }
  }, [selectedTool]);

  // Connect to the chosen server and pull its tool list.
  useEffect(() => {
    // A server switch invalidates any in-flight call: drop its late result and clear the
    // "Calling…" state + ticker, so a superseded call can't paint under the new server.
    callSeq.current += 1;
    // eslint-disable-next-line react-hooks/set-state-in-effect -- Invalidate the previous server call and clear its visible state together.
    abandonRun();
    if (tickerRef.current) clearInterval(tickerRef.current);
    setCalling(false);
    setElapsed(0);
    if (!serverId) {
      setTools(null);
      return;
    }
    let alive = true;
    setLoadingTools(true);
    setToolsError(null);
    setTools(null);
    setSelectedTool(null);
    setToolFilter("");
    setResult(null);
    setCallError(null);
    listServerTools(serverId)
      .then((t) => alive && setTools(t))
      .catch((e) => alive && setToolsError(String(e)))
      .finally(() => alive && setLoadingTools(false));
    return () => {
      alive = false;
    };
  }, [serverId, defaultAccessKey, overrideVersion, toolsReload]);

  const tool = useMemo(
    () => tools?.find((t) => t.name === selectedTool) ?? null,
    [tools, selectedTool],
  );

  // Filter the tool list by name or description. A server can expose dozens of
  // tools, so scanning a flat list to find one to test is painful without this.
  const filteredTools = useMemo(() => {
    const q = toolFilter.trim().toLowerCase();
    if (!q) return tools ?? [];
    return (tools ?? []).filter(
      (t) =>
        t.name.toLowerCase().includes(q) ||
        (t.description?.toLowerCase().includes(q) ?? false),
    );
  }, [tools, toolFilter]);
  const props = tool?.inputSchema?.properties ?? {};
  const required = useMemo(() => new Set(tool?.inputSchema?.required ?? []), [tool]);

  const serverEntry = useMemo(
    () => servers.find((s) => s.id === serverId) ?? null,
    [servers, serverId],
  );
  const disabledSet = useMemo(
    () => new Set(serverEntry?.disabledTools ?? []),
    [serverEntry],
  );
  const pinnedSet = useMemo(
    () => new Set(serverId ? (registry?.pinnedTools?.[serverId] ?? []) : []),
    [registry, serverId],
  );

  /** Is this tool exposed to clients right now? (per-tool off, or destructive
   * while the global switch is on, both hide it at the gateway.) */
  function isExposed(t: McpTool): boolean {
    if (disabledSet.has(t.name)) return false;
    if (denyDestructive && isDestructive(t)) return false;
    return true;
  }

  async function toggleTool(toolName: string, enabled: boolean) {
    if (!serverId) return;
    setPolicyBusy(true);
    try {
      onRegistryChange(await setToolEnabled(serverId, toolName, enabled));
    } catch (e) {
      toastError(translate("Couldn't update the tool: {value0}", { value0: String(e) }));
    } finally {
      setPolicyBusy(false);
    }
  }

  async function togglePin(toolName: string, pinned: boolean) {
    if (!serverId) return;
    setPolicyBusy(true);
    try {
      onRegistryChange(await setToolPinned(serverId, toolName, pinned));
    } catch (e) {
      toastError(translate("Couldn't pin the tool: {value0}", { value0: String(e) }));
    } finally {
      setPolicyBusy(false);
    }
  }

  // Fresh argument state whenever the selected tool changes.
  useEffect(() => {
    // Switching tools also invalidates an in-flight call (same reasoning as the server
    // effect), so a late result doesn't land under the newly selected tool.
    callSeq.current += 1;
    // eslint-disable-next-line react-hooks/set-state-in-effect -- Invalidate the previous tool call and clear its visible state together.
    abandonRun();
    if (tickerRef.current) clearInterval(tickerRef.current);
    setCalling(false);
    setElapsed(0);
    setArgs({});
    setResult(null);
    setCallError(null);
    setRawMode(false);
    setRawJson("{}");
  }, [selectedTool]);

  /** Assemble the arguments object from the form (or the raw JSON editor). */
  function buildArgs(): Record<string, unknown> | { error: string } {
    if (rawMode) {
      try {
        const parsed = JSON.parse(rawJson || "{}");
        if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) {
          return { error: translate("Arguments must be a JSON object.") };
        }
        return parsed as Record<string, unknown>;
      } catch (e) {
        return { error: translate("Invalid JSON: {value0}", { value0: String(e) }) };
      }
    }
    const out: Record<string, unknown> = {};
    for (const [k, schema] of Object.entries(props)) {
      const v = args[k];
      if (v === undefined || v === "") continue;
      out[k] = coerce(v, schema as JsonSchemaProp);
    }
    const missing = [...required].filter((k) => !(k in out));
    if (missing.length > 0) {
      return {
        error: translate(
          missing.length === 1
            ? msg("Fill in required field: {names}")
            : msg("Fill in required fields: {names}"),
          { names: missing.join(", ") },
        ),
      };
    }
    return out;
  }

  async function run() {
    if (!serverId || !selectedTool) return;
    const built = buildArgs();
    if ("error" in built && typeof built.error === "string") {
      setCallError(built.error);
      setResult(null);
      return;
    }
    const myId = ++callSeq.current;
    const runId = `run-${myId}-${Date.now()}`;
    runIdRef.current = runId;
    setStarting(false);
    setCalling(true);
    setResult(null);
    setCallError(null);
    setElapsed(0);
    const started = Date.now();
    if (tickerRef.current) clearInterval(tickerRef.current);
    tickerRef.current = setInterval(() => {
      if (callSeq.current === myId) setElapsed(Math.floor((Date.now() - started) / 1000));
    }, 1000);
    try {
      // The backend times the Run out itself and says so in the user's language.
      const r = await callTool(
        serverId,
        selectedTool,
        built as Record<string, unknown>,
        runId,
      );
      if (callSeq.current !== myId) return; // cancelled or superseded by a newer call
      setResult(r as ToolCallResult);
    } catch (e) {
      if (callSeq.current !== myId) return;
      setCallError(String(e));
    } finally {
      // Only tear down if we're still the current call: a cancelled/superseded call
      // resolving late must not clear a newer call's ticker (which would freeze its
      // elapsed timer) or flip its calling state.
      if (callSeq.current === myId) {
        if (tickerRef.current) clearInterval(tickerRef.current);
        if (runIdRef.current === runId) runIdRef.current = null;
        setStarting(false);
        setCalling(false);
      }
    }
  }

  // Cancel the Run: the gateway is told to cancel the call, including a wait for
  // the server to start or a pending approval, and its late result is dropped.
  function cancelCall() {
    callSeq.current++;
    abandonRun();
    if (tickerRef.current) clearInterval(tickerRef.current);
    setCalling(false);
    setCallError(translate("Stopped. The server was told to cancel the call."));
  }

  if (servers.length === 0) {
    return (
      <div className="flex flex-col items-center justify-center gap-3 py-24 text-center">
        <FlaskConical className="size-10 text-muted-foreground" />
        <div>
          <p className="font-medium">{t("No servers to test")}</p>
          <p className="max-w-md text-sm text-muted-foreground">
            {t(
              "Add a server to Toolport, then come back here to invoke its tools and see the raw results, without wiring up a client first.",
            )}
          </p>
        </div>
      </div>
    );
  }

  return (
    <div className="flex w-full min-w-0 flex-col gap-5">
      <p className="text-meta text-muted-foreground">
        {t("Calls use the default access and gateway policy, including approval gates.")}
      </p>

      {serverId && (
        <div className="flex w-fit gap-1 rounded-md border bg-muted/30 p-1 text-sm">
          {(["tools", "resources", "prompts"] as const).map((t) => (
            <button
              key={t}
              onClick={() => setTab(t)}
              aria-pressed={tab === t}
              className={`flex items-center gap-1.5 rounded-md px-3 py-1 capitalize transition-colors ${
                tab === t
                  ? "bg-background font-medium shadow-sm"
                  : "text-muted-foreground hover:text-foreground"
              }`}
            >
              {t === "resources" && <FileText className="size-3.5" />}
              {t === "prompts" && <MessageSquare className="size-3.5" />}
              {t === "tools"
                ? translate("Tools")
                : t === "resources"
                  ? translate("Resources")
                  : translate("Prompts")}
            </button>
          ))}
        </div>
      )}

      {serverId && tab === "resources" && (
        <ResourcesPanel key={serverId} serverId={serverId} />
      )}
      {serverId && tab === "prompts" && (
        <PromptsPanel key={serverId} serverId={serverId} />
      )}

      {tab === "tools" && (
        <>
          {loadingTools && <SkeletonRows label={t("Connecting to server…")} />}

          {toolsError &&
            (looksLikeAuthError(toolsError) ? (
              <Callout variant="warning">
                <span className="font-medium">
                  {t("Sign in to {server} first.", {
                    server: serverEntry?.name ?? t("this server"),
                  })}
                </span>{" "}
                <Trans
                  message="Toolport couldn't list its tools without credentials. Open {servers}, choose this server, and use {authenticate}."
                  values={{
                    servers: <span className="font-medium">{t("Servers")}</span>,
                    authenticate: (
                      <span className="font-medium">{t("Authenticate")}</span>
                    ),
                  }}
                />
                <p className="mt-1.5 font-mono text-meta break-words text-muted-foreground">
                  {toolsError}
                </p>
              </Callout>
            ) : (
              <ErrorCallout
                title={t("Tools")}
                cause={toolsError}
                onRetry={() => setToolsReload((value) => value + 1)}
                onViewLog={() =>
                  void openGatewayLog().catch((e: unknown) => toastError(String(e)))
                }
              />
            ))}

          {/* Server connected but exposes no tools at all. */}
          {!loadingTools && !toolsError && tools && tools.length === 0 && (
            <EmptyState title={t("This server advertises no tools.")} />
          )}

          {/* Argument form for the selected tool */}
          {tool && (
            <div
              ref={invokeRef}
              className="flex flex-col gap-4 rounded-lg border bg-card p-4"
            >
              {tool.description && (
                <p className="text-sm text-muted-foreground">{tool.description}</p>
              )}
              {!isExposed(tool) && (
                <p className="flex items-center gap-1.5 text-meta text-warning">
                  <ShieldAlert className="size-3.5" />
                  {t(
                    "Hidden from clients by policy. The gateway will refuse blocked calls.",
                  )}
                </p>
              )}

              {serverId && (
                <ToolOverrideEditor
                  serverId={serverId}
                  tool={tool}
                  registry={registry}
                  onRegistryChange={onRegistryChange}
                />
              )}

              <div className="flex items-center justify-between">
                <span className="text-xs font-medium tracking-wide text-muted-foreground uppercase">
                  {t("Arguments")}
                </span>
                <label className="flex items-center gap-2 text-meta text-muted-foreground">
                  {t("Raw JSON")}
                  <Switch checked={rawMode} onCheckedChange={setRawMode} />
                </label>
              </div>

              {rawMode ? (
                <div className="flex flex-col gap-1.5">
                  <span className="text-meta text-muted-foreground">
                    {t(
                      "Raw-JSON mode: type the full arguments object yourself. The generated form is bypassed.",
                    )}
                  </span>
                  <textarea
                    value={rawJson}
                    onChange={(e) => setRawJson(e.target.value)}
                    rows={6}
                    spellCheck={false}
                    className="w-full rounded-md border border-input bg-transparent px-3 py-2 font-mono text-meta shadow-sm focus-visible:ring-3 focus-visible:ring-ring/50 focus-visible:outline-none"
                  />
                </div>
              ) : Object.keys(props).length === 0 ? (
                <p className="text-sm text-muted-foreground">
                  {t("This tool takes no arguments.")}
                </p>
              ) : (
                <div className="flex flex-col gap-4">
                  {Object.entries(props).map(([name, schema]) => (
                    <ArgField
                      key={name}
                      name={name}
                      schema={schema as JsonSchemaProp}
                      required={required.has(name)}
                      value={args[name]}
                      onChange={(v) => setArgs((a) => ({ ...a, [name]: v }))}
                    />
                  ))}
                </div>
              )}

              <div className="flex items-center gap-2">
                <Button onClick={run} disabled={calling} size="sm">
                  {calling ? (
                    <Loader2 className="size-4 animate-spin" />
                  ) : (
                    <Play className="size-4" />
                  )}
                  {calling
                    ? starting
                      ? t("Starting {value0}… {value1}s", {
                          value0: serverEntry?.name ?? serverId,
                          value1: elapsed,
                        })
                      : t("Calling… {value0}s", { value0: elapsed })
                    : t("Call tool")}
                </Button>
                {calling && (
                  <Button onClick={cancelCall} variant="outline" size="sm">
                    <X className="size-4" />
                    {t("Cancel")}
                  </Button>
                )}
              </div>
            </div>
          )}

          {/* Call error (transport / connection failure) */}
          {callError && <Callout variant="danger">{callError}</Callout>}

          {/* Result */}
          {result && (
            <div className="flex flex-col gap-2">
              <div className="flex items-center gap-2 text-sm font-medium">
                {result.isError ? (
                  <XCircle className="size-4 text-destructive" />
                ) : (
                  <CheckCircle2 className="size-4 text-success" />
                )}
                {result.isError ? t("Tool returned an error") : t("Result")}
                <CopyButton text={renderResult(result)} />
              </div>
              <pre
                className={`max-h-96 overflow-auto rounded-lg border p-3 font-mono text-meta whitespace-pre-wrap ${
                  result.isError
                    ? "border-destructive/40 bg-destructive/5"
                    : "bg-muted/40"
                }`}
              >
                {renderResult(result)}
              </pre>
            </div>
          )}
          {/* Tool list: click to test, switch to enable/disable per client */}
          {tools && tools.length > 0 && (
            <div className="flex flex-col gap-1.5">
              <Label className="text-meta text-muted-foreground">
                {t("Tools")}{" "}
                {toolFilter.trim()
                  ? t("({value0} of {value1})", {
                      value0: filteredTools.length,
                      value1: tools.length,
                    })
                  : `(${tools.length})`}
              </Label>
              {/* A search box once the list is long enough to be worth scanning. */}
              {tools.length > 7 && (
                <div className="relative">
                  <Search className="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-muted-foreground" />
                  <Input
                    value={toolFilter}
                    onChange={(e) => setToolFilter(e.target.value)}
                    placeholder={t("Filter tools by name or description")}
                    className="h-9 pl-8"
                    aria-label={t("Filter tools")}
                  />
                </div>
              )}
              {filteredTools.length === 0 ? (
                <div className="flex flex-col items-center gap-2 rounded-xl border bg-card px-4 py-8 text-center">
                  <p className="text-sm text-muted-foreground">
                    {t('No tools match "{filter}".', { filter: toolFilter.trim() })}
                  </p>
                  <button
                    onClick={() => setToolFilter("")}
                    className="inline-flex items-center gap-1.5 rounded-md border px-2.5 py-1 text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
                  >
                    <X className="size-3.5" />
                    {t("Clear filter")}
                  </button>
                </div>
              ) : (
                <div className="flex flex-col divide-y rounded-lg border">
                  {filteredTools.map((t) => {
                    const destructive = isDestructive(t);
                    const exposed = isExposed(t);
                    const selected = t.name === selectedTool;
                    const perToolOff = disabledSet.has(t.name);
                    const pinned = pinnedSet.has(t.name);
                    const icon = pickIconSrc(t.icons);
                    return (
                      <div
                        key={t.name}
                        className={`flex items-center gap-3 px-3 py-2 ${selected ? "bg-accent" : ""}`}
                      >
                        <button
                          onClick={() => setSelectedTool(t.name)}
                          aria-pressed={selected}
                          className="flex min-w-0 flex-1 flex-col items-start gap-0.5 text-left"
                        >
                          <span className="flex w-full min-w-0 flex-wrap items-center gap-2">
                            {/* SEP-973. Only data: icons render (see pickIconSrc); a
                                remote URL is a request to a server-chosen host on every
                                paint, and the app's CSP blocks it anyway. Fixed box so a
                                malformed or oddly-shaped icon cannot shift the row. */}
                            {icon && (
                              <img
                                src={icon}
                                alt=""
                                aria-hidden="true"
                                className="size-4 shrink-0 rounded-sm object-contain"
                              />
                            )}
                            <span
                              title={t.name}
                              className="min-w-0 max-w-full truncate font-mono text-sm"
                            >
                              {t.name}
                            </span>
                            {destructive && (
                              <Badge variant="warning">{translate("destructive")}</Badge>
                            )}
                            {t.annotations?.readOnlyHint === true && (
                              <Badge variant="secondary">{translate("read-only")}</Badge>
                            )}
                            <Badge variant={perToolOff ? "off" : "success"}>
                              {perToolOff ? translate("Off") : translate("On")}
                            </Badge>
                            {t.toolportQuarantine !== "clear" && (
                              <Badge variant="warning">
                                {t.toolportQuarantine === "quarantined"
                                  ? translate("Quarantined")
                                  : translate("Quarantine unknown")}
                              </Badge>
                            )}
                            {!exposed && (
                              <span className="text-meta text-muted-foreground">
                                {translate("hidden")}
                              </span>
                            )}
                          </span>
                          {t.description && (
                            <span className="line-clamp-1 text-meta text-muted-foreground">
                              {t.description}
                            </span>
                          )}
                        </button>
                        <button
                          onClick={() => togglePin(t.name, !pinned)}
                          disabled={policyBusy}
                          title={
                            pinned
                              ? translate(
                                  "Pinned: always surfaced in lazy-discovery search",
                                )
                              : translate(
                                  "Pin as a prerequisite (always surfaced in search)",
                                )
                          }
                          aria-label={
                            pinned
                              ? translate("Unpin {value0}", { value0: t.name })
                              : translate("Pin {value0}", { value0: t.name })
                          }
                          aria-pressed={pinned}
                          className="shrink-0 rounded p-1 hover:bg-muted disabled:opacity-50"
                        >
                          <Pin
                            className={`size-4 ${
                              pinned ? "fill-current text-owned" : "text-muted-foreground"
                            }`}
                          />
                        </button>
                        <Switch
                          checked={!perToolOff}
                          onCheckedChange={(on) => toggleTool(t.name, on)}
                          disabled={policyBusy}
                          // The name stays the tool's; the switch state says on or
                          // off, and the tooltip says what a click does, as in GTK.
                          // While the global switch hides a destructive tool, a
                          // click changes only its own setting, so it says that.
                          aria-label={t.name}
                          title={
                            denyDestructive && destructive
                              ? translate(
                                  "Destructive tools are hidden from clients by the global switch even when individually enabled.",
                                )
                              : perToolOff
                                ? translate("Hidden from clients. Expose it.")
                                : translate("Exposed to clients. Hide it.")
                          }
                        />
                      </div>
                    );
                  })}
                </div>
              )}
              {denyDestructive && (
                <p className="text-meta text-muted-foreground">
                  {t(
                    "Destructive tools are hidden from clients by the global switch even when individually enabled.",
                  )}
                </p>
              )}
            </div>
          )}
        </>
      )}
    </div>
  );
}
