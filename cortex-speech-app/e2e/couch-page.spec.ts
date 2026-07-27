import { test, expect } from '@playwright/test';
import { AxeBuilder } from '@axe-core/playwright';
import { pathToFileURL } from 'node:url';
import { resolve } from 'node:path';

// The phone review page (src-tauri/assets/couch.html) is served straight out of the Rust binary via
// include_str!, so it never passes through Vite and NOTHING in the existing e2e suite touched it —
// axe.spec.ts covers the desktop App root and Settings only. It is also the surface handed to people
// who are not the owner, which makes it the one most in need of an accessibility floor, not the least.
//
// Loaded from the file system rather than a live server: this asserts the page's own markup,
// theming and i18n, which need no backend. The server-side contract (auth, leases, attribution,
// spot checks) is covered by the Rust suite against a real HTTP server in src-tauri/src/couch.rs.
const PAGE = pathToFileURL(resolve(process.cwd(), 'src-tauri/assets/couch.html')).href;

const WCAG_AA = ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'wcag22aa'];

/** Put the page into its REVIEWING state — the state a reviewer actually spends their session in.
 *
 * Evaluated as a STRING, not an arrow function. The page's state lives in top-level `let` bindings,
 * and `let` at global scope does NOT become a property of `window` — so `window.queue = …` would
 * create a second, unrelated variable that `show()` never reads (it did, and every assertion failed
 * against an empty card). A bare assignment resolves through the scope chain to the real binding. */
async function showAClip(page: import('@playwright/test').Page) {
  await page.evaluate(`
    queue = [
      { id: 's1', text: 'ئەمە دەقێکی نموونەیی کوردییە بۆ پێداچوونەوە', durationMs: 4200, speakerId: 'spk_01' },
      { id: 's2', text: 'دەقی دووەم', durationMs: 3100, speakerId: null },
    ];
    i = 0;
    document.getElementById('err').hidden = true;
    show();
  `);
  await expect(page.locator('#card')).toBeVisible();
}

test.describe('Couch Review phone page', () => {
  test.beforeEach(async ({ page }) => {
    // A fresh origin per test: the page persists locale/size/loop in localStorage, and a leaked
    // choice from a previous test would silently change what the next one asserts.
    await page.goto(PAGE);
    await page.evaluate(() => localStorage.clear());
    await page.goto(PAGE);
  });

  test('opens in Sorani RTL — a Kurdish-first app must not greet its reviewer in English', async ({ page }) => {
    await expect(page.locator('html')).toHaveAttribute('lang', 'ckb');
    await expect(page.locator('html')).toHaveAttribute('dir', 'rtl');
    await showAClip(page);
    await expect(page.locator('#accept')).toContainText('دروستە');
    await expect(page.locator('#save')).toHaveText('پاشەکەوت و دواتر');
    await expect(page.locator('#bad')).toContainText('ڕەتکردنەوە');
  });

  test('the language toggle flips the UI but never the transcript direction', async ({ page }) => {
    await showAClip(page);
    await page.locator('#lang').click();
    await expect(page.locator('html')).toHaveAttribute('dir', 'ltr');
    await expect(page.locator('#save')).toHaveText('Save & next');
    // The corpus is Sorani whatever language the chrome is in. An LTR transcript box would put the
    // caret on the wrong side of every correction the reviewer types.
    await expect(page.locator('#text')).toHaveCSS('direction', 'rtl');
  });

  test('a typed correction survives a reload', async ({ page }) => {
    await showAClip(page);
    await page.locator('#text').fill('ڕاستکراوەی من');
    await page.reload();
    await showAClip(page);
    await expect(page.locator('#text')).toHaveValue('ڕاستکراوەی من');
  });

  test('the empty state is actually empty — `hidden` is not defeated by a display rule', async ({ page }) => {
    // Regression: #card sets display:flex, which beats the hidden attribute's display:none default,
    // so the empty review card rendered next to the "all reviewed" message.
    await page.evaluate(`queue = []; i = 0; document.getElementById('err').hidden = true; show();`);
    await expect(page.locator('#card')).toBeHidden();
    await expect(page.locator('#done')).toBeVisible();
  });

  test('the transcript text size is adjustable and persists', async ({ page }) => {
    await showAClip(page);
    const size = () => page.locator('#text').evaluate((el) => getComputedStyle(el).fontSize);
    expect(await size()).toBe('18px');
    await page.locator('#textsize').click();
    const bigger = await size();
    expect(parseInt(bigger, 10)).toBeGreaterThan(18);
    await page.reload();
    await showAClip(page);
    expect(await size()).toBe(bigger);
  });

  for (const scheme of ['light', 'dark'] as const) {
    test(`has zero WCAG 2.2 AA violations while reviewing (${scheme})`, async ({ page }) => {
      // Both themes, because the page renders in whichever the phone is set to and a contrast
      // failure in one is invisible from the other.
      await page.emulateMedia({ colorScheme: scheme });
      await showAClip(page);
      const results = await new AxeBuilder({ page }).withTags(WCAG_AA).analyze();
      for (const v of results.violations) {
        for (const n of v.nodes) {
          console.log(`[axe:${scheme}] ${v.id}: ${n.target.join(' ')} — ${n.failureSummary?.split('\n')[1] ?? ''}`);
        }
      }
      expect(results.violations.map((v) => `${v.id} x${v.nodes.length}`)).toEqual([]);
    });
  }
});
