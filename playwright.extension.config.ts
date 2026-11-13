import { mkdirSync } from "node:fs";
import path from "node:path";

import { chromium, defineConfig } from "@playwright/test";

const downloadPath = path.resolve("test-results/extension-downloads");

// Playwright's CDP download interception suppresses the extension filename
// event that browser-download takeover relies on. Restoring browser-managed
// downloads keeps the takeover path observable without weakening assertions.
const launchPersistentContext = chromium.launchPersistentContext.bind(chromium);
chromium.launchPersistentContext = async (userDataDir, options) => {
  const context = await launchPersistentContext(userDataDir, options);
  mkdirSync(downloadPath, { recursive: true });
  const page = context.pages()[0] ?? (await context.newPage());
  const session = await context.newCDPSession(page);
  await session.send("Browser.setDownloadBehavior", {
    behavior: "default",
    downloadPath,
    eventsEnabled: true,
  });
  return context;
};

export default defineConfig({
  testDir: "extension/tests/e2e",
  fullyParallel: false,
  workers: 1,
  forbidOnly: Boolean(process.env.CI),
  retries: process.env.CI ? 2 : 0,
  reporter: process.env.CI ? "github" : "list",
  timeout: 120_000,
  outputDir: "test-results/extension",
  use: {
    trace: "on-first-retry",
  },
});
