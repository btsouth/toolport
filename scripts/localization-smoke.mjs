#!/usr/bin/env node
/* global window */
// 実fixtureで切替・保存・入力維持・OS判定・読込失敗を検証する。
import { chromium, expect } from "@playwright/test";
import { createServer } from "vite";
import { existsSync, realpathSync } from "node:fs";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const output = path.join(
  process.env.TOOLPORT_VERIFY_DIR || path.join(root, ".verify"),
  `localization-${Date.now()}-${process.pid}`,
);
await mkdir(output, { recursive: true });
const languages = ["en", "ja", "zh-CN", "zh-TW", "ko", "es", "fr", "de", "pt-BR"];
const catalogs = {};
for (const language of languages) {
  const groups = await Promise.all(
    ["shell", "connections", "workflows", "common"].map(async (group) =>
      JSON.parse(
        await readFile(path.join(root, "src/locales", group, `${language}.json`), "utf8"),
      ),
    ),
  );
  catalogs[language] = Object.assign({}, ...groups);
}
const errors = [];
const server = await createServer({
  root,
  server: {
    host: "127.0.0.1",
    port: 0,
    open: false,
    hmr: false,
    fs: { allow: [root, realpathSync(path.join(root, "node_modules"))] },
  },
  logLevel: "error",
});
let browser;
let page;
let context;
try {
  await server.listen();
  const baseURL = `http://127.0.0.1:${server.httpServer.address().port}`;
  browser = await chromium.launch({
    executablePath:
      process.env.TOOLPORT_BROWSER_BIN ||
      (existsSync("/usr/bin/chromium") ? "/usr/bin/chromium" : undefined),
    headless: true,
  });
  async function open(locale, stored, storageBlocked = false) {
    if (context) await context.close();
    context = await browser.newContext({
      locale,
      viewport: { width: 1240, height: 900 },
    });
    await context.addInitScript(
      ({ stored, storageBlocked }) => {
        localStorage.setItem("toolport.onboarded", "1");
        if (stored !== undefined) localStorage.setItem("toolport-language", stored);
        if (storageBlocked) {
          const original = Storage.prototype.setItem;
          const originalRead = Storage.prototype.getItem;
          Storage.prototype.getItem = function (key) {
            if (key === "toolport-language")
              throw new DOMException("Storage blocked", "SecurityError");
            return originalRead.call(this, key);
          };
          Storage.prototype.setItem = function (key, value) {
            if (key === "toolport-language")
              throw new DOMException("Storage blocked", "SecurityError");
            return original.call(this, key, value);
          };
        }
      },
      { stored, storageBlocked },
    );
    page = await context.newPage();
    page.on("pageerror", (error) => errors.push(error.message));
    await page.route("**/*", (route) =>
      new URL(route.request().url()).origin === baseURL
        ? route.continue()
        : route.abort(),
    );
    await page.goto(`${baseURL}/fixtures/`);
    await expect(page.getByText("GitHub", { exact: true }).first()).toBeVisible();
    return page;
  }
  async function settings(language) {
    await page
      .getByRole("button", { name: catalogs[language]["Settings"], exact: true })
      .click();
    await expect(page.locator("#interface-language")).toBeVisible();
  }
  async function selected(language) {
    await expect(page.locator("html")).toHaveAttribute("lang", language);
    await expect(page.locator("#interface-language")).toHaveAttribute(
      "aria-busy",
      "false",
    );
    await expect(
      page.locator('label[for="interface-language"] > span').first(),
    ).toHaveText(catalogs[language]["Language"]);
  }
  function message(language, key, values = {}) {
    return catalogs[language][key].replace(/\{(\w+)\}/g, (_, name) =>
      String(values[name]),
    );
  }
  await open("en-US");
  await settings("en");
  expect(
    await page
      .locator("#interface-language option")
      .evaluateAll((options) => options.map((option) => option.value)),
  ).toEqual(["system", ...languages]);
  const draftInput = page.getByPlaceholder("Client name (e.g. Open WebUI)");
  await draftInput.fill("Draft <keep> & client");
  await draftInput.evaluate((element) =>
    element.setAttribute("data-smoke-draft", "true"),
  );
  const registryReads = await page.evaluate(() =>
    Object.fromEntries(
      [
        "get_registry",
        "detect_clients",
        "probe_servers",
        "team_pair_state",
        "clients_needing_restart",
      ].map((key) => [key, window.toolportFixture.calls[key]]),
    ),
  );
  for (const language of languages) {
    await page.locator("#interface-language").selectOption(language);
    await selected(language);
    if (language === "es" || language === "fr") {
      await expect(
        page.getByRole("button", {
          name: language === "es" ? "Sistema" : "Système",
          exact: true,
        }),
      ).toBeVisible();
    }
    await expect(page.locator('[data-smoke-draft="true"]')).toHaveValue(
      "Draft <keep> & client",
    );
    expect(await page.evaluate(() => localStorage.getItem("toolport-language"))).toBe(
      language,
    );
    await page.locator("#interface-language").scrollIntoViewIfNeeded();
    await page.screenshot({ path: path.join(output, `${language}.png`) });
  }
  expect(
    await page.evaluate(() =>
      Object.fromEntries(
        [
          "get_registry",
          "detect_clients",
          "probe_servers",
          "team_pair_state",
          "clients_needing_restart",
        ].map((key) => [key, window.toolportFixture.calls[key]]),
      ),
    ),
  ).toEqual(registryReads);
  // 連続変更でも最後に選んだ辞書が勝つ。
  await page.locator("#interface-language").selectOption("ja");
  await page.locator("#interface-language").selectOption("de");
  await selected("de");
  await page.reload();
  await expect(page.locator("html")).toHaveAttribute("lang", "de");
  await settings("de");
  await selected("de");
  await expect(page.locator("#interface-language")).toHaveValue("de");
  expect(await page.evaluate(() => window.toolportFixture.missing)).toEqual([]);

  // 未キャッシュの古い辞書を保留し、新しい選択より後に完了させる。
  await open("en-US");
  await settings("en");
  let releaseOldLoad;
  const heldLoad = new Promise((resolve) => {
    releaseOldLoad = resolve;
  });
  await page.route("**/src/locales/ja.ts*", async (route) => {
    await heldLoad;
    await route.continue();
  });
  await page.locator("#interface-language").selectOption("ja");
  await page.locator("#interface-language").selectOption("de");
  await selected("de");
  releaseOldLoad();
  await page.evaluate(() => import("/src/locales/ja.ts"));
  await selected("de");

  await open("ja-JP", "invalid-locale");
  await expect(page.locator("html")).toHaveAttribute("lang", "ja");
  await settings("ja");
  await selected("ja");
  await expect(page.locator("#interface-language")).toHaveValue("system");
  await open("zh-HK");
  await expect(page.locator("html")).toHaveAttribute("lang", "zh-TW");
  await settings("zh-TW");
  await selected("zh-TW");
  await open("it-IT");
  await expect(page.locator("html")).toHaveAttribute("lang", "en");
  await settings("en");
  await selected("en");

  await open("en-US", undefined, true);
  await settings("en");
  await page.locator("#interface-language").selectOption("fr");
  await selected("fr");
  await expect(page.locator("#interface-language")).toHaveValue("fr");

  // 実共有プレビュー・credential dialogを全言語で操作する。すべてfixture値。
  await open("en-US", "en");
  await page.goto(`${baseURL}/fixtures/?localization-controls&pairing`);
  await page.getByRole("button", { name: catalogs.en["Hide"], exact: true }).click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  const pairingReads = await page.evaluate(
    () => window.toolportFixture.calls.team_pair_state,
  );
  for (const language of languages) {
    await page.locator("#fixture-language").selectOption(language);
    await expect(page.locator("#fixture-language")).toHaveAttribute("aria-busy", "false");
    await expect(page.locator("html")).toHaveAttribute("lang", language);
    await expect(page.getByRole("dialog")).toHaveCount(0);
    for (const change of ["Added", "Changed", "Removed"]) {
      await expect(
        page
          .getByTestId("share-preview")
          .getByText(`${catalogs[language][change]} (1)`, { exact: true }),
      ).toBeVisible();
    }
    await page.getByRole("button", { name: "HTTP credential", exact: true }).click();
    const dialog = page.getByRole("dialog");
    const token = `fixture-http-${language}`;
    await dialog.locator('input[type="password"]').first().fill(token);
    await dialog.locator('input[type="password"]').first().press("Enter");
    await expect
      .poll(() =>
        page.evaluate(
          (token) =>
            window.toolportFixture.writes.some(
              (write) => write.command === "set_auth_token" && write.args.token === token,
            ),
          token,
        ),
      )
      .toBe(true);
    await dialog
      .getByRole("button", { name: catalogs[language]["Close"], exact: true })
      .click();
    await page
      .getByRole("button", { name: "Environment credential", exact: true })
      .click();
    const secret = `fixture-env-${language}`;
    await dialog.locator('input[type="password"]').first().fill(secret);
    await dialog.locator('input[type="password"]').first().press("Enter");
    await expect
      .poll(() =>
        page.evaluate(
          (secret) =>
            window.toolportFixture.writes.some(
              (write) =>
                write.command === "set_secret" &&
                write.args.key === "RESEND_API_KEY" &&
                write.args.value === secret,
            ),
          secret,
        ),
      )
      .toBe(true);
    await dialog
      .getByRole("button", { name: catalogs[language]["Close"], exact: true })
      .click();
  }
  expect(await page.evaluate(() => window.toolportFixture.calls.team_pair_state)).toBe(
    pairingReads,
  );
  expect(await page.evaluate(() => window.toolportFixture.missing)).toEqual([]);

  // 実コンポーネントに残った結果・入力は、辞書変更だけで再描画する。
  await page.locator("#fixture-language").selectOption("en");
  await expect(page.locator("#fixture-language")).toHaveAttribute("aria-busy", "false");
  const rules = page.getByTestId("rules-state");
  const teams = page.getByTestId("teams-state");
  const join = page.getByTestId("join-state");
  await rules.getByRole("button", { name: "Start from a file", exact: true }).click();
  await rules.getByRole("button", { name: /Codex.*AGENTS/ }).click();
  await expect(rules.locator("textarea")).toHaveValue("fixture text");
  await rules.locator("textarea").fill("Draft <keep> & rules");
  await page.getByRole("button", { name: "Emit team review", exact: true }).click();
  await join.getByRole("button", { name: "Join team", exact: true }).click();
  await join
    .getByRole("textbox", { name: "Your name (optional)", exact: true })
    .fill("Draft teammate");
  const stateCalls = await page.evaluate(() => ({ ...window.toolportFixture.calls }));
  const reviewKey =
    "{value1} team servers are off until you review and enable them below. Check the command, address and authentication before enabling.";
  const blockedKey =
    "{value1} was blocked as unsafe (link-local or cloud-metadata URLs).";
  const importKey =
    'Created "{value1}" from your own text in {value2}; the block Toolport had written there was left out. ';
  for (const language of languages) {
    await page.locator("#fixture-language").selectOption(language);
    await expect(page.locator("#fixture-language")).toHaveAttribute("aria-busy", "false");
    await expect(join.getByRole("alert")).toHaveText(
      catalogs[language]["Enter the invite or connect code your team gave you."],
    );
    await expect(
      join.getByRole("textbox", {
        name: catalogs[language]["Your name (optional)"],
        exact: true,
      }),
    ).toHaveValue("Draft teammate");
    await expect(rules.locator("textarea")).toHaveValue("Draft <keep> & rules");
    await expect(
      rules.getByText(
        message(language, importKey, {
          value1: "Fixture rules",
          value2: "/fixture/AGENTS.md",
        }) +
          catalogs[language][
            "It is now your applied set. The file itself was not changed."
          ],
        { exact: true },
      ),
    ).toBeVisible();
    await expect(
      teams.getByText(
        message(language, reviewKey, { value1: 2 }) +
          " " +
          message(language, blockedKey, { value1: 1 }),
        { exact: true },
      ),
    ).toBeVisible();
    if (language === "en" || language === "de") {
      const amounts =
        language === "en"
          ? ["$0.05", "$9.50", "$43", "$1000", "$1,000", "$1,235"]
          : [
              "0,05\u00a0$",
              "9,50\u00a0$",
              "43\u00a0$",
              "1000\u00a0$",
              "1.000\u00a0$",
              "1.235\u00a0$",
            ];
      for (const [index, amount] of amounts.entries()) {
        await expect(page.getByTestId(`currency-${index}`)).toHaveText(amount);
      }
    }
  }
  expect(await page.evaluate(() => ({ ...window.toolportFixture.calls }))).toEqual(
    stateCalls,
  );
  await page.locator("#fixture-language").selectOption("en");
  await expect(page.locator("#fixture-language")).toHaveAttribute("aria-busy", "false");
  await rules.getByRole("button", { name: "Start from a file", exact: true }).click();
  await rules.getByRole("button", { name: "Choose a file…", exact: true }).click();
  await join
    .getByRole("textbox", { name: "Invite or connect code", exact: true })
    .fill("fixture-code");
  await join.getByRole("button", { name: "Join team", exact: true }).click();
  await expect(join.getByRole("alert")).toHaveText(
    "An admin declined your request to join this team.",
  );
  const errorCalls = await page.evaluate(() => ({ ...window.toolportFixture.calls }));
  for (const language of languages) {
    await page.locator("#fixture-language").selectOption(language);
    await expect(page.locator("#fixture-language")).toHaveAttribute("aria-busy", "false");
    await expect(rules.getByRole("alert")).toHaveText(
      message(language, "Couldn't open the file picker: {value1}", {
        value1: "Error: fixture-picker-error",
      }),
    );
    await expect(join.getByRole("alert")).toHaveText(
      catalogs[language]["An admin declined your request to join this team."],
    );
    await expect(
      join.getByRole("textbox", {
        name: catalogs[language]["Invite or connect code"],
        exact: true,
      }),
    ).toHaveValue("fixture-code");
  }
  expect(await page.evaluate(() => ({ ...window.toolportFixture.calls }))).toEqual(
    errorCalls,
  );
  await page.locator("#fixture-language").selectOption("en");
  await expect(page.locator("#fixture-language")).toHaveAttribute("aria-busy", "false");
  await join
    .getByRole("button", { name: "Self-hosted server? Set a custom URL", exact: true })
    .click();
  const urlCases = [
    ["", "Server URL is required."],
    ["invalid-url", "Team server URL must start with https://."],
    [
      "http://example.invalid",
      "Team server URL must use https:// unless it is loopback HTTP for local development.",
    ],
  ];
  for (const [url, key] of urlCases) {
    await page.locator("#fixture-language").selectOption("en");
    await expect(page.locator("#fixture-language")).toHaveAttribute("aria-busy", "false");
    await join
      .getByPlaceholder("https://toolport.yourcompany.com", { exact: true })
      .fill(url);
    await join.getByRole("button", { name: "Join team", exact: true }).click();
    await teams
      .getByPlaceholder("https://toolport.yourcompany.com", { exact: true })
      .fill(url);
    await teams.getByRole("button", { name: "Connect", exact: true }).click();
    const urlCalls = await page.evaluate(() => ({ ...window.toolportFixture.calls }));
    for (const language of languages) {
      await page.locator("#fixture-language").selectOption(language);
      await expect(page.locator("#fixture-language")).toHaveAttribute(
        "aria-busy",
        "false",
      );
      await expect(join.getByRole("alert")).toHaveText(catalogs[language][key]);
      await expect(
        teams.getByText(catalogs[language][key], { exact: true }),
      ).toBeVisible();
    }
    expect(await page.evaluate(() => ({ ...window.toolportFixture.calls }))).toEqual(
      urlCalls,
    );
  }
  expect(await page.evaluate(() => window.toolportFixture.missing)).toEqual([]);

  await open("en-US", "en");
  await settings("en");
  const outcomes = [
    { killed: ["Fixture α (PID 42)"], failed: [], needsRestart: [] },
    { killed: [], failed: ["fixture /旧版"], needsRestart: [] },
    { killed: ["Fixture α (PID 42)"], failed: ["fixture /旧版"], needsRestart: [] },
    { killed: [], failed: [], needsRestart: [] },
    {
      killed: [],
      failed: [],
      needsRestart: [{ client: "Codex", clientPid: 42, gateway: "/fixture/old-gateway" }],
    },
  ];
  for (const outcome of outcomes) {
    await page.locator("#interface-language").selectOption("en");
    await selected("en");
    await page.evaluate((outcome) => {
      window.toolportFixture.cleanup.outcome = outcome;
    }, outcome);
    await page.getByRole("button", { name: "Run", exact: true }).click();
    await expect(page.getByRole("button", { name: "Run", exact: true })).toBeEnabled();
    // サイドバーの通常の定期ポーリングと、清掃・初回取得の再実行を区別する。
    const cleanupCommands = [
      "stop_stale_gateways",
      "clients_needing_restart",
      "get_registry",
      "detect_clients",
      "probe_servers",
      "team_pair_state",
    ];
    const cleanupCalls = await page.evaluate(
      (commands) =>
        Object.fromEntries(
          commands.map((command) => [command, window.toolportFixture.calls[command]]),
        ),
      cleanupCommands,
    );
    for (const language of languages) {
      await page.locator("#interface-language").selectOption(language);
      await selected(language);
      const parts = [];
      if (outcome.killed.length)
        parts.push(
          message(language, "Stopped {value1}: {value2}", {
            value1: outcome.killed.length,
            value2: outcome.killed.join("; "),
          }),
        );
      if (outcome.failed.length)
        parts.push(
          message(language, "Could not stop {value1}: {value2}", {
            value1: outcome.failed.length,
            value2: outcome.failed.join("; "),
          }),
        );
      const result = parts.length
        ? parts.join(". ")
        : catalogs[language][
            outcome.needsRestart.length
              ? "Nothing to stop: these apps spawn the old gateway on their next tool call, so there is no process running between calls."
              : "No old gateway processes found."
          ];
      await expect(page.getByText(result, { exact: true })).toBeVisible();
    }
    expect(
      await page.evaluate(
        (commands) =>
          Object.fromEntries(
            commands.map((command) => [command, window.toolportFixture.calls[command]]),
          ),
        cleanupCommands,
      ),
    ).toEqual(cleanupCalls);
  }
  // 検索文字列をHTML化せず、文中の位置を各言語の辞書に任せる。
  let currentLanguage = "pt-BR";
  const query = '<img src=x onerror="throw 1"> & fixture';
  for (const language of languages) {
    await settings(currentLanguage);
    await page.locator("#interface-language").selectOption(language);
    await selected(language);
    await page
      .getByRole("button", { name: catalogs[language]["All servers"], exact: true })
      .click();
    await page
      .getByPlaceholder(catalogs[language]["Search servers"], { exact: true })
      .fill(query);
    await expect(
      page.getByText(message(language, 'No servers match "{query}".', { query }), {
        exact: true,
      }),
    ).toBeVisible();
    await expect(page.locator('img[src="x"]')).toHaveCount(0);
    currentLanguage = language;
  }
  expect(await page.evaluate(() => window.toolportFixture.missing)).toEqual([]);

  // 新規contextの未キャッシュ辞書を一度失敗させ、復旧して再選択する。
  await open("en-US");
  await settings("en");
  const failedModule = "**/src/locales/es.ts*";
  await page.route(failedModule, (route) => route.abort());
  await page.locator("#interface-language").selectOption("es");
  await expect(page.getByRole("alert")).toContainText("Couldn't load this language");
  await page.unroute(failedModule);
  await page.reload();
  await settings("es");
  await selected("es");
  expect(errors).toEqual([]);
  expect(await page.evaluate(() => window.toolportFixture.missing)).toEqual([]);
  console.log(
    `Localization smoke passed: ${languages.length} languages, persistence, rapid switching, preserved input, no repeated backend reads, OS fallback, blocked storage, dictionary recovery, share previews, Enter credential saves, hidden pairing dialogs, result/error language switching, currency formatting, and interpolated search. Screenshots: ${output}`,
  );
} catch (error) {
  if (errors.length) console.error("Browser errors:", errors);
  if (page) {
    await page.screenshot({ path: path.join(output, "failure.png") }).catch(() => {});
    await writeFile(path.join(output, "failure.html"), await page.content()).catch(
      () => {},
    );
  }
  throw error;
} finally {
  if (context) await context.close();
  if (browser) await browser.close();
  await server.close();
}
