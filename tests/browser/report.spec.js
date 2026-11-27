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

// Opens the fixture with every match shown and the first one in focus,
// or, with { gallery: true }, as it first appears.
async function openReport(page, { gallery = false } = {}) {
  await page.goto(`${origin}${fixtureUrl}`);
  await expect(page.locator("[data-group]")).toHaveCount(3);
  if (gallery) return;
  await page.locator('label[for="filter-all"]').click();
  await page.locator(".match-link:not(.is-filtered)").first().click();
}

// Opens one match from the gallery, the way a person would.
async function openMatch(page, target) {
  if (!(await page.locator("body").evaluate((body) => body.classList.contains("gallery-mode")))) {
    await page.locator("#gallery-toggle").click();
  }
  const link = page.locator(`.match-link[data-target="${target}"]`);
  const filtered = () => link.evaluate((element) => element.classList.contains("is-filtered"));
  if (await filtered()) {
    const all = page.locator('label[for="filter-all"]');
    if (await all.count()) await all.click();
  }
  if (await filtered()) await page.locator("#unreviewed-only").uncheck();
  await link.click();
}

async function decide(page, groupId, action) {
  const group = page.locator(`#${groupId}`);
  if (action === "remove") await group.locator("[data-remove-smaller]").click();
  else await group.locator(`[data-decision-action="${action}"]`).click();
}

function visibleGroups(page) {
  return page.locator('[data-group]:not(.is-filtered)');
}

test("scope, kind, and review filters compose as a user queue", async ({ page }) => {
  await openReport(page, { gallery: true });
  await page.locator('label[for="filter-all"]').click();
  await expect(page.locator("#visible-count")).toHaveText("3 shown");

  await page.locator('label[for="filter-same"]').click();
  await page.locator('label[for="kind-exact"]').click();
  await expect(visibleGroups(page)).toHaveCount(1);
  await expect(page.locator(".match-link:visible")).toHaveCount(1);
  await expect(page.locator("#visible-count")).toHaveText("1 shown");

  await page.locator("#unreviewed-only").check();
  await openMatch(page, "exact-1");
  await page.locator("#exact-1 [data-decision-action=keep_all]").click();
  await expect(visibleGroups(page)).toHaveCount(0);
  await expect(page.locator("#filter-empty")).toHaveClass(/is-visible/);
  await expect(page.locator("#filter-empty")).toBeVisible();
  await expect(page.locator("#visible-count")).toHaveText("0 shown");

  await page.locator("#gallery-toggle").click();
  await page.locator('label[for="filter-all"]').click();
  await expect(page.locator('[data-group].is-active')).toHaveCount(1);
  await expect(page.locator('[data-group].is-active')).toHaveAttribute("id", "exact-2");
});

test("the gallery opens on same-board matches with the easy calls first", async ({ page }) => {
  await openReport(page, { gallery: true });
  await expect(page.locator("body")).toHaveClass(/gallery-mode/);
  await expect(page.locator("#filter-same")).toBeChecked();
  await expect(page.locator("#visible-count")).toHaveText("2 shown");
  await expect(page.locator(".match-link:visible")).toHaveCount(2);
  await expect(page.locator("[data-group]:visible")).toHaveCount(0);
  await expect(page.locator(".scope-note")).toBeHidden();
  await page.locator('label[for="filter-cross"]').click();
  await expect(page.locator(".scope-note")).toBeVisible();
  await page.locator('label[for="filter-same"]').click();

  await expect(page.locator("#easy-calls")).toBeVisible();
  // The identical pair and the 98% similar pair on one board.
  await expect(page.locator("#easy-calls h2")).toHaveText("2 duplicates are easy calls");
  await expect(page.locator("#accept-easy")).toHaveText("Accept all 2");
  await expect(page.locator("#tray")).toBeHidden();
});

test("Look through them first selects the easy calls and advances review", async ({ page }) => {
  await openReport(page, { gallery: true });
  await page.evaluate(() => (window.__storageWrites = []));
  await page.locator("#quick-wins").click();
  await expect
    .poll(() => page.evaluate(() => window.__storageWrites.length))
    .toBeGreaterThanOrEqual(1);

  await expect(page.locator("#filter-same")).toBeChecked();
  await expect(page.locator("#kind-all")).toBeChecked();
  await expect(page.locator("#unreviewed-only")).toBeChecked();
  await expect(visibleGroups(page)).toHaveCount(2);
  await expect(page.locator("body")).not.toHaveClass(/gallery-mode/);
  await expect(page.locator('[data-group].is-active')).toHaveAttribute("id", "exact-1");

  await page.locator("#exact-1 [data-remove-smaller]").click();
  await expect(page.locator('[data-group].is-active')).toHaveAttribute("id", "visual-1");
  await expect(page.locator("#easy-count")).toHaveText("1");
  await page.keyboard.press("a");
  await expect(page.locator("#visual-1")).toHaveAttribute("data-decision", "keep_all");
  await expect(page.locator("#easy-calls")).toBeHidden();
  await expect(page.locator("#review-progress")).toHaveText("2 / 3");
});

