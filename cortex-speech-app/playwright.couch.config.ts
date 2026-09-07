import { defineConfig } from '@playwright/test';
import base from './playwright.config';

// The reviewer page is embedded HTML, not the Vite desktop app. Exercise its real browser
// behavior without starting an unrelated dev server or touching the working reviewers' sessions.
export default defineConfig({
  ...base,
  testMatch: 'couch-page.spec.ts',
  webServer: undefined,
  workers: 2,
  reporter: 'line',
});
