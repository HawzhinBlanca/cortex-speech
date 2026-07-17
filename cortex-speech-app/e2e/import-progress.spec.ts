import { test, expect } from './fixtures';
import { emitTauriEvent } from './helpers/tauri-mock';

test.describe('Import pipeline progress UI', () => {
  test.beforeEach(async ({ page }) => {
    await page.goto('/');
  });

  test('idle state hides import progress indicator', async ({ page }) => {
    await expect(page.getByTestId('pipeline-import-status')).not.toBeVisible();
    await expect(page.getByText(/Importing\.\.\./)).not.toBeVisible();
  });

  test('shows file counts when pipeline-progress events fire', async ({ page }) => {
    await emitTauriEvent(page, 'pipeline-progress', {
      current: 2,
      total: 5,
      file: 'podcast.wav',
      status: 'Processing...',
    });

    // Scope the count assertion to the import-status element itself. A bare getByText('2/5') is
    // ambiguous — the separate pipeline progress bar renders "2/5 chunks" (pipeline.progressCount)
    // alongside this "2/5 files" status, and Playwright strict mode rejects the two-element match.
    // This test is about the FILE count, which lives in pipeline-import-status.
    const importStatus = page.getByTestId('pipeline-import-status');
    await expect(importStatus).toBeVisible();
    await expect(importStatus).toContainText('2/5');
  });
});
