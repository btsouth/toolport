import { describe, expect, it } from "vitest";
import {
  isTextEntry,
  resolveShortcut,
  shortcutHelp,
  SHORTCUT_VIEWS,
  type ShortcutEvent,
} from "./shortcuts";

function key(k: string, mods: Partial<ShortcutEvent> = {}): ShortcutEvent {
  return {
    key: k,
    ctrlKey: false,
    metaKey: false,
    altKey: false,
    shiftKey: false,
    ...mods,
  };
}

describe("resolveShortcut (SBS-143)", () => {
  it("maps Ctrl+1..6 to the six views in order", () => {
    SHORTCUT_VIEWS.forEach((view, i) => {
      expect(resolveShortcut(key(String(i + 1), { ctrlKey: true }))).toEqual({
        kind: "view",
        view,
      });
    });
  });

  it("accepts Cmd as well as Ctrl", () => {
    expect(resolveShortcut(key("1", { metaKey: true }))).toEqual({
      kind: "view",
      view: "servers",
    });
  });

  it("does not claim a seventh number", () => {
    expect(resolveShortcut(key("7", { ctrlKey: true }))).toBeNull();
  });

  it("focuses search on bare / only outside a text field", () => {
    expect(resolveShortcut(key("/"))).toEqual({ kind: "focusSearch" });
    // Typing "/" into a field must insert the character, not steal focus.
    expect(resolveShortcut(key("/"), { tagName: "INPUT" })).toBeNull();
    expect(resolveShortcut(key("/"), { isContentEditable: true })).toBeNull();
  });

  it("keeps modifier chords working while typing", () => {
    // An explicit chord is never a character the user meant to enter.
    expect(resolveShortcut(key("f", { ctrlKey: true }), { tagName: "INPUT" })).toEqual({
      kind: "focusSearch",
    });
    expect(resolveShortcut(key("2", { ctrlKey: true }), { tagName: "TEXTAREA" })).toEqual(
      {
        kind: "view",
        view: "activity",
      },
    );
  });

  it("maps the action chords", () => {
    expect(resolveShortcut(key("n", { ctrlKey: true }))).toEqual({ kind: "addServer" });
    expect(resolveShortcut(key("r", { ctrlKey: true }))).toEqual({ kind: "refresh" });
    // Case-insensitive: Shift+Ctrl+N still reads as N.
    expect(resolveShortcut(key("N", { ctrlKey: true, shiftKey: true }))).toEqual({
      kind: "addServer",
    });
  });

  it("opens help on ? and closes it on Escape even from a field", () => {
    expect(resolveShortcut(key("?"))).toEqual({ kind: "help" });
    expect(resolveShortcut(key("?"), { tagName: "INPUT" })).toBeNull();
    // Escape means the same thing everywhere else in the app, so it works while typing.
    expect(resolveShortcut(key("Escape"), { tagName: "INPUT" })).toEqual({
      kind: "closeHelp",
    });
  });

  it("leaves Alt chords to the OS", () => {
    expect(resolveShortcut(key("1", { ctrlKey: true, altKey: true }))).toBeNull();
    expect(resolveShortcut(key("/", { altKey: true }))).toBeNull();
  });

  it("ignores ordinary typing", () => {
    for (const k of ["a", "Enter", "Tab", "ArrowDown", " "]) {
      expect(resolveShortcut(key(k))).toBeNull();
    }
  });
});

describe("isTextEntry", () => {
  it("treats inputs, textareas, selects and contenteditable as typing surfaces", () => {
    expect(isTextEntry({ tagName: "INPUT" })).toBe(true);
    expect(isTextEntry({ tagName: "textarea" })).toBe(true);
    expect(isTextEntry({ tagName: "SELECT" })).toBe(true);
    expect(isTextEntry({ isContentEditable: true })).toBe(true);
    expect(isTextEntry({ tagName: "DIV" })).toBe(false);
    expect(isTextEntry(null)).toBe(false);
  });
});

describe("shortcutHelp", () => {
  it("names the platform modifier so a Mac user is not told to press Ctrl", () => {
    expect(shortcutHelp(true)[0].keys).toContain("⌘");
    expect(shortcutHelp(false)[0].keys).toContain("Ctrl");
  });

  it("documents every action the resolver can produce", () => {
    const rows = shortcutHelp(false);
    expect(rows).toHaveLength(6);
    expect(rows.every((r) => r.keys && r.what)).toBe(true);
  });
});
