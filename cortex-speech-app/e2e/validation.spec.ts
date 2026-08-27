import { test, expect } from './fixtures';
import { openHeaderOverflow } from './helpers/header';

test.describe('Dataset validation panel', () => {
  test('opens validation modal from toolbar', async ({ page }) => {
    await page.goto('/');

    await expect(page.getByTestId('segments-empty-state')).not.toBeVisible({ timeout: 15_000 });

    await openHeaderOverflow(page);
    const validateBtn = page.getByTestId('validate-btn');
    await expect(validateBtn).toBeEnabled();
    await validateBtn.click();

    const panel = page.getByTestId('validation-panel');
    await expect(panel).toBeVisible();
    await expect(panel.locator('#validation-title')).toBeVisible();
    await expect(panel).toHaveAttribute('aria-modal', 'true');

    await panel
      .getByRole('button', { name: /close|داخستن/i })
      .first()
      .click();
    await expect(panel).not.toBeVisible();
  });

  test('validate button stays disabled with no segments', async ({ page }) => {
    await page.addInitScript(() => {
      // The shared mock reads this marker at invocation time. This avoids relying on the undefined
      // ordering of multiple addInitScript callbacks to wrap __TAURI_INTERNALS__ safely.
      localStorage.setItem('__cortex_e2e_empty_library__', '1');
    });

    await page.goto('/');
    await openHeaderOverflow(page);
    await expect(page.getByTestId('validate-btn')).toBeDisabled();
  });
});
