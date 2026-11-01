import { defineConfig, devices } from "@playwright/test";

const port = Number(process.env.E2E_APP_PORT ?? 18080);
const iPhone = devices["iPhone 15 Pro"];

export default defineConfig({
  testDir: "tests",
  timeout: 60_000,
  expect: { timeout: 20_000 },
  fullyParallel: true,
  retries: process.env.CI ? 1 : 0,
  reporter: [["list"], ["html", { open: "never", outputFolder: "report" }]],
  outputDir: "test-results",
  use: {
    baseURL: `http://127.0.0.1:${port}`,
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
    // E2E_VIDEO=1 records every test; E2E_SLOWMO=<ms> makes recordings watchable.
    video: process.env.E2E_VIDEO ? "on" : "retain-on-failure",
    launchOptions: { slowMo: Number(process.env.E2E_SLOWMO ?? 0) },
  },
  webServer: {
    command: "./scripts/stack.sh",
    url: `http://127.0.0.1:${port}/api/meta`,
    timeout: 600_000,
    reuseExistingServer: false,
    stdout: "pipe",
  },
  projects: [
    { name: "system", testDir: "tests/system" },
    {
      name: "desktop",
      testDir: "tests/ui",
      testIgnore: /mobile/,
      use: {
        ...devices["Desktop Chrome"],
        colorScheme: "dark",
        ...(process.env.E2E_VIDEO ? { video: { mode: "on" as const, size: { width: 1280, height: 720 } } } : {}),
      },
    },
    // iPhone viewport/UA/touch emulated in Chromium (WebKit optional in CI).
    {
      name: "iphone",
      testDir: "tests/ui",
      testMatch: /mobile/,
      use: {
        ...iPhone,
        browserName: "chromium",
        colorScheme: "light",
        ...(process.env.E2E_VIDEO ? { video: { mode: "on" as const, size: { width: iPhone.viewport.width * 2, height: iPhone.viewport.height * 2 } } } : {}),
      },
    },
    {
      name: "iphone-landscape",
      testDir: "tests/ui",
      testMatch: /mobile/,
      use: { ...devices["iPhone 15 Pro landscape"], browserName: "chromium" },
    },
  ],
});
