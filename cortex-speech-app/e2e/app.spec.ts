import { test, expect } from './fixtures';
import { openSettingsFromHeader } from './helpers/header';

test.describe('App smoke tests', () => {
  test('loads and renders the three-panel layout', async ({ page }) => {
    await page.goto('/');

    await expect(page.getByTestId('top-bar')).toBeVisible();
    await expect(page.getByTestId('top-bar').locator('h1')).toContainText('CORTEX');
    await expect(page.getByTestId('top-bar').locator('h1')).toContainText(
      'Kurdish Speech Processor',
    );

    await expect(page.getByTestId('left-panel')).toBeVisible();
    await expect(page.getByTestId('center-panel')).toBeVisible();
  });

  test('header is visible with app title', async ({ page }) => {
    await page.goto('/');

    const topBar = page.getByTestId('top-bar');
    await expect(topBar.locator('h1')).toBeVisible();
    await expect(topBar.getByText(/CORTEX/)).toBeVisible();
    await expect(topBar.getByText(/Kurdish Speech Processor/)).toBeVisible();
    await expect(topBar.getByText('v2.1.0')).toBeVisible();
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

    await openSettingsFromHeader(page);
    const settings = page.getByTestId('settings-panel');
    await expect(settings).toBeVisible();
    await expect(settings.getByText('Settings')).toBeVisible();

    await settings.getByTestId('settings-close-btn').click();
    await expect(settings).not.toBeVisible();
  });

  test('champion settings do not expose ElevenLabs Scribe in the production flow', async ({
    page,
  }) => {
    await page.goto('/');

    await openSettingsFromHeader(page);
    const settings = page.getByTestId('settings-panel');
    await expect(settings).toBeVisible();

    // Scribe is not part of the champion flow. Its former opt-in and key-status surfaces must stay
    // absent so a reviewer cannot accidentally switch transcript providers from ordinary Settings.
    await settings.getByRole('button', { name: 'Audio', exact: true }).click();
    await expect(settings.getByTestId('elevenlabs-key-status')).toHaveCount(0);
    await expect(settings.getByText(/ElevenLabs|Scribe/i)).toHaveCount(0);

    // Production engine selection is read-only and champion-only. This catches the old false green
    // where the test looked for obsolete consent copy while the real provider selector still shipped.
    await settings.getByRole('button', { name: 'ASR', exact: true }).click();
    await expect(settings.getByTestId('production-asr-model')).toContainText(
      'Meta OmniASR 7B Champion + LoRA',
    );
    await expect(settings.locator('option[value="ctc-300m"]')).toHaveCount(0);
    await expect(settings.locator('option[value="ctc-1b"]')).toHaveCount(0);
  });

  test('cloud advisory settings expose only fixed Gemini 2.5 Pro', async ({ page }) => {
    await page.goto('/');
    await openSettingsFromHeader(page);
    const panel = page.getByTestId('settings-panel');

    await panel.getByRole('button', { name: 'AI Post-Processing', exact: true }).click();
    await panel.getByTestId('llm-engine-select').selectOption('Gemini');
    await expect(panel.getByTestId('cloud-llm-model-fixed')).toContainText('gemini-2.5-pro');

    await panel.getByRole('button', { name: /Listening Jury/, exact: true }).click();
    await panel.getByTestId('jury-cloud-opt-in').check();
    await expect(panel.getByTestId('jury-model-fixed')).toContainText('gemini-2.5-pro');
    await expect(panel.getByText(/Gemini 2\.5 Flash|gemini-2\.5-flash/i)).toHaveCount(0);
    await expect(panel.locator('input[value*="gemini"], input[placeholder*="gemini"]')).toHaveCount(
      0,
    );
  });

  test('model registry lists registered models with a champion badge', async ({ page }) => {
    await page.goto('/');

    await openSettingsFromHeader(page);
    const settings = page.getByTestId('settings-panel');
    await expect(settings).toBeVisible();

    await settings.getByRole('button', { name: 'AI Models', exact: true }).click();

    const registry = settings.getByTestId('model-registry');
    await expect(registry).toBeVisible();

    const rows = settings.getByTestId('model-registry-row');
    await expect(rows).toHaveCount(2);
    await expect(rows.first()).toContainText('omniasr-7b-champion');
    await expect(rows.first()).toContainText('champion');
    await expect(rows.first()).toContainText('Apache-2.0');

    // The shipped model-manager surface contains support models only. Optional ASR artifacts and
    // their retired integrity action must not be discoverable through the production UI.
    await expect(settings.getByText('Silero VAD v4')).toBeVisible();
    await expect(settings.getByText('CAM++ Speaker Embedding')).toBeVisible();
    await expect(settings.getByText('AI Audio Denoiser')).toBeVisible();
    await expect(settings.getByText(/300M|CTC 1B|MMS|Scribe|ElevenLabs/i)).toHaveCount(0);
    await expect(settings.getByTestId('verify-model-btn')).toHaveCount(0);
  });

  test('model registry can import a checkpoint as a candidate', async ({ page }) => {
    await page.goto('/');

    await openSettingsFromHeader(page);
    const settings = page.getByTestId('settings-panel');
    await expect(settings).toBeVisible();
    await settings.getByRole('button', { name: 'AI Models', exact: true }).click();

    await settings.getByTestId('model-import-toggle').click();
    const form = settings.getByTestId('model-import-form');
    await expect(form).toBeVisible();

    await form.getByLabel('Model ID').fill('test-candidate');
    await form.getByLabel('Model source').fill('fine-tune');
    await form.getByLabel('Model license').fill('CC-BY-NC-4.0');

    // Submit is disabled until a checkpoint file is chosen.
    await expect(settings.getByTestId('model-import-submit')).toBeDisabled();
    await settings.getByTestId('model-import-pick').click();
    await expect(settings.getByTestId('model-import-submit')).toBeEnabled();

    await settings.getByTestId('model-import-submit').click();
    await expect(page.getByRole('alert').filter({ hasText: 'Imported checkpoint' })).toBeVisible();
  });

  test('refinery eval actions run an eval and build a scorecard', async ({ page }) => {
    await page.goto('/');
    await expect(page.getByTestId('segments-empty-state')).not.toBeVisible({ timeout: 15_000 });

    // Switch to the Insights view, where the RefineryPanel (with eval actions) renders.
    await page.getByRole('button', { name: 'Insights' }).click();
    const actions = page.getByTestId('refinery-eval-actions');
    await expect(actions).toBeVisible();
    await expect(actions.getByTestId('eval-local')).toHaveCount(0);
    await expect(actions.getByLabel('Eval model id')).toHaveCount(0);

    // Run the champion-only honest-CER eval (run_gold_eval_asr) and show the result.
    await page.getByTestId('eval-honest-cer').click();
    const result = page.getByTestId('eval-result');
    await expect(result).toBeVisible();
    await expect(result).toContainText('CER');
    await expect(result).toContainText('29.0%');

    // Build a scorecard from that result (build_scorecard).
    await page.getByTestId('eval-build-scorecard').click();
    const scorecard = page.getByTestId('eval-scorecard');
    await expect(scorecard).toBeVisible();
    await expect(scorecard).toContainText('Scorecard');
  });

  test('diagnostics panel shows tracing stats and recent spans', async ({ page }) => {
    await page.goto('/');

    await openSettingsFromHeader(page);
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

  test('champion segment actions expose no smaller or cloud ASR controls', async ({ page }) => {
    await page.goto('/');
    await expect(page.getByTestId('segments-empty-state')).not.toBeVisible({ timeout: 15_000 });

    // Select the (mock) segment so the per-segment action buttons render. The filename title is on
    // the span inside the list-item button; clicking it bubbles to the button's select handler.
    await page.locator('[title="sample.wav"]').first().click();

    await expect(page.getByTestId('transcribe-btn')).toBeVisible();
    await expect(page.getByTestId('transcribe-constrained-btn')).toHaveCount(0);
    await expect(page.getByTestId('transcribe-finetuned-btn')).toHaveCount(0);
    await expect(page.getByTestId('transcribe-scribe-btn')).toHaveCount(0);
    await expect(page.getByTestId('add-scribe-vote-btn')).toHaveCount(0);
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

  test('review mode: single-key shortcuts focus/blur the editor (F5)', async ({ page }) => {
    await page.goto('/');
    await expect(page.getByTestId('segments-empty-state')).not.toBeVisible({ timeout: 15_000 });

    // Enter the one-clip Review & Correct workspace via the nudge CTA.
    await page.getByTestId('review-nudge-start').click();
    const editor = page.locator('textarea');
    await expect(editor).toBeVisible();

    // Move focus off the editor, then 'e' must focus it (single-key flow works from the page body).
    await page.getByTestId('app-root').click();
    await expect(editor).not.toBeFocused();
    await page.keyboard.press('e');
    await expect(editor).toBeFocused();

    // Escape leaves the editor so single-key review shortcuts resume.
    await page.keyboard.press('Escape');
    await expect(editor).not.toBeFocused();
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
