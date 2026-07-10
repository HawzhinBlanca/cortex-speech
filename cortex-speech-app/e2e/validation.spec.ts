import { test, expect } from './fixtures';

test.describe('Dataset validation panel', () => {
  test('opens validation modal from toolbar', async ({ page }) => {
    await page.goto('/');

    await expect(page.getByTestId('segments-empty-state')).not.toBeVisible({ timeout: 15_000 });

    const validateBtn = page.getByTestId('validate-btn');
    await expect(validateBtn).toBeEnabled();
    await validateBtn.click();

    const panel = page.getByTestId('validation-panel');
    await expect(panel).toBeVisible();
    await expect(panel.locator('#validation-title')).toBeVisible();
    await expect(panel).toHaveAttribute('aria-modal', 'true');

    await panel.getByRole('button', { name: /close|داخستن/i }).first().click();
    await expect(panel).not.toBeVisible();
  });

  test('validate button stays disabled with no segments', async ({ page }) => {
    await page.addInitScript(() => {
      const internals = window.__TAURI_INTERNALS__;
      if (!internals) return;
      const invoke = internals.invoke.bind(internals);
      internals.invoke = async (cmd: string) => {
        // The store loads via the paged command now; keep the legacy override too.
        if (cmd === 'get_segments_page') return { items: [], total: 0, nextCursor: null };
        if (cmd === 'get_segments') return [];
        return invoke(cmd);
      };
    });

    await page.goto('/');
    await expect(page.getByTestId('validate-btn')).toBeDisabled();
  });
});
