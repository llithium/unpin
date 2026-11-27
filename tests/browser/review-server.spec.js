// Drives the local review server end to end: decisions saved to its file,
// pins opened in one Pinterest tab, and removal confirmed by asking a stub
// Pinterest when the reviewer comes back.
import { createServer } from "node:http";
import { spawn, spawnSync } from "node:child_process";
import { mkdtemp, readFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { createInterface } from "node:readline";

import { expect, test } from "@playwright/test";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const removed = new Set();
let pinterest;
let reviewServer;
let reviewUrl;
let storePath;
const pageErrors = new WeakMap();

const cargo = (args) => {
  const result = spawnSync("cargo", ["run", "--quiet", "--locked", ...args], { cwd: repoRoot, encoding: "utf8" });
  if (result.status !== 0) throw new Error(`cargo ${args.join(" ")} failed:\n${result.stderr}`);
};

test.beforeAll(async () => {
  const directory = await mkdtemp(join(tmpdir(), "unpin-review-"));
  const scan = join(directory, "scan.json");
  storePath = join(directory, "review.json");
  cargo(["--example", "render_test_reports", "--", scan, "--json"]);
  const build = spawnSync("cargo", ["build", "--quiet", "--locked", "--example", "serve_test_report"], { cwd: repoRoot, encoding: "utf8" });
  if (build.status !== 0) throw new Error(`building the review server failed:\n${build.stderr}`);

  // Stands in for Pinterest's pin resource: 404 once a pin is removed.
  pinterest = createServer((request, response) => {
    const url = new URL(request.url, "http://stub");
    if (!url.pathname.startsWith("/resource/PinResource/get/")) {
      response.writeHead(404).end();
      return;
    }
    const id = JSON.parse(url.searchParams.get("data")).options.id;
    response.writeHead(removed.has(id) ? 404 : 200, { "content-type": "application/json" });
    response.end(JSON.stringify(removed.has(id)
      ? { resource_response: { error: { http_status: 404, code: 50, message: "Pin not found." }, data: null } }
      : { resource_response: { data: { id, board: { name: "Ideas" } } } }));
  });
  await new Promise((done) => pinterest.listen(0, "127.0.0.1", done));
  const apiRoot = `http://127.0.0.1:${pinterest.address().port}/`;

  // Run the built example directly so stopping it never orphans a child.
  const binary = resolve(process.env.CARGO_TARGET_DIR || join(repoRoot, "target"), "debug", "examples", "serve_test_report");
  reviewServer = spawn(binary, [scan, "--api-root", apiRoot, "--store", storePath], { cwd: repoRoot });
  reviewUrl = await new Promise((done, fail) => {
    createInterface({ input: reviewServer.stdout }).once("line", done);
    reviewServer.once("exit", (code) => fail(new Error(`review server exited with ${code}`)));
  });
});

test.afterAll(async () => {
  reviewServer?.kill();
  if (pinterest) await new Promise((done) => pinterest.close(done));
});

test.beforeEach(async ({ context, page }) => {
  pageErrors.set(page, []);
  page.on("pageerror", (error) => pageErrors.get(page).push(error));
  await context.route("https://images.example.test/**", (route) => route.fulfill({
    status: 200, contentType: "image/png",
    body: Buffer.from("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=", "base64"),
  }));
  await context.route("https://www.pinterest.com/**", (route) => route.fulfill({
    status: 200, contentType: "text/html", body: "<title>Pinterest</title>",
  }));
});

test.afterEach(async ({ page }) => {
  expect(pageErrors.get(page), "the review page must not raise browser errors").toEqual([]);
});

test("decisions are saved to the review file, not browser storage", async ({ page }) => {
  await page.goto(reviewUrl);
  await page.locator('.match-link[data-target="exact-1"]').click();
  const match = page.locator("#exact-1");
  await match.getByRole("checkbox", { name: "Flag for removal, pin 102", exact: true }).check();
  await match.locator("[data-review-button]").click();
  await expect.poll(async () => {
    try { return JSON.parse(await readFile(storePath, "utf8")).decisions.length; } catch (_) { return 0; }
  }).toBe(1);
  expect(await page.evaluate(() => window.localStorage.getItem("unpin:review:v3"))).toBeNull();
  await expect(page.locator(".review-notice")).toContainText(storePath);

  // A fresh browser profile sees the same decisions.
  const other = await page.context().browser().newContext();
  const fresh = await other.newPage();
  await fresh.goto(reviewUrl);
  await expect(fresh.locator("#exact-1")).toHaveAttribute("data-reviewed", "true");
  await expect(fresh.locator("#cleanup-count")).toHaveText("1");
  await other.close();
});

test("returning from Pinterest confirms the removal and moves to the next pin", async ({ page, context }) => {
  await page.goto(reviewUrl);
  const visual = page.locator("#visual-1");
  await page.locator('label[for="filter-all"]').click();
  await page.locator('.match-link[data-target="visual-1"]').click();
  const boxes = visual.locator("[data-remove-pin]");
  await boxes.last().check();
  await visual.locator("[data-review-button]").click();
  await page.locator("#cleanup-toggle").click();
  await expect(page.locator("#cleanup-help")).toContainText("checks with Pinterest");
  await expect(page.locator("#cleanup-help")).toContainText("--cookies-from-browser");

  const rows = page.locator(".cleanup-row");
  const current = page.locator("#cleanup-current");
  const firstId = await rows.first().getAttribute("data-pin-id");
  await expect(current.locator(".current-title")).toContainText(`Pin ${firstId}`);
  const [pinterestTab] = await Promise.all([
    context.waitForEvent("page"),
    current.locator("[data-current-open]").click(),
  ]);
  await pinterestTab.waitForURL(`https://www.pinterest.com/pin/${firstId}/`);
  expect(await pinterestTab.evaluate(() => document.referrer)).toBe("");

  // Not deleted yet: coming back says so and keeps the pin on the list.
  await page.waitForTimeout(900);
  await page.bringToFront();
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect(rows.first()).toHaveAttribute("data-state", "present");
  await expect(rows.first().locator("[data-cleanup-status]")).toHaveText("Still on Pinterest, on Ideas");
  await expect(current.locator("[data-current-status]")).toHaveText("Still on Pinterest, on Ideas");
  await expect(current.locator("[data-current-open]")).toBeFocused();

  // A second open, from the full list, reuses the same Pinterest tab.
  const pagesBefore = context.pages().length;
  await rows.first().locator("[data-cleanup-open]").click();
  await pinterestTab.waitForLoadState();
  expect(context.pages().length).toBe(pagesBefore);

  removed.add(firstId);
  await page.waitForTimeout(900);
  await page.bringToFront();
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  await expect(rows.first()).toHaveAttribute("data-state", "removed");
  const secondId = await rows.nth(1).getAttribute("data-pin-id");
  await expect(current.locator(".current-title")).toContainText(`Pin ${secondId}`);
  await expect(current.locator("[data-current-open]")).toBeFocused();
  await expect.poll(async () => JSON.parse(await readFile(storePath, "utf8")).removed_pins[firstId]).toBeTruthy();

  // Check all picks up a pin removed elsewhere.
  removed.add(secondId);
  await page.locator("#cleanup-check-all").click();
  await expect(rows.nth(1)).toHaveAttribute("data-state", "removed");

  await page.reload();
  await expect(page.locator(`[data-group] .pin-card[data-pin-id="${firstId}"]`).first()).toHaveAttribute("data-removed", "true");
});

test("the server refuses other hosts and cross-site writes", async ({ request }) => {
  const url = new URL(reviewUrl);
  const api = `${reviewUrl}api/decisions`;
  expect((await request.get(reviewUrl, { headers: { host: "attacker.test" } })).status()).toBe(403);
  expect((await request.put(api, { headers: { "content-type": "text/plain" }, data: "{}" })).status()).toBe(403);
  expect((await request.put(api, { headers: { "content-type": "application/json", origin: "https://attacker.test" }, data: "{}" })).status()).toBe(403);
  expect((await request.get(`${url.origin}/wrong-token/api/pins/1`)).status()).toBe(404);
  expect((await request.get(`${reviewUrl}api/pins/not-a-number`)).status()).toBe(400);
  const response = await request.get(reviewUrl);
  expect(response.headers()["referrer-policy"]).toBe("no-referrer");
  expect(response.headers()["cache-control"]).toBe("no-store");
});
