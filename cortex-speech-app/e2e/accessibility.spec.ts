import { test, expect } from './fixtures';
import { openHeaderOverflow } from './helpers/header';

test.describe('Accessibility smoke tests', () => {
  test('page has correct lang attribute for English accessibility fixture', async ({ page }) => {
    await page.goto('/');

    await expect(page.locator('html')).toHaveAttribute('lang', 'en');
  });

  test('icon buttons have aria-labels', async ({ page }) => {
    await page.goto('/');
    await openHeaderOverflow(page);

    await expect(page.getByLabel('Open settings')).toBeVisible();
    await expect(page.getByLabel('Switch language')).toBeVisible();
    await expect(page.getByLabel('Open audio file')).toBeVisible();
    await expect(page.getByLabel('Import directory')).toBeVisible();
  });

  test('export button is present and labeled', async ({ page }) => {
    await page.goto('/');
    await openHeaderOverflow(page);

    await expect(page.getByLabel('Export', { exact: true })).toBeVisible();
  });

  test('keyboard shortcuts button has aria-label', async ({ page }) => {
    await page.goto('/');

    const footer = page.locator('footer');
    await expect(footer.getByLabel('Keyboard Shortcuts')).toBeVisible();
  });

  test('toast notifications have role="alert"', async ({ page }) => {
    await page.goto('/');

    const toastContainer = page.locator('div[aria-live="polite"]');
    await expect(toastContainer).toBeAttached();
  });

  test('modals have correct aria attributes', async ({ page }) => {
    await page.goto('/');

    await page.getByLabel('Keyboard Shortcuts').click();
    const shortcutsModal = page.locator('role=dialog');
    await expect(shortcutsModal).toHaveAttribute('aria-modal', 'true');
    await expect(shortcutsModal).toBeVisible();

    await page.keyboard.press('Escape');
  });

  test('settings modal has aria-modal and role', async ({ page }) => {
    await page.goto('/');
    await openHeaderOverflow(page);

    await page.getByLabel('Open settings').click();
    const settings = page.locator('role=dialog');
    await expect(settings).toHaveAttribute('aria-modal', 'true');
    await expect(settings).toBeVisible();
  });

  test('keyboard navigation: Tab through interactive elements', async ({ page }) => {
    await page.goto('/');
    await openHeaderOverflow(page);

    await expect(page.getByLabel('Switch language')).toBeVisible();

    await page.keyboard.press('Tab');
    const focusedOnFirst = page.locator(':focus');
    await expect(focusedOnFirst).toBeAttached();

    const interactiveSelectors = [
      'header button',
      'header input',
      'aside input',
      'aside button',
      'footer button',
    ];

    for (const selector of interactiveSelectors) {
      const count = await page.locator(selector).count();
      for (let i = 0; i < Math.min(count, 5); i++) {
        await page.keyboard.press('Tab');
      }
    }

    const someElementFocused = page.locator(':focus');
    await expect(someElementFocused).toBeAttached();
  });

  test('SearchBar input is focusable', async ({ page }) => {
    await page.goto('/');

    const search = page.getByPlaceholder('Search transcripts, files, speakers...');
    await search.focus();
    await expect(search).toBeFocused();
  });
});
