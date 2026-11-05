// Capture the real Rust-rendered report with offline photograph fixtures.
// Usage: node tests/browser/capture-ui-review.mjs <output-directory>
import { createServer } from "node:http";
import { mkdir, readFile } from "node:fs/promises";
import { spawnSync } from "node:child_process";
import { resolve } from "node:path";
import { chromium } from "@playwright/test";

const output = resolve(process.argv[2] || "test-results/ui-review/current");
await mkdir(output, { recursive: true });
for (const scenario of ["mixed", "empty", "long-content", "media"]) {
  const result = spawnSync("cargo", ["run", "--quiet", "--locked", "--example", "render_test_reports", "--", resolve(output, `${scenario}.html`), "--scenario", scenario], { encoding: "utf8" });
  if (result.status !== 0) throw new Error(result.stderr);
}
const server = createServer(async (request, response) => {
  const file = request.url.slice(1);
  if (!["mixed.html", "empty.html", "long-content.html", "media.html"].includes(file)) return response.writeHead(404).end();
  response.writeHead(200, { "content-type": "text/html" });
  response.end(await readFile(resolve(output, file)));
});
await new Promise(done => server.listen(0, "127.0.0.1", done));
const origin = `http://127.0.0.1:${server.address().port}`;
const browser = await chromium.launch();
try {
  const context = await browser.newContext({ viewport: { width: 1440, height: 1000 }, deviceScaleFactor: 1 });
  const mockImage = async route => {
    const id = new URL(route.request().url()).pathname.slice(1, 4);
    const file = id.startsWith("1") ? "chelsea.png" : id.startsWith("2") ? "astronaut.png" : "camera.png";
    await route.fulfill({ contentType: "image/png", body: await readFile(resolve("tests/fixtures/matching", file)) });
  };
  await context.route("https://images.example.test/**", mockImage);
  const page = await context.newPage();
  const capture = async (name, target = page) => {
    await target.locator(".image-stage img").evaluateAll(images => Promise.all(images.map(img => { img.loading = "eager"; return img.decode().catch(() => {}); })));
    await target.screenshot({ path: resolve(output, `${name}.png`), fullPage: true, animations: "disabled" });
  };
  await page.goto(`${origin}/mixed.html`);
  await capture("desktop-exact");
  await page.locator("#exact-1 .image-stage").first().focus();
  await capture("desktop-keyboard-focus");
  await page.locator("#exact-1 .pin-details > summary").first().click();
  await capture("desktop-details");
  await page.locator("#exact-1 .pin-details > summary").first().click();
  await page.locator('[data-target="visual-1"]').click();
  await capture("desktop-similar");
  await page.locator('#visual-1 [data-remove-pin="302"]').check();
  await page.locator("#visual-1 .decision-more").evaluate(el => el.open = true);
  await capture("desktop-decision");
  await page.locator("#visual-1 .decision-more").evaluate(el => el.open = false);
  await page.locator("#visual-1 .additional-saves > summary").click();
  await capture("desktop-additional-saves");
  await page.locator("#visual-1 .additional-saves > summary").click();
  await page.locator("#overview-toggle").click();
  await capture("desktop-overview");
  await context.clearCookies();
  await page.evaluate(() => localStorage.clear());
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto(`${origin}/mixed.html`);
  await capture("mobile-exact");
  await page.locator("#toggle-filters").click();
  await capture("mobile-filters");
  await page.setViewportSize({ width: 768, height: 1024 });
  await page.goto(`${origin}/mixed.html`);
  await capture("tablet-exact");
  await page.setViewportSize({ width: 1440, height: 1000 });
  await page.goto(`${origin}/empty.html`);
  await capture("desktop-empty");
  await page.goto(`${origin}/media.html`);
  await capture("desktop-media");
  await page.setViewportSize({ width: 320, height: 780 });
  await page.goto(`${origin}/long-content.html`);
  await capture("narrow-long-content");
  await page.locator("#exact-1 .pin-card").last().locator("[data-remove-pin]").focus();
  await capture("narrow-last-copy");
  await page.setViewportSize({ width: 1440, height: 1000 });
  await page.evaluate(() => localStorage.clear());
  await page.goto(`${origin}/mixed.html`);
  await page.locator("#unreviewed-only").check();
  for (let i = 0; i < 3; i++) await page.locator(".is-active [data-review-button]").click();
  await capture("desktop-complete");
  const touchContext = await browser.newContext({ viewport: { width: 390, height: 844 }, isMobile: true, hasTouch: true, deviceScaleFactor: 1 });
  await touchContext.route("https://images.example.test/**", mockImage);
  const touchPage = await touchContext.newPage();
  await touchPage.goto(`${origin}/mixed.html`);
  await capture("mobile-touch-exact", touchPage);
  await touchContext.close();
  console.log(`Captured sixteen report states in ${output}`);
} finally {
  await browser.close();
  await new Promise(done => server.close(done));
}
