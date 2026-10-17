import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
} from "react";
import { open as openFolderDialog } from "@tauri-apps/plugin-dialog";
import {
  AlertTriangle,
  Ban,
  Box,
  ChevronDown,
  ChevronRight,
  Cloud,
  Copy,
  CornerDownRight,
  FilePen,
  FolderOpen,
  GitBranch,
  Info,
  Library,
  Lock,
  RefreshCw,
  ScrollText,
  Search,
  Sparkles,
  Unlink,
  User,
} from "lucide-react";
import { toast } from "sonner";
import { ClientLogo } from "@/components/ClientLogo";
import { ConfirmDialog } from "@/components/ConfirmDialog";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { EmptyState } from "@/components/ui/empty-state";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { fixSkillIssue, openSkillPath, setSkillState, skillsInventory } from "@/lib/api";
import { SKILLS_REFRESH_EVENT } from "@/lib/shortcuts";
import { translate, useI18n } from "@/lib/i18n";
import { toastError } from "@/lib/toast";
import type {
  RuleFileEntry,
  SkillAgent,
  SkillEntry,
  SkillIssue,
  SkillLoadState,
  SkillSource,
  SkillsInventory,
  SkillWanted,
} from "@/lib/types";
import { cn, fmtBytes, fmtTs } from "@/lib/utils";

type Tab = "skills" | "rules" | "issues";

/** Claude Code keeps this many description characters (mirrors DESCRIPTION_CAP in skills.rs). */
const DESCRIPTION_CAP = 1536;
const PROJECT_KEY = "toolport-skills-project";
const TAB_ORDER: Tab[] = ["skills", "rules", "issues"];
const COLUMN = 28;
const SOURCE_ORDER: SkillSource[] = [
  "yours",
  "account",
  "package",
  "built-in",
  "library",
  "parked",
  "broken",
];

function readProject(): string | null {
  try {
    return localStorage.getItem(PROJECT_KEY) || null;
  } catch {
    return null;
  }
}

function writeProject(project: string | null) {
  try {
    if (project) localStorage.setItem(PROJECT_KEY, project);
    else localStorage.removeItem(PROJECT_KEY);
  } catch {
    // Storage can be unavailable; the choice then lasts for this session only.
  }
}

function folderName(path: string): string {
  const parts = path.split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] ?? path;
}

