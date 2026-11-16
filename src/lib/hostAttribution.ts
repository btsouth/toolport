import { msg, translate } from "./i18n";
import type { HostAttribution, T3CodeUse } from "./types";

/** The host id T3 Code passes on. Matches `crate::host_attribution::T3_CODE`. */
export const T3_CODE = "t3-code";
/** The product name, never translated. */
export const T3_CODE_NAME = "T3 Code";

/** Product names, not translated. Unknown hosts have none and show nothing extra. */
export function hostName(host?: string | null): string | null {
  return host === T3_CODE ? T3_CODE_NAME : null;
}

/** First 8 characters of a host thread id, with a leading `thread:` dropped. */
export function shortThread(id: string): string {
  const bare = id.startsWith("thread:") ? id.slice("thread:".length) : id;
  return Array.from(bare).slice(0, 8).join("");
}

/** "{client} via T3 Code" for a known host, the bare name otherwise. */
export function hostedClientName(name: string, row: HostAttribution): string {
  return row.host === T3_CODE
    ? translate("{client} via T3 Code", { client: name })
    : name;
}

/** The thread chip's text, tooltip, full id (what it copies) and accessible name, when a
 * known host passed a thread id. */
export function hostThreadChip(
  row: HostAttribution,
): { text: string; title: string; id: string; label: string } | null {
  // Validated again for display, as `host_attribution::valid_thread` does before storage.
  const thread = row.hostThread?.trim();
  if (row.host !== T3_CODE || !thread || !/^[A-Za-z0-9._:-]{1,128}$/.test(thread))
    return null;
  return {
    text: translate("Thread {id}", { id: shortThread(thread) }),
    title: translate("T3 Code thread {id}", { id: thread }),
    id: thread,
    label: translate("Copy thread id {id}", { id: thread }),
  };
}

/** The recent client activity segment for a client some of whose activity came through
 * T3 Code; null otherwise. */
export function t3CodeActivitySegment(row: {
  viaT3Code?: boolean;
  t3CodeSessions?: number;
}): string | null {
  if (!row.viaT3Code) return null;
  const sessions = row.t3CodeSessions ?? 0;
  if (sessions === 0) return translate("Calls via T3 Code");
  return translate(
    sessions === 1
      ? msg("{count} session via T3 Code")
      : msg("{count} sessions via T3 Code"),
    { count: sessions },
  );
}

function useName(use: T3CodeUse): string {
  return use.enabled
    ? use.displayName
    : translate("{name} (off)", { name: use.displayName });
}

/** The Clients row badge for the T3 Code instances that use a client's config: its text
 * and one tooltip line per instance. GTK's `t3_code_badge_text` says the same. */
export function t3CodeBadge(uses: T3CodeUse[]): { text: string; title: string } | null {
  if (uses.length === 0) return null;
  return {
    text: translate("Used by T3 Code: {names}", {
      names: uses.map(useName).join(", "),
    }),
    title: uses
      .map((use) =>
        use.enabled
          ? translate("{name} is on in T3 Code", { name: use.displayName })
          : translate("{name} is off in T3 Code", { name: use.displayName }),
      )
      .join("\n"),
  };
}
