/* eslint-disable react-refresh/only-export-components -- 言語storeとhookを同じ入口で提供する。 */
import {
  Fragment,
  useEffect,
  useMemo,
  useSyncExternalStore,
  type ReactNode,
} from "react";

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

// 地域を伴うOS言語も扱う。中国語は地域・scriptにより繁体字を判定する。
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
};
let messages: Messages = {};
let loadedLocale: Locale = "en";
let generation = 0;
const listeners = new Set<() => void>();
const cache = new Map<Locale, Promise<Messages>>();

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

// 世代番号により、遅れて完了した辞書読込が最新の選択を上書きしない。
export async function setLanguage(language: LanguageChoice): Promise<void> {
  if (!isLanguageChoice(language)) return;
  const locale = language === "system" ? systemLocale() : language;
  const request = ++generation;
  try {
    localStorage.setItem(LANGUAGE_STORAGE_KEY, language);
  } catch {
    // 保存できない環境でも、この起動中の言語選択は有効にする。
  }
  snapshot = { language, locale, loading: loadedLocale !== locale, error: false };
  notify();
  try {
    const nextMessages = await loadMessages(locale);
    if (request !== generation) return;
    messages = nextMessages;
    loadedLocale = locale;
    snapshot = { language, locale, loading: false, error: false };
  } catch {
    if (request !== generation) return;
    // 辞書の取得失敗は表示して再選択で再試行できるようにする。
    messages = {};
    loadedLocale = "en";
    snapshot = { language, locale, loading: false, error: true };
  }
  notify();
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
    // snapshot更新に追従し、呼出し元のmemo/callbackも再評価できるidentityにする。
    void current;
    return (message: string, values?: Values) => translate(message, values);
  }, [current]);
  return { ...current, t, setLanguage };
}

export function LanguageProvider({ children }: { children: ReactNode }) {
  const { language, locale } = useI18n();
  useEffect(() => {
    void setLanguage(readLanguage());
    const onStorage = (event: StorageEvent) => {
      if (event.key === LANGUAGE_STORAGE_KEY || event.key === null) {
        void setLanguage(readLanguage());
      }
    };
    const onSystemLanguage = () => {
      if (snapshot.language === "system") void setLanguage("system");
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

// JSX要素を補間する場合もHTML文字列は生成せず、Reactがそのままescapeする。
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
