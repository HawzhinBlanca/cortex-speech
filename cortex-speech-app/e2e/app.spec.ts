import { test, expect } from './fixtures';

test.describe('App smoke tests', () => {
  test('loads and renders the three-panel layout', async ({ page }) => {
    await page.goto('/');

    await expect(page.getByTestId('top-bar')).toBeVisible();
    await expect(page.getByTestId('top-bar').locator('h1')).toContainText('CORTEX');
    await expect(page.getByTestId('top-bar').locator('h1')).toContainText('Kurdish Speech Processor');

    await expect(page.getByTestId('left-panel')).toBeVisible();
    await expect(page.getByTestId('center-panel')).toBeVisible();
  });

  test('header is visible with app title', async ({ page }) => {
    await page.goto('/');

    const topBar = page.getByTestId('top-bar');
    await expect(topBar.locator('h1')).toBeVisible();
    await expect(topBar.getByText(/CORTEX/)).toBeVisible();
    await expect(topBar.getByText(/Kurdish Speech Processor/)).toBeVisible();
    await expect(topBar.getByText('v2.0')).toBeVisible();
  });

  test('keyboard shortcuts modal opens with ? key', async ({ page }) => {
    await page.goto('/');

    await page.keyboard.press('?');
    const modal = page.getByTestId('shortcuts-modal');
    await expect(modal).toBeVisible();
    await expect(modal.getByRole('heading', { name: 'Keyboard Shortcuts' })).toBeVisible();

    await modal.getByRole('button', { name: 'Close' }).click();
    await expect(modal).not.toBeVisible();
  });

  test('settings panel opens and closes', async ({ page }) => {
    await page.goto('/');

    await page.getByTestId('settings-btn').click();
    const settings = page.getByTestId('settings-panel');
    await expect(settings).toBeVisible();
    await expect(settings.getByText('Settings')).toBeVisible();

    await settings.getByTestId('settings-close-btn').click();
    await expect(settings).not.toBeVisible();
  });

  test('settings panel opens via Ctrl+, shortcut', async ({ page }) => {
    await page.goto('/');
    await page.getByTestId('app-root').click();

    await page.keyboard.press('Control+,');
    const settings = page.getByTestId('settings-panel');
    await expect(settings).toBeVisible();
    await expect(settings.getByText('Settings')).toBeVisible();

    await settings.getByTestId('settings-close-btn').click();
    await expect(settings).not.toBeVisible();
  });

  test('status bar is visible with shortcut hint', async ({ page }) => {
    await page.goto('/');

    const footer = page.getByTestId('status-bar');
    await expect(footer).toBeVisible();
    await expect(footer.getByText(/Press \? for shortcuts/)).toBeVisible();
    await expect(footer.getByRole('button', { name: 'Keyboard Shortcuts' })).toBeVisible();
  });

  test('segments load from mock on init', async ({ page }) => {
    await page.goto('/');

    await expect(page.getByTestId('segments-empty-state')).not.toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId('right-panel')).toBeVisible();
  });

  test('search bar accepts input', async ({ page }) => {
    await page.goto('/');

    await expect(page.getByTestId('search-bar')).toBeVisible();
    const searchInput = page.getByTestId('search-input');
    await expect(searchInput).toBeVisible();
    await searchInput.fill('test query');
    await expect(searchInput).toHaveValue('test query');
  });
});
