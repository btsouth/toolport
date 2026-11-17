import { msg, translate } from "./i18n";
import type { DetectedClient, HostAttribution, T3CodeOther, T3CodeUse } from "./types";

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

/** Rows an enabled instance uses that Toolport can connect now: installed, readable, not
 * connected and not customized. Keeps the given order. GTK's `t3_code_targets` matches. */
export function t3CodeTargets(clients: DetectedClient[]): DetectedClient[] {
  return clients.filter(
    (client) =>
      client.t3Code?.some((use) => use.enabled) &&
      client.appPresent &&
      !client.error &&
      !client.gatewayInstalled &&
      client.entryState !== "customized",
  );
}

/** By code point, the order Rust's `str` comparison uses. */
function compareCodePoints(a: string, b: string): number {
  const left = [...a];
  const right = [...b];
  for (let index = 0; index < Math.min(left.length, right.length); index++) {
    const difference = left[index].codePointAt(0)! - right[index].codePointAt(0)!;
    if (difference !== 0) return difference;
  }
  return left.length - right.length;
}

/** The order rows (and so Connect all's steps) are listed in among rows of the same
 * status, and a group's profiles by command or folder: by Unicode lowercase text, then by
 * id. GTK's `state::name_order` is the same rule. */
export function compareClientNames(
  a: { name: string; id: string },
  b: { name: string; id: string },
): number {
  return (
    compareCodePoints(a.name.toLowerCase(), b.name.toLowerCase()) ||
    compareCodePoints(a.id, b.id)
  );
}

/** Enabled instances on the given rows; the card counts instances, not rows. */
export function t3CodeInstanceCount(targets: DetectedClient[]): number {
  return targets.reduce(
    (sum, client) => sum + (client.t3Code ?? []).filter((use) => use.enabled).length,
    0,
  );
}

/** Why a row an enabled instance uses is not connected and can't be connected as it is. */
export type T3CodeLeftOutReason = "unreadable" | "customized" | "missing";

/** A row left out that way, with the enabled instances that use it. */
export interface T3CodeLeftOut {
  client: DetectedClient;
  reason: T3CodeLeftOutReason;
  names: string[];
}

const leftOutOrder: T3CodeLeftOutReason[] = ["customized", "unreadable", "missing"];

/** Rows used by an enabled instance that are neither connected nor targets,
 * with why (the row's own status): customized, then unreadable, then not installed, each
 * in the given (scan) order. GTK's `t3_code_left_out` finds the same, in the same order. */
export function t3CodeLeftOut(clients: DetectedClient[]): T3CodeLeftOut[] {
  return leftOutOrder.flatMap((want) =>
    leftOutOf(clients).filter((row) => row.reason === want),
  );
}

function leftOutOf(clients: DetectedClient[]): T3CodeLeftOut[] {
  return clients.flatMap((client) => {
    const names = (client.t3Code ?? [])
      .filter((use) => use.enabled)
      .map((use) => use.displayName);
    if (names.length === 0) return [];
    const reason: T3CodeLeftOutReason | null = client.error
      ? "unreadable"
      : client.entryState === "customized"
        ? "customized"
        : client.gatewayInstalled || client.appPresent
          ? null
          : "missing";
    return reason ? [{ client, reason, names }] : [];
  });
}

/** One line per left-out row: who uses it, why it isn't connected, what to do. */
export function t3CodeLeftOutLines(left: T3CodeLeftOut[]): string[] {
  return left.map(({ client, reason, names }) => {
    const one = names.length === 1;
    const values = { names: names.join(", "), client: client.name };
    switch (reason) {
      case "unreadable":
        return translate(
          one
            ? msg(
                "{names} uses {client}, but Toolport can't read its config. Open {client} below to see why, then connect it.",
              )
            : msg(
                "{names} use {client}, but Toolport can't read its config. Open {client} below to see why, then connect it.",
              ),
          values,
        );
      case "customized":
        return translate(
          one
            ? msg(
                "{names} uses {client}, whose Toolport entry was changed by hand. Open {client} below and reset its Toolport entry.",
              )
            : msg(
                "{names} use {client}, whose Toolport entry was changed by hand. Open {client} below and reset its Toolport entry.",
              ),
          values,
        );
      case "missing":
        return translate(
          one
            ? msg(
                "{names} uses {client}, which isn't installed. Install {client}, then connect it.",
              )
            : msg(
                "{names} use {client}, which isn't installed. Install {client}, then connect it.",
              ),
          values,
        );
    }
  });
}