test("accepting the easy calls keeps the best copy and fills the tray", async ({ page }) => {
  await openReport(page, { gallery: true });
  await page.locator("#accept-easy").click();
  await expect(page.locator("#exact-1")).toHaveAttribute("data-decision", "remove");
  await expect(page.locator("#exact-1")).toHaveAttribute("data-reviewed", "true");
  await expect(page.locator('#exact-1 [data-remove-pin="102"]')).toBeChecked();
  await expect(page.locator('#exact-1 [data-remove-pin="101"]')).not.toBeChecked();
  await expect(page.locator("#exact-2")).toHaveAttribute("data-decision", "");
  // The similar match keeps its largest image and drops the rest, extra saves included.
  await expect(page.locator('#visual-1 [data-remove-pin="301"]')).not.toBeChecked();
  await expect(page.locator('#visual-1 [data-remove-pin="302"]')).toBeChecked();
  await expect(page.locator('#visual-1 [data-remove-pin="303"]')).toBeChecked();
  await expect(page.locator("#easy-calls")).toBeHidden();
  await expect(page.locator('.match-link[data-target="exact-1"] [data-chip]')).toHaveText("1 in tray");
  await expect(page.locator('.match-link[data-target="visual-1"] [data-chip]')).toHaveText("2 in tray");
  await expect(page.locator("#review-progress")).toHaveText("2 / 3");

  await expect(page.locator("#tray")).toBeVisible();
  await expect(page.locator("#tray-count")).toHaveText("3 pins");
  await expect(page.locator("#tray-thumbs img")).toHaveCount(3);
  await expect(page.locator("#tray-start")).toBeFocused();
  await expect(page.locator("#cleanup-count")).toHaveText("3");

  await page.locator("#undo-review").click();
  await expect(page.locator("#exact-1")).toHaveAttribute("data-decision", "");
  await expect(page.locator("#easy-calls")).toBeVisible();
  await expect(page.locator("#tray")).toBeHidden();
});

test("the remove step shows one pin at a time with what stays behind", async ({ page }) => {
  await openReport(page, { gallery: true });
  await page.locator("#accept-easy").click();
  await page.locator("#tray-start").click();
  await expect(page.locator("body")).toHaveClass(/cleanup-mode/);
  await expect(page.locator("#cleanup-toggle")).toHaveAttribute("aria-current", "step");
  await expect(page.locator("#decide-step")).toHaveAttribute("aria-current", "false");
  await expect(page.locator("#tray")).toBeHidden();
  const current = page.locator("#cleanup-current");
  await expect(current).toBeVisible();
  await expect(current.locator(".current-title")).toHaveText("Pin 102 · Ideas");
  await expect(current.locator(".current-detail")).toHaveText("The 1200 × 800 copy. Your 1200 × 800 copy stays.");
  const open = current.getByRole("link", { name: "Open on Pinterest" });
  await expect(open).toBeFocused();
  await expect(open).toHaveAttribute("href", "https://www.pinterest.com/pin/102/");
  await expect(open).toHaveAttribute("target", "unpin-pinterest");

  // Keeping it takes it out of the tray but keeps the match reviewed.
  await current.getByRole("button", { name: "Keep this one" }).click();
  await expect(page.locator("#exact-1")).toHaveAttribute("data-decision", "keep_all");
  await expect(current.locator(".current-title")).toHaveText("Pin 303 · Mood board");
  await page.locator("#undo-review").click();
  await expect(page.locator("#exact-1")).toHaveAttribute("data-decision", "remove");

  for (const pin of ["303", "302", "102"]) {
    await expect(current.locator(".current-title")).toContainText(`Pin ${pin}`);
    await current.getByRole("button", { name: "I deleted it" }).click();
  }
  await expect(page.locator("#cleanup-done")).toBeVisible();
  await expect(page.locator("#cleanup-progress")).toHaveText("3 of 3");
  await expect(current).toBeHidden();

  await page.locator("#decide-step").click();
  await expect(page.locator("body")).toHaveClass(/gallery-mode/);
  await expect(page.locator("body")).not.toHaveClass(/cleanup-mode/);
  await expect(page.locator("#tray")).toBeHidden();
});

