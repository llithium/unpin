import { createServer } from "node:http";
import { mkdir, readFile } from "node:fs/promises";
import { spawnSync } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { expect, test } from "@playwright/test";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const fixturePath = resolve(repoRoot, "test-results/visual-report.html");
const fixtureUrl = "/visual-report.html";

let server;
let origin;
const pageErrors = new WeakMap();

test.beforeAll(async () => {
  await mkdir(resolve(repoRoot, "test-results"), { recursive: true });
  const result = spawnSync(
    "cargo",
    [
      "run",
      "--quiet",
      "--locked",
      "--example",
      "render_test_reports",
      "--",
      fixturePath,
    ],
    { cwd: repoRoot, encoding: "utf8" },
  );
  if (result.status !== 0) {
    throw new Error(`visual fixture failed:\n${result.stdout}\n${result.stderr}`);
  }

  server = createServer(async (request, response) => {
    if (request.url !== fixtureUrl) {
      response.writeHead(404).end();
      return;
    }
    response.writeHead(200, { "content-type": "text/html; charset=utf-8" });
    response.end(await readFile(fixturePath));
  });
  await new Promise((resolveServer) => server.listen(0, "127.0.0.1", resolveServer));
  const address = server.address();
  origin = `http://127.0.0.1:${address.port}`;
});

test.afterAll(async () => {
  if (!server) return;
  await new Promise((resolveServer) => server.close(resolveServer));
});

test.beforeEach(async ({ page }) => {
  pageErrors.set(page, []);
  page.on("pageerror", (error) => pageErrors.get(page).push(error));
  await page.addInitScript(() => {
    window.__storageWrites = [];
    const setItem = Storage.prototype.setItem;
    Storage.prototype.setItem = function (...args) {
      window.__storageWrites.push(args);
      return setItem.apply(this, args);
    };
  });
  await mockImages(page.context());
});

async function mockImages(context) {
  await context.route("**/*", async (route) => {
    if (route.request().resourceType() === "image") {
      // A valid local response lets the report's image-ready behavior be
      // tested without contacting Pinterest.
      await route.fulfill({
        status: 200,
        contentType: "image/png",
        body: Buffer.from(
          "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=",
          "base64",
        ),
      });
      return;
    }
    if (new URL(route.request().url()).origin === origin) {
      await route.continue();
      return;
    }
    await route.abort();
  });
}

test.afterEach(async ({ page }) => {
  expect(pageErrors.get(page), "the report must not raise browser errors").toEqual([]);
});

async function openReport(page) {
  await page.goto(`${origin}${fixtureUrl}`);
  await expect(page.locator("[data-group]")).toHaveCount(3);
}

function visibleGroups(page) {
  return page.locator('[data-group]:not(.is-filtered)');
}

test("scope, kind, and review filters compose as a user queue", async ({ page }) => {
  await openReport(page);
  await expect(page.locator("#visible-count")).toHaveText("3 / 3 shown");

  await page.locator('label[for="filter-same"]').click();
  await page.locator('label[for="kind-exact"]').click();
  await expect(visibleGroups(page)).toHaveCount(1);
  await expect(page.locator("#visible-count")).toHaveText("1 / 3 shown");

  await page.locator("#unreviewed-only").check();
  await page.locator("#exact-1 [data-review-button]").click();
  await expect(visibleGroups(page)).toHaveCount(0);
  await expect(page.locator("#filter-empty")).toHaveClass(/is-visible/);
  await expect(page.locator("#visible-count")).toHaveText("0 / 3 shown");

  await page.locator('label[for="filter-all"]').click();
  await expect(page.locator('[data-group].is-active')).toHaveCount(1);
  await expect(page.locator('[data-group].is-active')).toHaveAttribute("id", "exact-2");
});

test("Quick wins selects same-board exact matches and advances review", async ({ page }) => {
  await openReport(page);
  await page.evaluate(() => (window.__storageWrites = []));
  await page.locator("#quick-wins").click();
  await expect
    .poll(() => page.evaluate(() => window.__storageWrites.length))
    .toBe(1);

  await expect(page.locator("#filter-same")).toBeChecked();
  await expect(page.locator("#kind-exact")).toBeChecked();
  await expect(page.locator("#unreviewed-only")).toBeChecked();
  await expect(visibleGroups(page)).toHaveCount(1);
  await expect(page.locator('[data-group].is-active')).toHaveAttribute("id", "exact-1");

  await page.evaluate(() => (window.__storageWrites = []));
  await page.locator("#exact-1 [data-review-button]").click();
  await expect
    .poll(() => page.evaluate(() => window.__storageWrites.length))
    .toBe(1);
  await expect
    .poll(() =>
      page.evaluate(
        () => document.querySelector("#quick-wins")?.closest(".filter-controls-meta")?.hidden,
      ),
    )
    .toBe(true);
  await expect(page.locator("#review-progress")).toHaveText("1 / 3");
});