/** Enabled instances that are not connected: the targets', the left-out rows', and the
 * ones Toolport can't connect at all. Only zero means every enabled one is connected. */
export function t3CodeUnconnectedCount(
  targets: DetectedClient[],
  left: T3CodeLeftOut[],
  others: T3CodeOther[],
): number {
  return (
    t3CodeInstanceCount(targets) +
    left.reduce((sum, row) => sum + row.names.length, 0) +
    others.filter((other) => other.enabled && other.gap !== "unreadable").length
  );
}

/** Whether any instance on the rows is on, or any is counted as not connected. With every
 * instance off there is nothing to say is connected, so the card shows no status line. */
export function t3CodeAnyEnabled(
  clients: DetectedClient[],
  unconnected: number,
): boolean {
  return (
    unconnected > 0 ||
    clients.some((client) => (client.t3Code ?? []).some((instance) => instance.enabled))
  );
}

/** The card's status line. */
export function t3CodeStatus(count: number): string {
  if (count === 0) return translate("Every enabled T3 Code instance is connected.");
  return translate(
    count === 1
      ? msg("{count} enabled T3 Code instance is not connected.")
      : msg("{count} enabled T3 Code instances are not connected."),
    { count },
  );
}

function otherName(other: T3CodeOther): string {
  const name =
    !other.providerName || other.displayName === other.providerName
      ? other.displayName
      : translate("{name} ({provider})", {
          name: other.displayName,
          provider: other.providerName,
        });
  return other.enabled ? name : translate("{name} (off)", { name });
}

/** The card's lines for instances Toolport cannot connect, one per reason. */
export function t3CodeGapLines(others: T3CodeOther[]): string[] {
  const of = (gap: T3CodeOther["gap"]) => others.filter((other) => other.gap === gap);
  const names = (gap: T3CodeOther["gap"]) => of(gap).map(otherName).join(", ");
  const lines: string[] = [];
  const unsupported = names("unsupported");
  if (unsupported)
    lines.push(
      translate("Not supported by Toolport yet: {names}.", { names: unsupported }),
    );
  const malformed = of("malformed");
  if (malformed.length > 0)
    lines.push(
      translate(
        malformed.length === 1
          ? msg(
              "Toolport can't read {names} in T3 Code's settings. Fix this instance in T3 Code's settings.",
            )
          : msg(
              "Toolport can't read {names} in T3 Code's settings. Fix these instances in T3 Code's settings.",
            ),
        { names: names("malformed") },
      ),
    );
  const hiddenHome = of("hiddenHome");
  if (hiddenHome.length > 0)
    lines.push(
      translate(
        hiddenHome.length === 1
          ? msg(
              "Toolport can't see which home folder {names} uses: it is marked sensitive or isn't plain text. Set its home folder as plain text in T3 Code's provider settings.",
            )
          : msg(
              "Toolport can't see which home folders {names} use: they are marked sensitive or aren't plain text. Set each home folder as plain text in T3 Code's provider settings.",
            ),
        { names: names("hiddenHome") },
      ),
    );
  const relativeHome = of("relativeHome");
  if (relativeHome.length > 0)
    lines.push(
      translate(
        relativeHome.length === 1
          ? msg(
              "Toolport can't tell which home folder {names} uses: it is a relative path. Set it to a full path in T3 Code's provider settings.",
            )
          : msg(
              "Toolport can't tell which home folders {names} use: they are relative paths. Set each to a full path in T3 Code's provider settings.",
            ),
        { names: names("relativeHome") },
      ),
    );
  const outsideHome = of("outsideHome");
  if (outsideHome.length > 0)
    lines.push(
      translate(
        outsideHome.length === 1
          ? msg(
              "Toolport won't use the home folder {names} uses: it isn't written as a plain folder inside your home folder. Toolport only manages configs inside it; add it by hand as a client location if you trust it.",
            )
          : msg(
              "Toolport won't use the home folders {names} use: they aren't written as plain folders inside your home folder. Toolport only manages configs inside it; add each by hand as a client location if you trust it.",
            ),
        { names: names("outsideHome") },
      ),
    );
  const linkedHome = of("linkedHome");
  if (linkedHome.length > 0)
    lines.push(
      translate(
        linkedHome.length === 1
          ? msg(
              "Toolport won't use the home folder {names} uses: it, or its config file, is a link or not a plain folder or file. Point it at the real folder in T3 Code's provider settings.",
            )
          : msg(
              "Toolport won't use the home folders {names} use: they, or their config files, are links or not plain folders or files. Point each at the real folder in T3 Code's provider settings.",
            ),
        { names: names("linkedHome") },
      ),
    );
  const missingHome = of("missingHome");
  if (missingHome.length > 0)
    lines.push(
      translate(
        missingHome.length === 1
          ? msg(
              "Toolport can't connect {names} yet: its home folder doesn't exist yet. Start it once in T3 Code, then refresh Clients.",
            )
          : msg(
              "Toolport can't connect {names} yet: their home folders don't exist yet. Start each once in T3 Code, then refresh Clients.",
            ),
        { names: names("missingHome") },
      ),
    );
  const unreadable = others.find((other) => other.gap === "unreadable");
  if (unreadable)
    lines.push(
      translate(
        "Toolport can't read T3 Code's settings in {path}, so it can't list T3 Code's instances.",
        { path: unreadable.displayName },
      ),
    );
  return lines;
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
          : translate(
              "{name} is off in T3 Code. Connect all skips it; you can still connect it here.",
              { name: use.displayName },
            ),
      )
      .join("\n"),
  };
}

