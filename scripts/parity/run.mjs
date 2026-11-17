#!/usr/bin/env node
// GTK vs React parity report (report-only; never a gate).
//
//   npm run parity [-- --only <scenario name part>]
//
// Builds the GTK shell's test binary, renders each scenario in
// parity/scenarios/ with the real GTK page under xvfb (Rust also answers the
// scenario's IPC calls), renders the same answers in the React browser
// fixture, and prints every difference. Writes report.json and report.txt
// under .verify/parity/<run>/. Exits 0 whatever the shells show; exits 1 only
// when the tool itself could not read a screen. Run it on devbox:
// scripts/verify-devbox.sh parity.
/* global window */
import { spawn, spawnSync } from "node:child_process";
import { existsSync, realpathSync } from "node:fs";
import { mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { applyKnown, diffNodes, formatReport, validateKnown } from "./diff.mjs";
import { extractReact, installMutationCounter, pageIsIdle, selfCheck } from "./react.mjs";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const argv = process.argv.slice(2);
const only = argv.includes("--only") ? argv[argv.indexOf("--only") + 1] : null;
const started = Date.now();
const out = path.join(root, ".verify", "parity", `run-${started}`);
await mkdir(out, { recursive: true });
const say = (line) => process.stderr.write(`${line}\n`);
const problems = [];

// 0. The differ's own tests: quick, and they guard what the report says.
{
  const test = spawnSync(
    process.execPath,
    ["--test", path.join(root, "scripts/parity/diff.test.mjs")],
    {
      encoding: "utf8",
    },
  );
  if (test.status !== 0) {
    say(test.stdout + test.stderr);
    problems.push("diff.test.mjs failed");
  }
}

const scenarioDir = path.join(root, "parity/scenarios");
const scenarios = [];
for (const file of (await readdir(scenarioDir))
  .filter((name) => name.endsWith(".json"))
  .sort()) {
  const name = file.replace(/\.json$/, "");
  if (only && !name.includes(only)) continue;
  scenarios.push({
    name,
    file: path.join(scenarioDir, file),
    ...JSON.parse(await readFile(path.join(scenarioDir, file), "utf8")),
  });
}
if (!scenarios.length) throw new Error(`no scenarios match ${only}`);
const knownList = JSON.parse(
  await readFile(path.join(root, "parity/known-differences.json"), "utf8"),
);
problems.push(
  ...validateKnown(knownList).map((problem) => `known-differences.json: ${problem}`),
);

// 1. GTK: one process per scenario (the data dir and clock are process-wide).
const manifest = path.join(root, "src-tauri/Cargo.toml");
const cargoArgs = [
  "--manifest-path",
  manifest,
  "--no-default-features",
  "--features",
  "gtk-desktop",
  "--lib",
];
let testBinary;
{
  const t0 = Date.now();
  say("Building the GTK test binary (cargo test --no-run)...");
  const build = spawnSync(
    "cargo",
    [
      "test",
      "--locked",
      ...cargoArgs,
      "--no-run",
      "--message-format=json-render-diagnostics",
    ],
    {
      cwd: root,
      encoding: "utf8",
      maxBuffer: 256 * 1024 * 1024,
      stdio: ["ignore", "pipe", "inherit"],
    },
  );
  for (const line of build.stdout.split("\n")) {
    if (!line.startsWith("{")) continue;
    const message = JSON.parse(line);
    if (
      message.reason === "compiler-artifact" &&
      message.profile?.test &&
      message.target?.src_path?.endsWith("lib.rs") &&
      message.executable
    )
      testBinary = message.executable;
  }
  if (build.status !== 0 || !testBinary) {
    say("cargo could not build the GTK test binary; see the errors above");
    process.exit(1);
  }
  say(`GTK test binary ready (${((Date.now() - t0) / 1000).toFixed(1)} s)`);
}

const home = await mkdtemp(path.join(os.tmpdir(), "toolport-parity-home-"));
const gtkEnv = {
  ...process.env,
  HOME: home,
  XDG_CONFIG_HOME: path.join(home, ".config"),
  XDG_DATA_HOME: path.join(home, ".local/share"),
  XDG_CACHE_HOME: path.join(home, ".cache"),
  TZ: "UTC",
  LANG: "en_US.UTF-8",
  LANGUAGE: "en",
  LC_ALL: "C.UTF-8",
  GSK_RENDERER: "cairo",
  GTK_A11Y: "none",
  GDK_BACKEND: "x11",
  WAYLAND_DISPLAY: "",
  NO_AT_BRIDGE: "1",
};
function runGtkTest(testName, extraEnv) {
  return new Promise((resolve) => {
    // Always in a virtual display: never a window on a real desktop.
    const child = spawn(
      "xvfb-run",
      [
        "-a",
        "dbus-run-session",
        "--",
        testBinary,
        "--exact",
        "--ignored",
        "--nocapture",
        "--test-threads=1",
        testName,
      ],
      {
        cwd: path.join(root, "src-tauri"),
        env: { ...gtkEnv, ...extraEnv },
        stdio: ["ignore", "pipe", "pipe"],
        // Its own process group, so the deadline stops dbus-run-session and
        // the GTK test too, not only xvfb-run.
        detached: true,
      },
    );
    let output = "";
    child.stdout.on("data", (chunk) => (output += chunk));
    child.stderr.on("data", (chunk) => (output += chunk));
    const timer = setTimeout(() => {
      try {
        process.kill(-child.pid, "SIGKILL");
      } catch {
        child.kill("SIGKILL");
      }
    }, 120_000);
    child.on("close", (status) => {
      clearTimeout(timer);
      resolve({ ok: status === 0 && /test result: ok\. 1 passed/.test(output), output });
    });
  });
}
const failureOf = (output) =>
  (
    output.match(/panicked at [^\n]*\n([\s\S]*?)(\nnote:|\nstack backtrace|$)/)?.[1] ??
    output.slice(-2000)
  ).trim();

{
  const check = await runGtkTest("linux_native::parity::tests::extractor_self_check", {});
  if (!check.ok)
    problems.push(`GTK extractor self-check failed:\n${failureOf(check.output)}`);
  else say("GTK extractor self-check passed");
}
const gtkTimes = {};
for (const scenario of scenarios) {
  const t0 = Date.now();
  const result = await runGtkTest("linux_native::parity::tests::extract_scenario", {
    PARITY_SCENARIO: scenario.file,
    PARITY_OUT: out,
  });
  gtkTimes[scenario.name] = Date.now() - t0;
  if (!result.ok) {
    scenario.error = `GTK: ${failureOf(result.output)}`;
    await writeFile(path.join(out, `${scenario.name}.gtk.log`), result.output);
  }
}
await rm(home, { recursive: true, force: true });

// 2. React: the browser fixture, answering from each scenario's ipc.json.
const { chromium } = await import("@playwright/test");
const { createServer } = await import("vite");
const server = await createServer({
  root,
  server: {
    host: "127.0.0.1",
    port: 0,
    strictPort: false,
    open: false,
    hmr: false,
    fs: { allow: [root, realpathSync(path.join(root, "node_modules"))] },
  },
  logLevel: "error",
});
await server.listen();
const baseURL = `http://127.0.0.1:${server.httpServer.address().port}`;
const browser = await chromium.launch({
  executablePath:
    process.env.TOOLPORT_BROWSER_BIN ||
    (existsSync("/usr/bin/chromium") ? "/usr/bin/chromium" : undefined),
  headless: true,
});
try {
  const failures = await selfCheck(browser);
  if (failures.length)
    problems.push(`React extractor self-check failed: ${failures.join("; ")}`);
  else say("React extractor self-check passed");
  for (const scenario of scenarios) {
    if (scenario.error) continue;
    const t0 = Date.now();
    const ipc = JSON.parse(
      await readFile(path.join(out, `${scenario.name}.ipc.json`), "utf8"),
    );
    const context = await browser.newContext({
      viewport: scenario.viewport ?? { width: 1240, height: 900 },
      locale: "en-US",
      timezoneId: "UTC",
    });
    const page = await context.newPage();
    const pageErrors = [];
    page.on("pageerror", (error) => pageErrors.push(error.message));
    await page.route("**/*", (route) =>
      new URL(route.request().url()).origin === baseURL
        ? route.continue()
        : route.abort(),
    );
    await page.clock.setFixedTime(new Date(scenario.now_ms));
    await page.addInitScript((data) => {
      window.__TOOLPORT_PARITY__ = data;
      localStorage.setItem("toolport-theme", "dark");
    }, ipc);
    await page.addInitScript(installMutationCounter);
    try {
      await page.goto(`${baseURL}/fixtures/`);
      await page
        .getByRole("button", { name: "Servers", exact: true })
        .waitFor({ timeout: 60_000 });
      if (scenario.screen === "activity")
        await page.getByRole("button", { name: "Activity", exact: true }).click();
      const settle = () =>
        page
          .waitForFunction(pageIsIdle, null, { polling: 100, timeout: 30_000 })
          .catch(() => {
            throw new Error("the React page did not settle in 30 s");
          });
      await settle();
      // Sections the scenario opens in both shells, where each shell has them.
      for (const name of scenario.expand ?? []) {
        const section = page.locator('main button[aria-expanded="false"]', {
          hasText: name,
        });
        if ((await section.count()) > 0) {
          await section.first().click();
          await settle();
        }
      }
      const nodes = await extractReact(page, "main");
      const log = await page.evaluate(() => window.__TOOLPORT_PARITY_LOG__);
      const missing = log.calls.filter((call) => call.source === "missing");
      if (missing.length)
        throw new Error(
          `the scenario has no answer for ${[...new Set(missing.map((call) => `${call.cmd} ${JSON.stringify(call.args)}`))].join(", ")}; add them to its calls`,
        );
      scenario.fallback = [
        ...new Set(
          log.calls.filter((call) => call.source === "fixture").map((call) => call.cmd),
        ),
      ].sort();
      await writeFile(
        path.join(out, `${scenario.name}.react.json`),
        JSON.stringify(nodes, null, 2),
      );
      await page.screenshot({ path: path.join(out, `${scenario.name}.react.png`) });
      if (pageErrors.length)
        scenario.notes = [`React page errors: ${pageErrors.join("; ")}`];
    } catch (error) {
      scenario.error = `React: ${error.message}`;
      await page
        .screenshot({ path: path.join(out, `${scenario.name}.react-failed.png`) })
        .catch(() => {});
    }
    scenario.reactMs = Date.now() - t0;
    await context.close();
  }
} finally {
  await browser.close();
  await server.close();
}

// 3. Compare and report.
const entries = [];
for (const scenario of scenarios) {
  if (scenario.error) {
    problems.push(`${scenario.name}: ${scenario.error}`);
    continue;
  }
  const gtk = JSON.parse(
    await readFile(path.join(out, `${scenario.name}.gtk.json`), "utf8"),
  );
  const react = JSON.parse(
    await readFile(path.join(out, `${scenario.name}.react.json`), "utf8"),
  );
  for (const entry of diffNodes(gtk, react))
    entries.push({ scenario: scenario.name, screen: scenario.screen, ...entry });
}
const { drift, known, stale } = applyKnown(entries, knownList);
const report = {
  seconds: ((Date.now() - started) / 1000).toFixed(1),
  scenarios: scenarios.map((scenario) => ({
    name: scenario.name,
    screen: scenario.screen,
    description: scenario.description,
    gtkMs: gtkTimes[scenario.name],
    reactMs: scenario.reactMs,
    fallback: scenario.fallback ?? [],
    notes: scenario.notes ?? [],
    error: scenario.error ?? null,
  })),
  drift,
  known,
  stale,
  problems,
};
// Problems go into the saved report too: the file is what gets fetched and
// read, and it must not look clean when the run was not.
const text =
  formatReport(report) +
  (problems.length
    ? `\n\nThe parity tool could not finish cleanly:\n- ${problems.join("\n- ")}`
    : "");
await writeFile(path.join(out, "report.json"), JSON.stringify(report, null, 2));
await writeFile(path.join(out, "report.txt"), `${text}\n`);
console.log(text);
say(`\nReport: ${path.relative(root, path.join(out, "report.txt"))} (and report.json)`);
process.exit(problems.length ? 1 : 0);
