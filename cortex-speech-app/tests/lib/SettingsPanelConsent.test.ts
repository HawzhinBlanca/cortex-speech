import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';
import SettingsPanel from '../../src/lib/SettingsPanel.svelte';
import { locale } from '../../src/lib/i18n';
import {
  defaultSettings,
  settings,
  settingsTab,
  showSettings,
} from '../../src/lib/stores/settingsStore';

const mocks = vi.hoisted(() => ({
  updateSettings: vi.fn(),
  getConfiguredProviders: vi.fn(),
  setApiKey: vi.fn(),
}));

vi.mock('../../src/lib/commands', () => ({
  updateSettings: mocks.updateSettings,
  getConfiguredProviders: mocks.getConfiguredProviders,
  setApiKey: mocks.setApiKey,
}));

/**
 * Cloud consent is a privacy-sensitive PERMISSION, so its persistence is asymmetric on purpose —
 * the same fail-safe asymmetry `pipeline.rs`'s LiveConsent uses:
 *
 *   GRANTING waits for Save, so Cancel genuinely cancels.
 *   WITHDRAWING applies immediately, because a withdrawal is a stop instruction that must reach
 *   work already in flight.
 *
 * Before 2026-08-17 both directions persisted the instant the checkbox changed while Cancel skipped
 * the save path entirely — so cancelling out of Settings left cloud consent switched ON with nothing
 * in the UI saying so (external audit finding).
 */
describe('SettingsPanel cloud-consent transaction', () => {
  beforeEach(() => {
    // tauriAvailable must be TRUE or saveQuietly short-circuits to a store write and never calls IPC.
    window.__TAURI_INTERNALS__ = {} as never;
    locale.set('en');
    mocks.updateSettings.mockResolvedValue(undefined);
    mocks.getConfiguredProviders.mockResolvedValue([]);
    settings.set({ ...defaultSettings, cloudSttOptIn: false });
    settingsTab.set('audio');
    showSettings.set(true);
  });

  afterEach(() => {
    cleanup();
    delete window.__TAURI_INTERNALS__;
    settings.set(defaultSettings);
    settingsTab.set('general');
    showSettings.set(false);
    locale.set('ckb');
    vi.clearAllMocks();
  });

  function sttCheckbox(): HTMLInputElement {
    // Anchor on the real consent copy (en), then take the checkbox inside its label — the consent
    // text is the stable, user-visible identity of this control.
    const consent = screen.getByText(/Optional manual ElevenLabs Scribe tools/i);
    const box = consent.closest('label')?.querySelector('input[type="checkbox"]');
    if (!box) throw new Error('cloud STT consent checkbox not found');
    return box as HTMLInputElement;
  }

  it('GRANTING consent does not persist until Save — so Cancel really cancels', async () => {
    render(SettingsPanel);
    const box = sttCheckbox();
    expect(box.checked).toBe(false);

    await fireEvent.click(box); // grant

    // The whole point: nothing reached the backend yet.
    expect(mocks.updateSettings).not.toHaveBeenCalled();
    expect(get(settings).cloudSttOptIn).toBe(false);

    await fireEvent.click(screen.getByRole('button', { name: /cancel/i }));

    // Cancelled: the grant is discarded, never written, and the store still says OFF.
    expect(mocks.updateSettings).not.toHaveBeenCalled();
    expect(get(settings).cloudSttOptIn).toBe(false);
  });

  it('WITHDRAWING consent persists immediately — a stop instruction cannot wait for Save', async () => {
    settings.set({ ...defaultSettings, cloudSttOptIn: true });
    render(SettingsPanel);
    const box = sttCheckbox();
    expect(box.checked).toBe(true);

    await fireEvent.click(box); // withdraw

    expect(mocks.updateSettings).toHaveBeenCalledTimes(1);
    expect(mocks.updateSettings.mock.calls[0][0]).toMatchObject({ cloudSttOptIn: false });
    expect(get(settings).cloudSttOptIn).toBe(false);
  });
});
