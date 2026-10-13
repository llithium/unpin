// Manual provenance capture for the checked-in screenshot matching examples.
import { chromium } from "@playwright/test";
import { createHash } from "node:crypto";
import { readFile, writeFile, mkdir } from "node:fs/promises";
import { spawnSync } from "node:child_process";
import { resolve, dirname } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const fixtures = resolve(root, "tests/fixtures/matching");
const output = resolve(root, "test-results/quality-report.html");
await mkdir(dirname(output), { recursive: true });
const render = spawnSync("cargo", ["run", "--quiet", "--locked", "--example", "render_test_reports", "--", output], { cwd: root, encoding: "utf8" });
if (render.status !== 0) throw new Error(render.stderr);
const browser = await chromium.launch();
try {
  const page = await browser.newPage({ viewport: { width: 1280, height: 900 } });
  let photograph = "astronaut";
  await page.route("**/*", async (route) => {
    if (route.request().resourceType() === "image") {
      await route.fulfill({ contentType: "image/png", body: await readFile(resolve(fixtures, `${photograph}.png`)) });
    } else await route.continue();
  });
  for (const name of ["report", "report-changed"]) {
    if (name === "report-changed") photograph = "camera";
    await page.goto(pathToFileURL(output).href);
    await page.locator("#exact-1 img").evaluateAll((images) => Promise.all(images.map((image) => image.decode())));
    await page.locator("#exact-1").screenshot({ path: resolve(fixtures, `${name}.png`), animations: "disabled" });
  }
} finally { await browser.close(); }
const manifestPath = resolve(fixtures, "corpus.json");
const manifest = JSON.parse(await readFile(manifestPath, "utf8"));
const hashes = [];
for (const name of ["report", "report-changed"]) {
  const asset = manifest.assets.find((asset) => asset.id === name);
  asset.sha256 = createHash("sha256").update(await readFile(resolve(fixtures, asset.file))).digest("hex");
  asset.license = "Repository-authored screenshot; embedded photograph public domain";
  asset.source_url = "tests/browser/capture-quality-fixtures.mjs; embedded NASA or Lav Varshney photograph from scikit-image v0.25.2";
  hashes.push(asset.sha256);
}
if (hashes[0] === hashes[1]) throw new Error("Screenshot negatives must contain different displayed photographs.");
await writeFile(manifestPath, JSON.stringify(manifest, null, 2) + "\n");
