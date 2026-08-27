import { test, expect } from './fixtures';
import { openHeaderOverflow } from './helpers/header';

/**
 * The permanent application shell is deliberately small: identity, global state, command palette and
 * one explicit overflow. Secondary operations must remain reachable inside that overflow, but they may
 * never make the header wrap and consume the review workstation.
 */
const WIDTHS = [320, 500, 1000, 1024, 1280, 1440, 1920, 2560];
const PERMANENT_CONTROLS = ['command-palette-btn', 'header-overflow-btn'];
const SECONDARY_CONTROLS = [
  'overflow-curate-btn',
  'overflow-insights-btn',
  'review-correct-btn',
  'validate-btn',
  'review-inbox-btn',
  'settings-btn',
  'locale-toggle',
];

test.describe('one-row header and reachable overflow', () => {
  for (const width of WIDTHS) {
    test(`header remains one row and all operations remain reachable at ${width}px`, async ({
      page,
    }) => {
      await page.setViewportSize({ width, height: 700 });
      await page.goto('/');

      const header = page.getByTestId('top-bar');
      await expect(header).toBeVisible();
      const headerBox = await header.boundingBox();
      expect(headerBox, 'header has measurable layout geometry').not.toBeNull();
      expect(headerBox!.height, 'the permanent header must never wrap').toBeLessThanOrEqual(64);
      expect(headerBox!.x).toBeGreaterThanOrEqual(-0.5);
      expect(headerBox!.x + headerBox!.width).toBeLessThanOrEqual(width + 0.5);

      for (const id of PERMANENT_CONTROLS) {
        const control = page.getByTestId(id);
        await expect(control).toBeVisible();
        const box = await control.boundingBox();
        expect(box, `${id} has measurable layout geometry`).not.toBeNull();
        expect(box!.x, `${id} is not clipped at the inline start`).toBeGreaterThanOrEqual(-0.5);
        expect(box!.x + box!.width, `${id} is not clipped at the inline end`).toBeLessThanOrEqual(
          width + 0.5,
        );
      }

      const menu = await openHeaderOverflow(page);
      const menuBox = await menu.boundingBox();
      expect(menuBox, 'overflow menu has measurable layout geometry').not.toBeNull();
      expect(menuBox!.x).toBeGreaterThanOrEqual(-0.5);
      expect(menuBox!.x + menuBox!.width).toBeLessThanOrEqual(width + 0.5);

      for (const id of SECONDARY_CONTROLS) {
        const control = page.getByTestId(id);
        await control.scrollIntoViewIfNeeded();
        await expect(control, `${id} remains reachable inside the explicit overflow`).toBeVisible();
        const box = await control.boundingBox();
        expect(box, `${id} has measurable layout geometry`).not.toBeNull();
        expect(box!.x).toBeGreaterThanOrEqual(-0.5);
        expect(box!.x + box!.width).toBeLessThanOrEqual(width + 0.5);
      }
    });
  }
});
