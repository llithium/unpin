import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./tests/browser",
  outputDir: "./test-results/browser",
  timeout: 30_000,
  fullyParallel: false,
  reporter: "list",
  use: {
    browserName: "chromium",
    trace: "retain-on-failure",
  },
});
