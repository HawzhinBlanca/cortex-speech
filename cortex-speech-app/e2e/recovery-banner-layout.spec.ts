import { test, expect } from './fixtures';

test('320px interrupted-import recovery keeps both durable actions visible without overflow', async ({
  page,
}) => {
  await page.addInitScript(() => {
    localStorage.setItem('cortex-locale', 'ckb');
    localStorage.setItem('__cortex_e2e_interrupted_import__', '1');
  });
  await page.setViewportSize({ width: 320, height: 640 });
  await page.goto('/');

  const banner = page.getByTestId('resume-import-banner');
  const resume = page.getByTestId('resume-import-btn');
  const discard = page.getByTestId('dismiss-import-btn');
  await expect(banner).toBeVisible({ timeout: 15_000 });
  await expect(resume).toBeVisible();
  await expect(discard).toBeVisible();

  const geometry = await page.evaluate(() => {
    const bannerElement = document.querySelector<HTMLElement>(
      '[data-testid="resume-import-banner"]',
    );
    const resumeElement = document.querySelector<HTMLElement>('[data-testid="resume-import-btn"]');
    const discardElement = document.querySelector<HTMLElement>(
      '[data-testid="dismiss-import-btn"]',
    );
    const rect = (element: HTMLElement | null) => element?.getBoundingClientRect() ?? null;
    return {
      documentOverflow: document.documentElement.scrollWidth - document.documentElement.clientWidth,
      bodyOverflow: document.body.scrollWidth - document.body.clientWidth,
      bannerOverflow: bannerElement
        ? bannerElement.scrollWidth - bannerElement.clientWidth
        : Number.POSITIVE_INFINITY,
      banner: rect(bannerElement),
      resume: rect(resumeElement),
      discard: rect(discardElement),
    };
  });

  expect(geometry.documentOverflow).toBeLessThanOrEqual(1);
  expect(geometry.bodyOverflow).toBeLessThanOrEqual(1);
  expect(geometry.bannerOverflow).toBeLessThanOrEqual(1);
  expect(geometry.banner).not.toBeNull();
  expect(geometry.resume).not.toBeNull();
  expect(geometry.discard).not.toBeNull();
  for (const action of [geometry.resume!, geometry.discard!]) {
    expect(action.left).toBeGreaterThanOrEqual(geometry.banner!.left - 0.5);
    expect(action.right).toBeLessThanOrEqual(geometry.banner!.right + 0.5);
    expect(action.bottom).toBeLessThanOrEqual(640.5);
  }

  await discard.click();
  const dialog = page.getByRole('dialog');
  await expect(dialog).toBeVisible();
  const dialogGeometry = await dialog.evaluate((element) => {
    const dialogRect = element.getBoundingClientRect();
    const actions = Array.from(element.querySelectorAll<HTMLElement>('footer button')).map(
      (button) => button.getBoundingClientRect(),
    );
    return {
      dialog: dialogRect,
      overflow: element.scrollWidth - element.clientWidth,
      actions,
    };
  });

  expect(dialogGeometry.overflow).toBeLessThanOrEqual(1);
  expect(dialogGeometry.actions).toHaveLength(2);
  for (const action of dialogGeometry.actions) {
    expect(action.left).toBeGreaterThanOrEqual(dialogGeometry.dialog.left - 0.5);
    expect(action.right).toBeLessThanOrEqual(dialogGeometry.dialog.right + 0.5);
    expect(action.left).toBeGreaterThanOrEqual(-0.5);
    expect(action.right).toBeLessThanOrEqual(320.5);
  }
});

test('320x180 dual recovery notices and confirmation remain scrollable and actionable', async ({
  page,
}) => {
  await page.addInitScript(() => {
    localStorage.setItem('cortex-locale', 'en');
    localStorage.setItem('__cortex_e2e_interrupted_import__', '1');
    localStorage.setItem('__cortex_e2e_quarantine_notice__', '1');
  });
  await page.setViewportSize({ width: 320, height: 180 });
  await page.goto('/');

  const notices = page.getByTestId('recovery-notice-region');
  await expect(notices).toBeVisible({ timeout: 15_000 });
  const noticeGeometry = await notices.evaluate((element) => ({
    clientHeight: element.clientHeight,
    scrollHeight: element.scrollHeight,
    horizontalOverflow: element.scrollWidth - element.clientWidth,
    documentOverflow: document.documentElement.scrollWidth - document.documentElement.clientWidth,
  }));
  expect(noticeGeometry.clientHeight).toBeLessThanOrEqual(76);
  expect(noticeGeometry.scrollHeight).toBeGreaterThan(noticeGeometry.clientHeight);
  expect(noticeGeometry.horizontalOverflow).toBeLessThanOrEqual(1);
  expect(noticeGeometry.documentOverflow).toBeLessThanOrEqual(1);

  const discard = page.getByTestId('dismiss-import-btn');
  await discard.scrollIntoViewIfNeeded();
  await discard.click();

  const dialog = page.getByRole('dialog');
  await expect(dialog).toBeVisible();
  const destructiveAction = dialog.getByRole('button', { name: 'Delete recovery record' });
  await destructiveAction.scrollIntoViewIfNeeded();
  await expect(destructiveAction).toBeVisible();
  const shortGeometry = await page.evaluate(() => {
    const backdropElement = document.querySelector<HTMLElement>('.modal-backdrop');
    const action = Array.from(document.querySelectorAll<HTMLElement>('footer button')).at(-1);
    const actionRect = action?.getBoundingClientRect();
    return {
      backdropClientHeight: backdropElement?.clientHeight ?? 0,
      backdropScrollHeight: backdropElement?.scrollHeight ?? 0,
      backdropHorizontalOverflow: backdropElement
        ? backdropElement.scrollWidth - backdropElement.clientWidth
        : Number.POSITIVE_INFINITY,
      actionTop: actionRect?.top ?? Number.NEGATIVE_INFINITY,
      actionBottom: actionRect?.bottom ?? Number.POSITIVE_INFINITY,
      documentOverflow: document.documentElement.scrollWidth - document.documentElement.clientWidth,
    };
  });
  expect(shortGeometry.backdropScrollHeight).toBeGreaterThan(shortGeometry.backdropClientHeight);
  expect(shortGeometry.backdropHorizontalOverflow).toBeLessThanOrEqual(1);
  expect(shortGeometry.documentOverflow).toBeLessThanOrEqual(1);
  expect(shortGeometry.actionTop).toBeGreaterThanOrEqual(-0.5);
  expect(shortGeometry.actionBottom).toBeLessThanOrEqual(180.5);
});
