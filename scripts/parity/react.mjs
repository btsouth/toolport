// React side of the parity tool: walk what the browser fixture draws into the
// same { region, kind, value, on?, items? } nodes the GTK walker produces.
/* global window, document, getComputedStyle, HTMLElement, SVGElement, MutationObserver */

/** Installed before the app loads: counts DOM changes so "idle" is a condition, not a sleep. */
export function installMutationCounter() {
  window.__parityMutations = 0;
  const start = () =>
    new MutationObserver(() => {
      window.__parityMutations += 1;
    }).observe(document.documentElement, {
      subtree: true,
      childList: true,
      characterData: true,
      attributes: true,
    });
  if (document.documentElement) start();
  else document.addEventListener("DOMContentLoaded", start);
}

/** True once IPC is idle, nothing is loading and the DOM has not changed for 4 polls. */
export function pageIsIdle() {
  const main = document.querySelector("main");
  const log = window.__TOOLPORT_PARITY_LOG__;
  const busy =
    !main ||
    (log && log.pending > 0) ||
    /Loading…|Checking…|Checking server reachability|Checking protection/.test(
      main.innerText,
    );
  const count = window.__parityMutations;
  window.__parityStable =
    !busy && count === window.__parityLastCount ? (window.__parityStable ?? 0) + 1 : 0;
  window.__parityLastCount = count;
  return window.__parityStable >= 4;
}

/** First pass, in the page: every visible control and text run, in reading order. */
export function walkPage(rootSelector) {
  const CONTROL =
    'button, a[href], input, select, textarea, summary, [role="button"], [role="link"], [role="tab"], [role="switch"], [role="checkbox"], [role="combobox"], [role="menuitem"], [aria-haspopup="menu"]';
  const shown = (el) => {
    // A display: contents wrapper (the page header's action slot) has no box of
    // its own; its children are checked one by one.
    if (getComputedStyle(el).display === "contents") return true;
    if (!el.checkVisibility({ checkOpacity: true, checkVisibilityCSS: true }))
      return false;
    // Screen-reader-only text is not on screen.
    const box = el.getBoundingClientRect();
    return !(
      getComputedStyle(el).position === "absolute" &&
      box.width <= 1 &&
      box.height <= 1
    );
  };
  const isInline = (el) => getComputedStyle(el).display === "inline";
  const textOf = (el) => {
    let out = "";
    for (const child of el.childNodes) {
      if (child.nodeType === 3) out += child.textContent;
      else if (child instanceof HTMLElement && shown(child))
        out += isInline(child) ? textOf(child) : ` ${textOf(child)} `;
    }
    return out;
  };
  const clean = (text) => (text ?? "").replace(/\s+/g, " ").trim();
  const nameOf = (el) =>
    clean(textOf(el)) ||
    clean(el.getAttribute("aria-label")) ||
    clean(el.getAttribute("title")) ||
    "(unnamed)";
  const nodes = [];
  const menus = [];
  const push = (region, kind, value, extra = {}) =>
    nodes.push({ region, kind, value, ...extra });
  const walk = (el, region) => {
    if (!shown(el)) return;
    if (el.matches("header.app-header")) region = "header";
    else if (el.dataset.serverId) region = `row:${el.dataset.serverId}`;
    const role = el.getAttribute("role");
    const tag = el.tagName;
    if (el.getAttribute("aria-haspopup") === "menu") {
      push(region, "menu", nameOf(el));
      menus.push(el);
      el.dataset.parityMenu = String(menus.length - 1);
      return;
    }
    if (role === "switch") {
      push(region, "switch", clean(el.getAttribute("aria-label")) || nameOf(el), {
        on: el.getAttribute("aria-checked") === "true",
      });
      return;
    }
    if (
      role === "checkbox" ||
      (tag === "INPUT" && el.type === "checkbox") ||
      el.hasAttribute("aria-pressed")
    ) {
      const on =
        el.getAttribute("aria-checked") === "true" ||
        el.getAttribute("aria-pressed") === "true" ||
        el.checked === true;
      push(region, "toggle", nameOf(el), { on });
      return;
    }
    if (role === "combobox" || tag === "SELECT") {
      const value = tag === "SELECT" ? el.selectedOptions[0]?.textContent : textOf(el);
      push(region, "dropdown", clean(value) || nameOf(el));
      return;
    }
    if (tag === "INPUT" || tag === "TEXTAREA") {
      const search = el.type === "search" || /search/i.test(el.placeholder ?? "");
      push(
        region,
        search ? "search" : "entry",
        clean(el.placeholder) || clean(el.getAttribute("aria-label")),
      );
      return;
    }
    if (
      tag === "BUTTON" ||
      tag === "SUMMARY" ||
      (tag === "A" && el.hasAttribute("href")) ||
      ["button", "link", "tab", "menuitem"].includes(role)
    ) {
      push(region, "button", nameOf(el));
      return;
    }
    let run = "";
    const flush = () => {
      const text = clean(run);
      if (text) {
        const box = el.getBoundingClientRect();
        push(region, "text", text, {
          line: { parent: el.parentElement, top: box.top, bottom: box.bottom },
        });
      }
      run = "";
    };
    for (const child of el.childNodes) {
      if (child.nodeType === 3) {
        run += child.textContent;
        continue;
      }
      if (!(child instanceof HTMLElement) || child instanceof SVGElement || !shown(child))
        continue;
      if (isInline(child) && !child.matches(CONTROL) && !child.querySelector(CONTROL)) {
        run += textOf(child);
        continue;
      }
      flush();
      walk(child, region);
    }
    flush();
  };
  for (const el of document.querySelectorAll("[data-parity-menu]"))
    delete el.dataset.parityMenu;
  const root = document.querySelector(rootSelector);
  if (!root) throw new Error(`no ${rootSelector} on the page`);
  walk(root, "body");
  // Text that sits side by side on one line in one container reads as one
  // line (a name and its "· 2m ago" meta in two flex items); the GTK walker
  // joins the same way.
  const merged = [];
  for (const node of nodes) {
    const last = merged.at(-1);
    if (
      node.line &&
      last?.line &&
      last.region === node.region &&
      last.line.parent === node.line.parent &&
      node.line.top < last.line.bottom &&
      last.line.top < node.line.bottom
    ) {
      last.value = clean(`${last.value} ${node.value}`);
      continue;
    }
    merged.push(node);
  }
  for (const node of merged) delete node.line;
  const result = merged;
  return {
    nodes: result,
    menuIndexes: result.flatMap((node, index) => (node.kind === "menu" ? [index] : [])),
  };
}