function errorText(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function useSourceLabels() {
  const { t } = useI18n();
  return (source: SkillSource): { label: string; icon: ReactNode } => {
    switch (source) {
      case "yours":
        return { label: t("Yours"), icon: <User /> };
      case "account":
        return { label: t("From your claude.ai account"), icon: <Cloud /> };
      case "package":
        return { label: t("System package"), icon: <Box /> };
      case "built-in":
        return { label: t("Built into a client"), icon: <Lock /> };
      case "library":
        return { label: t("Client library"), icon: <Library /> };
      case "parked":
        return { label: t("Parked"), icon: <Ban /> };
      case "broken":
        return { label: t("Broken links"), icon: <Unlink /> };
    }
  };
}

function useStateLabel() {
  const { t } = useI18n();
  return (state: SkillLoadState | undefined): string => {
    switch (state) {
      case "on":
        return t("On");
      case "name-only":
        return t("Name only");
      case "off":
        return t("Off");
      case "shadowed":
        return t("Another copy wins");
      case "broken":
        return t("Broken");
      default:
        return t("Not loaded");
    }
  };
}

/** One cell in the agent matrix. Shape and color both carry the state. */
function StateDot({
  state,
  label,
}: {
  state: SkillLoadState | undefined;
  label: string;
}) {
  const shape = (() => {
    switch (state) {
      case "on":
        return <span className="block size-3 rounded-full bg-success" />;
      case "name-only":
        return (
          <span className="block size-3 rounded-full border-2 border-owned bg-[linear-gradient(90deg,var(--color-owned)_50%,transparent_50%)]" />
        );
      case "off":
        return <Ban className="size-3.5 text-muted-foreground" />;
      case "shadowed":
        return <span className="block size-3 rounded-full border-2 border-warning" />;
      case "broken":
        return (
          <span className="flex size-3.5 items-center justify-center rounded-full border-2 border-destructive">
            <span className="block size-1 rounded-full bg-destructive" />
          </span>
        );
      default:
        return <span className="block size-1 rounded-full bg-muted-foreground/40" />;
    }
  })();
  return (
    <span
      role="img"
      aria-label={label}
      title={label}
      className="flex h-5 items-center justify-center"
      style={{ width: COLUMN }}
    >
      {shape}
    </span>
  );
}

function Legend() {
  const { t } = useI18n();
  const items: [SkillLoadState | undefined, string][] = [
    ["on", t("On")],
    ["name-only", t("Name only")],
    ["off", t("Off")],
    ["shadowed", t("Another copy wins")],
    ["broken", t("Broken")],
    [undefined, t("Not loaded")],
  ];
  return (
    <div className="flex flex-wrap items-center gap-x-4 gap-y-1 border-t pt-3 text-xs text-muted-foreground">
      {items.map(([state, label]) => (
        <span key={label} className="flex items-center">
          <span aria-hidden="true">
            <StateDot state={state} label="" />
          </span>
          {label}
        </span>
      ))}
    </div>
  );
}

function AgentHeader({
  agents,
  focus,
  onFocus,
  label,
}: {
  agents: SkillAgent[];
  focus: string | null;
  onFocus: (id: string | null) => void;
  label: string;
}) {
  const { t } = useI18n();
  return (
    <div className="flex items-end justify-between gap-3 border-b px-2 pb-2">
      <span className="text-2xs font-semibold tracking-wider text-muted-foreground uppercase">
        {label}
      </span>
      <span className="flex">
        {agents.map((agent) => (
          <Tooltip key={agent.id}>
            <TooltipTrigger asChild>
              <button
                type="button"
                aria-pressed={focus === agent.id}
                aria-label={t("Show only what {client} loads", { client: agent.name })}
                onClick={() => onFocus(focus === agent.id ? null : agent.id)}
                className={cn(
                  "relative flex h-7 items-center justify-center rounded-md transition-colors hover:bg-accent/60 focus-visible:ring-1 focus-visible:ring-ring focus-visible:outline-none",
                  focus === agent.id && "bg-accent ring-1 ring-primary/60",
                  focus && focus !== agent.id && "opacity-40",
                )}
                style={{ width: COLUMN }}
              >
                <ClientLogo id={agent.id} name={agent.name} size={18} />
                {!agent.verified && (
                  <span className="absolute -top-0.5 -right-0.5 size-1.5 rounded-full bg-info" />
                )}
              </button>
            </TooltipTrigger>
            <TooltipContent>
              {agent.verified
                ? agent.name
                : t("{client}: folders from its docs, not checked on this machine", {
                    client: agent.name,
                  })}
            </TooltipContent>
          </Tooltip>
        ))}
      </span>
    </div>
  );
}

function GroupHeader({
  open,
  onToggle,
  icon,
  label,
  count,
  note,
}: {
  open: boolean;
  onToggle: () => void;
  icon: ReactNode;
  label: string;
  count: number;
  note?: string;
}) {
  return (
    <button
      type="button"
      onClick={onToggle}
      aria-expanded={open}
      className="flex w-full items-center gap-2 rounded-md px-2 pt-4 pb-1.5 text-left text-sm focus-visible:ring-1 focus-visible:ring-ring focus-visible:outline-none"
    >
      {open ? (
        <ChevronDown className="size-3.5 text-muted-foreground" />
      ) : (
        <ChevronRight className="size-3.5 text-muted-foreground" />
      )}
      <span className="text-muted-foreground [&_svg]:size-3.5">{icon}</span>
      <span className="font-semibold">{label}</span>
      <span className="rounded-full bg-muted px-1.5 text-2xs text-muted-foreground tabular-nums">
        {count}
      </span>
      {note && <span className="truncate text-xs text-muted-foreground">{note}</span>}
    </button>
  );
}

function FileActions({
  path,
  folder,
  openLabel,
  project,
}: {
  path: string;
  folder: string;
  openLabel: string;
  project: string | null;
}) {
  const { t } = useI18n();
  const run = (target: string, reveal: boolean) =>
    openSkillPath(target, reveal, project).catch((error) =>
      toastError(t("Couldn't open {path}", { path: target }), {
        description: errorText(error),
      }),
    );
  return (
    <div className="flex flex-wrap gap-2">
      <Button variant="outline" size="sm" onClick={() => void run(path, false)}>
        <FilePen /> {openLabel}
      </Button>
      <Button variant="outline" size="sm" onClick={() => void run(folder, true)}>
        <FolderOpen /> {t("Reveal")}
      </Button>
      <Tooltip>
        <TooltipTrigger asChild>
          <Button
            variant="outline"
            size="icon-sm"
            aria-label={t("Copy path")}
            onClick={() =>
              void navigator.clipboard.writeText(path).then(
                () => toast.success(t("Path copied")),
                (error) =>
                  toastError(t("Couldn't copy the path"), {
                    description: errorText(error),
                  }),
              )
            }
          >
            <Copy />
          </Button>
        </TooltipTrigger>
        <TooltipContent>{t("Copy path")}</TooltipContent>
      </Tooltip>
    </div>
  );
}

function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="grid grid-cols-[88px_minmax(0,1fr)] gap-3 py-1 text-sm">
      <span className="text-muted-foreground">{label}</span>
      <span className="min-w-0 break-words">{children}</span>
    </div>
  );
}

function PanelHeading({ children, aside }: { children: ReactNode; aside?: ReactNode }) {
  return (
    <div className="mt-5 mb-2 flex items-baseline justify-between gap-2">
      <h3 className="text-2xs font-semibold tracking-wider text-muted-foreground uppercase">
        {children}
      </h3>
      {aside && <span className="text-xs text-muted-foreground">{aside}</span>}
    </div>
  );
}

function StateControl({
  agent,
  skill,
  busy,
  onChange,
}: {
  agent: SkillAgent;
  skill: SkillEntry;
  busy: boolean;
  onChange: (wanted: SkillWanted) => void;
}) {
  const { t } = useI18n();
  const stateLabel = useStateLabel();
  const current = skill.states[agent.id]?.state;
  if (!current)
    return <span className="text-xs text-muted-foreground">{t("Not loaded")}</span>;
  if (current === "broken") return <Badge variant="destructive">{t("Broken")}</Badge>;
  if (current === "shadowed")
    return <Badge variant="warning">{t("Another copy wins")}</Badge>;
  if (!agent.canToggle) {
    return (
      <Tooltip>
        <TooltipTrigger asChild>
          <span>
            <Badge variant={current === "off" ? "outline" : "success"}>
              {stateLabel(current)}
            </Badge>
          </span>
        </TooltipTrigger>
        <TooltipContent className="max-w-64">{agent.switchNote}</TooltipContent>
      </Tooltip>
    );
  }
  if (agent.id === "claude-code") {
    const options: [SkillWanted, string][] = [
      ["on", t("On")],
      ["name-only", t("Name only")],
      ["off", t("Off")],
    ];
    // Radio group keyboard pattern: one tab stop, arrows move and select.
    const onKeyDown = (event: ReactKeyboardEvent<HTMLButtonElement>) => {
      const step = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[event.key];
      if (step === undefined || busy) return;
      event.preventDefault();
      const index = options.findIndex(([value]) => value === current);
      const next = options[(index + step + options.length) % options.length][0];
      onChange(next);
      event.currentTarget.parentElement
        ?.querySelector<HTMLButtonElement>(`[data-value="${next}"]`)
        ?.focus();
    };
    return (
      <div
        role="radiogroup"
        aria-label={t("{skill} in {client}", { skill: skill.name, client: agent.name })}
        className="flex rounded-md border p-0.5"
      >
        {options.map(([value, label]) => (
          <button
            key={value}
            type="button"
            role="radio"
            data-value={value}
            aria-checked={current === value}
            tabIndex={current === value ? 0 : -1}
            onKeyDown={onKeyDown}
            disabled={busy}
            onClick={() => current !== value && onChange(value)}
            className={cn(
              "rounded px-2 py-0.5 text-xs transition-colors focus-visible:ring-1 focus-visible:ring-ring focus-visible:outline-none disabled:opacity-50",
              current === value
                ? "bg-accent font-medium text-foreground"
                : "text-muted-foreground hover:text-foreground",
            )}
          >
            {label}
          </button>
        ))}
      </div>
    );
  }
  return (
    <Switch
      checked={current !== "off"}
      disabled={busy}
      aria-label={t("Load {skill} in {client}", {
        skill: skill.name,
        client: agent.name,
      })}
      onCheckedChange={(checked) => onChange(checked ? "on" : "off")}
    />
  );
}

