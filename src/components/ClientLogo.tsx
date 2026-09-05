import amazonQ from "@/assets/client-logos/amazon-q.svg?raw";
import { cn } from "@/lib/utils";

// Vendored official marks from @lobehub/icons-static-svg (MIT), simple-icons
// (CC0), devicon (MIT), and vendor-published marks for Factory Droid, BoltAI,
// AnythingLLM, Continue, and Oh My Pi.
// External local assets load only for clients actually on screen and can be
// cached by the webview. Keep the mixed-color Amazon mark inline because its
// orange accent and inherited foreground cannot both be expressed by a mask.
const URLS = import.meta.glob("../assets/client-logos/*.svg", {
  query: "?url&no-inline",
  import: "default",
  eager: true,
}) as Record<string, string>;
const LOGOS = Object.fromEntries(
  Object.entries(URLS).map(([path, url]) => [
    path.split("/").pop()!.replace(".svg", ""),
    url,
  ]),
);
const MONOCHROME = new Set([
  "amp",
  "anythingllm",
  "boltai",
  "cline",
  "continue",
  "cursor",
  "devin",
  "droid",
  "github-copilot-cli",
  "goose",
  "grok",
  "hermes",
  "kilo-code",
  "kimi-code",
  "lm-studio",
  "opencode",
  "pi",
  "roo-code",
]);

/**
 * Client id -> logo file basename. Most ids match their filename; the two Claude clients
 * share the Anthropic mark family but use distinct files. Ids absent here render a monogram
 * (Crush, Jan, and Witsy publish only a raster mark or a trademarked wordmark, so there is
 * nothing clean to vendor yet).
 */
const CLIENT_LOGO: Record<string, string> = {
  "claude-desktop": "claude",
  "claude-code": "claude-code",
  cursor: "cursor",
  vscode: "vscode",
  codex: "codex",
  antigravity: "antigravity",
  "gemini-cli": "gemini-cli",
  cline: "cline",
  "roo-code": "roo-code",
  kiro: "kiro",
  "lm-studio": "lm-studio",
  goose: "goose",
  hermes: "hermes",
  windsurf: "devin",
  "devin-cli": "devin",
  warp: "warp",
  zed: "zed",
  "amazon-q": "amazon-q",
  grok: "grok",
  opencode: "opencode",
  "qwen-code": "qwen-code",
  "kimi-code": "kimi-code",
  junie: "junie",
  "kilo-code": "kilo-code",
  "github-copilot-cli": "github-copilot-cli",
  amp: "amp",
  pi: "pi",
  omp: "omp",
  droid: "droid",
  boltai: "boltai",
  anythingllm: "anythingllm",
  continue: "continue",
};

/** Initials for the monogram fallback: two letters for multi-word names, else two chars. */
function initials(name: string): string {
  const words = name.trim().split(/\s+/).filter(Boolean);
  if (words.length >= 2) return (words[0][0] + words[1][0]).toUpperCase();
  return name.slice(0, 2).toUpperCase();
}

/**
 * The official brand logo for a client, or a neutral monogram badge when none is vendored.
 * Decorative: the client name always sits next to it, so it's aria-hidden.
 */
export function ClientLogo({
  id,
  name,
  size = 20,
  className,
}: {
  id: string;
  name: string;
  size?: number;
  className?: string;
}) {
  const key = CLIENT_LOGO[id] ?? "";
  const url = LOGOS[key];

  if (url) {
    return (
      <span
        aria-hidden
        className={cn("inline-flex shrink-0 items-center justify-center", className)}
        style={{ fontSize: size, lineHeight: 0, width: size, height: size }}
      >
        {key === "amazon-q" ? (
          <span dangerouslySetInnerHTML={{ __html: amazonQ }} />
        ) : MONOCHROME.has(key) ? (
          <span
            className="size-full bg-current"
            style={{
              mask: `url("${url}") center / contain no-repeat`,
              WebkitMask: `url("${url}") center / contain no-repeat`,
            }}
          />
        ) : (
          <img src={url} alt="" className="size-full object-contain" />
        )}
      </span>
    );
  }

  return (
    <span
      aria-hidden
      className={cn(
        "inline-flex shrink-0 items-center justify-center rounded-md border bg-muted font-semibold text-muted-foreground",
        className,
      )}
      style={{ width: size, height: size, fontSize: Math.round(size * 0.42) }}
    >
      {initials(name)}
    </span>
  );
}
