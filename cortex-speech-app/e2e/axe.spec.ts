import { test, expect } from './fixtures';
import { AxeBuilder } from '@axe-core/playwright';
import type { Page } from '@playwright/test';

// WCAG 2.2 Level AA tag set (axe-core has no `wcag22a` tag; AAA is out of scope).
const WCAG_AA = ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22aa'];

async function violations(page: Page) {
  const results = await new AxeBuilder({ page }).withTags(WCAG_AA).analyze();
  // On failure, print the offending nodes so the violation is diagnosable from the log alone.
  for (const v of results.violations) {
    for (const n of v.nodes) {
      console.log(
        `[axe] ${v.id}: ${n.target.join(' ')} — ${n.failureSummary?.split('\n')[1] ?? ''}`,
      );
    }
  }
  // Map to id + node count so a failure prints WHICH rules broke and where.
  return results.violations.map((v) => `${v.id} x${v.nodes.length}`);
}

// Settle-waits for the async conformal panel get a wider timeout than the 10s global default.
// These waits assert "the view has finished rendering so axe sees the real thing" — they are NOT
// performance assertions, and the zero-violation checks below are untouched by this. The global
// 10s proved load-sensitive: the gate went red inside a full verify-10 sweep (2026-07-25) while
// passing standalone. Measured on this rig after capping Playwright workers 32 -> 4: 12/12 idle,
// 23/24 under 24 concurrent CPU burners. This timeout targets that residual starvation case.
const SETTLE = { timeout: 30_000 };

/**
 * Open the Insights "Advanced & diagnostics" disclosure so axe actually scans what is inside it.
 *
 * P0 #7 moved the conformal certificate, inference internals, intelligence report and dataset tools
 * behind a collapsed native `<details>`. axe-core does NOT analyze hidden content, so leaving it
 * collapsed would drop every rule covering those panels from this gate while the suite still went
 * green — a silently narrower WCAG check. Repointing the settle-wait at an always-visible element
 * would have done exactly that; the original wait deliberately doubled as coverage of that panel
 * ("and actually covers that panel", above), so the disclosure gets opened instead.
 *
 * Clicking the summary (rather than setting `open` from script) also exercises the disclosure the
 * way a user does. Idempotent: a no-op if some future default ships it already open.
 */
async function openAdvanced(page: Page) {
  const advanced = page.getByTestId('stats-advanced');
  await expect(advanced).toBeVisible(SETTLE);
  if (!(await advanced.evaluate((el) => (el as HTMLDetailsElement).open))) {
    await advanced.locator('summary').click();
  }
  await expect(advanced).toHaveJSProperty('open', true, SETTLE);
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
    await openAdvanced(page);
    await expect(page.getByTestId('conformal-cert')).toBeVisible(SETTLE);
    expect(await violations(page)).toEqual([]);
  });

  test('App root has zero a11y violations (ckb / RTL)', async ({ page }) => {
    await page.goto('/');
    await page.getByLabel('Switch language').click();
    await expect(page.locator('html')).toHaveAttribute('lang', 'ckb');
    await openAdvanced(page);
    await expect(page.getByTestId('conformal-cert')).toBeVisible(SETTLE);
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
