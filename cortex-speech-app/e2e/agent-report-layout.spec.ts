import { expect, test } from './fixtures';

test('bounded long model identifiers cannot widen the workstation or evidence panel', async ({
  page,
}) => {
  await page.addInitScript(() => {
    localStorage.setItem('cortex-locale', 'en');
    localStorage.setItem('__cortex_e2e_agent_report__', '1');
  });
  await page.setViewportSize({ width: 1000, height: 600 });
  await page.goto('/');
  await expect(page.getByTestId('top-bar')).toBeVisible();
  // The responsive 1000px layout starts the secondary panel collapsed; open it through the real
  // workstation shortcut so this exercises the supported narrow-desktop path.
  await page.keyboard.press('Shift+d');

  const panel = page.getByTestId('agent-report-panel');
  await expect(panel).toBeVisible({ timeout: 15_000 });
  const geometry = await panel.evaluate((element) => {
    const panelRect = element.getBoundingClientRect();
    const descendants = Array.from(element.querySelectorAll<HTMLElement>('*')).map((child) =>
      child.getBoundingClientRect(),
    );
    return {
      documentOverflow: document.documentElement.scrollWidth - document.documentElement.clientWidth,
      bodyOverflow: document.body.scrollWidth - document.body.clientWidth,
      panelOverflow: element.scrollWidth - element.clientWidth,
      leftEscape:
        Math.min(panelRect.left, ...descendants.map((rect) => rect.left)) - panelRect.left,
      rightEscape:
        Math.max(panelRect.right, ...descendants.map((rect) => rect.right)) - panelRect.right,
    };
  });

  expect(geometry.documentOverflow).toBeLessThanOrEqual(1);
  expect(geometry.bodyOverflow).toBeLessThanOrEqual(1);
  expect(geometry.panelOverflow).toBeLessThanOrEqual(1);
  expect(geometry.leftEscape).toBeGreaterThanOrEqual(-1);
  expect(geometry.rightEscape).toBeLessThanOrEqual(1);
});
