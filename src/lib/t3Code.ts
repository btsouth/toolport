import { translate } from "./i18n";
import type { T3CodeUse } from "./types";

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
