import { defineConfig, devices } from '@playwright/test';

export default defineConfig({
  testDir: './e2e',
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 2 : 0,
  // Locally `undefined` lets Playwright scale workers to ~half the CPU count - on a 64-core
  // workstation that is ~32 Chromium instances for 47 tests. The oversubscription does not make
  // the suite faster (it is ~10s either way), but it makes every `expect(...).toBeVisible()`
  // race the dev server, so the suite only fails when something else is using the machine.
  // That is exactly how it failed on 2026-07-25: green standalone and on a re-run, red once
  // inside a full verify-10 sweep with cargo tests saturating the box.
  workers: process.env.CI ? 1 : 4,
  timeout: 30_000,
  expect: { timeout: 10_000 },
  reporter: 'html',
  use: {
    baseURL: 'http://localhost:1420',
    trace: 'on-first-retry',
    screenshot: 'only-on-failure',
  },
  projects: [
    {
      name: 'chromium',
      use: { ...devices['Desktop Chrome'] },
    },
  ],
  webServer: {
    command: 'npm run dev',
    url: 'http://localhost:1420',
    reuseExistingServer: !process.env.CI,
    timeout: 120000,
  },
});
