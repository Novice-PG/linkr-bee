import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./browser-test",
  fullyParallel: true,
  workers: 2,
  use: {
    browserName: "chromium",
    baseURL: "http://127.0.0.1:8765",
    viewport: { width: 390, height: 844 },
    isMobile: true,
    hasTouch: true,
    launchOptions: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE
      ? { executablePath: process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE }
      : {},
  },
  webServer: {
    command: "npm run dev -- --host 127.0.0.1 --port 8765 --strictPort",
    url: "http://127.0.0.1:8765",
    reuseExistingServer: !process.env.CI,
    stdout: "pipe",
  },
});
