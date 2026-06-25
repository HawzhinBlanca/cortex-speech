import { test, expect } from './fixtures';

test.describe('App smoke tests', () => {
  test('loads and renders the three-panel layout', async ({ page }) => {
    await page.goto('/');

    await expect(page.getByTestId('top-bar')).toBeVisible();
    await expect(page.getByTestId('top-bar').locator('h1')).toContainText('CORTEX');
    await expect(page.getByTestId('top-bar').locator('h1')).toContainText('Kurdish Speech Processor');

    await expect(page.getByTestId('left-panel')).toBeVisible();
    await expect(page.getByTestId('center-panel')).toBeVisible();
  });

  test('header is visible with app title', async ({ page }) => {
    await page.goto('/');

    const topBar = page.getByTestId('top-bar');
    await expect(topBar.locator('h1')).toBeVisible();
    await expect(topBar.getByText(/CORTEX/)).toBeVisible();
    await expect(topBar.getByText(/Kurdish Speech Processor/)).toBeVisible();
    await expect(topBar.getByText('v2.0')).toBeVisible();
  });

  test('keyboard shortcuts modal opens with ? key', async ({ page }) => {
    await page.goto('/');

    await page.keyboard.press('?');
    const modal = page.getByTestId('shortcuts-modal');
    await expect(modal).toBeVisible();
    await expect(modal.getByRole('heading', { name: 'Keyboard Shortcuts' })).toBeVisible();

    await modal.getByRole('button', { name: 'Close' }).click();
    await expect(modal).not.toBeVisible();
  });

  test('settings panel opens and closes', async ({ page }) => {
    await page.goto('/');

    await page.getByTestId('settings-btn').click();
    const settings = page.getByTestId('settings-panel');
    await expect(settings).toBeVisible();
    await expect(settings.getByText('Settings')).toBeVisible();

    await settings.getByTestId('settings-close-btn').click();
    await expect(settings).not.toBeVisible();
  });

  test('cloud STT opt-in surfaces ElevenLabs key status', async ({ page }) => {
    await page.goto('/');

    await page.getByTestId('settings-btn').click();
    const settings = page.getByTestId('settings-panel');
    await expect(settings).toBeVisible();

    // The cloud-transcription opt-in lives on the Audio tab.
    await settings.getByRole('button', { name: 'Audio', exact: true }).click();

    // No status until the opt-in is on (it only matters when cloud STT is enabled).
    await expect(settings.getByTestId('elevenlabs-key-status')).toHaveCount(0);

    // Enable the ElevenLabs Scribe opt-in; the mock reports its key as present.
    await settings
      .locator('label', { hasText: 'Cloud transcription (ElevenLabs Scribe)' })
      .getByRole('checkbox')
      .check();

    const status = settings.getByTestId('elevenlabs-key-status');
    await expect(status).toBeVisible();
    await expect(status).toContainText('ElevenLabs key detected');
  });

  test('model registry lists registered models with a champion badge', async ({ page }) => {
    await page.goto('/');

    await page.getByTestId('settings-btn').click();
    const settings = page.getByTestId('settings-panel');
    await expect(settings).toBeVisible();

    await settings.getByRole('button', { name: 'AI Models', exact: true }).click();

    const registry = settings.getByTestId('model-registry');
    await expect(registry).toBeVisible();

    const rows = settings.getByTestId('model-registry-row');
    await expect(rows).toHaveCount(2);
    await expect(rows.first()).toContainText('finetuned-mms-ckb');
    await expect(rows.first()).toContainText('champion');
    await expect(rows.first()).toContainText('CC-BY-NC-4.0');
  });

  test('diagnostics panel shows tracing stats and recent spans', async ({ page }) => {
    await page.goto('/');

    await page.getByTestId('settings-btn').click();
    const settings = page.getByTestId('settings-panel');
    await expect(settings).toBeVisible();

    await settings.getByRole('button', { name: 'Diagnostics', exact: true }).click();

    const panel = settings.getByTestId('diagnostics-panel');
    await expect(panel).toBeVisible();
    await expect(panel.getByTestId('diagnostics-stats')).toBeVisible();

    const spans = panel.getByTestId('diagnostics-spans').locator('li');
    await expect(spans).toHaveCount(2);
    await expect(spans.first()).toContainText('diff.compute');
  });

  test('Scribe actions are consent-gated and invoke the cloud STT commands', async ({ page }) => {
    await page.goto('/');
    await expect(page.getByTestId('segments-empty-state')).not.toBeVisible({ timeout: 15_000 });

    // Select the (mock) segment so the per-segment action buttons render. The filename title is on
    // the span inside the list-item button; clicking it bubbles to the button's select handler.
    await page.locator('[title="sample.wav"]').first().click();

    // Scribe buttons are hidden until cloud-STT opt-in is enabled.
    await expect(page.getByTestId('transcribe-scribe-btn')).toHaveCount(0);

    // Enable cloud STT via Settings → Audio.
    await page.getByTestId('settings-btn').click();
    const settings = page.getByTestId('settings-panel');
    await expect(settings).toBeVisible();
    await settings.getByRole('button', { name: 'Audio', exact: true }).click();
    await settings
      .locator('label', { hasText: 'Cloud transcription (ElevenLabs Scribe)' })
      .getByRole('checkbox')
      .check();
    await settings.getByTestId('settings-close-btn').click();
    await expect(settings).not.toBeVisible();

    // Now the consent-gated Scribe actions appear; clicking each runs its command's happy path.
    const scribe = page.getByTestId('transcribe-scribe-btn');
    await expect(scribe).toBeVisible();
    await scribe.click();
    await expect(page.getByRole('alert').filter({ hasText: 'Transcription complete' })).toBeVisible();

    await page.getByTestId('add-scribe-vote-btn').click();
    await expect(page.getByRole('alert').filter({ hasText: 'Scribe vote added' })).toBeVisible();
  });

  test('settings panel opens via Ctrl+, shortcut', async ({ page }) => {
    await page.goto('/');
    await page.getByTestId('app-root').click();

    await page.keyboard.press('Control+,');
    const settings = page.getByTestId('settings-panel');
    await expect(settings).toBeVisible();
    await expect(settings.getByText('Settings')).toBeVisible();

    await settings.getByTestId('settings-close-btn').click();
    await expect(settings).not.toBeVisible();
  });

  test('status bar is visible with shortcut hint', async ({ page }) => {
    await page.goto('/');

    const footer = page.getByTestId('status-bar');
    await expect(footer).toBeVisible();
    await expect(footer.getByText(/Press \? for shortcuts/)).toBeVisible();
    await expect(footer.getByRole('button', { name: 'Keyboard Shortcuts' })).toBeVisible();
  });

  test('segments load from mock on init', async ({ page }) => {
    await page.goto('/');

    await expect(page.getByTestId('segments-empty-state')).not.toBeVisible({ timeout: 15_000 });
    await expect(page.getByTestId('right-panel')).toBeVisible();
  });

  test('search bar accepts input', async ({ page }) => {
    await page.goto('/');

    await expect(page.getByTestId('search-bar')).toBeVisible();
    const searchInput = page.getByTestId('search-input');
    await expect(searchInput).toBeVisible();
    await searchInput.fill('test query');
    await expect(searchInput).toHaveValue('test query');
  });
});
