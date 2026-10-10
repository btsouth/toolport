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
  await expect(
    page.getByRole("button", { name: "35.0k catalog tokens avoided" }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "35.0k catalog tokens avoided" }),
  ).toHaveAttribute("title", /cl100k_base.*net of discovery.*once per session/);
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
  await expect(
    page.getByRole("main").getByText("35.0k catalog tokens avoided"),
  ).toBeVisible();
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
  await page.setViewportSize({ width: 1240, height: 900 });
  await page.screenshot({
    animations: "disabled",
    path: path.join(output, "onboarding-react-welcome.png"),
  });
  await page.getByRole("button", { name: /Set up MCP servers/ }).click();
  await expect(page.getByText(/import your existing servers/)).toBeVisible();
  await page.screenshot({
    animations: "disabled",
    path: path.join(output, "onboarding-react-add.png"),
  });
  await dialog.getByRole("button", { name: "Skip setup", exact: true }).click();
  const fixture = await page.evaluate(() => window.toolportFixture);
  expect(fixture.missing).toEqual([]);
  expect(errors).toEqual([]);
  await page.setViewportSize({ width: 1440, height: 1000 });
  await page.goto(`${baseURL}/fixtures/?pages-truth`);
  await expect(page.getByText("Needs sign-in", { exact: true })).toBeVisible();
  await expect(page.getByText("Unreachable", { exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Retry", exact: true })).toHaveCount(2);
  await page.getByRole("button", { name: "View log", exact: true }).first().click();
  await expect(page.getByRole("tab", { name: "Overview", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await expect(page.getByRole("combobox", { name: "Safety" })).toHaveValue("off");
  await expect(page.getByText(/Safety is set to Off/)).toBeVisible();
  await expect(page.getByText("Find tools as needed", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Team", exact: true }).click();
  await expect(
    page.getByText("Offline: cannot reach the team server", { exact: true }),
  ).toBeVisible();
  await expect(page.getByText(/Last successful sync:/)).toBeVisible();
  await expect(page.getByRole("button", { name: "Sign in", exact: true })).toBeVisible();
  await expect(page.getByText(/None declared/)).toHaveCount(0);

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
  await page.goto(`${baseURL}/fixtures/?approvals`);
  const approval = page.getByRole("alertdialog");
  await expect(
    approval.getByText("Claude Code wants to run this · destructive tool"),
  ).toBeVisible();
  await expect(approval.getByText("Reports itself as: Claude Code 2.1.0")).toBeVisible();
  await page.screenshot({ path: path.join(output, "approval-card.png") });
  await approval.getByRole("button", { name: "Deny", exact: true }).click();
  await expect(approval).toHaveCount(0);
  await page.getByRole("button", { name: "Activity", exact: true }).click();
  await page.getByRole("button", { name: /recent calls/i }).click();
  await expect(page.getByText("team_slack", { exact: true }).first()).toBeVisible();
  for (const label of [
    "Approved",
    "Denied",
    "No answer",
    "Withdrawn",
    "Changed after approval",
    "No approver available",
  ]) {
    await expect(page.getByText(label, { exact: true })).toBeVisible();
  }
  await expect(
    page.getByText('Claude Code (reports "Claude Code 2.1") · 2m ago · waited 1m 30s'),
  ).toBeVisible();
  await page.screenshot({ path: path.join(output, "approval-activity.png") });
  await page.setViewportSize({ width: 1240, height: 900 });
  for (const failure of ["", "launch", "credential", "optional", "vault", "verifying"]) {
    await page.goto(
      `${baseURL}/fixtures/?setup=1${failure === "verifying" ? "&setup-verifying=1" : failure ? `&setup-failure=${failure}` : ""}`,
    );
    await page.getByRole("button", { name: "Clients", exact: true }).click();
    await page.getByRole("button", { name: /Codex/ }).click();
    await page.getByRole("button", { name: "Connect to Toolport", exact: true }).click();
    await expect(
      page.getByRole("heading", { name: "Review and connect Codex" }),
    ).toBeVisible();
    await page.getByText("Details", { exact: true }).click();
    await expect(page.getByText(/Backups will be saved/)).toBeVisible();
    await page.getByText("Details", { exact: true }).click();
    await page.screenshot({
      animations: "disabled",
      path: path.join(output, "setup-client-review.png"),
    });
    if (failure === "credential" || failure === "optional") {
      await page.getByText("Calendar credentials and settings", { exact: true }).click();
      if (failure === "credential") {
        await expect(
          page.getByRole("button", { name: "Connect to Toolport", exact: true }),
        ).toBeDisabled();
        await expect(
          page.getByText("Enter PAT or deselect Calendar", { exact: true }),
        ).toBeVisible();
      } else {
        await expect(
          page.getByRole("button", { name: "Connect to Toolport", exact: true }),
        ).toBeEnabled();
        await expect(page.getByText("Optional", { exact: true }).first()).toBeVisible();
      }
      await page.screenshot({
        animations: "disabled",
        path: path.join(output, `setup-${failure}-review.png`),
      });
      if (failure === "credential") {
        await page.getByRole("button", { name: "Enter value", exact: true }).click();
        await page.getByLabel("PAT", { exact: true }).fill("synthetic-review-value");
        await page
          .getByRole("button", { name: "Use for connection", exact: true })
          .click();
        await expect(
          page.getByRole("button", { name: "Connect to Toolport", exact: true }),
        ).toBeEnabled();
      }
    }
    await page.getByRole("button", { name: "Connect to Toolport", exact: true }).click();
    if (failure === "verifying") {
      await expect(
        page.getByRole("button", { name: "Checking gateway...", exact: true }),
      ).toBeDisabled();
      await expect(
        page.getByRole("status").filter({ hasText: "Checking Notes" }),
      ).toBeVisible();
    } else if (failure && failure !== "optional") {
      await expect(
        page.getByRole("alert").filter({ hasText: "Client config unchanged" }),
      ).toBeVisible();
      await expect(page.getByRole("heading", { name: "Codex connected" })).toHaveCount(0);
    } else {
      await expect(page.getByText(/3 tools/)).toHaveCount(2);
      await page.getByText("What your agent sees", { exact: true }).click();
      await expect(
        page.getByText("toolport_search_tools", { exact: true }),
      ).toBeVisible();
      await page.getByText("What your agent sees", { exact: true }).click();
    }
    await page.screenshot({
      animations: "disabled",
      path: path.join(output, `setup-${failure || "gateway-result"}.png`),
    });
    expect((await page.evaluate(() => window.toolportFixture)).missing).toEqual([]);
  }
  await page.goto(`${baseURL}/fixtures/?setup=1`);
  await page.getByRole("button", { name: "Add server", exact: true }).click();
  await page.getByLabel("Name", { exact: true }).fill("Manual notes");
  await page.getByLabel("Command", { exact: true }).fill("fixture-manual");
  await page
    .getByRole("dialog")
    .getByRole("button", { name: "Add", exact: true })
    .click();
  await expect(
    page.getByRole("switch", { name: "Toggle Manual notes", exact: true }),
  ).toBeChecked();
  await page.screenshot({
    animations: "disabled",
    path: path.join(output, "setup-manual-add.png"),
  });
  await page.getByRole("button", { name: "Add server", exact: true }).click();
  await page.getByRole("button", { name: "Paste from client config" }).click();
  await page.locator("textarea").fill(
    JSON.stringify({
      mcpServers: {
        Alpha: { command: "fixture-alpha" },
        Beta: { command: "fixture-beta", env: { PAT: "synthetic-secret" } },
      },
    }),
  );
  await page.getByRole("button", { name: "Parse & fill" }).click();
  await expect(
    page.getByRole("heading", { name: "Review pasted servers" }),
  ).toBeVisible();
  await expect(page.getByText("synthetic-secret", { exact: true })).toHaveCount(0);
  await page.screenshot({
    animations: "disabled",
    path: path.join(output, "setup-multi-paste-review.png"),
  });
  await page.getByRole("button", { name: "Add selected servers" }).click();
  await expect(page.getByText("Alpha", { exact: true })).toBeVisible();
  await expect(page.getByText("Beta", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Browse catalog", exact: true }).click();
  await expect(page.getByText("NoteKit", { exact: true }).last()).toBeVisible();
  expect((await page.evaluate(() => window.toolportFixture)).missing).toEqual([]);
  await page.goto(`${baseURL}/fixtures/?setup=1`);
  await page.getByRole("button", { name: "Browse catalog", exact: true }).click();
  await page.getByRole("button", { name: "Add", exact: true }).first().click();
  await page.getByRole("button", { name: "Servers", exact: true }).click();
  await expect(
    page.getByRole("switch", { name: "Toggle NoteKit", exact: true }),
  ).toBeChecked();
  await page.screenshot({
    animations: "disabled",
    path: path.join(output, "setup-catalog-add.png"),
  });
  expect((await page.evaluate(() => window.toolportFixture)).missing).toEqual([]);
  await page.goto(`${baseURL}/fixtures/?sessions&approvals`);
  const unknownApproval = page.getByRole("alertdialog");
  await expect(
    unknownApproval.getByText(
      "Unknown app (via Cursor) wants to run this · destructive tool",
    ),
  ).toBeVisible();
  await expect(unknownApproval.getByText("Reports itself as: kt 1")).toBeVisible();
  await page.screenshot({ path: path.join(output, "session-approval.png") });
  await unknownApproval.getByRole("button", { name: "Deny", exact: true }).click();
  await expect(unknownApproval).toHaveCount(0);
  await page.getByRole("button", { name: "Activity", exact: true }).click();
  const calls = page.getByRole("button", { name: /Recent calls and approvals/ });
  await expect(calls).toBeVisible();
  if ((await calls.getAttribute("aria-expanded")) === "false") await calls.click();
  await expect(page.getByText(/Unknown app \(via Cursor\).*reports.*kt 1/)).toBeVisible();
  await expect(page.getByText(/dispatch 3 ms/)).toHaveCount(0);
  await page.screenshot({ path: path.join(output, "session-activity.png") });
  await page.getByRole("button", { name: "Clients", exact: true }).click();
  const sessions = page.getByRole("region", { name: "Recent client activity" });
  await sessions.getByRole("button", { name: /Recent client activity/ }).click();
  await expect(sessions.getByText("Unknown app (via Cursor)")).toBeVisible();
  await expect(
    sessions.getByText(/Last active.*12 calls today.*Last saw 4 tools/),
  ).toBeVisible();
  expect((await page.evaluate(() => window.toolportFixture)).missing).toEqual([]);
  await page.screenshot({ path: path.join(output, "session-clients.png") });
  await page.goto(`${baseURL}/fixtures/?sessions&caller-names`);
  for (const name of ["inbox", "inbox (reported)", "Unrecorded client"]) {
    await expect(
      page.getByText(`${name} wants to run this · destructive tool`, { exact: true }),
    ).toBeVisible();
  }
  await expect(
    page.getByText("Unrecorded client wants to run this · destructive tool"),
  ).toHaveAttribute("title", /Older Toolport versions did not record callers/);
  await page.screenshot({ path: path.join(output, "caller-approvals.png") });
  for (let i = 0; i < 3; i++)
    await page
      .getByRole("alertdialog")
      .first()
      .getByRole("button", { name: "Deny", exact: true })
      .first()
      .click();
  await page.getByRole("button", { name: "Activity", exact: true }).click();
  const callerCalls = page.getByRole("button", { name: /Recent calls and approvals/ });
  if ((await callerCalls.getAttribute("aria-expanded")) === "false")
    await callerCalls.click();
  for (const name of ["inbox", "inbox (reported)", "Unrecorded client", "[private]"]) {
    await expect(page.getByText(`${name} ·`, { exact: false })).toBeVisible();
  }
  await page.screenshot({ path: path.join(output, "caller-activity.png") });
  await page.getByRole("button", { name: "Clients", exact: true }).click();
  const callerSessions = page.getByRole("region", { name: "Recent client activity" });
  await callerSessions.getByRole("button", { name: /Recent client activity/ }).click();
  for (const name of ["inbox", "inbox (reported)", "Unrecorded client", "[private]"]) {
    await expect(callerSessions.getByText(name, { exact: true })).toBeVisible();
  }
  await expect(
    callerSessions.getByText("Unrecorded client", { exact: true }),
  ).toHaveAttribute("title", /Older Toolport versions did not record callers/);
  await page.screenshot({ path: path.join(output, "caller-clients.png") });
  expect((await page.evaluate(() => window.toolportFixture)).missing).toEqual([]);
  await page.goto(`${baseURL}/fixtures/?sessions&dogfood`);
  await page.getByRole("button", { name: "Clients", exact: true }).click();
  const groupedClients = page.getByRole("region", { name: "Recent client activity" });
  await expect(groupedClients.getByRole("button")).toHaveAttribute(
    "aria-expanded",
    "false",
  );
  await groupedClients.getByRole("button").click();
  await expect(groupedClients.getByText("Codex", { exact: true })).toHaveCount(1);
  await expect(
    groupedClients.getByText(/60 calls today.*Last saw 1,711 tools/),
  ).toBeVisible();
  await page.getByRole("button", { name: "Activity", exact: true }).click();
  await page.getByRole("button", { name: /Recent calls and approvals/ }).click();
  for (const label of [
    "Searched tools",
    "Looked up a tool",
    "Checked Toolport status",
    "Fetched tool details",
  ])
    await expect(page.getByText(label, { exact: true })).toBeVisible();
  await expect(page.getByText("1 value masked", { exact: true })).toHaveAttribute(
    "title",
    /before reaching the model/,
  );
  await expect(page.getByText(/5,225 tool calls retained/)).toContainText(
    "Showing 5 of the latest 5 events",
  );
  await page.getByRole("button", { name: "Clear", exact: true }).click();
  await expect(page.getByRole("dialog")).toContainText("Clear retained activity?");
  await page.getByRole("button", { name: "Cancel", exact: true }).click();
  expect((await page.evaluate(() => window.toolportFixture)).missing).toEqual([]);
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