function SkillDetail({
  skill,
  agents,
  project,
  busy,
  onChange,
}: {
  skill: SkillEntry;
  agents: SkillAgent[];
  project: string | null;
  busy: boolean;
  onChange: (agent: SkillAgent, wanted: SkillWanted) => void;
}) {
  const { t } = useI18n();
  const source = useSourceLabels()(skill.source);
  const loaded = agents.filter((a) => {
    const state = skill.states[a.id]?.state;
    return state === "on" || state === "name-only";
  }).length;
  const ratio = Math.min(1, skill.descriptionChars / DESCRIPTION_CAP);
  return (
    <div>
      <div className="flex items-start gap-3">
        <span className="flex size-10 shrink-0 items-center justify-center rounded-lg bg-primary/12 text-primary">
          <Sparkles className="size-5" />
        </span>
        <div className="min-w-0">
          <h2 className="truncate text-lg font-semibold">{skill.name}</h2>
          <div className="mt-1 flex flex-wrap gap-1.5">
            <Badge variant={skill.source === "broken" ? "destructive" : "info"}>
              {source.icon} {source.label}
            </Badge>
            {skill.gitRepo && (
              <Badge variant="secondary">
                <GitBranch /> {t("git repo: {name}", { name: skill.gitRepo })}
              </Badge>
            )}
            {!skill.editable && skill.source !== "broken" && (
              <Badge variant="outline">{t("Read only")}</Badge>
            )}
          </div>
        </div>
      </div>
      <div className="mt-4">
        <FileActions
          path={skill.path}
          folder={skill.folder}
          openLabel={t("Open SKILL.md")}
          project={project}
        />
      </div>

      {skill.source !== "broken" && (
        <>
          <PanelHeading>{t("Description")}</PanelHeading>
          <p className="line-clamp-6 text-sm leading-relaxed whitespace-pre-line">
            {skill.description || t("No description")}
          </p>
          <div
            className="mt-2 h-1.5 overflow-hidden rounded-full bg-muted"
            role="meter"
            aria-valuemin={0}
            aria-valuemax={DESCRIPTION_CAP}
            aria-valuenow={skill.descriptionChars}
            aria-label={t("Description length")}
          >
            <div
              className={cn(
                "h-full rounded-full",
                skill.descriptionChars > DESCRIPTION_CAP ? "bg-warning" : "bg-success",
              )}
              style={{ width: `${ratio * 100}%` }}
            />
          </div>
          <div className="mt-1 flex justify-between text-xs text-muted-foreground">
            <span>
              {t("{count} of {cap} characters", {
                count: skill.descriptionChars.toLocaleString(),
                cap: DESCRIPTION_CAP.toLocaleString(),
              })}
            </span>
            <span>{t("Claude Code's limit")}</span>
          </div>
        </>
      )}

      <PanelHeading>{t("File")}</PanelHeading>
      <Field label={t("Path")}>
        <span className="font-mono text-xs">{skill.path}</span>
      </Field>
      {skill.links.filter((l) => l !== skill.path).length > 0 && (
        <Field label={t("Reached by")}>
          <span className="flex flex-col gap-0.5 font-mono text-xs">
            {skill.links
              .filter((l) => l !== skill.path)
              .map((link) => (
                <span key={link}>{link}</span>
              ))}
          </span>
        </Field>
      )}
      <Field label={t("Source")}>{skill.sourceNote}</Field>
      {skill.source !== "broken" && (
        <Field label={t("Size")}>
          {skill.lines === 1
            ? t("{size} · 1 line", { size: fmtBytes(skill.sizeBytes) })
            : t("{size} · {lines} lines", {
                size: fmtBytes(skill.sizeBytes),
                lines: skill.lines.toLocaleString(),
              })}
        </Field>
      )}
      {skill.modifiedSecs !== null && (
        <Field label={t("Changed")}>{fmtTs(skill.modifiedSecs * 1000, "date")}</Field>
      )}

      <PanelHeading
        aside={t("{count} of {total} load it", { count: loaded, total: agents.length })}
      >
        {t("Agents")}
      </PanelHeading>
      <ul className="divide-y overflow-hidden rounded-lg border">
        {agents.map((agent) => {
          const state = skill.states[agent.id];
          return (
            <li
              key={agent.id}
              className={cn(
                "flex items-center gap-3 px-3 py-2.5",
                !state && "opacity-60",
              )}
            >
              <ClientLogo id={agent.id} name={agent.name} size={24} />
              <span className="min-w-0 flex-1">
                <span className="flex items-center gap-1.5 text-sm font-medium">
                  {agent.name}
                  {!agent.verified && (
                    <Badge variant="outline" className="h-4 px-1.5 text-2xs">
                      {t("from docs")}
                    </Badge>
                  )}
                </span>
                <span className="block text-xs break-words text-muted-foreground">
                  {state?.via ?? t("Not in any folder it reads")}
                </span>
              </span>
              <StateControl
                agent={agent}
                skill={skill}
                busy={busy}
                onChange={(wanted) => onChange(agent, wanted)}
              />
            </li>
          );
        })}
      </ul>
      {skill.preview && (
        <>
          <PanelHeading>{t("Preview")}</PanelHeading>
          <pre className="max-h-64 overflow-auto rounded-lg border bg-muted/40 p-3 font-mono text-2xs leading-relaxed whitespace-pre-wrap">
            {skill.preview}
          </pre>
        </>
      )}
    </div>
  );
}

