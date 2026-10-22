import {
  useCallback,
  useEffect,
  useId,
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
  Filter,
  FolderOpen,
  GitBranch,
  Info,
  Library,
  Lock,
  MoreHorizontal,
  RefreshCw,
  ScrollText,
  Search,
  Sparkles,
  Trash2,
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
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { EmptyState } from "@/components/ui/empty-state";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { Switch } from "@/components/ui/switch";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import {
  dismissSkillIssue,
  fixSkillIssue,
  openSkillPath,
  openSkillPathWith,
  removeSkill,
  setSkillState,
  skillOpenApps,
  skillsInventory,
} from "@/lib/api";
import { SKILLS_REFRESH_EVENT, SKILLS_SEARCH_EVENT } from "@/lib/shortcuts";
import { getLocale, translate, useI18n } from "@/lib/i18n";
import { toastError } from "@/lib/toast";
import type {
  RuleFileEntry,
  SkillAgent,
  SkillEntry,
  SkillIssue,
  SkillLoadState,
  SkillOpenApp,
  SkillSource,
  SkillsInventory,
  SkillWanted,
} from "@/lib/types";
import { columnNames } from "@/lib/agentNames";
import { cn, fmtBytes, fmtTs } from "@/lib/utils";

type Tab = "skills" | "rules" | "issues";

/** Claude Code keeps this many description characters (mirrors DESCRIPTION_CAP in skills.rs). */
const DESCRIPTION_CAP = 1536;
const PROJECT_KEY = "toolport-skills-project";
const TAB_ORDER: Tab[] = ["skills", "rules", "issues"];
const COLUMN = 32;
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

/** The agent a pointer or keyboard is on in the grid, and what the hovered dot says. */
type ColumnHover = { agent: string; detail?: string } | null;

/** One cell in the agent matrix. Shape and color both carry the state. */
function StateDot({
  state,
  label,
  width = COLUMN,
  highlighted = false,
  onHover,
}: {
  state: SkillLoadState | undefined;
  label: string;
  width?: number;
  highlighted?: boolean;
  onHover?: (on: boolean) => void;
}) {
  const shape = (() => {
    switch (state) {
      case "on":
        return <span className="block size-3 rounded-full bg-success" />;
      case "name-only":
        return (
          <span className="block size-3 rounded-full border-2 border-partial bg-[linear-gradient(90deg,var(--color-partial)_50%,transparent_50%)]" />
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
      aria-label={label || undefined}
      onMouseEnter={onHover && (() => onHover(true))}
      onMouseLeave={onHover && (() => onHover(false))}
      className={cn(
        "flex min-h-5 shrink-0 items-center justify-center self-stretch transition-colors",
        highlighted && "bg-primary/8",
      )}
      style={{ width }}
    >
      {shape}
    </span>
  );
}

/**
 * What the grid header says about a hovered dot. `via` is a folder for On and a sentence
 * for the other states ("Off in config.toml"), so only On gets "via".
 */
function dotDetail(at: SkillEntry["states"][string] | undefined, state: string): string {
  if (!at) return state;
  if (at.state === "on") return translate("{state} via {path}", { state, path: at.via });
  if (at.state === "name-only") return `${state} · ${at.via}`;
  return at.via;
}

/** Column width: room for short names when the list has it, logos only when it doesn't. */
const WIDE_COLUMN = 56;
const NARROW_COLUMN = 32;
/** What a row keeps for the skill's name and description before columns widen. */
const NAME_ROOM = 360;
/** The least a skill's name and description keep beside the columns. */
const NAME_MIN = 200;
/** The size beside a skill's dots. */
const SIZE_COLUMN = 64;
/** A row's own padding and the gaps around its size. */
const ROW_CHROME = 16 + 2 * 12;

/**
 * Whether a row's name keeps NAME_MIN beside `count` columns. Below it each row puts its name on
 * a line above its dots, so many agents in a narrow window never squeeze names out. The GTK
 * shell's `names_fit` matches.
 */
function namesFit(width: number, count: number, size: boolean): boolean {
  return (
    width <= 0 ||
    width >= ROW_CHROME + NAME_MIN + (size ? SIZE_COLUMN : 0) + count * NARROW_COLUMN
  );
}

/**
 * Whether the Skills and Rules lists fit beside the detail pane with `count` agent columns at
 * their narrowest. Below it the detail pane moves under the list; the fixed 980px step alone
 * squeezed the names out with many agents. The GTK shell's `side_by_side_width` matches.
 */
function sideBySide(width: number, count: number): boolean {
  return (
    width >= 981 &&
    width - detailWidth(width) >= 16 + NAME_MIN + SIZE_COLUMN + count * NARROW_COLUMN + 24
  );
}
/**
 * The detail pane's width: 30% of the page, between 380 and 520px. It follows the window, never
 * what the pane shows, so picking a skill can't move the split. The GTK shell's matches.
 */
function detailWidth(width: number): number {
  return Math.min(520, Math.max(380, width * 0.3));
}
/** The split layout's classes, for the grid and its detail pane. */
const SPLIT_GRID = "grid-cols-[minmax(0,1fr)_clamp(380px,30%,520px)]";
const SPLIT_ASIDE =
  "sticky top-0 max-h-[calc(100vh-9rem)] self-start overflow-y-auto border-l pl-6";

function useColumnWidth(
  count: number,
): [(node: HTMLElement | null) => void, number, number] {
  const [ref, width] = useWidth();
  return [
    ref,
    width - count * WIDE_COLUMN >= NAME_ROOM ? WIDE_COLUMN : NARROW_COLUMN,
    width,
  ];
}

function useWidth(): [(node: HTMLElement | null) => void, number] {
  const [width, setWidth] = useState(0);
  const observer = useRef<ResizeObserver | null>(null);
  const ref = useCallback((node: HTMLElement | null) => {
    observer.current?.disconnect();
    observer.current = null;
    if (!node) return;
    setWidth(node.clientWidth);
    if (typeof ResizeObserver === "undefined") return;
    observer.current = new ResizeObserver(([entry]) => setWidth(entry.contentRect.width));
    observer.current.observe(node);
  }, []);
  return [ref, width];
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
    <div className="flex flex-wrap items-center gap-x-4 gap-y-1 border-t pt-3 text-meta text-muted-foreground">
      {items.map(([state, label]) => (
        <span key={label} className="flex items-center">
          <span aria-hidden="true" className="flex">
            <StateDot state={state} label="" width={28} />
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
  column,
  hover,
  onHover,
  onHeaderFocus,
  skills = false,
}: {
  agents: SkillAgent[];
  focus: string | null;
  onFocus: (id: string | null) => void;
  label: string;
  column: number;
  hover: ColumnHover;
  onHover: (hover: ColumnHover) => void;
  onHeaderFocus: (focus: ColumnHover) => void;
  /** The Skills list keeps what an agent finds, switched off or not, so it can go back on. */
  skills?: boolean;
}) {
  const { t } = useI18n();
  const wide = column >= WIDE_COLUMN;
  const names = columnNames(agents.map((agent) => agent.name));
  const hovered = hover ? agents.find((agent) => agent.id === hover.agent) : undefined;
  const noteId = useId();
  return (
    <div>
      {/* The hovered column on a line of its own across the list, so it stays readable with
          many agents and showing it never moves the header. */}
      <p className="h-5 truncate px-2 text-meta text-muted-foreground" aria-live="off">
        {hovered && (
          <>
            <span className="font-semibold text-foreground">{hovered.name}</span>
            {hover?.detail && `: ${hover.detail}`}
          </>
        )}
      </p>
      <div className="flex items-end justify-between gap-3 border-b px-2 pb-2">
        <span className="flex min-w-0 items-baseline gap-2 pb-1">
          <span className="shrink-0 text-xs font-semibold tracking-wider text-muted-foreground uppercase">
            {label}
          </span>
        </span>
        <span className="flex shrink-0">
          {agents.map((agent, index) => {
            const note = agent.verified
              ? undefined
              : t("folders from its docs, not checked on this machine");
            return (
              // No tooltip: the line above names the hovered or focused column, and a
              // tooltip trails the pointer across the logos as a second name.
              <button
                key={agent.id}
                type="button"
                aria-pressed={focus === agent.id}
                aria-label={
                  skills
                    ? t("Show only skills {client} finds", { client: agent.name })
                    : t("Show only what {client} loads", { client: agent.name })
                }
                aria-describedby={note ? `${noteId}-${index}` : undefined}
                onClick={() => onFocus(focus === agent.id ? null : agent.id)}
                onMouseEnter={() => onHover({ agent: agent.id, detail: note })}
                onMouseLeave={() => onHover(null)}
                onFocus={() => onHeaderFocus({ agent: agent.id, detail: note })}
                onBlur={() => onHeaderFocus(null)}
                className={cn(
                  "relative flex flex-col items-center justify-center gap-1 rounded-md py-1.5 text-xs text-muted-foreground transition-colors focus-visible:ring-2 focus-visible:ring-ring/50 focus-visible:outline-none",
                  hover?.agent === agent.id && "bg-primary/8 text-foreground",
                  focus === agent.id &&
                    "bg-primary/12 text-foreground ring-1 ring-primary/60",
                )}
                style={{ width: column }}
              >
                <ClientLogo id={agent.id} name={agent.name} size={20} />
                {/* Always there, empty when only logos fit, so the header keeps its height. */}
                <span
                  aria-hidden={!wide}
                  className={cn(
                    "max-w-full truncate px-0.5 leading-none",
                    !wide && "invisible",
                  )}
                >
                  {wide ? names[index] : "\u00a0"}
                </span>
                {note && (
                  <span id={`${noteId}-${index}`} className="sr-only">
                    {note}
                  </span>
                )}
                {!agent.verified && (
                  <span
                    className={cn(
                      "absolute size-1.5 rounded-full bg-primary",
                      wide ? "top-1 right-2" : "top-0.5 right-0.5",
                    )}
                  />
                )}
              </button>
            );
          })}
        </span>
      </div>
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
      <span className="rounded-full bg-muted px-1.5 text-xs text-muted-foreground tabular-nums">
        {count}
      </span>
      {note && <span className="truncate text-meta text-muted-foreground">{note}</span>}
    </button>
  );
}

/** `target` is the absolute key the backend allows; `path` is what the user sees and copies. */
function FileActions({
  target,
  path,
  openLabel,
  project,
  canOpen = true,
  children,
}: {
  target: string;
  path: string;
  openLabel: string;
  project: string | null;
  /** False for a link to a missing folder: there is nothing to open, only where it sits. */
  canOpen?: boolean;
  children?: ReactNode;
}) {
  const { t } = useI18n();
  const [apps, setApps] = useState<SkillOpenApp[] | null>(null);
  const failed = (error: unknown) =>
    toastError(t("Couldn't open {path}", { path }), {
      description: errorText(error),
    });
  const run = (reveal: boolean) => openSkillPath(target, reveal, project).catch(failed);
  const chosen = apps?.find((app) => app.chosen) ?? apps?.find((app) => app.default);
  return (
    <div className="flex flex-wrap gap-2">
      {canOpen && (
        <div className="flex">
          <Button
            variant="outline"
            size="sm"
            className="rounded-r-none"
            onClick={() => void run(false)}
          >
            <FilePen /> {openLabel}
          </Button>
          <DropdownMenu
            onOpenChange={(open) => {
              if (open) void skillOpenApps(target).then(setApps, () => setApps([]));
            }}
          >
            <DropdownMenuTrigger asChild>
              <Button
                variant="outline"
                size="icon-sm"
                className="rounded-l-none border-l-0"
                aria-label={t("Open with")}
              >
                <ChevronDown />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="start" className="min-w-48">
              <DropdownMenuLabel>{t("Open with")}</DropdownMenuLabel>
              {apps === null ? (
                <DropdownMenuItem disabled>{t("Loading…")}</DropdownMenuItem>
              ) : apps.length === 0 ? (
                <DropdownMenuItem disabled>{t("No other apps found")}</DropdownMenuItem>
              ) : (
                <DropdownMenuRadioGroup
                  value={chosen?.id ?? ""}
                  onValueChange={(app) => {
                    setApps(
                      (prev) =>
                        prev?.map((each) => ({ ...each, chosen: each.id === app })) ??
                        prev,
                    );
                    void openSkillPathWith(target, app, project).catch(failed);
                  }}
                >
                  {apps.map((app) => (
                    <DropdownMenuRadioItem key={app.id} value={app.id}>
                      {app.default ? t("{app} (default)", { app: app.name }) : app.name}
                    </DropdownMenuRadioItem>
                  ))}
                </DropdownMenuRadioGroup>
              )}
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      )}
      <Button variant="outline" size="sm" onClick={() => void run(true)}>
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
      {children}
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
    <div className="mt-6 mb-2 flex items-baseline justify-between gap-2">
      <h3 className="text-xs font-semibold tracking-wider text-muted-foreground uppercase">
        {children}
      </h3>
      {aside && <span className="text-meta text-muted-foreground">{aside}</span>}
    </div>
  );
}

/** The status chip for one agent: the legend's word and color, never pressable. */
function SkillStateChip({
  state,
  label,
}: {
  state: SkillLoadState | undefined;
  /** Overrides the word, for rules ("Loaded"). */
  label?: string;
}) {
  const stateLabel = useStateLabel();
  const text = label ?? stateLabel(state);
  switch (state) {
    case "on":
      return (
        <Badge variant="success">
          <span aria-hidden="true" className="size-2 rounded-full bg-current" />
          {text}
        </Badge>
      );
    case "name-only":
      return (
        <Badge variant="partial">
          <span
            aria-hidden="true"
            className="size-2 rounded-full border-[1.5px] border-current bg-[linear-gradient(90deg,currentColor_50%,transparent_50%)]"
          />
          {text}
        </Badge>
      );
    case "off":
      return (
        <Badge variant="off">
          <Ban aria-hidden="true" />
          {text}
        </Badge>
      );
    case "shadowed":
      return (
        <Badge variant="warning">
          <span
            aria-hidden="true"
            className="size-2 rounded-full border-[1.5px] border-current"
          />
          {text}
        </Badge>
      );
    case "broken":
      return (
        <Badge variant="destructive">
          <Unlink aria-hidden="true" />
          {text}
        </Badge>
      );
    default:
      return (
        <Badge variant="off">
          <span aria-hidden="true" className="size-1 rounded-full bg-current" />
          {text}
        </Badge>
      );
  }
}

/** An agent's logo on a 28px tile, as in every Agents and By agent row. */
/**
 * One agent in an Agents list: logo, name, state and control on one line, and what the agent
 * reads on full-width lines under the name, so the notes don't share a line with the controls.
 * The GTK shell's `agent_card` matches.
 */
function AgentCard({
  agent,
  badge,
  trailing,
  children,
}: {
  agent: SkillAgent;
  badge?: ReactNode;
  trailing?: ReactNode;
  children?: ReactNode;
}) {
  return (
    <li className="grid grid-cols-[auto_minmax(0,1fr)_auto] items-center gap-x-3 gap-y-0.5 px-4 py-3">
      <LogoTile agent={agent} />
      <span className="flex min-w-0 items-center gap-2 text-sm font-semibold">
        <span className="truncate">{agent.name}</span>
        {badge}
      </span>
      <span className="flex items-center gap-2">{trailing}</span>
      {children && (
        <span className="col-span-2 col-start-2 min-w-0 text-meta text-muted-foreground">
          {children}
        </span>
      )}
    </li>
  );
}

function LogoTile({ agent }: { agent: SkillAgent }) {
  return (
    <span className="flex size-7 shrink-0 items-center justify-center rounded-md bg-muted">
      <ClientLogo id={agent.id} name={agent.name} size={20} />
    </span>
  );
}

/**
 * One row of a skill's Agents list: logo, where it loads from, the status chip and one
 * 44px control slot. A switch when Toolport can write the agent's own setting, a lock when it
 * can't, nothing when the agent doesn't load the skill. Claude Code's third state ("Name
 * only") sits in a small menu beside its switch.
 */
function AgentStateRow({
  agent,
  skill,
  onChange,
}: {
  agent: SkillAgent;
  skill: SkillEntry;
  /** Resolves false when the write failed, so the row shows the saved state again. */
  onChange: (wanted: SkillWanted) => Promise<boolean>;
}) {
  const { t } = useI18n();
  // What was just picked, shown until the write's scan arrives (a new `skill`), so a second
  // click starts from it rather than from the state before the first.
  const [picked, setPicked] = useState<{ skill: SkillEntry; wanted: SkillWanted } | null>(
    null,
  );
  const saved = skill.states[agent.id];
  const current = picked?.skill === skill ? picked.wanted : saved?.state;
  const pick = (wanted: SkillWanted) => {
    const choice = { skill, wanted };
    setPicked(choice);
    void onChange(wanted).then((ok) => {
      if (!ok) setPicked((now) => (now === choice ? null : now));
    });
  };
  const switchable = current === "on" || current === "name-only" || current === "off";
  const readOnlyReason = saved?.readOnlyReason;
  const writable = agent.canToggle && !readOnlyReason;
  const readOnly = switchable && !writable;
  const lockReason = readOnlyReason ?? agent.switchNote;
  let slot: ReactNode = null;
  if (switchable && writable) {
    slot = (
      <Switch
        checked={current !== "off"}
        aria-label={t("Load {skill} in {client}", {
          skill: skill.name,
          client: agent.name,
        })}
        onCheckedChange={(checked) => pick(checked ? "on" : "off")}
      />
    );
  } else if (readOnly) {
    slot = (
      <Tooltip>
        <TooltipTrigger asChild>
          <span
            role="img"
            // Focusable so keyboard users get the tooltip; it's a fact, not a control.
            // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex
            tabIndex={0}
            aria-label={lockReason}
            className="flex size-7 items-center justify-center rounded-md text-muted-foreground focus-visible:ring-2 focus-visible:ring-ring/50 focus-visible:outline-none"
          >
            <Lock className="size-3" />
          </span>
        </TooltipTrigger>
        <TooltipContent className="max-w-64">{lockReason}</TooltipContent>
      </Tooltip>
    );
  }
  const where = saved?.via ?? t("Not in any folder it reads");
  return (
    <AgentCard
      agent={agent}
      badge={!agent.verified && <Badge variant="off">{t("from docs")}</Badge>}
      trailing={
        <>
          <SkillStateChip state={current} />
          {switchable && writable && agent.nameOnly && (
            <DropdownMenu>
              <Tooltip>
                <TooltipTrigger asChild>
                  <DropdownMenuTrigger asChild>
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={t("More options for {client}", { client: agent.name })}
                    >
                      <MoreHorizontal />
                    </Button>
                  </DropdownMenuTrigger>
                </TooltipTrigger>
                <TooltipContent>
                  {t("More options for {client}", { client: agent.name })}
                </TooltipContent>
              </Tooltip>
              <DropdownMenuContent align="end" className="w-72">
                <DropdownMenuLabel>{agent.name}</DropdownMenuLabel>
                <DropdownMenuCheckboxItem
                  checked={current === "name-only"}
                  onCheckedChange={(checked) => pick(checked ? "name-only" : "on")}
                >
                  {t("Load name and description only")}
                </DropdownMenuCheckboxItem>
                <div className="px-2 pt-1 pb-1.5 text-meta text-muted-foreground">
                  {agent.switchNote}
                </div>
              </DropdownMenuContent>
            </DropdownMenu>
          )}
          <span className="flex w-11 shrink-0 justify-end">{slot}</span>
        </>
      }
    >
      <span className="block break-words">
        {readOnly && !readOnlyReason
          ? `${where} · ${t("No switch Toolport can write")}`
          : where}
      </span>
      {readOnlyReason && <span className="block">{readOnlyReason}</span>}
    </AgentCard>
  );
}

function SkillDetail({
  skill,
  agents,
  project,
  onChange,
  onRemove,
}: {
  skill: SkillEntry;
  agents: SkillAgent[];
  project: string | null;
  onChange: (agent: SkillAgent, wanted: SkillWanted) => Promise<boolean>;
  onRemove: () => Promise<void>;
}) {
  const { t } = useI18n();
  const source = useSourceLabels()(skill.source);
  const loaded = agents.filter((a) => {
    const state = skill.states[a.id]?.state;
    return state === "on" || state === "name-only";
  }).length;
  const ratio = Math.min(1, skill.descriptionChars / DESCRIPTION_CAP);
  // `links` holds folders, the skill's own among them; only the other ways in are news.
  const ownFolder = folderOf(skill);
  const otherLinks = skill.links.filter((l) => l !== skill.path && l !== ownFolder);
  return (
    <div>
      <div className="flex items-start gap-3">
        <span className="flex size-10 shrink-0 items-center justify-center rounded-md bg-primary/12 text-primary">
          <Sparkles className="size-5" />
        </span>
        <div className="min-w-0">
          <h2 className="truncate text-lg font-semibold">{skill.name}</h2>
          <div className="mt-1 flex flex-wrap gap-1.5">
            <Badge variant={skill.source === "broken" ? "destructive" : "off"}>
              {source.icon} {source.label}
            </Badge>
            {skill.gitRepo && (
              <Badge variant="off">
                <GitBranch /> {t("git repo: {name}", { name: skill.gitRepo })}
              </Badge>
            )}
            {!skill.editable && skill.source !== "broken" && (
              <Badge variant="off">{t("Read only")}</Badge>
            )}
          </div>
        </div>
      </div>
      <div className="mt-4">
        <FileActions
          target={skill.key}
          path={skill.path}
          openLabel={t("Open SKILL.md")}
          project={project}
          canOpen={skill.source !== "broken"}
        >
          {skill.removable && (
            <ConfirmDialog
              trigger={
                <Button
                  variant="outline"
                  size="icon-sm"
                  aria-label={t("Remove {skill}", { skill: skill.name })}
                  title={t("Remove {skill}", { skill: skill.name })}
                >
                  <Trash2 />
                </Button>
              }
              title={t("Remove {skill}?", { skill: skill.name })}
              description={
                skill.trashOnRemove
                  ? t(
                      "Its folder goes to the trash and links to it are removed, so no client loads it. You can restore the folder from the trash.",
                    )
                  : t(
                      "Only the links to it are removed, so no client loads it. Its folder at {path} stays where it is.",
                      { path: ownFolder },
                    )
              }
              confirmLabel={t("Remove")}
              destructive
              onConfirm={onRemove}
            />
          )}
        </FileActions>
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
          <div className="mt-1 flex justify-between text-meta text-muted-foreground">
            <span>
              {t("{count} of {cap} characters", {
                count: skill.descriptionChars.toLocaleString(getLocale()),
                cap: DESCRIPTION_CAP.toLocaleString(getLocale()),
              })}
            </span>
            <span>{t("Claude Code's limit")}</span>
          </div>
        </>
      )}

      <PanelHeading>{t("File")}</PanelHeading>
      <Field label={t("Path")}>
        <span className="font-mono text-meta">{skill.path}</span>
      </Field>
      {otherLinks.length > 0 && (
        <Field label={t("Reached by")}>
          <span className="flex flex-col gap-0.5 font-mono text-meta">
            {otherLinks.map((link) => (
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
                lines: skill.lines.toLocaleString(getLocale()),
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
      <ul className="divide-y overflow-hidden rounded-xl border bg-card">
        {agents.map((agent) => (
          <AgentStateRow
            key={agent.id}
            agent={agent}
            skill={skill}
            onChange={(wanted) => onChange(agent, wanted)}
          />
        ))}
      </ul>
      {skill.preview && (
        <>
          <PanelHeading>{t("Preview")}</PanelHeading>
          <pre className="max-h-64 overflow-auto rounded-lg border bg-muted/40 p-3 font-mono text-meta leading-relaxed whitespace-pre-wrap">
            {skill.preview}
          </pre>
        </>
      )}
    </div>
  );
}

/** Why an agent doesn't load this rules file: the files it reads instead. */
/**
 * The folder a skill's file sits in, as shown. The file may be a link to one with another
 * name, so this drops the last part of the path whatever it is called. A broken link has no
 * file; the link is the folder.
 */
function folderOf(skill: SkillEntry): string {
  return skill.source === "broken" ? skill.path : skill.path.replace(/[\\/][^\\/]+$/, "");
}

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
  return (
    <div>
      <div className="flex items-start gap-3">
        <span className="flex size-10 shrink-0 items-center justify-center rounded-md bg-primary/12 text-primary">
          <ScrollText className="size-5" />
        </span>
        <div className="min-w-0">
          <h2 className="truncate text-lg font-semibold">{rule.name}</h2>
          <div className="mt-1 flex gap-1.5">
            <Badge variant="off">
              {rule.scope === "project" ? t("Project") : t("Global")}
            </Badge>
          </div>
        </div>
      </div>
      <div className="mt-4">
        <FileActions
          target={rule.key}
          path={rule.path}
          openLabel={t("Open file")}
          project={project}
        />
      </div>
      <PanelHeading>{t("File")}</PanelHeading>
      <Field label={t("Path")}>
        <span className="font-mono text-meta">{rule.path}</span>
      </Field>
      <Field label={t("Size")}>{fmtBytes(rule.sizeBytes)}</Field>

      <PanelHeading
        aside={t("{count} of {total} load it", { count: loaded, total: agents.length })}
      >
        {t("Agents")}
      </PanelHeading>
      <ul className="divide-y overflow-hidden rounded-xl border bg-card">
        {agents.map((agent) => {
          const chain = rule.agents[agent.id];
          return (
            <AgentCard
              key={agent.id}
              agent={agent}
              trailing={
                <>
                  <SkillStateChip
                    state={chain ? "on" : undefined}
                    label={chain ? t("Loaded") : t("Not loaded")}
                  />
                  <span className="flex w-11 shrink-0 justify-end">
                    {chain && (
                      <Tooltip>
                        <TooltipTrigger asChild>
                          <span
                            role="img"
                            // A focusable fact exposes its explanation to keyboard users.
                            // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex
                            tabIndex={0}
                            aria-label={t("No switch Toolport can write")}
                            className="flex size-7 items-center justify-center rounded-md text-muted-foreground focus-visible:ring-2 focus-visible:ring-ring/50 focus-visible:outline-none"
                          >
                            <Lock className="size-3" />
                          </span>
                        </TooltipTrigger>
                        <TooltipContent>
                          {t("No switch Toolport can write")}
                        </TooltipContent>
                      </Tooltip>
                    )}
                  </span>
                </>
              }
            >
              {chain ? (
                <span className="flex flex-col gap-0.5">
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
                <span className="block">
                  {agent.rulesNote ?? otherRulesNote(agent, rule)}
                </span>
              )}
            </AgentCard>
          );
        })}
      </ul>
      {rule.preview && (
        <>
          <PanelHeading>{t("Preview")}</PanelHeading>
          <pre className="max-h-72 overflow-auto rounded-lg border bg-muted/40 p-3 font-mono text-meta leading-relaxed whitespace-pre-wrap">
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
  onFix,
  onDismiss,
  onSkill,
}: {
  issue: SkillIssue;
  agents: SkillAgent[];
  onFix: () => Promise<void>;
  onDismiss: () => void;
  onSkill: (name: string) => void;
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
    // The title keeps its width: below 560px of card the actions move under the chips.
    <article
      className={cn(
        "@container flex gap-3 rounded-xl border bg-card p-4",
        issue.level === "error"
          ? "border-l-[3px] border-l-destructive"
          : issue.level === "warn" && "border-l-[3px] border-l-warning",
      )}
    >
      <span
        className={cn(
          "flex size-8 shrink-0 items-center justify-center rounded-md",
          issue.level === "error"
            ? "bg-destructive/12"
            : issue.level === "warn"
              ? "bg-warning/12"
              : "bg-muted",
        )}
      >
        {icon}
      </span>
      <div className="grid min-w-0 flex-1 grid-cols-[minmax(0,1fr)_auto] content-start gap-x-3 @max-[560px]:grid-cols-1">
        <h3 className="col-start-1 row-start-1 self-center text-sm font-semibold">
          {issue.title}
        </h3>
        <p className="col-span-full mt-1 text-sm text-muted-foreground">{issue.detail}</p>
        {(named.length > 0 || issue.skillNames.length > 0) && (
          <div className="col-span-full mt-2.5 flex flex-wrap items-center gap-1.5">
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
              <button
                type="button"
                key={name}
                title={t("Show {skill}", { skill: name })}
                onClick={() => onSkill(name)}
                className="flex h-6 items-center gap-1 rounded-md bg-muted px-2 text-xs hover:bg-accent focus-visible:ring-2 focus-visible:ring-ring focus-visible:outline-none"
              >
                <Sparkles className="size-3 text-muted-foreground" />
                {name}
              </button>
            ))}
            {issue.skillNames.length > 12 && (
              <span className="text-meta text-muted-foreground">
                {t("+{count} more", { count: issue.skillNames.length - 12 })}
              </span>
            )}
          </div>
        )}
        <div
          data-slot="issue-actions"
          className="col-start-2 row-start-1 flex flex-wrap items-start gap-2 @max-[560px]:col-start-1 @max-[560px]:row-start-auto @max-[560px]:mt-3"
        >
          <Button variant="ghost" size="sm" onClick={onDismiss}>
            {issue.dismissed ? t("Show again") : t("Dismiss")}
          </Button>
          {issue.fixLabel && !issue.dismissed && (
            <ConfirmDialog
              trigger={
                <Button variant="outline" size="sm">
                  {issue.fixLabel}
                </Button>
              }
              title={issue.fixLabel}
              contentClassName={issue.fixPaths?.length ? "sm:max-w-md" : undefined}
              description={
                issue.fixPaths?.length ? (
                  <div className="space-y-2">
                    <p>{issue.fixNote}</p>
                    <ul className="space-y-1 rounded-md bg-muted px-3 py-2 font-mono text-xs break-words text-foreground">
                      {issue.fixPaths.map((path) => (
                        <li key={path}>{path}</li>
                      ))}
                    </ul>
                  </div>
                ) : (
                  (issue.fixNote ??
                  t(
                    "Toolport backs up any config file before it changes it, and never deletes a skill folder.",
                  ))
                )
              }
              confirmLabel={issue.fixLabel}
              onConfirm={onFix}
            />
          )}
        </div>
      </div>
    </article>
  );
}

function AgentSummary({ inv }: { inv: SkillsInventory }) {
  const { t } = useI18n();
  return (
    <div>
      <PanelHeading>{t("By agent")}</PanelHeading>
      <ul className="divide-y overflow-hidden rounded-xl border bg-card">
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
            <AgentCard
              key={agent.id}
              agent={agent}
              trailing={
                broken > 0 ? (
                  <Badge variant="destructive">
                    {t("{count} broken", { count: broken })}
                  </Badge>
                ) : !agent.supportsSkills ? null : (
                  <span
                    title={agent.switchNote}
                    className="shrink-0 text-meta text-muted-foreground"
                  >
                    {agent.canToggle ? t("Own switch") : t("No switch")}
                  </span>
                )
              }
            >
              {parts.join(" · ")}
            </AgentCard>
          );
        })}
      </ul>
      <p className="mt-2 text-meta text-muted-foreground">
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
  // Writes run one after another in the order made. A quick second click is kept, and no
  // control has to be disabled meanwhile, which would take keyboard focus away.
  const writes = useRef({ tail: Promise.resolve() as Promise<unknown>, waiting: 0 });
  function enqueue<T>(work: () => Promise<T>): Promise<{ value: T; last: boolean }> {
    const queue = writes.current;
    queue.waiting++;
    const run = queue.tail
      .then(work, work)
      .finally(() => {
        queue.waiting--;
      })
      .then((value) => ({ value, last: queue.waiting === 0 }));
    queue.tail = run.catch(() => undefined);
    return run;
  }
  const [project, setProject] = useState<string | null>(readProject);
  const [tab, setTab] = useState<Tab>("skills");
  const [query, setQuery] = useState("");
  const [pickedAgent, setFocus] = useState<string | null>(null);
  const [connectedOnly, setConnectedOnly] = useState(true);
  const [selectedSkill, setSelectedSkill] = useState<string | null>(null);
  const [selectedRule, setSelectedRule] = useState<string | null>(null);
  const [collapsed, setCollapsed] = useState<Set<string>>(
    // Open on your own skills; the rest are a click away.
    () =>
      new Set([
        "account",
        "package",
        "built-in",
        "library",
        "parked",
        "issues:dismissed",
      ]),
  );

  // Latest request wins, so a slow scan of an old project can't overwrite a newer one.
  const request = useRef(0);
  const hasInventory = useRef(false);
  useEffect(() => {
    hasInventory.current = inv !== null;
  }, [inv]);
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
          if (id !== request.current) return;
          setError(errorText(e));
          // With a page on screen the error screen doesn't show, so say the scan failed.
          if (hasInventory.current) {
            toastError(t("Couldn't read skills and rules"), {
              description: errorText(e),
            });
          }
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

  // Its Find shortcut focuses this page's search, from the Skills tab when Needs a look
  // (which has none) is open.
  const searchRef = useRef<HTMLInputElement>(null);
  const tabRef = useRef(tab);
  useEffect(() => {
    tabRef.current = tab;
  }, [tab]);
  useEffect(() => {
    const onSearch = () => {
      if (tabRef.current === "issues") {
        setFocus(null);
        setTab("skills");
      }
      let frames = 0;
      const tick = () => {
        if (searchRef.current) {
          searchRef.current.focus();
          searchRef.current.select();
        } else if (frames++ < 15) requestAnimationFrame(tick);
      };
      requestAnimationFrame(tick);
    };
    window.addEventListener(SKILLS_SEARCH_EVENT, onSearch);
    return () => window.removeEventListener(SKILLS_SEARCH_EVENT, onSearch);
  }, []);

  /** Show a write's result unless the user has moved to another project meanwhile. */
  function applyWrite(next: SkillsInventory, forProject: string | null) {
    if (projectRef.current !== forProject) return;
    request.current++;
    setInv(next);
    setError(null);
    setLoading(false);
  }

  const hasConnected = inv?.agents.some((agent) => agent.connected) ?? false;
  const focus = inv?.agents.some(
    (agent) =>
      agent.id === pickedAgent && (!connectedOnly || !hasConnected || agent.connected),
  )
    ? pickedAgent
    : null;
  const filteringAgents = Boolean(focus || (connectedOnly && hasConnected));
  const scopedAgents = useMemo(() => {
    const agents = inv?.agents ?? [];
    if (connectedOnly) {
      return agents.some((agent) => agent.connected)
        ? agents.filter((agent) => agent.connected)
        : agents;
    }
    return agents;
  }, [inv, connectedOnly]);
  const skillAgents = useMemo(
    () => scopedAgents.filter((a) => a.supportsSkills),
    [scopedAgents],
  );
  const allColumnAgents = useMemo(
    () => inv?.agents.filter((a) => tab === "rules" || a.supportsSkills) ?? [],
    [inv, tab],
  );
  const scopedSkills = useMemo(
    () =>
      inv?.skills.filter(
        (skill) =>
          !filteringAgents ||
          (focus
            ? skill.states[focus]
            : skillAgents.some((agent) => skill.states[agent.id])),
      ) ?? [],
    [inv, filteringAgents, skillAgents, focus],
  );
  const scopedRules = useMemo(
    () =>
      inv?.rules.filter(
        (rule) =>
          !filteringAgents ||
          (focus
            ? rule.agents[focus]
            : scopedAgents.some((agent) => rule.agents[agent.id])),
      ) ?? [],
    [inv, filteringAgents, scopedAgents, focus],
  );
  const needle = query.trim().toLowerCase();
  const columnAgents = tab === "rules" ? scopedAgents : skillAgents;
  const [gridRef, column, gridWidth] = useColumnWidth(columnAgents.length);
  const stacked = !namesFit(gridWidth, columnAgents.length, tab !== "rules");
  const [panelRef, panelWidth] = useWidth();
  // One step for both tabs, like the GTK shell, so switching tabs never moves the detail pane.
  const split = sideBySide(panelWidth, Math.max(skillAgents.length, scopedAgents.length));
  const [pointerHover, setHover] = useState<ColumnHover>(null);
  const [keyboardColumn, setKeyboardColumn] = useState<ColumnHover>(null);
  const hover = pointerHover ?? keyboardColumn;

  const visibleSkills = useMemo(() => {
    if (!inv) return [];
    return scopedSkills.filter(
      (s) =>
        !needle ||
        s.name.toLowerCase().includes(needle) ||
        s.description.toLowerCase().includes(needle) ||
        s.path.toLowerCase().includes(needle),
    );
  }, [inv, scopedSkills, needle]);

  const visibleRules = useMemo(() => {
    if (!inv) return [];
    return scopedRules.filter(
      (r) =>
        !needle ||
        r.name.toLowerCase().includes(needle) ||
        r.path.toLowerCase().includes(needle) ||
        r.preview.toLowerCase().includes(needle),
    );
  }, [inv, scopedRules, needle]);

  // The picked skill while the list still shows it; otherwise the first row shown, in the
  // page's group order with collapsed groups skipped, so the details never describe a hidden row.
  const skill =
    visibleSkills.find((s) => s.key === selectedSkill) ??
    SOURCE_ORDER.filter((source) => !collapsed.has(source) || Boolean(needle))
      .map((source) => visibleSkills.find((s) => s.source === source))
      .find(Boolean) ??
    null;
  const rule =
    visibleRules.find((r) => r.key === selectedRule) ?? visibleRules[0] ?? null;

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

  async function change(
    agent: SkillAgent,
    target: SkillEntry,
    wanted: SkillWanted,
  ): Promise<boolean> {
    const forProject = project;
    try {
      const { value, last } = await enqueue(() =>
        setSkillState(agent.id, target.key, wanted, forProject),
      );
      // Only the last of a run of quick changes redraws, so controls keep what was clicked.
      if (!last) return true;
      applyWrite(value, forProject);
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
      return true;
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
      return false;
    }
  }

  async function fix(issue: SkillIssue) {
    const forProject = project;
    try {
      const { value } = await enqueue(() =>
        fixSkillIssue(issue.id, issue.fingerprint, forProject),
      );
      applyWrite(value, forProject);
      toast.success(t("Fixed: {title}", { title: issue.title }));
    } catch (e) {
      toastError(t("Couldn't fix this"), { description: errorText(e) });
      // Part of a fix may have landed before the error; rescan so the page shows it.
      void load();
      throw e;
    }
  }

  async function dismiss(issue: SkillIssue) {
    const forProject = project;
    try {
      const { value } = await enqueue(() =>
        dismissSkillIssue(issue.fingerprint, !issue.dismissed, forProject),
      );
      applyWrite(value, forProject);
    } catch (e) {
      toastError(t("Couldn't change this"), { description: errorText(e) });
      // An earlier queued change may have left its result for this write to show.
      void load();
    }
  }

  async function remove(target: SkillEntry) {
    const forProject = project;
    try {
      const { value: removed } = await enqueue(() => removeSkill(target.key, forProject));
      applyWrite(removed.inventory, forProject);
      toast.success(
        removed.trashed
          ? t("Moved {skill} to the trash", { skill: target.name })
          : t("Removed the links to {skill}. Its folder stays where it is.", {
              skill: target.name,
            }),
      );
    } catch (e) {
      toastError(t("Couldn't remove {skill}", { skill: target.name }), {
        description: errorText(e),
      });
      void load();
      throw e;
    }
  }

  /** Jump from a Needs a look chip to that skill on the Skills tab. */
  function showSkill(issue: SkillIssue, name: string) {
    const found = inv?.skills.find(
      (s) => s.name === name && issue.skills.includes(s.key),
    );
    if (!found) return;
    setQuery("");
    setFocus(null);
    setConnectedOnly(false);
    setSelectedSkill(found.key);
    setCollapsed((prev) => {
      const next = new Set(prev);
      next.delete(found.source);
      return next;
    });
    setTab("skills");
  }

  const activeIssues = inv?.issues.filter((i) => !i.dismissed) ?? [];
  const dismissedIssues = inv?.issues.filter((i) => i.dismissed) ?? [];

  // Tabs follow the APG pattern: one tab stop, arrows and Home/End move between tabs.
  const tabRefs = useRef<Partial<Record<Tab, HTMLButtonElement | null>>>({});
  function selectTab(next: Tab) {
    // An agent filter from one tab can name an agent the other tab has no column for.
    setFocus(null);
    setHover(null);
    setKeyboardColumn(null);
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
      scopedSkills.filter((s) => s.source !== "broken").length,
      false,
    ],
    ["rules", t("Rules"), scopedRules.length, false],
    ["issues", t("Needs a look"), activeIssues.length, activeIssues.length > 0],
  ];

  const focusedAgent = allColumnAgents.find((agent) => agent.id === focus);
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
              <div className="px-2 py-1.5 font-mono text-meta break-words text-muted-foreground">
                {project}
              </div>
            </>
          )}
        </DropdownMenuContent>
      </DropdownMenu>
      {tab !== "issues" && (
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button
              variant="outline"
              size="sm"
              className="max-w-56"
              aria-label={t("Filter by agent")}
            >
              {focusedAgent ? (
                <ClientLogo id={focusedAgent.id} name={focusedAgent.name} size={16} />
              ) : (
                <Filter />
              )}
              <span className="truncate">
                {focusedAgent?.name ??
                  (connectedOnly && hasConnected
                    ? t("Connected to Toolport")
                    : t("All detected agents"))}
              </span>
              <ChevronDown className="text-muted-foreground" />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="max-h-96 w-56 overflow-y-auto">
            <DropdownMenuLabel>{t("Filter by agent")}</DropdownMenuLabel>
            <DropdownMenuRadioGroup
              value={focus ?? (connectedOnly && hasConnected ? "@connected" : "")}
              onValueChange={(id) => {
                if (id === "@connected" || !id) {
                  setConnectedOnly(id === "@connected");
                  setFocus(null);
                } else {
                  if (!inv?.agents.find((agent) => agent.id === id)?.connected)
                    setConnectedOnly(false);
                  setFocus(id);
                }
              }}
            >
              {hasConnected && (
                <DropdownMenuRadioItem value="@connected">
                  {t("Connected to Toolport")}
                </DropdownMenuRadioItem>
              )}
              <DropdownMenuRadioItem value="">
                {t("All detected agents")}
              </DropdownMenuRadioItem>
              {allColumnAgents.map((agent) => (
                <DropdownMenuRadioItem key={agent.id} value={agent.id}>
                  <ClientLogo id={agent.id} name={agent.name} size={16} />
                  {agent.name}
                </DropdownMenuRadioItem>
              ))}
            </DropdownMenuRadioGroup>
          </DropdownMenuContent>
        </DropdownMenu>
      )}
      {tab !== "issues" && (
        <div className="relative">
          <Search className="pointer-events-none absolute top-1/2 left-2.5 size-3.5 -translate-y-1/2 text-muted-foreground" />
          <Input
            ref={searchRef}
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
            aria-controls={tab === id ? `skills-panel-${id}` : undefined}
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
                "rounded-full px-1.5 text-xs tabular-nums",
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
    activeIssues.length > 0 && tab !== "issues" ? (
      <div className="mb-4 flex items-center gap-3 rounded-xl border border-l-[3px] border-l-warning bg-card px-4 py-3">
        <span className="flex size-8 shrink-0 items-center justify-center rounded-md bg-warning/12">
          <AlertTriangle className="size-4 text-warning" />
        </span>
        <span className="min-w-0 flex-1">
          <span className="block text-sm font-semibold">
            {activeIssues.length === 1
              ? t("1 thing needs a look")
              : t("{count} things need a look", { count: activeIssues.length })}
          </span>
          <span className="block truncate text-sm text-muted-foreground">
            {activeIssues[0].title}
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
      <div className={cn("grid gap-6", split && SPLIT_GRID)}>
        <section ref={gridRef} aria-label={t("Skills")} className="min-w-0">
          {banner}
          <AgentHeader
            agents={skillAgents}
            focus={focus}
            onFocus={setFocus}
            label={t("Skill")}
            column={column}
            hover={hover}
            onHover={setHover}
            onHeaderFocus={setKeyboardColumn}
            skills
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
                          "flex w-full gap-3 rounded-md px-2 py-2 text-left transition-colors hover:bg-accent/40 focus-visible:ring-1 focus-visible:ring-ring focus-visible:outline-none",
                          // Too narrow for names beside the dots: the name and size on one line
                          // and the dots under it. The description is left to the detail pane.
                          stacked ? "flex-col gap-1" : "items-center",
                          skill?.key === s.key && "bg-accent/70 hover:bg-accent/70",
                        )}
                      >
                        <span
                          className={cn(
                            "min-w-0 flex-1",
                            stacked && "flex items-center gap-3",
                          )}
                        >
                          <span className="flex min-w-0 flex-1 items-center gap-1.5 text-sm font-medium">
                            <span className="truncate">{s.name}</span>
                            {s.descriptionChars > DESCRIPTION_CAP && (
                              <AlertTriangle
                                className="size-3.5 shrink-0 text-warning"
                                aria-label={t("Description is cut off in Claude Code")}
                              />
                            )}
                          </span>
                          {stacked ? (
                            s.source !== "broken" && (
                              // Ends where the last dot does.
                              <span
                                className="shrink-0 text-xs text-muted-foreground tabular-nums"
                                style={{ paddingRight: (column - 12) / 2 }}
                              >
                                {fmtBytes(s.sizeBytes)}
                              </span>
                            )
                          ) : (
                            <span className="block truncate text-meta text-muted-foreground">
                              {s.source === "broken" ? s.path : s.description}
                            </span>
                          )}
                        </span>
                        {!stacked && s.source !== "broken" && (
                          <span className="hidden shrink-0 text-xs text-muted-foreground tabular-nums sm:block">
                            {fmtBytes(s.sizeBytes)}
                          </span>
                        )}
                        <span
                          className={cn(
                            "flex shrink-0",
                            stacked ? "self-end" : "-my-2 self-stretch",
                          )}
                        >
                          {skillAgents.map((agent) => {
                            const at = s.states[agent.id];
                            const state = stateLabel(at?.state);
                            return (
                              <StateDot
                                key={agent.id}
                                state={at?.state}
                                width={column}
                                highlighted={hover?.agent === agent.id}
                                onHover={(on) =>
                                  setHover(
                                    on
                                      ? {
                                          agent: agent.id,
                                          detail: dotDetail(at, state),
                                        }
                                      : null,
                                  )
                                }
                                label={t("{client}: {state}", {
                                  client: agent.name,
                                  state,
                                })}
                              />
                            );
                          })}
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
          className={cn("min-w-0", split && SPLIT_ASIDE)}
        >
          {skill ? (
            <SkillDetail
              skill={skill}
              agents={skillAgents}
              project={project}
              onChange={(agent, wanted) => change(agent, skill, wanted)}
              onRemove={() => remove(skill)}
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
      <div className={cn("grid gap-6", split && SPLIT_GRID)}>
        <section ref={gridRef} aria-label={t("Rules")} className="min-w-0">
          {banner}
          <AgentHeader
            agents={scopedAgents}
            focus={focus}
            onFocus={setFocus}
            label={t("Rules file")}
            column={column}
            hover={hover}
            onHover={setHover}
            onHeaderFocus={setKeyboardColumn}
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
                        "flex w-full gap-3 rounded-md px-2 py-2 text-left transition-colors hover:bg-accent/40 focus-visible:ring-1 focus-visible:ring-ring focus-visible:outline-none",
                        // Too narrow for names beside the dots: the name above them, and the
                        // path left to the detail pane.
                        stacked ? "flex-col gap-1" : "items-center",
                        rule?.key === r.key && "bg-accent/70 hover:bg-accent/70",
                      )}
                    >
                      <span className="min-w-0 flex-1">
                        <span className="block truncate text-sm font-medium">
                          {r.name}
                        </span>
                        {!stacked && (
                          <span className="block truncate font-mono text-meta text-muted-foreground">
                            {r.path}
                          </span>
                        )}
                      </span>
                      <span
                        className={cn(
                          "flex shrink-0",
                          stacked ? "self-end" : "-my-2 self-stretch",
                        )}
                      >
                        {scopedAgents.map((agent) => {
                          const state = r.agents[agent.id]
                            ? t("Loaded")
                            : t("Not loaded");
                          return (
                            <StateDot
                              key={agent.id}
                              state={r.agents[agent.id] ? "on" : undefined}
                              width={column}
                              highlighted={hover?.agent === agent.id}
                              onHover={(on) =>
                                setHover(on ? { agent: agent.id, detail: state } : null)
                              }
                              label={t("{client}: {state}", {
                                client: agent.name,
                                state,
                              })}
                            />
                          );
                        })}
                      </span>
                    </button>
                  ))}
              </div>
            ))
          )}
        </section>
        <aside
          aria-label={t("Rules file details")}
          className={cn("min-w-0", split && SPLIT_ASIDE)}
        >
          {rule ? (
            <RuleDetail rule={rule} agents={scopedAgents} project={project} />
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
      <div className="grid gap-6 @min-[981px]:grid-cols-[minmax(0,1fr)_clamp(380px,30%,520px)]">
        <section aria-label={t("Needs a look")} className="flex min-w-0 flex-col gap-3">
          {activeIssues.length === 0 ? (
            <EmptyState
              icon={<Sparkles />}
              title={t("Nothing needs a look")}
              description={t("Every skill and rules file loads the way it's set.")}
            />
          ) : (
            activeIssues.map((issue) => (
              <IssueCard
                key={issue.id}
                issue={issue}
                agents={inv.agents}
                onFix={() => fix(issue)}
                onDismiss={() => void dismiss(issue)}
                onSkill={(name) => showSkill(issue, name)}
              />
            ))
          )}
          {dismissedIssues.length > 0 && (
            <>
              <GroupHeader
                open={!collapsed.has("issues:dismissed")}
                onToggle={() =>
                  setCollapsed((prev) => {
                    const next = new Set(prev);
                    if (!next.delete("issues:dismissed")) next.add("issues:dismissed");
                    return next;
                  })
                }
                icon={<Ban className="size-4" />}
                label={t("Dismissed")}
                count={dismissedIssues.length}
                note={t("Back if anything about them changes")}
              />
              {!collapsed.has("issues:dismissed") &&
                dismissedIssues.map((issue) => (
                  <IssueCard
                    key={issue.id}
                    issue={issue}
                    agents={inv.agents}
                    onFix={() => fix(issue)}
                    onDismiss={() => void dismiss(issue)}
                    onSkill={(name) => showSkill(issue, name)}
                  />
                ))}
            </>
          )}
        </section>
        <aside
          aria-label={t("By agent")}
          className="min-w-0 @min-[981px]:border-l @min-[981px]:pl-6"
        >
          <AgentSummary inv={inv} />
        </aside>
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-4">
      {header}
      <div
        ref={panelRef}
        role="tabpanel"
        className="@container"
        id={`skills-panel-${tab}`}
        aria-labelledby={`skills-tab-${tab}`}
      >
        {body}
      </div>
    </div>
  );
}
