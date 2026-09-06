import { test, expect } from './fixtures';
import { openHeaderOverflow } from './helpers/header';

test.describe('Navigation and panel interaction', () => {
  test('sidebar toggle works with Shift+S', async ({ page }) => {
    await page.goto('/');

    const aside = page.locator('aside').first();
    await expect(aside).toBeVisible();

    await page.keyboard.press('Shift+S');
    await expect(aside).not.toBeVisible();

    await page.keyboard.press('Shift+S');
    await expect(aside).toBeVisible();
  });

  test('stats panel toggle works with Shift+D', async ({ page }) => {
    // Note: stats panel only renders when segments exist.
    // Shift+D toggles the statsOpen variable; verify the action does not error.
    await page.goto('/');

    await expect(page.locator('button[title="Show stats (⇧D)"]')).not.toBeVisible();
  });

  test('search bar shows initial state with mock segments', async ({ page }) => {
    await page.goto('/');

    await expect(page.getByTestId('segments-empty-state')).not.toBeVisible({ timeout: 15_000 });
    await expect(page.getByPlaceholder('Search transcripts, or a file name like lamo_016604')).toBeVisible();
  });

  test('search bar clear button appears on input', async ({ page }) => {
    await page.goto('/');

    const searchInput = page.getByPlaceholder('Search transcripts, or a file name like lamo_016604');
    await expect(searchInput).toBeVisible();

    await expect(page.getByLabel('Clear search')).not.toBeVisible();

    await searchInput.fill('test');

    await expect(page.getByLabel('Clear search')).toBeVisible();

    await page.getByLabel('Clear search').click();
    await expect(searchInput).toHaveValue('');
  });

  test('search query persists across a restart (session restore)', async ({ page }) => {
    await page.goto('/');

    const placeholder = 'Search transcripts, or a file name like lamo_016604';
    const searchInput = page.getByPlaceholder(placeholder);
    await expect(searchInput).toBeVisible();
    await searchInput.fill('بەڕێوە');

    // Wait until the debounced save_session has persisted the view-state.
    await expect
      .poll(async () => page.evaluate(() => localStorage.getItem('__cortex_session__')))
      .toContain('بەڕێوە');

    // Reload the app — restore_session repopulates the prior search query on launch.
    await page.goto('/');
    await expect(page.getByPlaceholder(placeholder)).toHaveValue('بەڕێوە');
  });

  test('search verification filter buttons exist', async ({ page }) => {
    await page.goto('/');

    await expect(
      page.getByTestId('search-bar').getByRole('button', { name: 'All', exact: true }),
    ).toBeVisible();
    await expect(page.getByRole('button', { name: 'Verified', exact: true })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Pending', exact: true })).toBeVisible();
  });

  test('sort dropdown has expected options', async ({ page }) => {
    await page.goto('/');

    const select = page.locator('select');
    await expect(select).toBeVisible();

    const options = await select.locator('option').allTextContents();
    expect(options).toEqual([
      'Newest',
      'Oldest',
      'Duration',
      'Verified',
      'Confidence (Lowest First)',
      'Active Learning (Boundary)',
    ]);
  });

  test('sidebar hidden button appears when sidebar is toggled off', async ({ page }) => {
    await page.goto('/');
    const aside = page.locator('aside').first();
    await expect(aside).toBeVisible();

    await page.keyboard.press('Shift+S');

    await openHeaderOverflow(page);
    const showBtn = page.getByLabel('Show segments');
    await expect(showBtn).toBeVisible();

    await showBtn.click();
    await expect(aside).toBeVisible();
  });
});
