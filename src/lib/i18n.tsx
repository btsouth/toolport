/* eslint-disable react-refresh/only-export-components -- Shared language store and React bindings. */
import {
  Fragment,
  useEffect,
  useMemo,
  useSyncExternalStore,
  type ReactNode,
} from "react";
import { getUiLanguage, setUiLanguage } from "./api";

export const LANGUAGES = [
  { code: "en", label: "English" },
  { code: "ja", label: "日本語" },
  { code: "zh-CN", label: "简体中文" },
  { code: "zh-TW", label: "繁體中文" },
  { code: "ko", label: "한국어" },
  { code: "es", label: "Español" },
  { code: "fr", label: "Français" },
  { code: "de", label: "Deutsch" },
  { code: "pt-BR", label: "Português (Brasil)" },
] as const;

declare const sourceMessage: unique symbol;
export type SourceMessage = string & { readonly [sourceMessage]: true };
export function msg<const T extends string>(message: T): T & SourceMessage {
  return message as T & SourceMessage;
}

export type Locale = (typeof LANGUAGES)[number]["code"];
export type LanguageChoice = "system" | Locale;
export const LANGUAGE_STORAGE_KEY = "toolport-language";
type Messages = Record<string, string>;
type Values = Record<string, string | number>;

const loaders: Record<Exclude<Locale, "en">, () => Promise<{ default: Messages }>> = {
  ja: () => import("../locales/ja"),
  "zh-CN": () => import("../locales/zh-CN"),
  "zh-TW": () => import("../locales/zh-TW"),
  ko: () => import("../locales/ko"),
  es: () => import("../locales/es"),
  fr: () => import("../locales/fr"),
  de: () => import("../locales/de"),
  "pt-BR": () => import("../locales/pt-BR"),
};

export function isLanguageChoice(value: unknown): value is LanguageChoice {
  return value === "system" || LANGUAGES.some(({ code }) => code === value);
}

export function resolveLocale(languages: readonly string[]): Locale {
  for (const tag of languages) {
    const normalized = tag.replace(/_/g, "-").toLowerCase();
    const base = normalized.split("-")[0];
    if (base === "zh") {
      const parts = normalized.split("-");
      if (parts.includes("hant")) return "zh-TW";
      if (parts.includes("hans")) return "zh-CN";
      return /(?:^|-)(?:hant|tw|hk|mo)(?:-|$)/.test(normalized) ? "zh-TW" : "zh-CN";
    }
    if (base === "pt") return "pt-BR";
    const match = LANGUAGES.find(({ code }) => code === base);
    if (match) return match.code;
  }
  return "en";
}

function systemLocale(): Locale {
  return typeof navigator === "undefined"
    ? "en"
    : resolveLocale(
        navigator.languages?.length ? navigator.languages : [navigator.language],
      );
}

function readLanguage(): LanguageChoice {
  try {
    const stored = localStorage.getItem(LANGUAGE_STORAGE_KEY);
    return isLanguageChoice(stored) ? stored : "system";
  } catch {
    return "system";
  }
}

const initialLanguage = readLanguage();
let snapshot = {
  language: initialLanguage,
  locale: initialLanguage === "system" ? systemLocale() : initialLanguage,
  loading: false,
  error: false,
  saveError: false,
};
let messages: Messages = {};
let loadedLocale: Locale = "en";
let generation = 0;
let saves: Promise<void> = Promise.resolve();
const listeners = new Set<() => void>();
const cache = new Map<Locale, Promise<Messages>>();

/** Wait for the selected UI language to reach the shared native message catalog. */
export async function awaitLanguagePersistence(): Promise<void> {
  await saves;
}

function notify() {
  for (const listener of listeners) listener();
}

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function getSnapshot() {
  return snapshot;
}

export function getLocale(): Locale {
  return snapshot.locale;
}

function loadMessages(locale: Locale): Promise<Messages> {
  if (locale === "en") return Promise.resolve({});
  let pending = cache.get(locale);
  if (!pending) {
    pending = loaders[locale]().then((module) => module.default);
    cache.set(locale, pending);
    void pending.catch(() => cache.delete(locale));
  }
  return pending;
}

function cacheLanguage(language: LanguageChoice) {
  try {
    localStorage.setItem(LANGUAGE_STORAGE_KEY, language);
  } catch {
    // Storage can be unavailable in private or restricted webviews.
  }
}

