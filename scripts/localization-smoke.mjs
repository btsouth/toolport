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
    `Localization smoke passed: ${languages.length} languages, persistence, rapid switching, preserved input, no repeated backend reads, OS fallback, blocked storage, dictionary recovery, share previews, Enter credential saves, and hidden pairing dialogs. Screenshots: ${output}`,
  );
} catch (error) {
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