/** Why an agent doesn't load this rules file: the files it reads instead. */
function otherRulesNote(agent: SkillAgent, rule: RuleFileEntry): string {
  const others = agent.ruleLocations.filter((path) => path !== rule.path);
  return others.length > 0
    ? translate("Reads {paths}, which doesn't lead here", { paths: others.join(", ") })
    : translate("No global rules file");
}

function RuleDetail({
  rule,
  agents,
  project,
}: {
  rule: RuleFileEntry;
  agents: SkillAgent[];
  project: string | null;
}) {
  const { t } = useI18n();
  const loaded = agents.filter((a) => rule.agents[a.id]).length;
  const folder = rule.path.replace(/[\\/][^\\/]*$/, "");
  return (
    <div>
      <div className="flex items-start gap-3">
        <span className="flex size-10 shrink-0 items-center justify-center rounded-lg bg-primary/12 text-primary">
          <ScrollText className="size-5" />
        </span>
        <div className="min-w-0">
          <h2 className="truncate text-lg font-semibold">{rule.name}</h2>
          <div className="mt-1 flex gap-1.5">
            <Badge variant="secondary">
              {rule.scope === "project" ? t("Project") : t("Global")}
            </Badge>
          </div>
        </div>
      </div>
      <div className="mt-4">
        <FileActions
          path={rule.path}
          folder={folder}
          openLabel={t("Open file")}
          project={project}
        />
      </div>
      <PanelHeading>{t("File")}</PanelHeading>
      <Field label={t("Path")}>
        <span className="font-mono text-xs">{rule.path}</span>
      </Field>
      <Field label={t("Size")}>{fmtBytes(rule.sizeBytes)}</Field>

      <PanelHeading
        aside={t("{count} of {total} load it", { count: loaded, total: agents.length })}
      >
        {t("Agents")}
      </PanelHeading>
      <ul className="divide-y overflow-hidden rounded-lg border">
        {agents.map((agent) => {
          const chain = rule.agents[agent.id];
          return (
            <li
              key={agent.id}
              className={cn("flex gap-3 px-3 py-2.5", !chain && "opacity-60")}
            >
              <ClientLogo id={agent.id} name={agent.name} size={24} />
              <span className="min-w-0 flex-1">
                <span className="flex items-center justify-between gap-2">
                  <span className="text-sm font-medium">{agent.name}</span>
                  {chain ? (
                    <Badge variant="success">{t("Loaded")}</Badge>
                  ) : (
                    <span className="text-xs text-muted-foreground">
                      {t("Not loaded")}
                    </span>
                  )}
                </span>
                {chain ? (
                  <span className="mt-0.5 flex flex-col gap-0.5 text-xs text-muted-foreground">
                    {chain.map((step, i) => (
                      <span
                        key={`${step.kind}:${step.path}:${i}`}
                        className={cn(
                          "flex items-start gap-1",
                          step.kind === "skipped-import" && "text-warning",
                        )}
                      >
                        {step.kind !== "file" && (
                          <CornerDownRight className="mt-0.5 size-3 shrink-0" />
                        )}
                        <span className="min-w-0 break-words">
                          {step.kind === "file" ? (
                            <span className="font-mono">{step.path}</span>
                          ) : step.kind === "link" ? (
                            t("links to {path}", { path: step.path })
                          ) : step.kind === "import" ? (
                            t("@imports {path}", { path: step.path })
                          ) : (
                            t("skips @import {path}", { path: step.path })
                          )}
                        </span>
                      </span>
                    ))}
                  </span>
                ) : (
                  <span className="block text-xs text-muted-foreground">
                    {agent.rulesNote ?? otherRulesNote(agent, rule)}
                  </span>
                )}
              </span>
            </li>
          );
        })}
      </ul>
      {rule.preview && (
        <>
          <PanelHeading>{t("Preview")}</PanelHeading>
          <pre className="max-h-72 overflow-auto rounded-lg border bg-muted/40 p-3 font-mono text-2xs leading-relaxed whitespace-pre-wrap">
            {rule.preview}
          </pre>
        </>
      )}
    </div>
  );
}

