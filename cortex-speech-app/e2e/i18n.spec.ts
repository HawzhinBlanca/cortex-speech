import { test, expect } from './fixtures';
import { installTauriMock } from './helpers/tauri-mock';
import { openHeaderOverflow, openSettingsFromHeader } from './helpers/header';

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
    await openHeaderOverflow(page);

    // The toggle shows the language you'll switch TO, in its own endonym ('English' / 'کوردی').
    const toggle = page.getByTestId('locale-toggle');
    await expect(toggle).toHaveText('English');
  });

  test('toggle switches locale to English', async ({ page }) => {
    await page.goto('/');
    await openHeaderOverflow(page);

    const toggle = page.getByTestId('locale-toggle');
    expect(await toggle.textContent()).toBe('English');

    await toggle.click();

    // Switching locale re-renders the header, which closes the overflow menu that holds the
    // toggle -- exactly what the sibling test below already does before its own assertion. The
    // assertion is unchanged: once in English, the toggle must offer Kurdish (endonym).
    await openHeaderOverflow(page);
    await expect(toggle).toHaveText('کوردی');
  });

  test('UI text updates when locale changes', async ({ page }) => {
    await page.goto('/');
    await openHeaderOverflow(page);

    await expect(page.getByLabel('کردنەوەی فایلی دەنگ')).toBeVisible();

    await page.getByTestId('locale-toggle').click();
    await openHeaderOverflow(page);

    await expect(page.getByLabel('Open audio file')).toBeVisible();
  });

  test('locale toggle is accessible by keyboard', async ({ page }) => {
    await page.goto('/');
    await openHeaderOverflow(page);

    const toggle = page.getByTestId('locale-toggle');
    await toggle.focus();
    await expect(toggle).toBeFocused();
  });

  test('settings panel language selects are present', async ({ page }) => {
    await page.goto('/');

    await openSettingsFromHeader(page);
    const settings = page.locator('role=dialog');

    const langSelect = settings
      .locator('select')
      .filter({ has: page.locator('option[value="ckb"]') });
    await expect(langSelect).toBeVisible();
  });
});