test("focus navigation, gallery, keyboard shortcuts, and active state stay coherent", async ({ page }) => {
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
  await page.locator("#exact-2 [data-remove-pin]").first().focus();
  await page.keyboard.press("j");
  await expect(page.locator('[data-group].is-active')).toHaveAttribute("id", "exact-2");
  await page.locator("main").click({ position: { x: 5, y: 5 } });
  await page.keyboard.press("j");
  await expect(page.locator('[data-group].is-active')).toHaveAttribute("id", "visual-1");
  await page.keyboard.press("k");
  await expect(page.locator('[data-group].is-active')).toHaveAttribute("id", "exact-2");
  // Shortcuts keep working after a button click leaves focus on a button.
  await page.locator("#next-match").focus();
  await page.keyboard.press("k");
  await expect(page.locator('[data-group].is-active')).toHaveAttribute("id", "exact-1");
  await page.keyboard.press("j");
  await expect(page.locator('[data-group].is-active')).toHaveAttribute("id", "exact-2");

  await page.locator("#gallery-toggle").click();
  await expect(page.locator("body")).toHaveClass(/gallery-mode/);
  await expect(page.locator('.match-link[data-target="exact-2"]')).toBeFocused();
  await page.locator("main").click({ position: { x: 5, y: 5 } });
  await page.keyboard.press("o");
  await expect(page.locator("body")).not.toHaveClass(/gallery-mode/);
  await expect(page.locator('[data-group].is-active')).toHaveCount(1);
  await page.keyboard.press("Escape");
  await expect(page.locator("body")).toHaveClass(/gallery-mode/);
  await page.keyboard.press("o");

  await page.keyboard.press("a");
  await expect(page.locator("#exact-2")).toHaveAttribute("data-reviewed", "true");
  await expect(page.locator("#review-progress")).toHaveText("1 / 3");
});

test("review state persists, rejects inconsistent state, and resets", async ({ page }) => {
  await openReport(page);
  const key = "unpin:review:v3";

  await page.evaluate(() => (window.__storageWrites = []));
  await page.locator("#exact-1 [data-decision-action=keep_all]").click();
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

  await openMatch(page, "exact-1");
  await page.locator("#exact-1 [data-decision-action=keep_all]").click();
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
  await expect(noJsPage.locator(".steps")).toBeHidden();
  await expect(noJsPage.locator(".gallery")).toBeHidden();
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
  await expect(page.locator(".gallery")).toBeHidden();
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
  const review = page.locator("#exact-1 [data-decision-action=keep_all]");
  await review.focus();
  await page.keyboard.press("Enter");
  await expect(page.locator("#exact-2 [data-remove-smaller]")).toBeFocused();
  await page.locator("#undo-review").click();
  await expect(page.locator("#exact-1 [data-remove-smaller]")).toBeFocused();
  await expect(page.locator("#exact-1")).toHaveAttribute("data-reviewed", "false");
  await expect(page.locator("#review-progress")).toHaveText("0 / 3");

  await page.locator("#gallery-toggle").click();
  await page.locator("#quick-wins").click();
  await review.click();
  await page.locator("#visual-1 [data-decision-action=keep_all]").click();
  await expect(page.locator("#clear-filters")).toBeFocused();
  await page.locator("#clear-filters").click();
  await expect(visibleGroups(page)).toHaveCount(3);
  await expect(page.locator("#exact-1")).toHaveAttribute("data-reviewed", "true");
  await page.locator("#reset-review").click();
  await page.locator("#undo-review").click();
  await expect(page.locator("#exact-1")).toHaveAttribute("data-reviewed", "true");
  await expect(page.locator("#review-progress")).toHaveText("2 / 3");
});

test("gallery cards decide in one tap without opening the match", async ({ page }) => {
  await openReport(page, { gallery: true });
  const card = page.locator('.match-card:has([data-target="visual-1"])');
  const remove = card.getByRole("button", { name: "Remove 2 copies" });
  await remove.click();
  await expect(page.locator("body")).toHaveClass(/gallery-mode/);
  await expect(page.locator("#visual-1")).toHaveAttribute("data-decision", "remove");
  await expect(page.locator("#visual-1")).toHaveAttribute("data-reviewed", "true");
  await expect(remove).toHaveAttribute("aria-pressed", "true");
  await expect(card.locator("[data-chip]")).toHaveText("2 in tray");
  await expect(page.locator("#tray-count")).toHaveText("2 pins");
  await card.getByRole("button", { name: "Keep all" }).click();
  await expect(page.locator("#visual-1")).toHaveAttribute("data-decision", "keep_all");
  await expect(remove).toHaveAttribute("aria-pressed", "false");
  await expect(page.locator("#tray")).toBeHidden();
  await page.locator("#undo-review").click();
  await expect(page.locator("#visual-1")).toHaveAttribute("data-decision", "remove");
});

