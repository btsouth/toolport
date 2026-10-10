import { chromium, expect } from "@playwright/test";
import { createServer } from "vite";
import { realpathSync } from "node:fs";
import { mkdir } from "node:fs/promises";
import path from "node:path";
const output = process.env.TOOLPORT_DRIFT_SHOTS || ".verify/drift";
await mkdir(output, { recursive: true });
const server = await createServer({
  configLoader: "runner",
  cacheDir: "/tmp/toolport-drift-vite",
  server: {
    host: "127.0.0.1",
    port: 0,
    open: false,
    fs: { allow: [process.cwd(), realpathSync("node_modules")] },
  },
  logLevel: "error",
});
let browser;
try {
  await server.listen();
  browser = await chromium.launch({
    executablePath: process.env.TOOLPORT_BROWSER_BIN || "/usr/bin/chromium",
    headless: true,
  });
  const page = await browser.newPage({ viewport: { width: 1240, height: 900 } });
  const origin = `http://127.0.0.1:${server.httpServer.address().port}`;
  await page.route("**/*", (route) =>
    new URL(route.request().url()).origin === origin ? route.continue() : route.abort(),
  );
  await page.goto(`${origin}/fixtures/?tool-changes=1`);
  await page.getByRole("button", { name: "Activity", exact: true }).click();
  await expect(
    page.getByText(/Tool security notices|Tool changes/, { exact: true }),
  ).toBeVisible();
  await page.screenshot({ path: path.join(output, "react-summary.png") });
  const group = page.getByRole("button", {
    name: /cloudflare_full_api:|Cloudflare \(Full API\):/,
  });
  if (await group.count()) {
    await group.click();
    await page.getByText("Update dns record", { exact: true }).click();
    await expect(
      page.getByText("Added parameters: comment", { exact: true }).first(),
    ).toBeVisible();
    await page.screenshot({ path: path.join(output, "react-expanded.png") });
  }
  console.log(`Screenshots: ${output}`);
} finally {
  await browser?.close();
  await server.close();
}
