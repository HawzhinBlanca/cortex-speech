import { test, expect } from './fixtures';

test.describe('Import pipeline progress UI', () => {
  test.beforeEach(async ({ page }) => {
    await page.goto('/');
    // The persisted English locale is an intentional lazy chunk. Wait for the mounted workstation
    // before exercising global shortcuts; the browser load event can precede that async boundary.
    await expect(page.getByTestId('status-bar')).toBeVisible();
  });

  test('idle state hides import progress indicator', async ({ page }) => {
    await expect(page.getByTestId('pipeline-import-status')).not.toBeVisible();
    await expect(page.getByText(/Importing\.\.\./)).not.toBeVisible();
  });

  test('shows only caller-bound progress even when events beat the command response', async ({
    page,
  }) => {
    // The compact production header intentionally keeps Import in overflow at this viewport. The
    // global shortcut exercises the same visible user path without clicking a CSS-hidden menu item.
    await page.keyboard.press('Control+i');

    // Scope the count assertion to the import-status element itself. A bare getByText('2/5') is
    // ambiguous — the separate pipeline progress bar renders "2/5 chunks" (pipeline.progressCount)
    // alongside this "2/5 files" status, and Playwright strict mode rejects the two-element match.
    // This test is about the FILE count, which lives in pipeline-import-status.
    const importStatus = page.getByTestId('pipeline-import-status');
    await expect(importStatus).toBeVisible();
    await expect(importStatus).toContainText('2/5');
    await expect(importStatus).not.toContainText('99/99');
  });

  test('keeps an admitted run authoritative when its command response is lost', async ({
    page,
  }) => {
    await page.evaluate(() => localStorage.setItem('__cortex_e2e_import_response_lost__', '1'));
    await page.keyboard.press('Control+i');

    await expect(page.getByTestId('processing-progress')).toBeVisible();
    await expect(page.getByTestId('pipeline-import-status')).toContainText('2/5');
    await expect(page.getByTestId('processing-progress')).not.toBeVisible({ timeout: 5_000 });
    await expect(page.getByText('Import failed', { exact: true })).not.toBeVisible();
  });

  test('watchdog clears a normally accepted run when both terminal events are lost', async ({
    page,
  }) => {
    await page.evaluate(() =>
      localStorage.setItem('__cortex_e2e_import_status_only_settlement__', '1'),
    );
    await page.keyboard.press('Control+i');

    await expect(page.getByTestId('processing-progress')).toBeVisible();
    await expect(page.getByTestId('processing-progress')).not.toBeVisible({ timeout: 8_000 });
    await expect(page.getByText('Import failed', { exact: true })).not.toBeVisible();
  });

  test('watchdog clears a settled run even when the invoke promise never returns', async ({
    page,
  }) => {
    await page.evaluate(() => localStorage.setItem('__cortex_e2e_import_never_returns__', '1'));
    await page.keyboard.press('Control+i');

    await expect(page.getByTestId('processing-progress')).toBeVisible();
    await expect(page.getByTestId('processing-progress')).not.toBeVisible({ timeout: 8_000 });
    await expect(page.getByText('Import failed', { exact: true })).not.toBeVisible();

    await page.evaluate(() => localStorage.removeItem('__cortex_e2e_import_never_returns__'));
    await page.keyboard.press('Control+i');
    await expect(page.getByTestId('pipeline-import-status')).toContainText('2/5');
  });

  test('worker settlement starts a fresh read when the completion refresh hangs', async ({
    page,
  }) => {
    await page.evaluate(() => localStorage.setItem('__cortex_e2e_empty_library__', '1'));
    await page.reload();
    await expect(page.getByTestId('segments-empty-state')).toBeVisible();
    await page.evaluate(() => {
      localStorage.setItem('__cortex_e2e_import_completion_refresh_hangs__', '1');
      localStorage.setItem('__cortex_e2e_import_refresh_reads__', '0');
    });
    await page.keyboard.press('Control+i');

    await expect(page.getByTestId('processing-progress')).not.toBeVisible({ timeout: 2_000 });
    await expect(page.getByTestId('recovery-notice-region')).not.toBeVisible();
    await expect
      .poll(() =>
        page.evaluate(() => Number(localStorage.getItem('__cortex_e2e_import_refresh_reads__'))),
      )
      .toBeGreaterThanOrEqual(2);
    await expect(page.getByTestId('segment-card')).toBeVisible();

    await page.evaluate(() => {
      const mocked = window as unknown as { __releaseHungImportRefresh?: () => void };
      mocked.__releaseHungImportRefresh?.();
      localStorage.removeItem('__cortex_e2e_import_completion_refresh_hangs__');
    });
    await page.waitForTimeout(250);
    await expect(page.getByTestId('segment-card')).toBeVisible();
  });

  test('native folder cancellation is a definite rejected run and clears quietly', async ({
    page,
  }) => {
    await page.evaluate(() => localStorage.setItem('__cortex_e2e_import_cancel__', '1'));
    await page.keyboard.press('Control+i');

    await expect(page.getByTestId('processing-progress')).not.toBeVisible({ timeout: 2_000 });
    await expect(page.getByText('Import failed', { exact: true })).not.toBeVisible();
    await expect(
      page.getByText('The import response was interrupted', { exact: true }),
    ).not.toBeVisible();
  });

  test('folder cancellation racing the watchdog still clears quietly', async ({ page }) => {
    await page.evaluate(() => localStorage.setItem('__cortex_e2e_import_delayed_cancel__', '1'));
    await page.keyboard.press('Control+i');

    await expect(page.getByTestId('processing-progress')).toBeVisible();
    await expect(page.getByTestId('processing-progress')).not.toBeVisible({ timeout: 8_000 });
    await expect(page.getByText('Import failed', { exact: true })).not.toBeVisible();
    await expect(
      page.getByText('The import response was interrupted', { exact: true }),
    ).not.toBeVisible();
  });

  test('file-picker timeout releases the pre-run guard and a second open reaches native code', async ({
    page,
  }) => {
    await page.evaluate(() => localStorage.setItem('__cortex_e2e_file_picker_timeout__', '1'));
    await page.keyboard.press('Control+o');

    await expect(page.getByText('Failed to open file', { exact: true })).toBeVisible();
    await page.keyboard.press('Control+o');
    await expect
      .poll(() =>
        page.evaluate(() => Number(localStorage.getItem('__cortex_e2e_open_audio_file_calls__'))),
      )
      .toBe(2);
  });

  test('a forever-running file picker is cancellable and a fresh picker can open', async ({
    page,
  }) => {
    await page.evaluate(() => localStorage.setItem('__cortex_e2e_file_picker_wedged__', '1'));
    await page.keyboard.press('Control+o');

    await expect(page.getByTestId('processing-progress')).toBeVisible();
    await page.getByRole('button', { name: 'Cancel' }).first().click();
    await expect(page.getByTestId('processing-progress')).not.toBeVisible({ timeout: 3_000 });
    await expect(page.getByText('Failed to open file', { exact: true })).not.toBeVisible();

    await page.keyboard.press('Control+o');
    await expect
      .poll(() =>
        page.evaluate(() => Number(localStorage.getItem('__cortex_e2e_open_audio_file_calls__'))),
      )
      .toBe(2);
  });

  test('file import stops advertising Cancel after the native picker has returned', async ({
    page,
  }) => {
    await page.evaluate(() => localStorage.setItem('__cortex_e2e_readiness_delayed__', '1'));
    await page.keyboard.press('Control+o');
    await expect
      .poll(() => page.evaluate(() => localStorage.getItem('__cortex_e2e_readiness_waiting__')))
      .toBe('1');

    await expect(page.getByTestId('processing-progress')).not.toBeVisible();
    await expect(page.getByRole('button', { name: 'Cancel' })).not.toBeVisible();

    await page.evaluate(() => {
      const mocked = window as unknown as { __releaseDelayedReadiness?: () => void };
      mocked.__releaseDelayedReadiness?.();
    });
    await expect
      .poll(() =>
        page.evaluate(() => Number(localStorage.getItem('__cortex_e2e_import_audio_file_calls__'))),
      )
      .toBe(1);
  });

  test('directory-picker timeout rejects the exact run and permits a fresh import', async ({
    page,
  }) => {
    await page.evaluate(() => localStorage.setItem('__cortex_e2e_directory_picker_timeout__', '1'));
    await page.keyboard.press('Control+i');

    await expect(page.getByTestId('processing-progress')).not.toBeVisible({ timeout: 3_000 });
    await expect(
      page.getByTestId('status-bar').getByText('Import failed', { exact: true }),
    ).toBeVisible();
    await page.keyboard.press('Control+i');
    await expect
      .poll(() =>
        page.evaluate(() => Number(localStorage.getItem('__cortex_e2e_import_directory_calls__'))),
      )
      .toBe(2);
  });

  test('a forever-running directory picker is cancellable before it emits any event', async ({
    page,
  }) => {
    await page.evaluate(() => localStorage.setItem('__cortex_e2e_directory_picker_wedged__', '1'));
    await page.keyboard.press('Control+i');

    await expect(page.getByTestId('processing-progress')).toBeVisible();
    await page.getByRole('button', { name: 'Cancel' }).first().click();
    await expect(page.getByTestId('processing-progress')).not.toBeVisible({ timeout: 3_000 });
    await expect(page.getByText('Import failed', { exact: true })).not.toBeVisible();
  });

  test('hung advisory readiness probe cannot permanently block import controls', async ({
    page,
  }) => {
    await page.evaluate(() => localStorage.setItem('__cortex_e2e_readiness_never_returns__', '1'));
    await page.keyboard.press('Control+i');

    await expect(page.getByTestId('pipeline-import-status')).toContainText('2/5', {
      timeout: 20_000,
    });
  });

  test('hung resume response cannot leave recovery permanently busy', async ({ page }) => {
    await page.evaluate(() => {
      localStorage.setItem('__cortex_e2e_interrupted_import__', '1');
      localStorage.setItem('__cortex_e2e_resume_never_returns__', '1');
    });
    await page.reload();
    const resume = page.getByTestId('resume-import-btn');
    await expect(resume).toBeVisible();
    await resume.click();

    await expect(page.getByTestId('processing-progress')).toBeVisible();
    await expect(page.getByTestId('processing-progress')).not.toBeVisible({ timeout: 8_000 });
    await expect(page.getByTestId('resume-import-banner')).not.toBeVisible({ timeout: 8_000 });
  });
});
