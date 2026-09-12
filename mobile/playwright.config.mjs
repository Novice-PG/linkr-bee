import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./browser-test",
  fullyParallel: true,
  workers: 2,
  /* The Agent specs drive a real Pi agent loop and the geometry specs wait on
   * xterm's asynchronous parser; the 5 s default is tight for a loaded CI
   * runner, where a missed assertion looks like a product failure. */
  expect: { timeout: 10_000 },
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
  webServer: [
    {
      command: "npm run dev -- --host 127.0.0.1 --port 8765 --strictPort",
      url: "http://127.0.0.1:8765",
      reuseExistingServer: !process.env.CI,
      stdout: "pipe",
    },
    {
      /* The plain hosting path documented for desktop users: no bundler, so the
       * assistant comes from the vendored runtime in web/vendor/agent/. */
      command: "node browser-test/static-server.mjs 8766 ../web",
      url: "http://127.0.0.1:8766",
      reuseExistingServer: !process.env.CI,
      stdout: "pipe",
    },
  ],
});
