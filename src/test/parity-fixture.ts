// Parity mode for the browser fixture (`npm run parity`). The parity runner
// injects `window.__TOOLPORT_PARITY__` before the app loads: IPC answers the
// Rust side produced from a scenario (scripts/parity/run.mjs). Commands it
// answers come only from there; anything else falls through to the normal
// fixture and is listed in the report, so a screen reading data the scenario
// does not control is visible.

export interface ParityAnswer {
  cmd: string;
  /** Exact arguments, or "*" for any. */
  args: Record<string, unknown> | "*";
  result?: unknown;
  error?: string;
}

interface ParityLog {
  pending: number;
  calls: { cmd: string; args: unknown; source: "scenario" | "fixture" | "missing" }[];
}

interface ParityWindow {
  __TOOLPORT_PARITY__?: { answers: ParityAnswer[] };
  __TOOLPORT_PARITY_LOG__?: ParityLog;
}

/** Sorted-key JSON, so argument order never matters. */
export function canonical(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value && typeof value === "object")
    return `{${Object.keys(value)
      .sort()
      .filter((key) => (value as Record<string, unknown>)[key] !== undefined)
      .map(
        (key) =>
          `${JSON.stringify(key)}:${canonical((value as Record<string, unknown>)[key])}`,
      )
      .join(",")}}`;
  return JSON.stringify(value);
}

export function findAnswer(
  answers: ParityAnswer[],
  command: string,
  args: Record<string, unknown>,
): ParityAnswer | "missing" | undefined {
  const forCommand = answers.filter((answer) => answer.cmd === command);
  if (forCommand.length === 0) return undefined;
  return (
    forCommand.find(
      (answer) => answer.args === "*" || canonical(answer.args) === canonical(args),
    ) ?? "missing"
  );
}

/** Wraps the fixture's IPC handler. Without parity data it changes nothing. */
export function withParity<P>(
  handler: (command: string, payload: P) => unknown,
): (command: string, payload: P) => unknown {
  const target = window as unknown as ParityWindow;
  const data = target.__TOOLPORT_PARITY__;
  if (!data) return handler;
  const log: ParityLog = { pending: 0, calls: [] };
  target.__TOOLPORT_PARITY_LOG__ = log;
  return (command, payload) => {
    const args =
      payload && typeof payload === "object" && !Array.isArray(payload)
        ? (payload as Record<string, unknown>)
        : {};
    const found = findAnswer(data.answers, command, args);
    if (found === "missing") {
      log.calls.push({ cmd: command, args, source: "missing" });
      throw new Error(
        `Parity scenario has no answer for ${command} ${canonical(args)}; add it to the scenario's calls`,
      );
    }
    if (found) {
      log.calls.push({ cmd: command, args, source: "scenario" });
      if (found.error !== undefined) throw new Error(found.error);
      return structuredClone(found.result);
    }
    const call: ParityLog["calls"][number] = { cmd: command, args, source: "fixture" };
    log.calls.push(call);
    let result: unknown;
    try {
      result = handler(command, payload);
    } catch (error) {
      // The default fixture throws for commands it does not implement: that is
      // a missing answer, which must fail the run even if the screen recovers.
      if (
        error instanceof Error &&
        error.message.startsWith("Unimplemented fixture command")
      )
        call.source = "missing";
      throw error;
    }
    if (result instanceof Promise) {
      log.pending += 1;
      const done = () => {
        log.pending -= 1;
      };
      result.then(done, done);
    }
    return result;
  };
}