test("the match view saves a decision and moves on in one step", async ({ page }) => {
  await openReport(page);
  await expect(page.locator("#exact-1 [data-remove-smaller]")).toHaveText(/Remove the duplicate/);
  await expect(page.locator("#exact-1 [data-decision-action=keep_all]")).toHaveText(/Keep both/);
  await page.locator("#exact-1 [data-remove-smaller]").click();
  await expect(page.locator("#exact-1")).toHaveAttribute("data-decision", "remove");
  await expect(page.locator("#exact-1")).toHaveAttribute("data-reviewed", "true");
  await expect(page.locator('#exact-1 [data-remove-pin="102"]')).toBeChecked();
  await expect(page.locator('[data-group].is-active')).toHaveAttribute("id", "exact-2");
  await page.keyboard.press("r");
  await expect(page.locator("#exact-2")).toHaveAttribute("data-decision", "remove");
  await expect(page.locator('[data-group].is-active')).toHaveAttribute("id", "visual-1");
  await expect(page.locator("#visual-1 [data-remove-smaller]")).toHaveText(/Remove 2 smaller copies/);
  await expect(page.locator("#review-progress")).toHaveText("2 / 3");
});

test("a completed queue explains completion and can be revisited", async ({ page }) => {
  await openReport(page, { gallery: true });
  await page.locator('label[for="filter-all"]').click();
  await page.locator("#unreviewed-only").check();
  await openMatch(page, "exact-1");
  for (const id of ["exact-1", "exact-2", "visual-1"]) {
    await page.locator(`#${id} [data-decision-action=keep_all]`).click();
  }
  await expect(page.locator("#filter-empty-title")).toHaveText("All matches reviewed");
  await expect(page.locator("#clear-filters")).toBeFocused();
  await page.locator("#undo-review").click();
  await expect(page.locator("#visual-1 [data-remove-smaller]")).toBeFocused();
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
  await page.locator("#exact-1 [data-decision-action=keep_all]").click();
  await expect(page.locator("#storage-notice")).toBeVisible();
  await expect(page.locator("#review-progress")).toHaveText("1 / 3");
});

test("empty scans show their identity and warnings without unusable review controls", async ({ page }) => {
  await page.goto(`${origin}/empty.html`);
  await expect(page.locator(".report-intro")).toBeVisible();
  await expect(page.locator(".empty")).toBeVisible();
  await expect(page.locator(".empty strong")).toHaveText("No matches found");
  await expect(page.locator(".scan-warning")).toBeVisible();
  await expect(page.locator("#scan-warnings")).toHaveAttribute("open", "");
  await expect(page.locator(".gallery")).toHaveCount(0);
  await expect(page.locator(".steps")).toHaveCount(0);
  await expect(page.locator("#tray")).toHaveCount(0);
});

test("mobile comparisons align images and retain progress, labels, and touch targets", async ({ browser }) => {
  const context = await browser.newContext({ viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true });
  await mockImages(context);
  const page = await context.newPage();
  await page.goto(`${origin}${fixtureUrl}`);
  await expect(page.locator("#review-progress")).toBeVisible();
  await expect(page.locator("#match-filters")).toBeVisible();
  for (const label of await page.locator(".filters label").all()) {
    expect((await label.boundingBox()).height).toBeGreaterThanOrEqual(44);
  }
  await openMatch(page, "exact-1");
  await expect(page.locator("#gallery-toggle")).toBeVisible();
  const images = await page.locator("#exact-1 .image-stage").evaluateAll(elements => elements.map(el => ({ x: el.getBoundingClientRect().x, y: el.getBoundingClientRect().y })));
  expect(images[0].y).toBe(images[1].y);
  expect(images[1].x).toBeGreaterThan(images[0].x);
  for (const selector of ["#previous-match", "#next-match", "#gallery-toggle", "#exact-1 .button.primary"]) {
    expect((await page.locator(selector).first().boundingBox()).height).toBeGreaterThanOrEqual(44);
  }
  await page.locator("#next-match").click();
  const heading = await page.locator("[data-group].is-active .match-heading").boundingBox();
  const header = await page.locator(".topbar").boundingBox();
  expect(heading.y).toBeGreaterThanOrEqual(header.y + header.height);
  expect(heading.y).toBeLessThan(200);
  await context.close();
});