function IssueCard({
  issue,
  agents,
  busy,
  onFix,
}: {
  issue: SkillIssue;
  agents: SkillAgent[];
  busy: boolean;
  onFix: () => Promise<void>;
}) {
  const { t } = useI18n();
  const named = issue.agents
    .map((id) => agents.find((a) => a.id === id))
    .filter((a): a is SkillAgent => Boolean(a));
  const icon =
    issue.level === "error" ? (
      <Unlink className="size-4 text-destructive" />
    ) : issue.level === "warn" ? (
      <AlertTriangle className="size-4 text-warning" />
    ) : (
      <Info className="size-4 text-info" />
    );
  return (
    <article className="flex gap-3 rounded-xl border bg-card p-4">
      <span
        className={cn(
          "flex size-8 shrink-0 items-center justify-center rounded-lg",
          issue.level === "error"
            ? "bg-destructive/12"
            : issue.level === "warn"
              ? "bg-warning/14"
              : "bg-info/12",
        )}
      >
        {icon}
      </span>
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-start justify-between gap-2">
          <h3 className="min-w-0 flex-1 text-sm font-semibold">{issue.title}</h3>
          {issue.fixLabel && (
            <ConfirmDialog
              trigger={
                <Button size="sm" disabled={busy}>
                  {issue.fixLabel}
                </Button>
              }
              title={issue.fixLabel}
              description={t(
                "Toolport backs up any config file before it changes it, and never deletes a skill folder.",
              )}
              confirmLabel={issue.fixLabel}
              onConfirm={onFix}
            />
          )}
        </div>
        <p className="mt-1 text-sm text-muted-foreground">{issue.detail}</p>
        {(named.length > 0 || issue.skillNames.length > 0) && (
          <div className="mt-2.5 flex flex-wrap items-center gap-1.5">
            {named.map((agent) => (
              <span
                key={agent.id}
                title={agent.name}
                className="flex size-6 items-center justify-center rounded-md bg-muted"
              >
                <ClientLogo id={agent.id} name={agent.name} size={14} />
              </span>
            ))}
            {issue.skillNames.slice(0, 12).map((name) => (
              <span
                key={name}
                className="flex h-6 items-center gap-1 rounded-md bg-muted px-2 text-xs"
              >
                <Sparkles className="size-3 text-muted-foreground" />
                {name}
              </span>
            ))}
            {issue.skillNames.length > 12 && (
              <span className="text-xs text-muted-foreground">
                {t("+{count} more", { count: issue.skillNames.length - 12 })}
              </span>
            )}
          </div>
        )}
      </div>
    </article>
  );
}

function AgentSummary({ inv }: { inv: SkillsInventory }) {
  const { t } = useI18n();
  return (
    <div>
      <PanelHeading>{t("By agent")}</PanelHeading>
      <ul className="divide-y overflow-hidden rounded-lg border">
        {inv.agents.map((agent) => {
          let on = 0;
          let nameOnly = 0;
          let off = 0;
          let broken = 0;
          for (const skill of inv.skills) {
            const state = skill.states[agent.id]?.state;
            if (state === "on") on++;
            else if (state === "name-only") nameOnly++;
            else if (state === "off") off++;
            else if (state === "broken") broken++;
          }
          const rules = inv.rules.filter((r) => r.agents[agent.id]).length;
          const parts = agent.supportsSkills
            ? [
                t("{count} on", { count: on }),
                ...(nameOnly ? [t("{count} name only", { count: nameOnly })] : []),
                ...(off ? [t("{count} off", { count: off })] : []),
              ]
            : [t("No file-based skills")];
          parts.push(
            rules === 0
              ? t("no rules files")
              : rules === 1
                ? t("1 rules file")
                : t("{count} rules files", { count: rules }),
          );
          return (
            <li key={agent.id} className="flex items-center gap-3 px-3 py-2.5">
              <ClientLogo id={agent.id} name={agent.name} size={24} />
              <span className="min-w-0 flex-1">
                <span className="block text-sm font-medium">{agent.name}</span>
                <span className="block text-xs text-muted-foreground">
                  {parts.join(" · ")}
                </span>
              </span>
              {broken > 0 ? (
                <Badge variant="destructive">
                  {t("{count} broken", { count: broken })}
                </Badge>
              ) : !agent.supportsSkills ? null : (
                <Tooltip>
                  <TooltipTrigger asChild>
                    <span>
                      <Badge variant="outline">
                        {agent.canToggle ? t("Own switch") : t("No switch")}
                      </Badge>
                    </span>
                  </TooltipTrigger>
                  <TooltipContent className="max-w-64">{agent.switchNote}</TooltipContent>
                </Tooltip>
              )}
            </li>
          );
        })}
      </ul>
      <p className="mt-2 text-xs text-muted-foreground">
        {t(
          '"Own switch" means the agent has its own per-skill setting that Toolport can write. Toolport never moves or deletes your skill folders to turn a skill off.',
        )}
      </p>
    </div>
  );
}

