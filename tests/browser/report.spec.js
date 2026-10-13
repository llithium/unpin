import { createServer } from "node:http";
import { mkdir, readFile } from "node:fs/promises";
import { spawnSync } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

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

  for (const scenario of ["empty", "long-content", "subset", "media"]) {
    const result = spawnSync("cargo", ["run", "--quiet", "--locked", "--example", "render_test_reports", "--", resolve(repoRoot, `test-results/${scenario}.html`), "--scenario", scenario], { cwd: repoRoot, encoding: "utf8" });
    if (result.status !== 0) throw new Error(result.stderr);
  }
  server = createServer(async (request, response) => {
    if (![fixtureUrl, "/empty.html", "/long-content.html", "/subset.html", "/media.html"].includes(request.url)) {
      response.writeHead(404).end();
      return;
    }
    response.writeHead(200, { "content-type": "text/html; charset=utf-8" });
    response.end(await readFile(resolve(repoRoot, `test-results${request.url}`)));
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

async function decide(page, groupId, action) {
  const group = page.locator(`#${groupId}`);
  if (action !== "keep_all") await group.locator(".decision-more > summary").click();
  await group.locator(`[data-decision-action="${action}"]`).click();
}

function visibleGroups(page) {
  return page.locator('[data-group]:not(.is-filtered)');
}

test("scope, kind, and review filters compose as a user queue", async ({ page }) => {
  await openReport(page);
  await expect(page.locator("#visible-count")).toHaveText("3 shown");

  await page.locator('label[for="filter-same"]').click();
  await page.locator('label[for="kind-exact"]').click();
  await expect(visibleGroups(page)).toHaveCount(1);
  await expect(page.locator("#visible-count")).toHaveText("1 shown");

  await page.locator("#unreviewed-only").check();
  await page.locator("#exact-1 [data-review-button]").click();
  await expect(visibleGroups(page)).toHaveCount(0);
  await expect(page.locator("#filter-empty")).toHaveClass(/is-visible/);
  await expect(page.locator("#visible-count")).toHaveText("0 shown");

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

test("review state persists, rejects inconsistent state, and resets", async ({ page }) => {
  await openReport(page);
  const key = "unpin:review:v3";

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
    state.decisions[0].action = "unsupported-action";
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
  await expect(page.locator("#visible-count")).toHaveText("3 shown");
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
  await expect(noJsPage.locator(".topbar-actions")).toBeHidden();
  await expect(noJsPage.locator(".match-actions").first()).toBeHidden();
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
  await page.evaluate(() => window.dispatchEvent(new Event("beforeprint")));
  for (const image of await page.locator(".image-stage img").all()) {
    await expect(image).toHaveAttribute("loading", "eager");
    await expect(image).toHaveCSS("opacity", "1");
  }
  for (const group of await page.locator("[data-group]").all()) await expect(group).toBeVisible();
});


test("review advance preserves keyboard focus and Undo restores the match", async ({ page }) => {
  await openReport(page);
  const review = page.locator("#exact-1 [data-review-button]");
  await review.focus();
  await page.keyboard.press("Enter");
  await expect(page.locator("#exact-2 [data-review-button]")).toBeFocused();
  await page.locator("#undo-review").click();
  await expect(review).toBeFocused();
  await expect(page.locator("#exact-1")).toHaveAttribute("data-reviewed", "false");
  await expect(page.locator("#review-progress")).toHaveText("0 / 3");

  await page.locator("#quick-wins").click();
  await review.click();
  await expect(page.locator("#clear-filters")).toBeFocused();
  await page.locator("#clear-filters").click();
  await expect(visibleGroups(page)).toHaveCount(3);
  await expect(page.locator("#exact-1")).toHaveAttribute("data-reviewed", "true");
  await page.locator("#reset-review").click();
  await page.locator("#undo-review").click();
  await expect(page.locator("#exact-1")).toHaveAttribute("data-reviewed", "true");
  await expect(page.locator("#review-progress")).toHaveText("1 / 3");
});

test("a completed queue explains completion and can be revisited", async ({ page }) => {
  await openReport(page);
  await page.locator("#unreviewed-only").check();
  for (const id of ["exact-1", "exact-2", "visual-1"]) {
    await page.locator(`#${id} [data-review-button]`).click();
  }
  await expect(page.locator("#filter-empty-title")).toHaveText("All matches reviewed");
  await expect(page.locator("#clear-filters")).toBeFocused();
  await page.locator("#undo-review").click();
  await expect(page.locator("#visual-1 [data-review-button]")).toBeFocused();
  await expect(visibleGroups(page)).toHaveCount(1);
});

test("failed images explain recovery and storage failures disclose unsaved progress", async ({ page }) => {
  await page.route("https://images.example.test/**", route => route.abort());
  await page.addInitScript(() => {
    Storage.prototype.setItem = () => { throw new Error("quota exceeded"); };
  });
  await openReport(page);
  await expect(page.locator("#exact-1 .image-stage").first()).toHaveClass(/is-error/);
  const errorText = await page.locator("#exact-1 .image-stage").first().evaluate(el => getComputedStyle(el, "::before").content);
  expect(errorText).toContain("Open the pin");
  await expect(page.locator("#exact-1 .button.primary").first()).toBeVisible();
  await page.locator("#exact-1 [data-review-button]").click();
  await expect(page.locator("#storage-notice")).toBeVisible();
  await expect(page.locator("#review-progress")).toHaveText("1 / 3");
});

test("empty scans show their identity and warnings without unusable review controls", async ({ page }) => {
  await page.goto(`${origin}/empty.html`);
  await expect(page.locator(".report-intro")).toBeVisible();
  await expect(page.locator(".empty")).toBeVisible();
  await expect(page.locator(".scan-warning")).toBeVisible();
  await expect(page.locator("#scan-warnings")).toHaveAttribute("open", "");
  await expect(page.locator(".rail")).toBeHidden();
  await expect(page.locator(".topbar-actions")).toBeHidden();
});

test("mobile comparisons align images and retain progress, labels, and touch targets", async ({ browser }) => {
  const context = await browser.newContext({ viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true });
  await mockImages(context);
  const page = await context.newPage();
  await page.goto(`${origin}${fixtureUrl}`);
  await expect(page.locator("#review-progress")).toBeVisible();
  await expect(page.locator("#overview-toggle .control-label")).toBeVisible();
  const images = await page.locator("#exact-1 .image-stage").evaluateAll(elements => elements.map(el => ({ x: el.getBoundingClientRect().x, y: el.getBoundingClientRect().y })));
  expect(images[0].y).toBe(images[1].y);
  expect(images[1].x).toBeGreaterThan(images[0].x);
  for (const selector of ["#previous-match", "#next-match", "#overview-toggle", "#exact-1 .button.primary"]) {
    expect((await page.locator(selector).first().boundingBox()).height).toBeGreaterThanOrEqual(44);
  }
  await expect(page.locator("#match-filters")).toBeHidden();
  await page.locator("#toggle-filters").click();
  await expect(page.locator("#match-filters")).toBeVisible();
  await page.locator("#toggle-filters").click();
  await page.locator("#next-match").click();
  const heading = await page.locator("#exact-2 .match-heading").boundingBox();
  const header = await page.locator(".topbar").boundingBox();
  expect(heading.y).toBeGreaterThanOrEqual(header.y + header.height);
  expect(heading.y).toBeLessThan(200);
  await context.close();
});

test("long content and three-pin groups fit narrow, tablet, desktop, and overview layouts", async ({ page }) => {
  await page.goto(`${origin}/long-content.html`);
  for (const width of [320, 390, 620, 768, 880, 1024, 1440]) {
    await page.setViewportSize({ width, height: 900 });
    for (const overview of [false, true]) {
      if ((await page.locator("#overview-toggle").getAttribute("aria-pressed")) !== String(overview)) await page.locator("#overview-toggle").click();
      const overflow = await page.evaluate(() => document.documentElement.scrollWidth > innerWidth);
      expect(overflow, `${width}px, overview=${overview}`).toBe(false);
      const cards = await page.locator("#exact-1 .pin-card").evaluateAll(elements => elements.map(el => ({width: el.clientWidth, content: el.scrollWidth})));
      for (const card of cards) expect(card.content).toBeLessThanOrEqual(card.width + 1);
    }
  }
});

test("grouped visual variants retain every save with two previews", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await openReport(page);
  await page.locator('[data-target="visual-1"]').click();
  await expect(page.locator("#visual-1 .image-stage")).toHaveCount(2);
  await page.locator("#visual-1 .additional-saves > summary").click();
  await expect(page.locator('#visual-1 a[href="https://www.pinterest.com/pin/303/"]')).toBeVisible();
  const widths = await page.evaluate(() => [document.documentElement.scrollWidth, window.innerWidth]);
  expect(widths[0]).toBeLessThanOrEqual(widths[1] + 1);
});

test("decisions survive board subsets, export/import, and undo", async ({ page }) => {
  await openReport(page);
  await page.locator('#exact-1 [data-remove-pin="102"]').check();
  await expect(page.locator("#exact-1")).toHaveAttribute("data-decision", "remove");
  await expect(page.locator("#exact-1")).toHaveAttribute("data-reviewed", "false");
  await page.locator('#exact-1 [data-review-button]').click();
  await expect(page.locator("#exact-1")).toHaveAttribute("data-decision", "remove");
  await page.locator('.match-link[data-target="exact-2"]').click();
  await decide(page, "exact-2", "keep_all");
  await page.locator('.match-link[data-target="visual-1"]').click();
  await decide(page, "visual-1", "later");
  await expect(page.locator("#review-progress")).toHaveText("2 / 3");
  const downloadReady = page.waitForEvent("download");
  await page.locator(".report-tools > summary").click();
  await page.locator("#export-review").click();
  const download = await downloadReady;
  const exported = JSON.parse(await readFile(await download.path(), "utf8"));
  expect(exported.schema_version).toBe(1);
  expect(exported.decisions).toHaveLength(3);
  expect(exported.decisions.find((decision) => decision.action === "remove").remove_pin_ids).toEqual(["102"]);
  expect(exported.decisions.every((decision) => decision.items.every((item) => item.pin_url && item.image_url))).toBe(true);

  await page.goto(`${origin}/subset.html`);
  await expect(page.locator("#exact-1")).toHaveAttribute("data-decision", "remove");
  await expect(page.locator('#exact-1 [data-remove-pin="102"]')).toBeChecked();
  await page.locator("#reset-review").click();
  const remaining = await page.evaluate(() => JSON.parse(localStorage.getItem("unpin:review:v3")).decisions);
  expect(remaining.map((decision) => decision.action).sort()).toEqual(["keep_all", "later"]);
  await page.evaluate(() => localStorage.clear());
  await page.goto(`${origin}${fixtureUrl}`);
  await page.locator("#review-file").setInputFiles({ name: "decisions.json", mimeType: "application/json", buffer: Buffer.from(JSON.stringify(exported)) });
  await expect(page.locator("#transfer-status")).toHaveText("Imported 3 decisions; 3 match this report.");
  await expect(page.locator("#exact-1")).toHaveAttribute("data-decision", "remove");
  await expect(page.locator("#exact-2")).toHaveAttribute("data-decision", "keep_all");
  await expect(page.locator("#visual-1")).toHaveAttribute("data-decision", "later");
  await expect(page.locator("#visual-1")).toHaveAttribute("data-reviewed", "false");
  await page.locator("#undo-review").click();
  await expect(page.locator("#review-progress")).toHaveText("0 / 3");
  await page.locator('.match-link[data-target="visual-1"]').click();
  await decide(page, "visual-1", "not_duplicate");
  await expect(page.locator('[data-group].is-active [data-review-button]')).toBeFocused();
  await page.reload();
  await expect(page.locator("#visual-1")).toHaveAttribute("data-decision", "not_duplicate");
});

test("imports validate every decision before changing current progress", async ({ page }) => {
  await openReport(page);
  await decide(page, "exact-1", "keep_all");
  const record = await page.evaluate(() => JSON.parse(localStorage.getItem("unpin:review:v3")).decisions[0]);
  const valid = { ...record, action: "not_duplicate" };
  const invalid = { ...record, key: "visual:same-board:" + "a".repeat(64), action: "remove", remove_pin_ids: ["not-a-member"] };
  await page.locator("#review-file").setInputFiles({ name: "bad.json", mimeType: "application/json", buffer: Buffer.from(JSON.stringify({ schema_version: 1, decisions: [valid, invalid] })) });
  await expect(page.locator("#transfer-status")).toContainText("Invalid review decision");
  await expect(page.locator("#exact-1")).toHaveAttribute("data-decision", "keep_all");
  await page.locator('.match-link[data-target="exact-1"]').click();
  await decide(page, "exact-1", "later");
  await expect(page.locator("#reset-review")).toBeEnabled();
  await expect(page.locator("#review-progress")).toHaveText("0 / 3");
});

test("media covers remain inspection-only and are excluded from Quick wins", async ({ page }) => {
  await page.goto(`${origin}/media.html`);
  await expect(page.locator("#exact-1 .match-description")).toContainText("Matching covers");
  await expect(page.locator("#exact-1 .badge").filter({ hasText: "Inspect pin content" })).toHaveCount(0);
  await expect(page.locator("#visual-1 .match-description")).toContainText("Matching covers");
  await expect(page.locator("#visual-1 .badge").filter({ hasText: "Inspect pin content" })).toHaveCount(0);
  await expect(page.locator("#quick-wins").locator("..")).toBeHidden();
  for (const width of [375, 768, 1440]) {
    await page.setViewportSize({ width, height: 900 });
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  }
});

test("open reports merge decisions and resets without losing another report's work", async ({ page }) => {
  await openReport(page);
  const other = await page.context().newPage();
  const errors = [];
  other.on("pageerror", (error) => errors.push(error));
  try {
    await other.goto(`${origin}/subset.html`);
    await page.locator(".match-link[data-target='exact-2']").click();
    await decide(page, "exact-2", "keep_all");
    await decide(other, "exact-1", "not_duplicate");
    await expect(page.locator('#exact-1')).toHaveAttribute("data-decision", "not_duplicate");
    await other.locator('#reset-review').click();
    await expect(page.locator('#exact-1')).toHaveAttribute("data-decision", "");
    await expect(page.locator('#exact-2')).toHaveAttribute("data-decision", "keep_all");
    const actions = await page.evaluate(() => JSON.parse(localStorage.getItem("unpin:review:v3")).decisions.map((decision) => decision.action));
    expect(actions).toEqual(["keep_all"]);
    expect(errors).toEqual([]);
  } finally { await other.close(); }
});

test("Chromium local HTML reports reuse decisions across different report paths", async ({ page }) => {
  await page.route("file://**/*.html", (route) => route.continue());
  await page.goto(pathToFileURL(fixturePath).href);
  await decide(page, "exact-1", "keep_all");
  await page.goto(pathToFileURL(resolve(repoRoot, "test-results/subset.html")).href);
  await expect(page.locator('#exact-1')).toHaveAttribute("data-decision", "keep_all");
});

test("comparison images fit their viewing area and decisions stay on screen", async ({ page }) => {
  await page.route("https://images.example.test/**", async route => route.fulfill({ contentType: "image/png", body: await readFile(resolve(repoRoot, "tests/fixtures/matching/astronaut.png")) }));
  await page.setViewportSize({ width: 1280, height: 800 });
  await openReport(page);
  const bounds = await page.locator('#exact-1 .image-stage').first().evaluate(stage => {
    const a = stage.getBoundingClientRect(), b = stage.querySelector('img').getBoundingClientRect();
    return { stage: {x:a.x,y:a.y,right:a.right,bottom:a.bottom}, image: {x:b.x,y:b.y,right:b.right,bottom:b.bottom}, fit: getComputedStyle(stage.querySelector('img')).objectFit };
  });
  expect(bounds.fit).toBe("contain");
  expect(bounds.image.x).toBeGreaterThanOrEqual(bounds.stage.x);
  expect(bounds.image.y).toBeGreaterThanOrEqual(bounds.stage.y);
  expect(bounds.image.right).toBeLessThanOrEqual(bounds.stage.right + 1);
  expect(bounds.image.bottom).toBeLessThanOrEqual(bounds.stage.bottom + 1);
  const done = await page.locator('#exact-1 [data-review-button]').boundingBox();
  expect(done.y + done.height).toBeLessThanOrEqual(800);
  await expect(page.locator('.report-tools')).not.toHaveAttribute('open', '');
});

test("decision confirmations expire and newer actions get a fresh Undo window", async ({ page }) => {
  await page.clock.install();
  await openReport(page);
  await page.locator('#exact-1 [data-review-button]').click();
  await expect(page.locator('#review-feedback')).toBeVisible();
  await page.clock.fastForward(4000);
  await page.locator('#exact-2 [data-review-button]').click();
  await page.clock.fastForward(4000);
  await expect(page.locator('#review-feedback')).toBeVisible();
  await page.clock.fastForward(1100);
  await expect(page.locator('#review-feedback')).toBeHidden();
});

test("Undo stays available during interaction and confirmations can be dismissed", async ({ page }) => {
  await page.clock.install();
  await openReport(page);
  await page.locator('#exact-1 [data-review-button]').click();
  await page.locator('#undo-review').hover();
  await page.clock.fastForward(10000);
  await expect(page.locator('#review-feedback')).toBeVisible();
  await page.locator('#undo-review').focus();
  await page.mouse.move(0, 0);
  await page.clock.fastForward(10000);
  await expect(page.locator('#review-feedback')).toBeVisible();
  await page.locator('#exact-2 [data-review-button]').focus();
  await page.clock.fastForward(5100);
  await expect(page.locator('#review-feedback')).toBeHidden();
  await page.locator('#exact-2 [data-review-button]').click();
  await page.locator('#dismiss-feedback').click();
  await expect(page.locator('#review-feedback')).toBeHidden();
  await expect(page.locator('[data-group].is-active [data-review-button]')).toBeFocused();
  await page.locator('#reset-review').click();
  await page.clock.fastForward(5100);
  await expect(page.locator('#review-feedback')).toBeHidden();
  await page.locator('[data-group].is-active [data-review-button]').click();
  await page.keyboard.press('Escape');
  await expect(page.locator('#review-feedback')).toBeHidden();
});
