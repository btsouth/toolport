import type { View } from "./types";

/** Views in the order their number key selects them (SBS-143). */
export const SHORTCUT_VIEWS: View[] = [
  "servers",
  "activity",
  "catalog",
  "playground",
  "teams",
  "settings",
];

/** What a keystroke resolved to, or `null` when it is not a shortcut. */
export type ShortcutAction =
  | { kind: "view"; view: View }
  | { kind: "focusSearch" }
  | { kind: "addServer" }
  | { kind: "refresh" }
  | { kind: "help" }
  | { kind: "closeHelp" };

/** The subset of a keyboard event this needs, so it is testable without a DOM. */
export interface ShortcutEvent {
  key: string;
  ctrlKey: boolean;
  metaKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
}

/** Where the keystroke landed. A bare `/` must reach the page, not steal a character
 * from whatever the user is typing into. */
export interface ShortcutTarget {
  tagName?: string;
  isContentEditable?: boolean;
}

const TEXT_ENTRY_TAGS = new Set(["INPUT", "TEXTAREA", "SELECT"]);

/** True when the keystroke is being typed into a field, so bare-key shortcuts defer. */
export function isTextEntry(target: ShortcutTarget | null | undefined): boolean {
  if (!target) return false;
  if (target.isContentEditable) return true;
  return TEXT_ENTRY_TAGS.has((target.tagName ?? "").toUpperCase());
}

/**
 * Resolve a keystroke to an action, or `null`.
 *
 * Pure and DOM-free on purpose: the interesting behaviour here is which keys are
 * claimed and which are deliberately left alone, and that is worth testing directly
 * rather than through a rendered tree.
 *
 * Rules:
 *   * `Ctrl/Cmd+1..6` switch views, and work even while typing — an explicit modifier
 *     chord is never something the user meant as text.
 *   * `/` focuses search, but only outside a text field, or it eats the character.
 *   * `Ctrl/Cmd+F` also focuses search, everywhere. `Ctrl/Cmd+N` adds a server,
 *     `Ctrl/Cmd+R` refreshes.
 *   * `?` opens the cheat sheet, `Escape` closes it.
 *   * Anything with `Alt` is left alone: Alt chords are OS/menu territory.
 */
export function resolveShortcut(
  event: ShortcutEvent,
  target?: ShortcutTarget | null,
): ShortcutAction | null {
  if (event.altKey) return null;
  const mod = event.ctrlKey || event.metaKey;
  const typing = isTextEntry(target);

  if (mod) {
    const index = SHORTCUT_VIEWS.findIndex((_, i) => event.key === String(i + 1));
    if (index >= 0) return { kind: "view", view: SHORTCUT_VIEWS[index] };
    switch (event.key.toLowerCase()) {
      case "f":
        return { kind: "focusSearch" };
      case "n":
        return { kind: "addServer" };
      case "r":
        return { kind: "refresh" };
      default:
        return null;
    }
  }

  // Bare keys. Escape closes the sheet even from a field, since that is what Escape
  // means everywhere else in the app.
  if (event.key === "Escape") return { kind: "closeHelp" };
  if (typing) return null;
  if (event.key === "/") return { kind: "focusSearch" };
  if (event.key === "?") return { kind: "help" };
  return null;
}

/** One row of the `?` cheat sheet. `keys` is already display-formatted. */
export interface ShortcutHelpRow {
  keys: string;
  what: string;
}

/** The cheat sheet, built for the platform so a Mac user is not told to press Ctrl. */
export function shortcutHelp(isMac: boolean): ShortcutHelpRow[] {
  const mod = isMac ? "⌘" : "Ctrl";
  return [
    { keys: `${mod}1 – ${mod}6`, what: "Switch view" },
    { keys: `/ or ${mod}F`, what: "Focus the server search" },
    { keys: `${mod}N`, what: "Add a server" },
    { keys: `${mod}R`, what: "Refresh servers and clients" },
    { keys: "?", what: "Show this list" },
    { keys: "Esc", what: "Close this list" },
  ];
}
