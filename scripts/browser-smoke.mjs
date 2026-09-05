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
  await page.goto(`${baseURL}/fixtures/`);
  await expect(page.getByText("GitHub", { exact: true })).toBeVisible();
  await page.screenshot({ path: path.join(output, "servers.png") });
  await page.getByRole("button", { name: "Activity", exact: true }).click();
  await expect(page.getByText("Protection active.", { exact: true })).toBeVisible();
  await page.screenshot({ path: path.join(output, "activity.png") });
  const fixture = await page.evaluate(() => window.toolportFixture);
  expect(fixture.missing).toEqual([]);
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