test("long content and three-pin groups fit narrow, tablet, desktop, and gallery layouts", async ({ page }) => {
  await page.goto(`${origin}/long-content.html`);
  for (const width of [320, 390, 620, 768, 880, 1024, 1440]) {
    await page.setViewportSize({ width, height: 900 });
    for (const gallery of [true, false]) {
      if (gallery) { if (await page.locator("#gallery-toggle").isVisible()) await page.locator("#gallery-toggle").click(); }
      else await openMatch(page, "exact-1");
      const overflow = await page.evaluate(() => document.documentElement.scrollWidth > innerWidth);
      expect(overflow, `${width}px, gallery=${gallery}`).toBe(false);
      const cards = await page.locator("#exact-1 .pin-card").evaluateAll(elements => elements.map(el => ({width: el.clientWidth, content: el.scrollWidth})));
      for (const card of cards) expect(card.content).toBeLessThanOrEqual(card.width + 1);
    }
  }
});

test("reviewed gallery cards fit long match numbers on a narrow screen", async ({ page }) => {
  await page.setViewportSize({ width: 320, height: 844 });
  await openReport(page, { gallery: true });
  const tile = page.locator('.match-link[data-target="visual-1"]');
  await tile.click();
  await decide(page, "visual-1", "keep_all");
  await page.locator("#gallery-toggle").click();
  await expect(tile).toHaveAccessibleName(/Reviewed/);
  await expect(tile.locator("[data-chip]")).toHaveText("Kept all");

  const number = tile.locator(".match-number");
  await number.evaluate(label => { label.textContent = "Visual 1000"; });
  const [tileBox, copyBox, stateBox, stackBox] = await Promise.all([
    tile.boundingBox(),
    tile.locator(".match-link-copy").boundingBox(),
    tile.locator(".match-link-state").boundingBox(),
    tile.locator(".match-stack").boundingBox(),
  ]);
  expect(copyBox.x + copyBox.width).toBeLessThanOrEqual(tileBox.x + tileBox.width + 1);
  expect(stateBox.x + stateBox.width).toBeLessThanOrEqual(stackBox.x + stackBox.width);
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(320);
});

test("grouped visual variants retain every save with two previews", async ({ page }) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await openReport(page);
  await openMatch(page, "visual-1");
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
  await expect(page.locator("#exact-1")).toHaveAttribute("data-reviewed", "true");
  await expect(page.locator('[data-group].is-active')).toHaveAttribute("id", "exact-2");
  await decide(page, "exact-2", "keep_all");
  await expect(page.locator("#review-progress")).toHaveText("2 / 3");
  const downloadReady = page.waitForEvent("download");
  await page.locator("#gallery-toggle").click();
  await page.locator(".report-tools > summary").click();
  await page.locator("#export-review").click();
  const download = await downloadReady;
  const exported = JSON.parse(await readFile(await download.path(), "utf8"));
  expect(exported.schema_version).toBe(1);
  expect(exported.decisions).toHaveLength(2);
  expect(exported.decisions.find((decision) => decision.action === "remove").remove_pin_ids).toEqual(["102"]);
  expect(exported.decisions.every((decision) => decision.items.every((item) => item.pin_url && item.image_url))).toBe(true);

  await page.goto(`${origin}/subset.html`);
  await expect(page.locator("#exact-1")).toHaveAttribute("data-decision", "remove");
  await expect(page.locator('#exact-1 [data-remove-pin="102"]')).toBeChecked();
  await page.locator("#reset-review").click();
  const remaining = await page.evaluate(() => JSON.parse(localStorage.getItem("unpin:review:v3")).decisions);
  expect(remaining.map((decision) => decision.action).sort()).toEqual(["keep_all"]);
  await page.evaluate(() => localStorage.clear());
  await page.goto(`${origin}${fixtureUrl}`);
  await page.locator("#review-file").setInputFiles({ name: "decisions.json", mimeType: "application/json", buffer: Buffer.from(JSON.stringify(exported)) });
  await expect(page.locator("#transfer-status")).toHaveText("Imported 2 decisions; 2 match this report.");
  await expect(page.locator("#exact-1")).toHaveAttribute("data-decision", "remove");
  await expect(page.locator("#exact-2")).toHaveAttribute("data-decision", "keep_all");
  await expect(page.locator("#visual-1")).toHaveAttribute("data-decision", "");
  await page.locator("#undo-review").click();
  await expect(page.locator("#review-progress")).toHaveText("0 / 3");

  // With three saves, a pick saves but stays on the match for more picks.
  await openMatch(page, "visual-1");
  await page.locator('#visual-1 [data-remove-pin="302"]').check();
  await expect(page.locator("#visual-1")).toHaveAttribute("data-decision", "remove");
  await page.reload();
  await expect(page.locator("#visual-1")).toHaveAttribute("data-decision", "remove");
  await expect(page.locator('#visual-1 [data-remove-pin="302"]')).toBeChecked();
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
  await openMatch(page, "exact-1");
  await page.locator('#exact-1 [data-remove-pin="102"]').check();
  await expect(page.locator("#exact-1")).toHaveAttribute("data-decision", "remove");
  await expect(page.locator("#reset-review")).toBeEnabled();
  await expect(page.locator("#review-progress")).toHaveText("1 / 3");
});

