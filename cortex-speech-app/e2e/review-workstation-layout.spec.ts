import { test, expect } from './fixtures';

async function enterReview(page: import('@playwright/test').Page) {
  await page.goto('/');
  await expect(page.getByTestId('review-nudge-start')).toBeVisible({ timeout: 15_000 });
  await page.getByTestId('review-nudge-start').click();
  await expect(page.getByTestId('review-action-bar')).toBeVisible();
}

test.describe('review workstation geometry', () => {
  test('1000x600 keeps transcript, playback and decisions simultaneously visible', async ({
    page,
  }) => {
    await page.setViewportSize({ width: 1000, height: 600 });
    await enterReview(page);

    const header = await page.getByTestId('top-bar').boundingBox();
    const audio = await page.getByTestId('audio-player-controls').boundingBox();
    const transcript = await page.locator('.review-transcript-input').boundingBox();
    const actions = await page.getByTestId('review-action-bar').boundingBox();

    expect(header).not.toBeNull();
    expect(audio).not.toBeNull();
    expect(transcript).not.toBeNull();
    expect(actions).not.toBeNull();
    expect(header!.height).toBeLessThanOrEqual(64);
    expect(audio!.y).toBeGreaterThanOrEqual(header!.y + header!.height);
    expect(transcript!.y).toBeGreaterThanOrEqual(header!.y + header!.height);
    expect(audio!.y + audio!.height, 'playback ends before the decision rail').toBeLessThanOrEqual(
      actions!.y + 0.5,
    );
    expect(
      transcript!.y + transcript!.height,
      'the editable transcript ends before the decision rail',
    ).toBeLessThanOrEqual(actions!.y + 0.5);
    expect(actions!.y + actions!.height).toBeLessThanOrEqual(600.5);

    const state = await page.evaluate(() => ({
      documentOverflow: document.documentElement.scrollWidth - document.documentElement.clientWidth,
      nonFiniteClock: /Infinity|NaN/.test(document.body.innerText),
    }));
    expect(state.documentOverflow).toBeLessThanOrEqual(1);
    expect(state.nonFiniteClock).toBe(false);
  });

  test('320x640 fully reflows without document or review-scroller overflow', async ({ page }) => {
    await page.setViewportSize({ width: 320, height: 640 });
    await enterReview(page);

    const state = await page.evaluate(() => {
      const stack = document.querySelector('.review-stack');
      const scroller = stack?.parentElement;
      const header = document.querySelector<HTMLElement>('[data-testid="top-bar"]');
      const actions = document.querySelector<HTMLElement>('[data-testid="review-action-bar"]');
      const activityRail = document.querySelector<HTMLElement>('[data-testid="activity-rail"]');
      return {
        documentOverflow:
          document.documentElement.scrollWidth - document.documentElement.clientWidth,
        scrollerOverflow: (scroller?.scrollWidth ?? 0) - (scroller?.clientWidth ?? 0),
        headerHeight: header?.getBoundingClientRect().height ?? Infinity,
        actionBottom: actions?.getBoundingClientRect().bottom ?? Infinity,
        activityRailWidth: activityRail?.getBoundingClientRect().width ?? Infinity,
      };
    });

    expect(state.documentOverflow).toBeLessThanOrEqual(1);
    expect(state.scrollerOverflow).toBeLessThanOrEqual(1);
    expect(state.headerHeight).toBeLessThanOrEqual(64);
    expect(state.actionBottom).toBeLessThanOrEqual(640.5);
    expect(state.activityRailWidth, 'duplicate rail retires at the full-reflow tier').toBe(0);
  });
});
