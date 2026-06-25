import { test, expect } from './fixtures';
import { AxeBuilder } from '@axe-core/playwright';
import type { Page } from '@playwright/test';

// WCAG 2.2 Level AA tag set (axe-core has no `wcag22a` tag; AAA is out of scope).
const WCAG_AA = ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22aa'];

async function violations(page: Page) {
  const results = await new AxeBuilder({ page }).withTags(WCAG_AA).analyze();
  // Map to id + node count so a failure prints WHICH rules broke and where.
  return results.violations.map((v) => `${v.id} x${v.nodes.length}`);
}

// M3.6 — ENFORCED WCAG 2.2 AA gate (axe-core) over the App root (en + ckb/RTL) and the settings
// dialog. The four violation classes originally surfaced (aria-required-children, color-contrast,
// scrollable-region-focusable, select-name) were fixed; this gate now asserts ZERO violations and
// fails the e2e suite on any regression.
test.describe('axe-core WCAG 2.2 AA gate (M3.6)', () => {
  test('App root has zero a11y violations (en)', async ({ page }) => {
    await page.goto('/');
    await expect(page.getByLabel('Open settings')).toBeVisible();
    // The conformal panel loads async; wait for it so axe analyzes the settled view
    // (and actually covers that panel) instead of racing its mid-render state.
    await expect(page.getByTestId('conformal-cert')).toBeVisible();
    expect(await violations(page)).toEqual([]);
  });

  test('App root has zero a11y violations (ckb / RTL)', async ({ page }) => {
    await page.goto('/');
    await page.getByLabel('Switch language').click();
    await expect(page.locator('html')).toHaveAttribute('lang', 'ckb');
    await expect(page.getByTestId('conformal-cert')).toBeVisible();
    expect(await violations(page)).toEqual([]);
  });

  test('Settings dialog has zero a11y violations', async ({ page }) => {
    await page.goto('/');
    await page.getByLabel('Open settings').click();
    const dialog = page.locator('role=dialog');
    await expect(dialog).toBeVisible();
    expect(await violations(page)).toEqual([]);

    // Also cover the AI Models tab, where the model-registry panel + import form render.
    await dialog.getByRole('button', { name: 'AI Models', exact: true }).click();
    await expect(page.getByTestId('model-registry')).toBeVisible();
    await dialog.getByTestId('model-import-toggle').click();
    await expect(page.getByTestId('model-import-form')).toBeVisible();
    expect(await violations(page)).toEqual([]);

    // And the Diagnostics tab (tracing panel).
    await dialog.getByRole('button', { name: 'Diagnostics', exact: true }).click();
    await expect(page.getByTestId('diagnostics-panel')).toBeVisible();
    expect(await violations(page)).toEqual([]);
  });
});
