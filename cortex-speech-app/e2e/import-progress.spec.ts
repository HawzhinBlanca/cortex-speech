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

    await expect(page.getByTestId('pipeline-import-status')).toBeVisible();
    await expect(page.getByText('2/5')).toBeVisible();
  });
});
