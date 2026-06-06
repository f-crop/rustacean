import { defineConfig, devices } from "@playwright/test";

// PR CI: Chromium only.
// Nightly: Chromium + Firefox + WebKit (set PLAYWRIGHT_NIGHTLY=true).
const isNightly = !!process.env.PLAYWRIGHT_NIGHTLY;

export default defineConfig({
  testDir: "./e2e",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 2 : 0,
  workers: process.env.CI ? 1 : undefined,
  reporter: [
    ["html", { outputFolder: "playwright-report", open: "never" }],
    ["list"],
  ],
  use: {
    baseURL: "http://localhost:4173",
    trace: "on-first-retry",
    screenshot: "on",
  },
  webServer: {
    command: "npm run preview",
    url: "http://localhost:4173",
    reuseExistingServer: !process.env.CI,
    timeout: 30_000,
  },
  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"] },
      // Quarantine specs and feature-flag-gated specs run under dedicated projects.
      // chat-panel*.spec.ts files require VITE_FEATURE_CHAT_PANEL=true at build time;
      // they are exercised by the chat-smoke CI job, not the main suite.
      testIgnore: ["**/quarantine/**", "**/chat-panel.spec.ts", "**/chat-panel-rusaa-1907.spec.ts", "**/chat-panel-turn-complete-flush.spec.ts", "**/chat-panel-rusaa-1932.spec.ts"],
    },
    ...(isNightly
      ? [
          {
            name: "firefox",
            use: { ...devices["Desktop Firefox"] },
            testIgnore: ["**/quarantine/**", "**/chat-panel.spec.ts", "**/chat-panel-rusaa-1907.spec.ts", "**/chat-panel-turn-complete-flush.spec.ts", "**/chat-panel-rusaa-1932.spec.ts"],
          },
          {
            name: "webkit",
            use: { ...devices["Desktop Safari"] },
            testIgnore: ["**/quarantine/**", "**/chat-panel.spec.ts", "**/chat-panel-rusaa-1907.spec.ts", "**/chat-panel-turn-complete-flush.spec.ts", "**/chat-panel-rusaa-1932.spec.ts"],
          },
        ]
      : []),
    // Dedicated project for the chat-smoke CI job.
    // Only registered when PLAYWRIGHT_CHAT_SMOKE=1 so the main CI run
    // (which does not set that env var) never picks up chat-panel*.spec.ts files.
    ...(process.env.PLAYWRIGHT_CHAT_SMOKE
      ? [
          {
            name: "chat-smoke",
            testMatch: ["**/chat-panel.spec.ts", "**/chat-panel-rusaa-1907.spec.ts", "**/chat-panel-turn-complete-flush.spec.ts", "**/chat-panel-rusaa-1932.spec.ts"],
            use: { ...devices["Desktop Chrome"] },
            retries: process.env.CI ? 2 : 0,
          },
        ]
      : []),
    // Quarantine bucket: flaky chat tests, 5 retries in CI.
    // Run explicitly: npx playwright test --project=chat-quarantine
    {
      name: "chat-quarantine",
      testDir: "./e2e/quarantine/chat",
      use: { ...devices["Desktop Chrome"] },
      retries: process.env.CI ? 5 : 1,
    },
  ],
  outputDir: "test-results",
});
