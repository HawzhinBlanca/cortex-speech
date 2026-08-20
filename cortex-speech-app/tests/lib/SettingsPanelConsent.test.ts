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

  it('a REJECTED Save rolls the store back instead of leaving refused values on screen', async () => {
    mocks.updateSettings.mockRejectedValueOnce(new Error('backend refused'));
    render(SettingsPanel);
    const box = sttCheckbox();

    await fireEvent.click(box); // grant, then press the real Save button
    await fireEvent.click(screen.getByRole('button', { name: /^save$/i }));

    expect(mocks.updateSettings).toHaveBeenCalledTimes(1);
    // The backend refused, so nothing may claim the grant took effect — not the store the rest of
    // the app reads, and not the checkbox the user is looking at.
    expect(get(settings).cloudSttOptIn).toBe(false);
    expect(sttCheckbox().checked).toBe(false);
    expect(get(showSettings)).toBe(true); // and the panel stays open, showing the failure
  });

  it('the store never shares the object the inputs mutate', async () => {
    render(SettingsPanel);
    await fireEvent.click(screen.getByRole('button', { name: /^save$/i }));
    const published = get(settings);

    // A later edit in the panel must not reach into the value the store already published: that
    // mutation notifies nobody, so the app would silently run on half-typed settings.
    await fireEvent.click(sttCheckbox());
    expect(published.cloudSttOptIn).toBe(false);
  });

  it('a slow failing save cannot roll back past a newer successful one (persists are serialized)', async () => {
    // The race (audit fix 2026-08-20): two overlapping persists each snapshot their own `prev`; if
    // the OLDER write fails after a NEWER one succeeded, its rollback reverts the store past the
    // newer persisted state — worst case re-granting a consent the user successfully withdrew.
    // Serialized, the second write only starts after the first settles, so its snapshot is honest.
    settings.set({ ...defaultSettings, cloudSttOptIn: true });
    let failFirst!: (e: Error) => void;
    mocks.updateSettings
      .mockImplementationOnce(() => new Promise<void>((_, reject) => (failFirst = reject)))
      .mockResolvedValueOnce(undefined);
    render(SettingsPanel);

    await fireEvent.click(sttCheckbox()); // withdraw -> save #1, held in flight
    expect(mocks.updateSettings).toHaveBeenCalledTimes(1);
    await fireEvent.click(screen.getByRole('button', { name: /^save$/i })); // save #2, must queue
    expect(mocks.updateSettings).toHaveBeenCalledTimes(1); // queued, not interleaved

    failFirst(new Error('backend hiccup')); // the OLD save fails after the new one was requested
    await Promise.resolve().then(() => {}); // let the queue drain: rollback #1, then run save #2
    await vi.waitFor(() => expect(mocks.updateSettings).toHaveBeenCalledTimes(2));

    // The newer state (withdrawn) is what save #2 persisted and what the app must end on. Without
    // the queue, save #1's rollback lands LAST and silently re-grants the withdrawn consent.
    expect(mocks.updateSettings.mock.calls[1][0]).toMatchObject({ cloudSttOptIn: false });
    expect(get(settings).cloudSttOptIn).toBe(false);
    expect(sttCheckbox().checked).toBe(false);
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