test("media covers remain inspection-only and are never easy calls", async ({ page }) => {
  await page.goto(`${origin}/media.html`);
  await expect(page.locator("#exact-1 .match-description")).toContainText("Matching covers");
  await expect(page.locator("#exact-1 .badge").filter({ hasText: "Inspect pin content" })).toHaveCount(0);
  await expect(page.locator("#visual-1 .match-description")).toContainText("Matching covers");
  await expect(page.locator("#visual-1 .badge").filter({ hasText: "Inspect pin content" })).toHaveCount(0);
  await expect(page.locator("#easy-calls")).toHaveCount(0);
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
    await openMatch(page, "exact-2");
    await decide(page, "exact-2", "keep_all");
    await openMatch(other, "exact-1");
    await other.locator('#exact-1 [data-remove-pin="102"]').check();
    await expect(page.locator('#exact-1')).toHaveAttribute("data-decision", "remove");
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
  await openMatch(page, "exact-1");
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
  const done = await page.locator('#exact-1 [data-decision-action=keep_all]').boundingBox();
  expect(done.y + done.height).toBeLessThanOrEqual(800);
  await expect(page.locator('.report-tools')).not.toHaveAttribute('open', '');
});

test("widescreen focus uses the available width and keeps large portraits together", async ({ page }) => {
  await page.route("https://images.example.test/20*.jpg", route => route.fulfill({
    contentType: "image/svg+xml",
    body: '<svg xmlns="http://www.w3.org/2000/svg" width="900" height="1200"><rect width="900" height="1200" fill="green"/></svg>',
  }));
  await page.setViewportSize({ width: 2560, height: 1200 });
  await openReport(page);
  const landscape = await page.locator("#exact-1 .comparison").boundingBox();
  const landscapeRightGutter = 2560 - landscape.x - landscape.width;

  await openMatch(page, "exact-2");
  const stages = page.locator("#exact-2 .image-stage");
  await expect(stages.locator("img.is-loaded")).toHaveCount(2);
  const [first, second] = await stages.evaluateAll(stages =>
    stages.map(stage => {
      const { x, y, width, height } = stage.getBoundingClientRect();
      const image = stage.querySelector("img");
      const scale = Math.min(width / image.naturalWidth, height / image.naturalHeight);
      const paintedWidth = image.naturalWidth * scale;
      const paintedHeight = image.naturalHeight * scale;
      return {
        top: y + (height - paintedHeight) / 2,
        bottom: y + (height + paintedHeight) / 2,
        height: paintedHeight,
        left: x + (width - paintedWidth) / 2,
        right: x + (width + paintedWidth) / 2,
      };
    })
  );
  expect.soft(landscapeRightGutter, "landscape comparison fills the report width").toBeLessThan(40);
  expect.soft(first.height, "portrait uses most of the viewport height").toBeGreaterThanOrEqual(820);
  expect(second.top).toBe(first.top);
  expect(second.height).toBe(first.height);
  expect(first.top).toBeGreaterThanOrEqual(0);
  expect(first.bottom).toBeLessThanOrEqual(1200);
  expect.soft(second.left - first.right, "both portraits are close enough for direct comparison").toBeLessThan(30);
  const review = await page.locator("#exact-2 [data-decision-action=keep_all]").boundingBox();
  expect(review.y + review.height).toBeLessThanOrEqual(1200);
  expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(2560);
});

test("decision confirmations expire and newer actions get a fresh Undo window", async ({ page }) => {
  await page.clock.install();
  await openReport(page);
  await page.locator('#exact-1 [data-decision-action=keep_all]').click();
  await expect(page.locator('#review-feedback')).toBeVisible();
  await page.clock.fastForward(4000);
  await page.locator('#exact-2 [data-decision-action=keep_all]').click();
  await page.clock.fastForward(4000);
  await expect(page.locator('#review-feedback')).toBeVisible();
  await page.clock.fastForward(1100);
  await expect(page.locator('#review-feedback')).toBeHidden();
});