export function SkillsView() {
  const { t, locale } = useI18n();
  const sourceLabel = useSourceLabels();
  const stateLabel = useStateLabel();
  const [inv, setInv] = useState<SkillsInventory | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [project, setProject] = useState<string | null>(readProject);
  const [tab, setTab] = useState<Tab>("skills");
  const [query, setQuery] = useState("");
  const [focus, setFocus] = useState<string | null>(null);
  const [selectedSkill, setSelectedSkill] = useState<string | null>(null);
  const [selectedRule, setSelectedRule] = useState<string | null>(null);
  const [collapsed, setCollapsed] = useState<Set<string>>(
    () => new Set(["built-in", "parked"]),
  );

  // Latest request wins, so a slow scan of an old project can't overwrite a newer one.
  const request = useRef(0);
  // The project the user has picked right now; a write that returns for another one is stale.
  const projectRef = useRef(project);
  const fetchInventory = useCallback(() => {
    const id = ++request.current;
    return skillsInventory(project)
      .then(
        (next) => {
          if (id !== request.current) return;
          setInv(next);
          setError(null);
          // The backend drops a saved folder that no longer exists; follow it.
          if (project && next.project === null) {
            writeProject(null);
            projectRef.current = null;
            setProject(null);
            toast.info(t("That project folder is gone. Showing global only."));
          }
        },
        (e) => {
          if (id === request.current) setError(errorText(e));
        },
      )
      .finally(() => {
        if (id === request.current) setLoading(false);
      });
  }, [project, t]);

  const load = useCallback(() => {
    setLoading(true);
    return fetchInventory();
  }, [fetchInventory]);

  // Reload on language change too: notes and issue text come translated from the backend.
  useEffect(() => {
    void fetchInventory();
  }, [fetchInventory, locale]);

  // The app's Refresh shortcut rescans here instead of reloading servers.
  useEffect(() => {
    const onRefresh = () => void load();
    window.addEventListener(SKILLS_REFRESH_EVENT, onRefresh);
    return () => window.removeEventListener(SKILLS_REFRESH_EVENT, onRefresh);
  }, [load]);

  /** Show a write's result unless the user has moved to another project meanwhile. */
  function applyWrite(next: SkillsInventory, forProject: string | null) {
    if (projectRef.current !== forProject) return;
    request.current++;
    setInv(next);
    setError(null);
    setLoading(false);
  }

  const skillAgents = useMemo(
    () => inv?.agents.filter((a) => a.supportsSkills) ?? [],
    [inv],
  );
  const needle = query.trim().toLowerCase();

  const visibleSkills = useMemo(() => {
    if (!inv) return [];
    return inv.skills.filter(
      (s) =>
        (!focus || s.states[focus]) &&
        (!needle ||
          s.name.toLowerCase().includes(needle) ||
          s.description.toLowerCase().includes(needle) ||
          s.path.toLowerCase().includes(needle)),
    );
  }, [inv, focus, needle]);

  const visibleRules = useMemo(() => {
    if (!inv) return [];
    return inv.rules.filter(
      (r) =>
        (!focus || r.agents[focus]) &&
        (!needle ||
          r.name.toLowerCase().includes(needle) ||
          r.path.toLowerCase().includes(needle) ||
          r.preview.toLowerCase().includes(needle)),
    );
  }, [inv, focus, needle]);

  const skill =
    inv?.skills.find((s) => s.key === selectedSkill) ?? visibleSkills[0] ?? null;
  const rule = inv?.rules.find((r) => r.key === selectedRule) ?? visibleRules[0] ?? null;

  function switchProject(next: string | null) {
    if (next === projectRef.current) return;
    writeProject(next);
    projectRef.current = next;
    setLoading(true);
    setProject(next);
  }

  async function chooseProject() {
    try {
      const picked = await openFolderDialog({ directory: true, multiple: false });
      if (typeof picked === "string") switchProject(picked);
    } catch (e) {
      toastError(t("Couldn't open the folder picker"), { description: errorText(e) });
    }
  }

  function clearProject() {
    switchProject(null);
  }

  async function change(agent: SkillAgent, target: SkillEntry, wanted: SkillWanted) {
    setBusy(true);
    const forProject = project;
    try {
      applyWrite(
        await setSkillState(agent.id, target.key, wanted, forProject),
        forProject,
      );
      toast.success(
        wanted === "off"
          ? t("Turned off {skill} in {client}", {
              skill: target.name,
              client: agent.name,
            })
          : wanted === "name-only"
            ? t("{client} now lists {skill} by name only", {
                skill: target.name,
                client: agent.name,
              })
            : t("Turned on {skill} in {client}", {
                skill: target.name,
                client: agent.name,
              }),
      );
    } catch (e) {
      toastError(
        t("Couldn't change {skill} in {client}", {
          skill: target.name,
          client: agent.name,
        }),
        {
          description: errorText(e),
        },
      );
      // Show what is on disk now, whatever the write managed to do.
      void load();
    } finally {
      setBusy(false);
    }
  }

  async function fix(issue: SkillIssue) {
    setBusy(true);
    const forProject = project;
    try {
      applyWrite(await fixSkillIssue(issue.id, forProject), forProject);
      toast.success(t("Fixed: {title}", { title: issue.title }));
    } catch (e) {
      toastError(t("Couldn't fix this"), { description: errorText(e) });
      // Part of a fix may have landed before the error; rescan so the page shows it.
      void load();
      throw e;
    } finally {
      setBusy(false);
    }
  }

  // Tabs follow the APG pattern: one tab stop, arrows and Home/End move between tabs.
  const tabRefs = useRef<Partial<Record<Tab, HTMLButtonElement | null>>>({});
  function selectTab(next: Tab) {
    // An agent filter from one tab can name an agent the other tab has no column for.
    setFocus(null);
    setTab(next);
  }
  function onTabKey(event: ReactKeyboardEvent<HTMLButtonElement>) {
    const index = TAB_ORDER.indexOf(tab);
    const target = {
      ArrowRight: (index + 1) % TAB_ORDER.length,
      ArrowLeft: (index - 1 + TAB_ORDER.length) % TAB_ORDER.length,
      Home: 0,
      End: TAB_ORDER.length - 1,
    }[event.key];
    if (target === undefined) return;
    event.preventDefault();
    const next = TAB_ORDER[target];
    selectTab(next);
    tabRefs.current[next]?.focus();
  }

  const tabs: [Tab, string, number, boolean][] = [
    [
      "skills",
      t("Skills"),
      inv?.skills.filter((s) => s.source !== "broken").length ?? 0,
      false,
    ],
    ["rules", t("Rules"), inv?.rules.length ?? 0, false],
    ["issues", t("Needs a look"), inv?.issues.length ?? 0, (inv?.issues.length ?? 0) > 0],
  ];

  const toolbar = (
    <div className="flex flex-wrap items-center gap-2">
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button variant="outline" size="sm" className="max-w-72">
            <FolderOpen />
            <span className="truncate">
              {project
                ? t("Global + {folder}", { folder: folderName(project) })
                : t("Global only")}
            </span>
            <ChevronDown className="text-muted-foreground" />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="w-64">
          <DropdownMenuItem onSelect={clearProject}>{t("Global only")}</DropdownMenuItem>
          <DropdownMenuItem onSelect={() => void chooseProject()}>
            {t("Add a project folder…")}
          </DropdownMenuItem>
          {project && (
            <>
              <DropdownMenuSeparator />
              <div className="px-2 py-1.5 font-mono text-2xs break-all text-muted-foreground">
                {project}
              </div>
            </>
          )}
        </DropdownMenuContent>
      </DropdownMenu>
      {tab !== "issues" && (
        <div className="relative">
          <Search className="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-muted-foreground" />
          <Input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={tab === "skills" ? t("Search skills") : t("Search rules")}
            aria-label={tab === "skills" ? t("Search skills") : t("Search rules")}
            className="h-8 w-52 pl-8"
          />
        </div>
      )}
      <Tooltip>
        <TooltipTrigger asChild>
          <Button
            variant="outline"
            size="icon-sm"
            onClick={() => void load()}
            disabled={loading}
            aria-label={t("Scan again")}
          >
            <RefreshCw className={cn(loading && "animate-spin")} />
          </Button>
        </TooltipTrigger>
        <TooltipContent>{t("Scan again")}</TooltipContent>
      </Tooltip>
    </div>
  );

  const header = (
    <div className="flex flex-wrap items-end justify-between gap-3 border-b">
      <div role="tablist" aria-label={t("Skills and rules")} className="flex gap-5">
        {tabs.map(([id, label, count, warn]) => (
          <button
            key={id}
            id={`skills-tab-${id}`}
            ref={(node) => {
              tabRefs.current[id] = node;
            }}
            type="button"
            role="tab"
            aria-selected={tab === id}
            aria-controls={`skills-panel-${id}`}
            tabIndex={tab === id ? 0 : -1}
            onKeyDown={onTabKey}
            onClick={() => selectTab(id)}
            className={cn(
              "-mb-px flex items-center gap-2 border-b-2 pb-2.5 text-sm transition-colors focus-visible:ring-1 focus-visible:ring-ring focus-visible:outline-none",
              tab === id
                ? "border-primary font-medium text-foreground"
                : "border-transparent text-muted-foreground hover:text-foreground",
            )}
          >
            {label}
            <span
              className={cn(
                "rounded-full px-1.5 text-2xs tabular-nums",
                warn ? "bg-warning/16 text-warning" : "bg-muted text-muted-foreground",
              )}
            >
              {count}
            </span>
          </button>
        ))}
      </div>
      <div className="pb-2">{toolbar}</div>
    </div>
  );

  if (loading && !inv) {
    return (
      <div className="flex flex-col gap-2">
        {Array.from({ length: 8 }).map((_, i) => (
          <Skeleton key={i} className="h-12 w-full rounded-lg" />
        ))}
      </div>
    );
  }
  if (error && !inv) {
    return (
      <EmptyState
        icon={<AlertTriangle />}
        title={t("Couldn't read skills and rules")}
        description={error}
        action={
          <Button variant="outline" size="sm" onClick={() => void load()}>
            {t("Try again")}
          </Button>
        }
      />
    );
  }
  if (!inv) return null;

  if (inv.agents.length === 0) {
    return (
      <EmptyState
        icon={<Sparkles />}
        title={t("No agents found")}
        description={t(
          "Install an AI coding agent such as Claude Code, Codex or Cursor, then scan again.",
        )}
        action={
          <Button variant="outline" size="sm" onClick={() => void load()}>
            {t("Scan again")}
          </Button>
        }
      />
    );
  }

  const banner =
    inv.issues.length > 0 && tab !== "issues" ? (
      <div className="mb-4 flex items-center gap-3 rounded-xl border border-l-4 border-l-warning bg-card px-4 py-3">
        <span className="flex size-8 shrink-0 items-center justify-center rounded-lg bg-warning/14">
          <AlertTriangle className="size-4 text-warning" />
        </span>
        <span className="min-w-0 flex-1">
          <span className="block text-sm font-semibold">
            {inv.issues.length === 1
              ? t("1 thing needs a look")
              : t("{count} things need a look", { count: inv.issues.length })}
          </span>
          <span className="block truncate text-sm text-muted-foreground">
            {inv.issues[0].title}
          </span>
        </span>
        <Button variant="outline" size="sm" onClick={() => selectTab("issues")}>
          {t("Review")}
        </Button>
      </div>
    ) : null;

  let body: ReactNode;
  if (tab === "skills") {
    const groups = SOURCE_ORDER.map((source) => ({
      source,
      items: visibleSkills.filter((s) => s.source === source),
    })).filter((g) => g.items.length > 0);
    body = (
      <div className="grid gap-6 lg:grid-cols-[minmax(0,1fr)_minmax(320px,380px)]">
        <section aria-label={t("Skills")} className="min-w-0">
          {banner}
          <AgentHeader
            agents={skillAgents}
            focus={focus}
            onFocus={setFocus}
            label={t("Skill")}
          />
          {groups.length === 0 ? (
            <p className="py-10 text-center text-sm text-muted-foreground">
              {needle || focus
                ? t("No skills match.")
                : t("No skills found in any agent's folders.")}
            </p>
          ) : (
            groups.map(({ source, items }) => {
              const { label, icon } = sourceLabel(source);
              const open = !collapsed.has(source) || Boolean(needle);
              return (
                <div key={source}>
                  <GroupHeader
                    open={open}
                    onToggle={() =>
                      setCollapsed((prev) => {
                        const next = new Set(prev);
                        if (next.has(source)) next.delete(source);
                        else next.add(source);
                        return next;
                      })
                    }
                    icon={icon}
                    label={label}
                    count={items.length}
                  />
                  {open &&
                    items.map((s) => (
                      <button
                        key={s.key}
                        type="button"
                        onClick={() => setSelectedSkill(s.key)}
                        aria-current={skill?.key === s.key ? "true" : undefined}
                        className={cn(
                          "flex w-full items-center gap-3 rounded-lg px-2 py-2 text-left transition-colors hover:bg-accent/40 focus-visible:ring-1 focus-visible:ring-ring focus-visible:outline-none",
                          skill?.key === s.key && "bg-accent/70 hover:bg-accent/70",
                        )}
                      >
                        <span className="min-w-0 flex-1">
                          <span className="flex items-center gap-1.5 text-sm font-medium">
                            <span className="truncate">{s.name}</span>
                            {s.descriptionChars > DESCRIPTION_CAP && (
                              <AlertTriangle
                                className="size-3.5 shrink-0 text-warning"
                                aria-label={t("Description is cut off in Claude Code")}
                              />
                            )}
                          </span>
                          <span className="block truncate text-xs text-muted-foreground">
                            {s.source === "broken" ? s.path : s.description}
                          </span>
                        </span>
                        {s.source !== "broken" && (
                          <span className="hidden shrink-0 text-xs text-muted-foreground tabular-nums sm:block">
                            {fmtBytes(s.sizeBytes)}
                          </span>
                        )}
                        <span className="flex shrink-0">
                          {skillAgents.map((agent) => (
                            <StateDot
                              key={agent.id}
                              state={s.states[agent.id]?.state}
                              label={t("{client}: {state}", {
                                client: agent.name,
                                state: stateLabel(s.states[agent.id]?.state),
                              })}
                            />
                          ))}
                        </span>
                      </button>
                    ))}
                </div>
              );
            })
          )}
          <div className="mt-4">
            <Legend />
          </div>
        </section>
        <aside
          aria-label={t("Skill details")}
          className="min-w-0 lg:sticky lg:top-0 lg:max-h-[calc(100vh-9rem)] lg:self-start lg:overflow-y-auto lg:border-l lg:pl-6"
        >
          {skill ? (
            <SkillDetail
              skill={skill}
              agents={skillAgents}
              project={project}
              busy={busy}
              onChange={(agent, wanted) => void change(agent, skill, wanted)}
            />
          ) : (
            <p className="py-10 text-center text-sm text-muted-foreground">
              {t("Pick a skill to see where it loads.")}
            </p>
          )}
        </aside>
      </div>
    );
  } else if (tab === "rules") {
    const scopes = (["global", "project"] as const)
      .map((scope) => ({ scope, items: visibleRules.filter((r) => r.scope === scope) }))
      .filter((g) => g.items.length > 0);
    body = (
      <div className="grid gap-6 lg:grid-cols-[minmax(0,1fr)_minmax(320px,380px)]">
        <section aria-label={t("Rules")} className="min-w-0">
          {banner}
          <AgentHeader
            agents={inv.agents}
            focus={focus}
            onFocus={setFocus}
            label={t("Rules file")}
          />
          {scopes.length === 0 ? (
            <p className="py-10 text-center text-sm text-muted-foreground">
              {needle || focus ? t("No rules files match.") : t("No rules files found.")}
            </p>
          ) : (
            scopes.map(({ scope, items }) => (
              <div key={scope}>
                <GroupHeader
                  open={!collapsed.has(`rules:${scope}`) || Boolean(needle)}
                  onToggle={() =>
                    setCollapsed((prev) => {
                      const next = new Set(prev);
                      if (next.has(`rules:${scope}`)) next.delete(`rules:${scope}`);
                      else next.add(`rules:${scope}`);
                      return next;
                    })
                  }
                  icon={scope === "global" ? <User /> : <FolderOpen />}
                  label={scope === "global" ? t("Global") : t("Project")}
                  count={items.length}
                  note={
                    scope === "global"
                      ? t("every session")
                      : project
                        ? folderName(project)
                        : undefined
                  }
                />
                {(!collapsed.has(`rules:${scope}`) || Boolean(needle)) &&
                  items.map((r) => (
                    <button
                      key={r.key}
                      type="button"
                      onClick={() => setSelectedRule(r.key)}
                      aria-current={rule?.key === r.key ? "true" : undefined}
                      className={cn(
                        "flex w-full items-center gap-3 rounded-lg px-2 py-2 text-left transition-colors hover:bg-accent/40 focus-visible:ring-1 focus-visible:ring-ring focus-visible:outline-none",
                        rule?.key === r.key && "bg-accent/70 hover:bg-accent/70",
                      )}
                    >
                      <span className="min-w-0 flex-1">
                        <span className="block truncate text-sm font-medium">
                          {r.name}
                        </span>
                        <span className="block truncate font-mono text-2xs text-muted-foreground">
                          {r.path}
                        </span>
                      </span>
                      <span className="flex shrink-0">
                        {inv.agents.map((agent) => (
                          <StateDot
                            key={agent.id}
                            state={r.agents[agent.id] ? "on" : undefined}
                            label={t("{client}: {state}", {
                              client: agent.name,
                              state: r.agents[agent.id] ? t("Loaded") : t("Not loaded"),
                            })}
                          />
                        ))}
                      </span>
                    </button>
                  ))}
              </div>
            ))
          )}
        </section>
        <aside
          aria-label={t("Rules file details")}
          className="min-w-0 lg:sticky lg:top-0 lg:max-h-[calc(100vh-9rem)] lg:self-start lg:overflow-y-auto lg:border-l lg:pl-6"
        >
          {rule ? (
            <RuleDetail rule={rule} agents={inv.agents} project={project} />
          ) : (
            <p className="py-10 text-center text-sm text-muted-foreground">
              {t("Pick a rules file to see which agents read it.")}
            </p>
          )}
        </aside>
      </div>
    );
  } else {
    body = (
      <div className="grid gap-6 lg:grid-cols-[minmax(0,1fr)_minmax(320px,380px)]">
        <section aria-label={t("Needs a look")} className="flex min-w-0 flex-col gap-3">
          {inv.issues.length === 0 ? (
            <EmptyState
              icon={<Sparkles />}
              title={t("Nothing needs a look")}
              description={t("Every skill and rules file loads the way it's set.")}
            />
          ) : (
            inv.issues.map((issue) => (
              <IssueCard
                key={issue.id}
                issue={issue}
                agents={inv.agents}
                busy={busy}
                onFix={() => fix(issue)}
              />
            ))
          )}
        </section>
        <aside aria-label={t("By agent")} className="min-w-0 lg:border-l lg:pl-6">
          <AgentSummary inv={inv} />
        </aside>
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-4">
      {header}
      <div
        role="tabpanel"
        id={`skills-panel-${tab}`}
        aria-labelledby={`skills-tab-${tab}`}
      >
        {body}
      </div>
    </div>
  );
}