/** The enabled instances on a row, by name, as each Connect all step lists them. */
export function t3CodeEnabledNames(client: DetectedClient | undefined): string[] {
  return (client?.t3Code ?? [])
    .filter((use) => use.enabled)
    .map((use) => use.displayName);
}

/** What `connectAllSummary` reports, worked out when the batch ends against the rows as
 * they are then: an instance turned off or removed during the batch is neither counted
 * nor named. `connectedIds` are the rows connected, `notConnectedIds` the rows skipped,
 * failed or never reached, in step order. Left-out rows are found again, without the
 * batch's own rows. GTK's `t3_code_batch_result` does the same. */
export function connectAllOutcome(
  clients: DetectedClient[],
  connectedIds: string[],
  notConnectedIds: string[],
): { connected: number; total: number; notConnected: string[]; leftOut: string[] } {
  const byId = (id: string) => clients.find((client) => client.id === id);
  // The rescan decides, as the row shows it: connected when its entry is Toolport's and
  // not customized, whatever the step reported (a connect that took effect but reported
  // a failure counts; one undone since does not).
  const connectedNow = (id: string) => {
    const client = byId(id);
    return client?.gatewayInstalled === true && client.entryState !== "customized";
  };
  const rows = [...connectedIds, ...notConnectedIds];
  const connected = rows
    .filter(connectedNow)
    .reduce((sum, id) => sum + t3CodeEnabledNames(byId(id)).length, 0);
  const notConnected = rows
    .filter((id) => !connectedNow(id))
    .flatMap((id) => t3CodeEnabledNames(byId(id)));
  const batchIds = new Set([...connectedIds, ...notConnectedIds]);
  return {
    connected,
    total: connected + notConnected.length,
    notConnected,
    leftOut: t3CodeLeftOutLines(
      t3CodeLeftOut(clients).filter((row) => !batchIds.has(row.client.id)),
    ),
  };
}

/** The summary when Connect all ends: "[Connect all stopped. ]Connected X of Y T3 Code
 * instance(s).[ Not connected: NAMES.][ LEFT OUT LINES]". `stopped` is true when the user
 * ended it (Cancel, close or Escape). A full success only when nothing was stopped, left
 * unconnected or left out. GTK's `t3_code_batch_summary` builds the same text. */
export function connectAllSummary({
  connected,
  total,
  stopped,
  notConnected,
  leftOut = [],
}: {
  connected: number;
  total: number;
  stopped: boolean;
  notConnected: string[];
  leftOut?: string[];
}): {
  text: string;
  title: string;
  detail: string;
  success: boolean;
  tone: "success" | "warning" | "info";
} {
  const parts = [
    stopped && translate("Connect all stopped."),
    translate(
      total === 1
        ? msg("Connected {connected} of {total} T3 Code instance.")
        : msg("Connected {connected} of {total} T3 Code instances."),
      { connected, total },
    ),
    notConnected.length > 0 &&
      translate("Not connected: {names}.", { names: notConnected.join(", ") }),
    ...leftOut,
  ].filter(Boolean) as string[];
  // The card's title is the first sentence; the rest is its detail. GTK splits the same.
  const [title, ...rest] = parts;
  const success = !stopped && notConnected.length === 0 && leftOut.length === 0;
  return {
    text: parts.join(" "),
    title,
    detail: rest.join(" "),
    success,
    // Neutral when nothing was left to count (every row gone or off by the end), not a
    // green "0 of 0". GTK picks the same tone.
    tone: success ? (total === 0 ? "info" : "success") : "warning",
  };
}
