// Vitest setup for React component tests. The existing src/lib tests use explicit
// vitest imports (globals off), so we register jest-dom matchers and Testing Library
// cleanup here rather than relying on auto-injected globals.
import { afterEach } from "vitest";
import { cleanup } from "@testing-library/react";
import "@testing-library/jest-dom/vitest";

afterEach(() => {
  cleanup();
  // Reset the in-memory storage so localStorage state never leaks across tests.
  storageData.clear();
});

// Node 25+ ships `localStorage` on globalThis as a method-less placeholder for
// the experimental Web Storage API (it only works with `--localstorage-file`).
// Vitest's jsdom environment deliberately skips copying jsdom's own localStorage
// because the key already exists on the Node global, so tests would otherwise
// call into a dead stub (`localStorage.clear is not a function`). Install a
// working in-memory Storage that matches jsdom's semantics.
const storageData = new Map<string, string>();
const memoryStorage: Storage = {
  get length() {
    return storageData.size;
  },
  clear: () => storageData.clear(),
  getItem: (key: string) => storageData.get(key) ?? null,
  key: (index: number) => [...storageData.keys()][index] ?? null,
  removeItem: (key: string) => {
    storageData.delete(key);
  },
  setItem: (key: string, value: string) => {
    storageData.set(key, String(value));
  },
};
Object.defineProperty(globalThis, "localStorage", {
  value: memoryStorage,
  configurable: true,
  writable: true,
});

// jsdom is missing a few DOM APIs that Radix UI (Dialog/Select) calls at runtime.
// Stub them so component tests can render those primitives without throwing.
if (typeof globalThis.ResizeObserver === "undefined") {
  globalThis.ResizeObserver = class {
    observe() {}
    unobserve() {}
    disconnect() {}
  } as unknown as typeof ResizeObserver;
}
const elemProto = Element.prototype as unknown as Record<string, unknown>;
for (const method of [
  "hasPointerCapture",
  "setPointerCapture",
  "releasePointerCapture",
  "scrollIntoView",
]) {
  if (typeof elemProto[method] !== "function") {
    elemProto[method] = () => {};
  }
}

if (typeof window.matchMedia === "undefined") {
  window.matchMedia = ((query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addListener: () => {},
    removeListener: () => {},
    addEventListener: () => {},
    removeEventListener: () => {},
    dispatchEvent: () => false,
  })) as typeof window.matchMedia;
}
