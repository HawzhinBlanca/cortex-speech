import { test, expect } from '@playwright/test';
import { installTauriMock } from './helpers/tauri-mock';

test.describe('i18n and locale switching', () => {
  test.beforeEach(async ({ page }) => {
    await page.addInitScript(() => localStorage.removeItem('cortex-locale'));
    await installTauriMock(page);
  });
  test('default locale is ckb in HTML lang attribute', async ({ page }) => {
    await page.goto('/');

    await expect(page.locator('html')).toHaveAttribute('lang', 'ckb');
  });

  test('default state shows the English toggle target (locale is ckb)', async ({ page }) => {
    await page.goto('/');

    // The toggle shows the language you'll switch TO, in its own endonym ('English' / 'کوردی').
    const toggle = page.getByTestId('locale-toggle');
    await expect(toggle).toHaveText('English');
  });

  test('toggle switches locale to English', async ({ page }) => {
    await page.goto('/');

    const toggle = page.getByTestId('locale-toggle');
    expect(await toggle.textContent()).toBe('English');

    await toggle.click();

    // Now in English, the toggle offers Kurdish (endonym).
    await expect(toggle).toHaveText('کوردی');
  });

  test('UI text updates when locale changes', async ({ page }) => {
    await page.goto('/');

    await expect(page.getByLabel('کردنەوەی فایلی دەنگ')).toBeVisible();

    await page.getByTestId('locale-toggle').click();

    await expect(page.getByLabel('Open audio file')).toBeVisible();
  });

  test('locale toggle is accessible by keyboard', async ({ page }) => {
    await page.goto('/');

    const toggle = page.getByTestId('locale-toggle');
    await toggle.focus();
    await expect(toggle).toBeFocused();
  });

  test('settings panel language selects are present', async ({ page }) => {
    await page.goto('/');

    await page.getByTestId('settings-btn').click();
    const settings = page.locator('role=dialog');

    const langSelect = settings.locator('select').filter({ has: page.locator('option[value="ckb"]') });
    await expect(langSelect).toBeVisible();
  });
});
