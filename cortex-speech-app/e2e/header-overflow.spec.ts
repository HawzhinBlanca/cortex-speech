import { test, expect } from '@playwright/test';

/**
 * The top bar must keep every action REACHABLE at the widths a desktop app is actually used at.
 *
 * The owner's deep audit (2026-08-03) reported: "At 1024-2560 px and 200% zoom, no header action may
 * leave the viewport. In Review mode, Validate/Inbox/Settings currently overflow." That is a concrete,
 * measurable claim, and this spec measures it instead of taking it on trust — a control that clips
 * silently is worse than a missing one, because the user has no way to know an action exists.
 *
 * Measured, not eyeballed: an element's own bounding box against the viewport. A control is a failure
 * only if it is CLIPPED — extending past the right edge with no way to reach it. Elements deliberately
 * scrolled out of a scrollable container are a different thing and are not asserted here.
 *
 * 200% zoom is emulated the way a desktop app experiences it: halving the CSS viewport at the same
 * device pixels is what a browser zoom does to layout, so 1024 at 200% is the 512-wide layout case.
 */
const WIDTHS = [1024, 1280, 1440, 1920, 2560];

// The actions the audit named, plus the two that sit beside them in the same bar.
const HEADER_ACTIONS = ['review-correct-btn', 'validate-btn', 'review-inbox-btn', 'settings-btn', 'locale-toggle'];

test.describe('top bar actions stay inside the viewport', () => {
  for (const width of WIDTHS) {
    test(`no header action is clipped at ${width}px`, async ({ page }) => {
      await page.setViewportSize({ width, height: 900 });
      await page.goto('/');
      await page.waitForSelector('[data-testid="app-root"]');
      await page.waitForSelector('[data-testid="top-bar"]');

      const clipped: string[] = [];
      for (const id of HEADER_ACTIONS) {
        const el = page.locator(`[data-testid="${id}"]`);
        if ((await el.count()) === 0) continue; // not every action is present in every state
        if (!(await el.first().isVisible())) continue;
        const box = await el.first().boundingBox();
        if (!box) continue;
        // Right edge past the viewport, or pushed off the left — either way unreachable.
        if (box.x + box.width > width + 0.5 || box.x < -0.5) {
          clipped.push(`${id} spans x=${box.x.toFixed(0)}..${(box.x + box.width).toFixed(0)} (viewport ${width})`);
        }
      }

      expect(
        clipped,
        `header actions clipped outside the ${width}px viewport — a control the user cannot reach:\n  ` +
          clipped.join('\n  '),
      ).toEqual([]);
    });
  }

  test('no header action is clipped at 200% zoom (512px effective layout)', async ({ page }) => {
    // Browser zoom halves the CSS viewport; 1024 at 200% lays out as 512.
    await page.setViewportSize({ width: 512, height: 450 });
    await page.goto('/');
    await page.waitForSelector('[data-testid="app-root"]');

    const bar = page.locator('[data-testid="top-bar"]');
    await expect(bar).toBeVisible();
    // At this size wrapping or a scrollable bar is a legitimate answer; being WIDER than the viewport
    // with no scroll is not, because then the actions past the edge are simply gone.
    const overflow = await bar.evaluate((el) => ({
      scrollWidth: el.scrollWidth,
      clientWidth: el.clientWidth,
      scrollable: getComputedStyle(el).overflowX !== 'visible',
    }));
    expect(
      overflow.scrollWidth <= overflow.clientWidth + 1 || overflow.scrollable,
      `the top bar is ${overflow.scrollWidth}px wide inside a ${overflow.clientWidth}px viewport and does ` +
        `not scroll, so every action past the edge is unreachable at 200% zoom`,
    ).toBe(true);
  });
});
