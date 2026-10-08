#!/usr/bin/env node
/* global window, document, getComputedStyle, Image */
import { chromium, expect } from "@playwright/test";
import { createServer } from "vite";
import { existsSync, realpathSync } from "node:fs";
import { mkdir, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const output = path.join(root, ".verify", `browser-${Date.now()}-${process.pid}`);
await mkdir(output, { recursive: true });
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
let browser;
let context;
let page;
const errors = [];
try {
  await server.listen();
  const address = server.httpServer.address();
  const baseURL = `http://127.0.0.1:${address.port}`;
  browser = await chromium.launch({
    executablePath:
      process.env.TOOLPORT_BROWSER_BIN ||
      (existsSync("/usr/bin/chromium") ? "/usr/bin/chromium" : undefined),
    headless: true,
  });
  context = await browser.newContext({ viewport: { width: 1240, height: 900 } });
  await context.tracing.start({ screenshots: true, snapshots: true });
  page = await context.newPage();
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("response", (response) => {
    if (response.status() >= 400)
      errors.push(`HTTP ${response.status()}: ${response.url()}`);
  });
  // Fixtures must stay offline even if an application path starts using fetch.
  await page.route("**/*", (route) => {
    if (new URL(route.request().url()).origin === baseURL) return route.continue();
    errors.push(`Unexpected external request: ${route.request().url()}`);
    return route.abort();
  });
  for (const state of ["normal", "outage", "timeout", "empty-outage", "installed"]) {
    await page.goto(`${baseURL}/fixtures/catalog.html?state=${state}`);
    await expect(page.getByText("GitHub", { exact: true })).toBeVisible();
    if (state === "outage" || state === "timeout" || state === "empty-outage") {
      await page
        .getByRole("textbox")
        .fill(state === "empty-outage" ? "unknown-fixture" : "github");
      await expect(page.getByText(/Showing curated matches only/)).toBeVisible();
      await expect(page.getByText(/No catalog results/)).toHaveCount(0);
      if (state === "empty-outage")
        await expect(page.getByText(/No curated matches/)).toBeVisible();
      else await expect(page.getByText("GitHub", { exact: true })).toBeVisible();
    }
    if (state === "installed") {
      await expect(page.getByText("in Toolport", { exact: true })).toHaveCount(1);
      await expect(page.getByRole("button", { name: "Add", exact: true })).toHaveCount(2);
    }
    await page.screenshot({ path: path.join(output, `catalog-react-${state}.png`) });
  }
  await page.goto(`${baseURL}/fixtures/`);
  await expect(page.getByText("GitHub", { exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "35.0k tokens saved" })).toBeVisible();
  await expect(page.getByRole("button", { name: "35.0k tokens saved" })).toHaveAttribute(
    "title",
    /cl100k_base.*net of discovery.*once per session/,
  );
  await page.screenshot({ path: path.join(output, "servers.png") });
  await page.getByRole("button", { name: "Show GitHub details", exact: true }).click();
  await page.getByRole("tab", { name: "Tools", exact: true }).click();
  await expect(page.getByText("get_issue", { exact: true })).toBeVisible();
  await expect(page.getByText("read-only", { exact: true })).toBeVisible();
  await expect(page.getByText("destructive", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: /^get_issue / }).click();
  await page.getByLabel("number", { exact: false }).fill("42");
  await page.getByRole("button", { name: "Call tool", exact: true }).click();
  await expect(page.getByText(/Fixture result:/)).toBeVisible();
  await page.screenshot({ path: path.join(output, "server-tools.png") });
  await page.getByRole("button", { name: "Activity", exact: true }).click();
  await expect(page.getByText("Protection active.", { exact: true })).toBeVisible();
  await expect(
    page.getByText("Tool definitions kept out of your agent's context"),
  ).toBeVisible();
  await expect(page.getByRole("main").getByText("35.0k tokens saved")).toBeVisible();
  await expect(page.getByText(/Historical bytes\/4: ≈41.1k/)).toBeVisible();
  await page.screenshot({ path: path.join(output, "activity.png") });
  await page.getByRole("button", { name: "Clients", exact: true }).click();
  await page.getByRole("button", { name: /Codex/ }).click();
  await expect(page.getByRole("combobox", { name: "Access", exact: true })).toBeVisible();
  await page.getByRole("combobox", { name: "Access", exact: true }).click();
  await expect(
    page.getByRole("option", { name: "All enabled servers", exact: true }),
  ).toBeVisible();
  await expect(page.getByRole("option", { name: "Work", exact: true })).toBeVisible();
  await page.screenshot({ path: path.join(output, "client-access.png") });
  await page.keyboard.press("Escape");
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await page.getByText("Advanced", { exact: true }).click();
  await expect(
    page.getByRole("combobox", { name: "Default access", exact: true }),
  ).toBeVisible();
  await expect(page.getByText("Access sets", { exact: true })).toBeVisible();
  await page
    .getByText("Advanced", { exact: true })
    .evaluate((element) => element.scrollIntoView({ block: "start" }));
  await page.screenshot({
    path: path.join(output, "settings-advanced.png"),
    fullPage: true,
  });
  await page.goto(`${baseURL}/fixtures/?long-names=1`);
  await page.setViewportSize({ width: 480, height: 360 });
  const longServer = "A".repeat(70);
  await expect(page.getByTitle(longServer, { exact: true })).toBeVisible();
  const firstToggle = await page
    .getByRole("switch", { name: `Toggle ${longServer}`, exact: true })
    .boundingBox();
  expect(firstToggle.y + firstToggle.height).toBeLessThanOrEqual(360);
  await page
    .getByRole("button", { name: `Show ${longServer} details`, exact: true })
    .click();
  await page.getByRole("tab", { name: "Tools", exact: true }).click();
  await expect(page.getByTitle("t".repeat(70), { exact: true })).toBeVisible();
  expect(
    await page.evaluate(() =>
      [
        ...document.querySelectorAll(
          "main [role=button], main button, main [role=switch]",
        ),
      ]
        .filter((element) => element.getBoundingClientRect().width > 0)
        .every((element) => element.getBoundingClientRect().right <= window.innerWidth),
    ),
  ).toBe(true);
  await page.getByRole("button", { name: "Add server", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog.getByText("Give the server a name.", { exact: true })).toHaveCount(
    0,
  );
  const footer = dialog.locator('[data-slot="dialog-footer"]');
  await page.evaluate(() =>
    Promise.all(
      document.getAnimations().map((animation) => animation.finished.catch(() => {})),
    ),
  );
  const footerBefore = await footer.boundingBox();
  await dialog.locator('[data-slot="dialog-body"]').evaluate((element) => {
    element.scrollTop = element.scrollHeight;
  });
  expect(await footer.boundingBox()).toEqual(footerBefore);
  expect(footerBefore.y + footerBefore.height).toBeLessThanOrEqual(360);
  await expect(dialog.getByRole("button", { name: "Cancel", exact: true })).toBeVisible();
  await page.screenshot({ path: path.join(output, "short-add-server.png") });
  await dialog.getByRole("button", { name: "Cancel", exact: true }).click();
  await page.getByRole("button", { name: "Help", exact: true }).click();
  await page.getByRole("button", { name: "Run setup again", exact: true }).click();
  await expect(
    dialog.getByRole("button", { name: "Skip setup", exact: true }),
  ).toBeVisible();
  await page.evaluate(() =>
    Promise.all(
      document.getAnimations().map((animation) => animation.finished.catch(() => {})),
    ),
  );
  const setupFooterBefore = await footer.boundingBox();
  await dialog.locator('[data-slot="dialog-body"]').evaluate((element) => {
    element.scrollTop = element.scrollHeight;
  });
  expect(await footer.boundingBox()).toEqual(setupFooterBefore);
  expect(setupFooterBefore.y + setupFooterBefore.height).toBeLessThanOrEqual(360);
  await page.screenshot({ path: path.join(output, "short-onboarding.png") });
  await dialog.getByRole("button", { name: "Skip setup", exact: true }).click();
  const fixture = await page.evaluate(() => window.toolportFixture);
  expect(fixture.missing).toEqual([]);
  expect(errors).toEqual([]);
  await page.setViewportSize({ width: 1440, height: 1000 });
  await page.goto(`${baseURL}/fixtures/?teams-review`);
  await page.getByRole("button", { name: "Review team changes", exact: true }).click();
  const memberReview = page.getByRole("dialog");
  await expect(
    memberReview.getByText("Server: Project tools", { exact: true }),
  ).toBeVisible();
  await expect(
    memberReview.getByText(/Alice.*via dashboard.*approved by Bob/),
  ).toBeVisible();
  await expect(memberReview.getByText("Call-log export", { exact: true })).toBeVisible();
  await expect(memberReview.getByRole("button", { name: /^Accept / })).toHaveCount(4);
  await expect(memberReview.getByRole("button", { name: /^Reject / })).toHaveCount(4);
  await page.evaluate(() => document.fonts.ready);
  await page.screenshot({ path: path.join(output, "teams-member-review.png") });
  await memberReview
    .getByRole("button", { name: /^Reject / })
    .first()
    .click();
  await expect(
    memberReview.getByText("Server: Project tools", { exact: true }),
  ).toHaveCount(0);
  await memberReview
    .getByRole("button", { name: /^Accept / })
    .first()
    .click();
  await expect(memberReview.getByText("Team instructions", { exact: true })).toHaveCount(
    0,
  );
  expect((await page.evaluate(() => window.toolportFixture)).missing).toEqual([]);
  expect(errors).toEqual([]);
  await page.goto(`${baseURL}/fixtures/?logos`);
  await expect(page.getByText("Dark logo fixture")).toBeVisible();
  await page.evaluate(() => document.fonts.ready);
  await expect
    .poll(() =>
      page
        .locator("img")
        .evaluateAll((images) =>
          images.every((img) => img.complete && img.naturalWidth > 0),
        ),
    )
    .toBe(true);
  // Wait for CSS mask assets too, so screenshots do not capture blank logos.
  await page.evaluate(async () => {
    await Promise.all(
      [...document.querySelectorAll("[style]")].map(async (element) => {
        const match = getComputedStyle(element).maskImage.match(/^url\("?(.*?)"?\)$/);
        if (!match) return;
        const image = new Image();
        image.src = match[1];
        await image.decode();
      }),
    );
  });
  await page.screenshot({ path: path.join(output, "logos.png"), fullPage: true });
  expect(errors).toEqual([]);
  console.log(`Browser smoke passed. Screenshots: ${output}`);
} catch (error) {
  if (page) {
    await page.screenshot({ path: path.join(output, "failure.png") }).catch(() => {});
    await writeFile(path.join(output, "failure.html"), await page.content()).catch(
      () => {},
    );
  }
  console.error(`Browser smoke failed. Artifacts: ${output}`);
  throw error;
} finally {
  await writeFile(path.join(output, "errors.json"), JSON.stringify(errors, null, 2));
  await context?.tracing.stop({ path: path.join(output, "trace.zip") });
  await browser?.close();
  await server.close();
}