async function applyLanguage(
  language: LanguageChoice,
  request: number,
  saveError = false,
) {
  const locale = language === "system" ? systemLocale() : language;
  if (request !== generation) return;
  snapshot = {
    ...snapshot,
    language,
    loading: loadedLocale !== locale,
    error: false,
    saveError,
  };
  notify();
  try {
    const nextMessages = await loadMessages(locale);
    if (request !== generation) return;
    messages = nextMessages;
    loadedLocale = locale;
    snapshot = { language, locale, loading: false, error: false, saveError };
  } catch {
    if (request !== generation) return;
    messages = {};
    loadedLocale = "en";
    snapshot = { language, locale: "en", loading: false, error: true, saveError };
  }
  document.documentElement.lang = snapshot.locale;
  document.documentElement.dir = "ltr";
  notify();
}

/** Resolve machine settings and its dictionary before mounting any UI. */
export async function initializeLanguage(): Promise<void> {
  const request = ++generation;
  const cached = readLanguage();
  const response = Promise.resolve()
    .then(getUiLanguage)
    .then((saved) => {
      if (!isLanguageChoice(saved)) throw new Error("Invalid language setting");
      return saved;
    });
  const answer = response.then(
    (language) => ({ kind: "saved" as const, language }),
    () => ({ kind: "failed" as const }),
  );
  let timer: ReturnType<typeof setTimeout> | undefined;
  const deadline = new Promise<{ kind: "timeout" }>((resolve) => {
    timer = setTimeout(() => resolve({ kind: "timeout" }), 1500);
  });
  const result = await Promise.race([answer, deadline]);
  clearTimeout(timer);
  if (result.kind === "saved") {
    if (request === generation) cacheLanguage(result.language);
    await applyLanguage(result.language, request);
    return;
  }
  if (result.kind === "timeout") {
    // Keep the late authoritative reply useful, but a selection made after
    // bootstrap takes precedence. A new generation also cancels cached loads.
    void answer.then(async (late) => {
      if (late.kind !== "saved" || request !== generation) return;
      const latest = ++generation;
      cacheLanguage(late.language);
      await applyLanguage(late.language, latest);
    });
  }
  await applyLanguage(cached, request, true);
}

export async function setLanguage(language: LanguageChoice): Promise<void> {
  if (!isLanguageChoice(language)) throw new Error("Invalid language choice");
  const request = ++generation;
  // Serialize persistence as well as guarding display updates: an older slow save
  // must never overwrite a newer choice on disk.
  const save = saves.then(() => setUiLanguage(language));
  saves = save.catch(() => undefined);
  const applying = applyLanguage(language, request);
  try {
    await save;
    if (request === generation) cacheLanguage(language);
  } catch {
    await applying;
    if (request === generation) {
      snapshot = { ...snapshot, saveError: true };
      notify();
    }
    return;
  }
  await applying;
}

function translatedMessage(message: string): string {
  return Object.prototype.hasOwnProperty.call(messages, message)
    ? messages[message]
    : message;
}

export function translate(message: string, values: Values = {}): string {
  const translated = translatedMessage(message);
  return translated.replace(/\{([a-zA-Z][a-zA-Z0-9_]*)\}/g, (token, name: string) =>
    Object.prototype.hasOwnProperty.call(values, name) ? String(values[name]) : token,
  );
}

export function useI18n() {
  const current = useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
  const t = useMemo(() => {
    void current;
    return (message: string, values?: Values) => translate(message, values);
  }, [current]);
  return { ...current, t, setLanguage };
}

export function LanguageProvider({ children }: { children: ReactNode }) {
  const { language, locale } = useI18n();
  useEffect(() => {
    const onStorage = (event: StorageEvent) => {
      if (event.key === LANGUAGE_STORAGE_KEY || event.key === null) {
        void initializeLanguage();
      }
    };
    const onSystemLanguage = () => {
      if (snapshot.language === "system") void applyLanguage("system", ++generation);
    };
    window.addEventListener("storage", onStorage);
    window.addEventListener("languagechange", onSystemLanguage);
    return () => {
      window.removeEventListener("storage", onStorage);
      window.removeEventListener("languagechange", onSystemLanguage);
    };
  }, []);
  useEffect(() => {
    document.documentElement.lang = locale;
    document.documentElement.dir = "ltr";
  }, [language, locale]);
  return children;
}

export function Trans({
  message,
  values = {},
}: {
  message: string;
  values?: Record<string, ReactNode>;
}) {
  useI18n();
  const translated = translatedMessage(message);
  return translated.split(/(\{[a-zA-Z][a-zA-Z0-9_]*\})/g).map((part, index) => {
    const name = part.slice(1, -1);
    return (
      <Fragment key={index}>
        {Object.prototype.hasOwnProperty.call(values, name) ? values[name] : part}
      </Fragment>
    );
  });
}

/** Commands, paths and protocol examples shown verbatim, outside the catalog. */
export function technicalText(value: string): string {
  return value;
}
