import { test, expect } from './fixtures';

test.describe('Dataset quality UI', () => {
  test.beforeEach(async ({ page }) => {
    await page.goto('/');
  });

  test('stats dashboard shows WER metrics when annotated segments exist', async ({ page }) => {
    await page.goto('/');

    await expect(page.getByTestId('right-panel')).toBeVisible({ timeout: 15_000 });
    await expect(page.getByText('Dataset Statistics')).toBeVisible();
    await expect(page.getByTestId('right-panel').getByText(/Mean WER/)).toBeVisible();
    await expect(page.getByTestId('right-panel').getByText('0.0%').first()).toBeVisible();
  });

  test('validation panel opens and shows summary', async ({ page }) => {
    await page.getByTestId('validate-btn').click();
    const panel = page.getByTestId('validation-panel');
    await expect(panel).toBeVisible();
    await expect(panel.getByText(/1 segment checked/i)).toBeVisible();
  });
});
