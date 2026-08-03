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
    // Reuse is a LOCAL DEVELOPER convenience: keep `npm run dev` running and tests attach to it.
    // It is wrong for a GATE. Playwright reuses whatever answers on 1420 without checking what it
    // is, so `test-e2e+a11y` was green or red about a server it never verified. DEMONSTRATED
    // 2026-08-03: a trivial impostor server put on 1420 was reused, and the accessibility spec ran
    // against "not the app". A foreign server makes the leg red (confusing but honest); a STALE but
    // valid dev server — another branch, a watcher that died — makes it GREEN about code that is not
    // under test, which is the vacuous pass this project's charter forbids.
    //
    // CORTEX_GATE is set by scripts/verify_10.py for everything it runs. With reuse off, Playwright
    // refuses on a busy port and names the reason, which is exactly what the sibling e2e harnesses
    // already do (see e2e_profile.cjs refuseIfDebugPortBusy). Interactive `npm run test:e2e` is
    // untouched.
    reuseExistingServer: !process.env.CI && !process.env.CORTEX_GATE,
    timeout: 120000,
  },
});
