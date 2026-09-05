// Render the fixture first with examples/render_test_reports. Optional second
// argument selects another checkout's template for an identical-DOM comparison.
import { readFile } from "node:fs/promises";
import { chromium } from "@playwright/test";

const fixture = await readFile(process.argv[2] || "test-results/visual-report.html", "utf8");
const template = await readFile(process.argv[3] || "templates/report.html", "utf8");
const start = template.lastIndexOf("<script>") + "<script>".length;
const script = template.slice(start, template.indexOf("</script>", start));
const expand = `
const group = document.querySelector('[data-group]');
const link = document.querySelector('.match-link[data-target]');
const groupParent = group.parentElement;
const linkParent = link.parentElement;
const groupTemplate = group.cloneNode(true);
const linkTemplate = link.cloneNode(true);
document.querySelectorAll('[data-group], .match-link[data-target]').forEach(node => node.remove());
for (let i = 0; i < 2000; i++) {
  const g = groupTemplate.cloneNode(true);
  g.id = 'bench-' + i;
  g.dataset.reviewKey = 'bench:' + i;
  g.classList.remove('is-active');
  groupParent.append(g);
  const l = linkTemplate.cloneNode(true);
  l.dataset.target = g.id;
  l.setAttribute('href', '#' + g.id);
  l.setAttribute('aria-current', 'false');
  linkParent.append(l);
}
`;
const scriptStart = fixture.lastIndexOf("<script>");
const html = fixture.slice(0, scriptStart) + `<script>${expand}\n${script}</script></body></html>`;
const browser = await chromium.launch();
const times = [];
const mutations = [];
try {
  for (let run = 0; run < 5; run++) {
    const page = await browser.newPage();
    await page.route("**/*", route => route.request().resourceType() === "document"
      ? route.fulfill({contentType: "text/html", body: html}) : route.abort());
    await page.goto("http://fixture.test/report");
    const result = await page.evaluate(() => {
      const next = document.querySelector('#next-match');
      for (let i = 0; i < 5; i++) next.click();
      const observer = new MutationObserver(() => {});
      observer.observe(document.body, {subtree: true, attributes: true, attributeFilter: ['aria-current']});
      const start = performance.now();
      for (let i = 0; i < 100; i++) next.click();
      const elapsed = performance.now() - start;
      const writes = observer.takeRecords().length;
      observer.disconnect();
      if (document.querySelector('[data-group].is-active')?.id !== 'bench-105') throw new Error('navigation result changed');
      if (document.querySelectorAll('.match-link[aria-current="true"]').length !== 1) throw new Error('active links changed');
      return {elapsed, writes};
    });
    times.push(result.elapsed);
    mutations.push(result.writes);
    await page.close();
  }
} finally { await browser.close(); }
console.log(JSON.stringify({matches: 2000, navigations: 100, elapsed_ms: times, aria_current_writes: mutations}));