test("Undo stays available during interaction and confirmations can be dismissed", async ({ page }) => {
  await page.clock.install();
  await openReport(page);
  await page.locator('#exact-1 [data-decision-action=keep_all]').click();
  await page.locator('#undo-review').hover();
  await page.clock.fastForward(10000);
  await expect(page.locator('#review-feedback')).toBeVisible();
  await page.locator('#undo-review').focus();
  await page.mouse.move(0, 0);
  await page.clock.fastForward(10000);
  await expect(page.locator('#review-feedback')).toBeVisible();
  await page.locator('#exact-2 [data-decision-action=keep_all]').focus();
  await page.clock.fastForward(5100);
  await expect(page.locator('#review-feedback')).toBeHidden();
  await page.locator('#exact-2 [data-decision-action=keep_all]').click();
  await page.locator('#dismiss-feedback').click();
  await expect(page.locator('#review-feedback')).toBeHidden();
  await expect(page.locator('[data-group].is-active [data-remove-smaller]')).toBeFocused();
  await page.locator('#reset-review').click();
  await page.clock.fastForward(5100);
  await expect(page.locator('#review-feedback')).toBeHidden();
  await page.locator('[data-group].is-active [data-decision-action=keep_all]').click();
  await page.keyboard.press('Escape');
  await expect(page.locator('#review-feedback')).toBeHidden();
});

for (const width of [320, 1280]) {
  test(`picking a copy to remove saves in one step at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 900 });
    await openReport(page);
    const match = page.locator('#exact-1');
    await expect(match.getByRole('checkbox', { name: 'Remove this one, pin 102', exact: true })).toBeVisible();
    const flag = match.locator('[data-remove-pin="102"]');
    await expect(match.locator('.decision-help')).toHaveText('Each choice saves straight away. To choose the copy yourself, tap Remove this one on it.');
    await expect(match.locator('[data-review-button], .decision-more')).toHaveCount(0);
    // Until something is decided, the suggestion is marked on the copies.
    await expect(match.locator('.pin-card[data-pin-id="102"] .suggestion')).toHaveText('Suggested to remove');
    await expect(match.locator('.pin-card[data-pin-id="102"] .suggestion')).toBeVisible();
    await expect(match.locator('.pin-card[data-pin-id="101"] .suggestion')).toHaveText('Keep');
    await expect(page.locator('.report-tools')).not.toHaveAttribute('open', '');
    await flag.focus();
    await flag.press('Space');
    await expect(flag).toBeChecked();
    await expect(match).toHaveAttribute('data-reviewed', 'true');
    await expect(match.locator('.pin-card[data-pin-id="102"] .suggestion')).toBeHidden();
    await expect(page.locator('#review-feedback-label')).toHaveText('Decision saved');
    await expect(page.locator('#review-progress')).toHaveText('1 / 3');
    await expect(page.locator('[data-group].is-active')).toHaveAttribute('id', 'exact-2');
    await expect(page.locator('#exact-2 [data-remove-smaller]')).toBeFocused();
    await page.getByRole('button', { name: 'Undo', exact: true }).click();
    await expect(flag).not.toBeChecked();
    await expect(page.locator('#review-progress')).toHaveText('0 / 3');
    await flag.check();
    await page.reload();
    await openMatch(page, 'exact-1');
    await expect(flag).toBeChecked();
    await expect(match.locator('[data-decision-notice]')).toHaveText('1 copy going to the removal tray');
    await flag.uncheck();
    await expect(page.locator('#review-feedback-label')).toHaveText('Decision cleared');
    await expect(match).toHaveAttribute('data-reviewed', 'false');
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(width);
  });
}

test('pin controls have specific accessible names, including additional saves', async ({ page }) => {
  await openReport(page);
  const exact = page.locator('#exact-1');
  for (const id of ['101', '102']) {
    await expect(exact.getByRole('checkbox', { name: `Remove this one, pin ${id}`, exact: true })).toHaveCount(1);
    await expect(exact.getByRole('link', { name: `Open pin ${id}`, exact: true })).toHaveAttribute('href', `https://www.pinterest.com/pin/${id}/`);
    await expect(exact.getByRole('link', { name: `Open image for pin ${id}`, exact: true })).toHaveCount(1);
  }
  await openMatch(page, "visual-1");
  const visual = page.locator('#visual-1');
  await expect(page.locator('.match-link[data-target="visual-1"]')).toContainText('98%');
  await visual.getByText('1 additional save', { exact: true }).click();
  const extra = visual.getByRole('checkbox', { name: 'Remove this one, pin 303', exact: true });
  await extra.check();
  await expect(visual.locator('[data-decision-notice]')).toHaveText('1 copy going to the removal tray');
  await page.reload();
  await expect(visual.locator('[data-remove-pin="303"]')).toBeChecked();
});