test("focus navigation, overview, keyboard shortcuts, and active state stay coherent", async ({ page }) => {
  await openReport(page);
  await expect(page.locator('[data-group].is-active')).toHaveCount(1);
  await expect(page.locator('[data-group].is-active')).toHaveAttribute("id", "exact-1");

  await page.evaluate(() => {
    window.__storageWrites = [];
    window.__activeLinkChanges = [];
    window.__activeLinkObserver = new MutationObserver((records) => {
      window.__activeLinkChanges.push(...records.map((record) => record.target.dataset.target));
    });
    window.__activeLinkObserver.observe(document.body, {
      subtree: true, attributes: true, attributeFilter: ["aria-current"],
    });
  });
  await page.locator("#next-match").click();
  await expect.poll(() => page.evaluate(() => window.__activeLinkChanges)).toEqual(["exact-1", "exact-2"]);
  await page.evaluate(() => window.__activeLinkObserver.disconnect());
  await expect
    .poll(() => page.evaluate(() => window.__storageWrites.length))
    .toBe(1);
  await expect(page.locator('[data-group].is-active')).toHaveAttribute("id", "exact-2");
  await page.locator("#next-match").focus();
  await page.keyboard.press("j");
  await expect(page.locator('[data-group].is-active')).toHaveAttribute("id", "exact-2");
  await page.locator("#unreviewed-only").focus();
  await page.keyboard.press("j");
  await expect(page.locator('[data-group].is-active')).toHaveAttribute("id", "exact-2");
  await page.locator("main").click({ position: { x: 5, y: 5 } });
  await page.keyboard.press("j");
  await expect(page.locator('[data-group].is-active')).toHaveAttribute("id", "visual-1");
  await page.keyboard.press("k");
  await expect(page.locator('[data-group].is-active')).toHaveAttribute("id", "exact-2");

  await page.locator("#overview-toggle").click();
  await expect(page.locator("body")).toHaveClass(/overview-mode/);
  await expect(page.locator("#overview-toggle")).toHaveAttribute("aria-pressed", "true");
  await page.locator("main").click({ position: { x: 5, y: 5 } });
  await page.keyboard.press("o");
  await expect(page.locator("body")).not.toHaveClass(/overview-mode/);
  await expect(page.locator('[data-group].is-active')).toHaveCount(1);

  await page.keyboard.press("e");
  await expect(page.locator("#exact-2")).toHaveAttribute("data-reviewed", "true");
  await expect(page.locator("#review-progress")).toHaveText("1 / 3");
});

test("review state persists, invalidates when groups change, and resets", async ({ page }) => {
  await openReport(page);
  const storageKey = await page.locator("html").getAttribute("data-review-storage-key");
  const key = `unpin:review:v2:${storageKey}`;

  await page.evaluate(() => (window.__storageWrites = []));
  await page.locator("#exact-1 [data-review-button]").click();
  await expect
    .poll(() => page.evaluate(() => window.__storageWrites.length))
    .toBe(1);
  await page.reload();
  await expect(page.locator("#exact-1")).toHaveAttribute("data-reviewed", "true");
  await expect(page.locator("#unreviewed-only")).toBeChecked();
  const validState = await page.evaluate((storageKey) => localStorage.getItem(storageKey), key);

  await page.evaluate((storageKey) => localStorage.setItem(storageKey, "{malformed"), key);
  await page.reload();
  await expect(page.locator("#exact-1")).toHaveAttribute("data-reviewed", "false");
  await expect(page.locator("#unreviewed-only")).not.toBeChecked();

  await page.evaluate(([storageKey, previousState]) => {
    const state = JSON.parse(previousState);
    state.groups = ["a-different-group"];
    localStorage.setItem(storageKey, JSON.stringify(state));
  }, [key, validState]);
  await page.reload();
  await expect(page.locator("#exact-1")).toHaveAttribute("data-reviewed", "false");
  await expect(page.locator("#unreviewed-only")).not.toBeChecked();

  await page.locator("#exact-1 [data-review-button]").click();
  await page.evaluate(() => (window.__storageWrites = []));
  await page.locator("#reset-review").click();
  await expect
    .poll(() => page.evaluate(() => window.__storageWrites.length))
    .toBe(1);
  await expect(page.locator("#exact-1")).toHaveAttribute("data-reviewed", "false");
  await expect(page.locator("#review-progress")).toHaveText("0 / 3");
});

test("unavailable local storage does not disable the report", async ({ page }) => {
  await page.addInitScript(() => {
    Object.defineProperty(window, "localStorage", {
      configurable: true,
      get() {
        throw new Error("storage unavailable");
      },
    });
  });
  await openReport(page);
  await expect(page.locator("#visible-count")).toHaveText("3 / 3 shown");
  await page.locator("#next-match").click();
  await expect(page.locator('[data-group].is-active')).toHaveAttribute("id", "exact-2");
});

test("images reveal after load and no-JavaScript reports remain readable", async ({ page, browser }) => {
  await openReport(page);
  await expect(page.locator(".image-stage img").first()).toHaveClass(/is-loaded/);

  const noJsContext = await browser.newContext({ javaScriptEnabled: false });
  await mockImages(noJsContext);
  const noJsPage = await noJsContext.newPage();
  await noJsPage.goto(`${origin}${fixtureUrl}`);
  await expect(noJsPage.locator("[data-group]")).toHaveCount(3);
  await expect(noJsPage.locator("[data-group]").first()).toBeVisible();
  await expect(noJsPage.locator("[data-group]").nth(1)).toBeVisible();
  await expect(noJsPage.locator("[data-group]").nth(2)).toBeVisible();
  await expect(noJsPage.locator("#report-content")).toBeVisible();
  await noJsContext.close();
});

test("responsive and print modes keep report content within their presentation boundary", async ({ page }) => {
  await page.setViewportSize({ width: 480, height: 900 });
  await openReport(page);
  const viewport = await page.evaluate(() => ({
    width: document.documentElement.scrollWidth,
    viewport: window.innerWidth,
  }));
  expect(viewport.width).toBeLessThanOrEqual(viewport.viewport + 1);

  await page.emulateMedia({ media: "print" });
  await expect(page.locator(".rail")).toBeHidden();
  await expect(page.locator("footer")).toBeHidden();
  await expect(page.locator("#report-content")).toBeVisible();
});
