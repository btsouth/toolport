/** An icon as advertised by a downstream MCP server (SEP-973). */
export interface McpIcon {
  src: string;
  mimeType?: string;
  sizes?: string;
}

/** Largest icon we will inline, in characters of data URI. A server controls this
 * string, and a multi-megabyte one would bloat every render for no visual gain. */
const MAX_ICON_CHARS = 64 * 1024;

/** Media types we will render. SVG is deliberately included: an SVG loaded through
 * `<img>` is a passive context, so script inside it does not execute, and the app's
 * `script-src 'self'` blocks it regardless. */
const ALLOWED =
  /^data:image\/(png|jpeg|jpg|gif|webp|svg\+xml|x-icon|vnd\.microsoft\.icon);/i;

/**
 * Pick an icon that is safe to render, or `null`.
 *
 * **Only `data:` URIs.** Icons come from downstream servers, which are the same
 * attacker-controlled surface content-defense exists for, so a remote URL is not a
 * picture — it is a request the app makes to a host of the server's choosing, every
 * time the list paints. That would tell a server when you open Toolport, how often,
 * and from what IP, with no tool call involved.
 *
 * The app's CSP (`img-src 'self' data:`) already blocks remote sources, so this
 * function is the second half of the same decision rather than the only guard: it
 * keeps a blocked URL from rendering as a broken image, and it documents why.
 *
 * If remote icons are ever wanted, the gateway should fetch and cache them so the
 * app never contacts a server host directly — not a widened `img-src`.
 */
export function pickIconSrc(icons?: McpIcon[] | null): string | null {
  if (!Array.isArray(icons)) return null;
  for (const icon of icons) {
    const src = typeof icon?.src === "string" ? icon.src.trim() : "";
    if (!src || src.length > MAX_ICON_CHARS) continue;
    if (!ALLOWED.test(src)) continue;
    return src;
  }
  return null;
}
