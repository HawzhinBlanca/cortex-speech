import { test as base, expect } from '@playwright/test';
import { installTauriMock } from './helpers/tauri-mock';

export const test = base.extend({
  page: async ({ page }, use) => {
    await page.addInitScript(() => {
      localStorage.setItem('cortex-locale', 'en');
    });
    await installTauriMock(page);
    await use(page);
  },
});

export { expect };