async function readOpenMenu() {
  const menu = [...document.querySelectorAll('[role="menu"]')].at(-1);
  if (!menu) return null;
  // Let the open animation finish, so items are not read while still fading in.
  await Promise.all(
    menu.getAnimations({ subtree: true }).map((animation) => animation.finished),
  );
  return [
    ...menu.querySelectorAll(
      '[role="menuitem"], [role="menuitemcheckbox"], [role="menuitemradio"]',
    ),
  ]
    .filter((item) => item.checkVisibility({ checkVisibilityCSS: true }))
    .map((item) =>
      (item.textContent || item.getAttribute("aria-label") || "")
        .replace(/\s+/g, " ")
        .trim(),
    );
}

/**
 * Walk the page, then open each menu and read its items. Menus are opened only
 * after the whole page is read, as on the GTK side. A menu that cannot be found
 * again, does not open, or opens empty fails the extraction.
 */
export async function extractReact(page, rootSelector = "main") {
  const { nodes, menuIndexes } = await page.evaluate(walkPage, rootSelector);
  for (let menu = 0; menu < menuIndexes.length; menu++) {
    const node = nodes[menuIndexes[menu]];
    const describe = `menu "${node.value}" in ${node.region}`;
    const trigger = page.locator(`[data-parity-menu="${menu}"]`);
    if ((await trigger.count()) !== 1)
      throw new Error(
        `${describe} left the page before it was opened; the page re-rendered while it was read`,
      );
    await trigger.click();
    await page
      .locator('[role="menu"]')
      .last()
      .waitFor({ state: "attached", timeout: 10_000 })
      .catch(() => {
        throw new Error(`${describe} did not open`);
      });
    const items = await page.evaluate(readOpenMenu);
    await page.keyboard.press("Escape");
    await page
      .locator('[role="menu"]')
      .waitFor({ state: "detached", timeout: 10_000 })
      .catch(() => {
        throw new Error(`${describe} did not close`);
      });
    if (!items || items.length === 0) throw new Error(`${describe} opened with no items`);
    node.items = items;
  }
  return nodes;
}

/** The extractor's own checks, on a static page: hidden text stays out, menus are read, an empty menu fails. */
export async function selfCheck(browser) {
  const page = await browser.newPage();
  try {
    await page.setContent(`<!doctype html><main>
      <p>Shown text</p>
      <div style="display:flex"><span>Name</span><span> · 2m ago</span></div>
      <div style="display:flex;flex-direction:column"><span>Line one</span><span>Line two</span></div>
      <p style="display:none">Hidden text</p>
      <div style="visibility:hidden"><p>Text in a hidden box</p></div>
      <span style="position:absolute;width:1px;height:1px;overflow:hidden;clip:rect(0,0,0,0)">Screen reader text</span>
      <button aria-haspopup="menu" aria-label="Full menu" id="full"><svg width="10" height="10"></svg></button>
      <button aria-haspopup="menu" aria-label="Empty menu" id="empty"><svg width="10" height="10"></svg></button>
    </main>
    <script>
      const open = (items) => {
        const menu = document.createElement("div");
        menu.setAttribute("role", "menu");
        menu.style.padding = "4px";
        for (const [label, hidden] of items) {
          const item = document.createElement("div");
          item.setAttribute("role", "menuitem");
          item.textContent = label;
          if (hidden) item.style.display = "none";
          menu.append(item);
        }
        document.body.append(menu);
      };
      document.getElementById("full").onclick = () => open([["First item"], ["Hidden item", true], ["Second item"]]);
      document.getElementById("empty").onclick = () => open([]);
      document.addEventListener("keydown", (e) => {
        if (e.key === "Escape") document.querySelectorAll('[role="menu"]').forEach((m) => m.remove());
      });
    </script>`);
    const failures = [];
    let error = null;
    try {
      await extractReact(page);
    } catch (caught) {
      error = caught.message;
    }
    if (!error?.includes('menu "Empty menu" in body opened with no items'))
      failures.push(`an empty menu did not fail the extraction (${error ?? "no error"})`);
    await page.evaluate(() => document.getElementById("empty").remove());
    const nodes = await extractReact(page);
    const values = nodes.map((node) => node.value);
    if (
      JSON.stringify(values) !==
      JSON.stringify(["Shown text", "Name · 2m ago", "Line one", "Line two", "Full menu"])
    )
      failures.push(
        `hidden text leaked, or lines were joined wrongly: ${JSON.stringify(values)}`,
      );
    if (
      JSON.stringify(nodes.at(-1)?.items) !==
      JSON.stringify(["First item", "Second item"])
    )
      failures.push(`menu items were not read: ${JSON.stringify(nodes.at(-1)?.items)}`);
    return failures;
  } finally {
    await page.close();
  }
}
