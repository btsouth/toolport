/**
 * Up to 8 characters of an agent's name, for under its column logo in the Skills grid
 * ("Claude", "Copilot"). The GTK shell's `short_agent_name` in linux_native/skills.rs matches.
 */
export function shortAgentName(name: string): string {
  let short = name.replace(/\s+CLI$/i, "");
  if (short.length > 8) short = short.split(/\s+/)[0];
  return cap(short);
}

/** At most 8 characters, the last an ellipsis when cut. */
function cap(text: string): string {
  return text.length > 8 ? `${text.slice(0, 7)}…` : text;
}

/**
 * The labels under the column logos. Profiles of one client share a logo, so they are named
 * by their profile ("claude2", "becca" for `Codex (codex-becca)`) instead of all "Claude".
 * The GTK shell's `column_names` matches.
 */
export function columnNames(names: string[]): string[] {
  const parts = names.map((name) => {
    const match = /^(.*) \((.+)\)$/.exec(name.trim());
    return match
      ? { base: match[1].trim(), profile: match[2].trim() }
      : { base: name.trim(), profile: undefined };
  });
  return parts.map(({ base, profile }) => {
    const shared = parts.filter((other) => other.base === base).length > 1;
    if (!profile || !shared) return shortAgentName(base);
    const client = (base.split(/\s+/)[0] ?? "").toLowerCase();
    const rest = profile.startsWith(`${client}-`) ? profile.slice(client.length + 1) : "";
    return cap(rest || profile);
  });
}
