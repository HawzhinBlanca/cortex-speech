import { expect, type Page } from '@playwright/test';

export async function openHeaderOverflow(page: Page) {
  const menu = page.getByTestId('header-overflow-menu');
  if (!(await menu.isVisible())) {
    await page.getByTestId('header-overflow-btn').click();
  }
  await expect(menu).toBeVisible();
  return menu;
}

export async function openSettingsFromHeader(page: Page) {
  await openHeaderOverflow(page);
  await page.getByTestId('settings-btn').click();
}

export async function switchLocaleFromHeader(page: Page) {
  await openHeaderOverflow(page);
  await page.getByTestId('locale-toggle').click();
}