test('filter radios belong to their named groups and retain keyboard selection styling', async ({ page }) => {
  await openReport(page, { gallery: true });
  await page.locator('label[for="filter-all"]').click();
  const scope = page.getByRole('radiogroup', { name: 'Filter matches by board scope', exact: true });
  const kind = page.getByRole('radiogroup', { name: 'Filter matches by type', exact: true });
  await expect(scope.getByRole('radio')).toHaveCount(3);
  await expect(kind.getByRole('radio')).toHaveCount(3);
  await scope.getByRole('radio', { name: 'All 3', exact: true }).focus();
  await page.keyboard.press('ArrowRight');
  await expect(scope.getByRole('radio', { name: 'Same board 2', exact: true })).toBeChecked();
  await expect(scope.getByRole('radio', { name: 'Same board 2', exact: true })).toBeFocused();
  await expect(kind.getByRole('radio', { name: 'All types 3', exact: true })).toBeChecked();
  const selectedStyle = await scope.locator('label[for="filter-same"]').evaluate(label => ({
    background: getComputedStyle(label).backgroundColor,
    outline: getComputedStyle(label).outlineStyle,
  }));
  expect(selectedStyle.background).toBe('rgb(255, 255, 255)');
  expect(selectedStyle.outline).toBe('solid');
  await kind.getByRole('radio', { name: 'All types 3', exact: true }).focus();
  await page.keyboard.press('ArrowRight');
  await expect(kind.getByRole('radio', { name: 'Identical 2', exact: true })).toBeChecked();
  await expect(visibleGroups(page)).toHaveCount(1);
});

for (const width of [320, 390, 2560]) {
  test(`long board names wrap in full at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 900 });
    await page.goto(`${origin}/long-content.html`);
    await openMatch(page, 'exact-1');
    const board = page.locator('#exact-1 .board').first();
    await expect(board).toHaveText('A very long board name with art references and illustration inspiration');
    await expect(page.locator('#exact-1 details.board-details')).toHaveCount(0);
    expect(await board.evaluate(element => element.scrollWidth <= element.clientWidth)).toBe(true);
    expect(await page.evaluate(() => document.documentElement.scrollWidth)).toBe(width);
  });
}

test("flagged copies form a removal list whose marks survive reloads", async ({ page }) => {
  await openReport(page);
  await expect(page.locator("#cleanup-count")).toHaveText("0");
  await page.locator("#cleanup-toggle").click();
  await expect(page.locator("#cleanup")).toBeVisible();
  await expect(page.locator("#cleanup-empty")).toBeVisible();
  await expect(page.locator("[data-group]:visible")).toHaveCount(0);
  await page.locator("#cleanup-back").click();
  await expect(page.locator("#cleanup")).toBeHidden();
  await expect(page.locator("body")).toHaveClass(/gallery-mode/);

  await openMatch(page, "exact-1");
  const match = page.locator("#exact-1");
  await match.getByRole("checkbox", { name: "Remove this one, pin 102", exact: true }).check();
  await expect(page.locator("#cleanup-count")).toHaveText("1");
  await page.locator("#cleanup-toggle").click();
  const row = page.locator('.cleanup-row[data-pin-id="102"]');
  await expect(row).toHaveAttribute("data-state", "pending");
  await expect(row.locator("[data-cleanup-open]")).toHaveAttribute("href", "https://www.pinterest.com/pin/102/");
  await expect(row.locator("[data-cleanup-open]")).toHaveAttribute("target", "unpin-pinterest");
  await expect(row.locator("[data-cleanup-check]")).toHaveCount(0);
  await expect(page.locator("#cleanup-check-all")).toBeHidden();
  await expect(page.locator("#cleanup-help")).toContainText("mark it removed");

  await row.getByRole("button", { name: "Mark pin 102 removed" }).click();
  await expect(row).toHaveAttribute("data-state", "removed");
  await expect(page.locator("#cleanup-progress")).toHaveText("1 of 1");
  await expect(page.locator("#cleanup-done")).toBeVisible();
  await expect(page.locator("#cleanup-count")).toHaveText("0");

  await page.reload();
  await expect(page.locator('#exact-1 .pin-card[data-pin-id="102"]')).toHaveAttribute("data-removed", "true");
  await page.locator("#cleanup-toggle").click();
  await expect(row).toHaveAttribute("data-state", "removed");
  await row.getByRole("button", { name: "Put pin 102 back on the list" }).click();
  await expect(row).toHaveAttribute("data-state", "pending");
  await expect(page.locator("#cleanup-count")).toHaveText("1");

  await row.locator(".cleanup-jump").click();
  await expect(page.locator("#cleanup")).toBeHidden();
  await expect(page.locator("#exact-1")).toHaveClass(/is-active/);
  await expect(page.locator("#exact-1")).toBeVisible();
});

test("flagging every copy of a match warns in the removal list", async ({ page }) => {
  await openReport(page);
  await openMatch(page, "visual-1");
  const match = page.locator("#visual-1");
  await match.locator(".additional-saves > summary").click();
  for (const box of await match.locator("[data-remove-pin]").all()) await box.check();
  await page.locator("#cleanup-toggle").click();
  await expect(page.locator(".cleanup-row .cleanup-warning").first()).toContainText("Every copy in this match is flagged");
});
