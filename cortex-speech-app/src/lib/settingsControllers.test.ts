import { get } from 'svelte/store';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { activeOperations } from './invoke';
import { locale } from './i18n';
import { exportVerifiedAudioFromSettings } from './settingsAudioExport';
import { ApiKeySaveFailure, createSettingsKeyController } from './settingsKeyController.svelte';
import { createSettingsPersistenceController } from './settingsPersistenceController';
import { notifications } from './stores/notificationStore';
import { segments } from './stores/segmentStore';
import { defaultSettings, settings, showSettings, type AppSettings } from './stores/settingsStore';
import { batchProgress, isProcessing, statusMessage } from './stores/uiStore';
import type { SpeechSegment } from './types';

const commandMocks = vi.hoisted(() => ({
  exportAudio: vi.fn(),
  getConfiguredProviders: vi.fn(),
  setApiKey: vi.fn(),
  updateSettings: vi.fn(),
}));

const dialogMocks = vi.hoisted(() => ({
  chooseDirectory: vi.fn(),
}));

vi.mock('./commands', () => ({
  AudioExportFormat: { Wav: 'Wav' },
  exportAudio: commandMocks.exportAudio,
  getConfiguredProviders: commandMocks.getConfiguredProviders,
  setApiKey: commandMocks.setApiKey,
  updateSettings: commandMocks.updateSettings,
}));

vi.mock('./fileDialogs', () => ({
  chooseDirectory: dialogMocks.chooseDirectory,
}));

function segment(overrides: Partial<SpeechSegment> = {}): SpeechSegment {
  return {
    id: 'segment-1',
    audioPath: 'C:\\audio\\sample.wav',
    rawTranscript: 'real transcript',
    normalizedTranscript: null,
    annotatedTranscript: null,
    alignmentJson: null,
    durationMs: 1_000,
    speakerId: null,
    verified: false,
    ...overrides,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

describe('settings audio export transaction', () => {
  beforeEach(() => {
    locale.set('en');
    segments.set([]);
    isProcessing.set(false);
    statusMessage.set('Ready');
    batchProgress.set({ status: 'idle', completed: 0, total: 0, percent: 0 });
    activeOperations.set(new Set());
    commandMocks.exportAudio.mockReset();
    dialogMocks.chooseDirectory.mockReset();
  });

  afterEach(() => {
    vi.restoreAllMocks();
    segments.set([]);
    activeOperations.set(new Set());
    locale.set('ckb');
  });

  it('fails closed outside the desktop runtime before reading a destination', async () => {
    const info = vi.spyOn(notifications, 'info').mockImplementation(() => 'notice');
    const busy = vi.fn();

    await exportVerifiedAudioFromSettings(false, busy);

    expect(info).toHaveBeenCalledWith('Desktop app runtime required');
    expect(dialogMocks.chooseDirectory).not.toHaveBeenCalled();
    expect(commandMocks.exportAudio).not.toHaveBeenCalled();
    expect(busy).not.toHaveBeenCalled();
  });

  it('does not treat a finalized human rejection as verified export material', async () => {
    segments.set([
      segment({ id: 'rejected', verified: true, humanDecision: 'reject' }),
      segment({ id: 'pending', verified: false }),
    ]);
    const warning = vi.spyOn(notifications, 'warning').mockImplementation(() => 'notice');

    await exportVerifiedAudioFromSettings(true, vi.fn());

    expect(warning).toHaveBeenCalledWith('No verified segments to export');
    expect(dialogMocks.chooseDirectory).not.toHaveBeenCalled();
    expect(commandMocks.exportAudio).not.toHaveBeenCalled();
  });

  it('exports only verified-good IDs with the fixed lossless contract and restores all busy state', async () => {
    segments.set([
      segment({ id: 'accepted', verified: true, humanDecision: 'accept' }),
      segment({ id: 'rejected', verified: true, verdict: 'human_reject' }),
    ]);
    dialogMocks.chooseDirectory.mockResolvedValue('D:\\verified-audio');
    commandMocks.exportAudio.mockResolvedValue({
      total: 1,
      succeeded: 1,
      failed: 0,
      output_dir: 'D:\\verified-audio',
      files: ['accepted.wav'],
      errors: [],
    });
    const success = vi.spyOn(notifications, 'success').mockImplementation(() => 'notice');
    const busy = vi.fn();

    await exportVerifiedAudioFromSettings(true, busy);

    expect(commandMocks.exportAudio).toHaveBeenCalledWith(['accepted'], {
      output_dir: 'D:\\verified-audio',
      format: 'Wav',
      sample_rate: 16_000,
      include_metadata: true,
    });
    expect(success).toHaveBeenCalledWith('Exported 1 audio file(s)', {
      detail: 'D:\\verified-audio',
    });
    expect(busy.mock.calls).toEqual([[true], [false]]);
    expect(get(isProcessing)).toBe(false);
    expect(get(statusMessage)).toBe('Ready');
    expect(get(batchProgress)).toEqual({ status: 'idle', completed: 0, total: 0, percent: 0 });
    expect(get(activeOperations)).toEqual(new Set());
  });

  it('reports partial counts honestly and still clears the operation authority', async () => {
    segments.set([segment({ id: 'accepted', verified: true })]);
    dialogMocks.chooseDirectory.mockResolvedValue('D:\\partial');
    commandMocks.exportAudio.mockResolvedValue({
      total: 2,
      succeeded: 1,
      failed: 1,
      output_dir: 'D:\\partial',
      files: ['accepted.wav'],
      errors: ['disk full'],
    });
    const warning = vi.spyOn(notifications, 'warning').mockImplementation(() => 'notice');
    const success = vi.spyOn(notifications, 'success').mockImplementation(() => 'notice');

    await exportVerifiedAudioFromSettings(true, vi.fn());

    expect(warning).toHaveBeenCalledWith('Exported 1, 1 failed', {
      detail: 'D:\\partial',
    });
    expect(success).not.toHaveBeenCalled();
    expect(get(isProcessing)).toBe(false);
    expect(get(activeOperations)).toEqual(new Set());
  });

  it('preserves a clean idle state when the picker is cancelled or export throws', async () => {
    segments.set([segment({ id: 'accepted', verified: true })]);
    const busy = vi.fn();
    dialogMocks.chooseDirectory.mockResolvedValueOnce(null);

    await exportVerifiedAudioFromSettings(true, busy);

    expect(commandMocks.exportAudio).not.toHaveBeenCalled();
    expect(busy.mock.calls).toEqual([[false]]);
    expect(get(activeOperations)).toEqual(new Set());

    const failure = new Error('write failed');
    const error = vi.spyOn(notifications, 'error').mockImplementation(() => 'notice');
    dialogMocks.chooseDirectory.mockResolvedValueOnce('D:\\broken');
    commandMocks.exportAudio.mockRejectedValueOnce(failure);

    await exportVerifiedAudioFromSettings(true, busy);

    expect(error).toHaveBeenCalledWith('Audio export failed', { cause: failure });
    expect(busy.mock.calls.slice(-2)).toEqual([[true], [false]]);
    expect(get(isProcessing)).toBe(false);
    expect(get(activeOperations)).toEqual(new Set());
  });
});

describe('settings API-key controller', () => {
  beforeEach(() => {
    locale.set('en');
    commandMocks.getConfiguredProviders.mockReset();
    commandMocks.setApiKey.mockReset();
  });

  afterEach(() => {
    vi.restoreAllMocks();
    locale.set('ckb');
  });

  it('is inert in preview mode', async () => {
    const controller = createSettingsKeyController(false);
    controller.geminiKeyInput = 'secret';
    controller.openrouterKeyInput = 'other-secret';

    await controller.loadConfiguredProviders();
    await controller.flushPendingKeys();

    expect(commandMocks.getConfiguredProviders).not.toHaveBeenCalled();
    expect(commandMocks.setApiKey).not.toHaveBeenCalled();
    expect(controller.hasPendingKey).toBe(true);
  });

  it('coalesces an in-flight OpenRouter save and never clears a newer value', async () => {
    const save = deferred<string[]>();
    commandMocks.setApiKey.mockReturnValueOnce(save.promise).mockResolvedValueOnce(['openrouter']);
    const success = vi.spyOn(notifications, 'success').mockImplementation(() => 'notice');
    const controller = createSettingsKeyController(true);
    controller.openrouterKeyInput = ' first-secret ';

    const first = controller.saveOpenrouterKey();
    const joined = controller.saveOpenrouterKey();
    expect(controller.savingOpenrouterKey).toBe(true);
    expect(commandMocks.setApiKey).toHaveBeenCalledTimes(1);
    expect(commandMocks.setApiKey).toHaveBeenCalledWith('openrouter', 'first-secret');

    controller.openrouterKeyInput = 'newer-secret';
    save.resolve(['openrouter']);
    await Promise.all([first, joined]);

    expect(controller.openrouterKeyInput).toBe('newer-secret');
    expect(controller.savingOpenrouterKey).toBe(false);
    expect(controller.configuredProviders).toEqual(['openrouter']);
    expect(success).toHaveBeenCalledWith('OpenRouter key saved to secrets.env');

    await controller.saveOpenrouterKey();
    expect(commandMocks.setApiKey).toHaveBeenCalledTimes(2);
    expect(controller.openrouterKeyInput).toBe('');
  });

  it('retains the exact input and exposes a typed failure when secure persistence fails', async () => {
    const failure = new Error('credential store unavailable');
    commandMocks.setApiKey.mockRejectedValueOnce(failure);
    const error = vi.spyOn(notifications, 'error').mockImplementation(() => 'notice');
    const controller = createSettingsKeyController(true);
    controller.geminiKeyInput = '  retry-me  ';

    await expect(controller.saveGeminiKey()).rejects.toEqual(
      expect.objectContaining<ApiKeySaveFailure>({
        name: 'ApiKeySaveFailure',
        message: 'The Gemini API key was not saved',
      }),
    );

    expect(controller.geminiKeyInput).toBe('  retry-me  ');
    expect(controller.savingGeminiKey).toBe(false);
    expect(controller.hasPendingKey).toBe(true);
    expect(error).toHaveBeenCalledWith('Failed to save Gemini key', { cause: failure });
  });

  it('flushes both pending providers sequentially so the first failure cannot be hidden', async () => {
    const callOrder: string[] = [];
    commandMocks.setApiKey.mockImplementation(async (provider: string) => {
      callOrder.push(provider);
      return [provider];
    });
    vi.spyOn(notifications, 'success').mockImplementation(() => 'notice');
    const controller = createSettingsKeyController(true);
    controller.openrouterKeyInput = 'openrouter-secret';
    controller.geminiKeyInput = 'gemini-secret';

    await controller.flushPendingKeys();

    expect(callOrder).toEqual(['openrouter', 'gemini']);
    expect(controller.openrouterKeyInput).toBe('');
    expect(controller.geminiKeyInput).toBe('');
    expect(controller.hasPendingKey).toBe(false);
  });
});

describe('settings persistence controller', () => {
  beforeEach(() => {
    locale.set('en');
    settings.set({ ...defaultSettings });
    showSettings.set(true);
    commandMocks.updateSettings.mockReset();
  });

  afterEach(() => {
    vi.restoreAllMocks();
    settings.set({ ...defaultSettings });
    showSettings.set(false);
    locale.set('ckb');
  });

  function controllerHarness(
    overrides: {
      tauriAvailable?: boolean;
      local?: AppSettings;
      flushPendingKeys?: () => Promise<void>;
    } = {},
  ) {
    let local = { ...(overrides.local ?? defaultSettings) };
    const savingChanges: boolean[] = [];
    const controller = createSettingsPersistenceController({
      tauriAvailable: overrides.tauriAvailable ?? true,
      getLocal: () => local,
      setLocal: (value) => (local = { ...value }),
      flushPendingKeys: overrides.flushPendingKeys ?? (async () => {}),
      onSavingChange: (saving) => savingChanges.push(saving),
    });
    return { controller, getLocal: () => local, savingChanges };
  }

  it('uses backend-authoritative settings after a rejected optimistic save', async () => {
    const attempted = { ...defaultSettings, theme: 'light' as const, cloudLlmOptIn: true };
    const authoritative = { ...defaultSettings, theme: 'dark' as const, cloudLlmOptIn: false };
    settings.set({ ...defaultSettings });
    commandMocks.updateSettings.mockRejectedValueOnce({
      code: 'SETTINGS_REVISION_CONFLICT',
      authoritativeSettings: authoritative,
    });
    const error = vi.spyOn(notifications, 'error').mockImplementation(() => 'notice');
    const harness = controllerHarness({ local: attempted });

    await harness.controller.save();

    expect(harness.getLocal()).toEqual(authoritative);
    expect(get(settings)).toEqual(authoritative);
    expect(get(showSettings)).toBe(true);
    expect(harness.savingChanges).toEqual([true, false]);
    expect(error).toHaveBeenCalledOnce();
  });

  it('coerces non-finite numeric input to persisted truth in preview mode', async () => {
    const persisted = {
      ...defaultSettings,
      vadThreshold: 0.73,
      minSegmentSec: 2,
      maxSegmentSec: 17,
      maxSpeakers: 6,
      maxWerThreshold: 0.4,
      maxCerThreshold: 0.2,
      jurySelfConsistencyN: 5,
    };
    settings.set(persisted);
    const local = {
      ...persisted,
      asrModel: 'wsl-7b' as const,
      useFinetuned: true,
      vadThreshold: Number.NaN,
      minSegmentSec: Number.POSITIVE_INFINITY,
      maxSegmentSec: Number.NEGATIVE_INFINITY,
      maxSpeakers: Number.NaN,
      maxWerThreshold: Number.NaN,
      maxCerThreshold: Number.NaN,
      jurySelfConsistencyN: Number.NaN,
    };
    const info = vi.spyOn(notifications, 'info').mockImplementation(() => 'notice');
    const harness = controllerHarness({ tauriAvailable: false, local });

    await harness.controller.save();

    expect(harness.getLocal()).toMatchObject({
      asrModel: 'wsl-7b',
      useFinetuned: false,
      vadThreshold: 0.73,
      minSegmentSec: 2,
      maxSegmentSec: 17,
      maxSpeakers: 6,
      maxWerThreshold: 0.4,
      maxCerThreshold: 0.2,
      jurySelfConsistencyN: 5,
    });
    expect(commandMocks.updateSettings).not.toHaveBeenCalled();
    expect(info).toHaveBeenCalledWith(
      'Settings preview only; persistent settings require the desktop app runtime.',
    );
    expect(get(showSettings)).toBe(false);
  });

  it('coalesces repeated Save gestures while the first durable save is in flight', async () => {
    const flush = deferred<void>();
    commandMocks.updateSettings.mockResolvedValue(undefined);
    vi.spyOn(notifications, 'success').mockImplementation(() => 'notice');
    const flushPendingKeys = vi.fn(() => flush.promise);
    const harness = controllerHarness({ flushPendingKeys });

    const first = harness.controller.save();
    const repeated = harness.controller.save();

    expect(flushPendingKeys).toHaveBeenCalledTimes(1);
    expect(commandMocks.updateSettings).not.toHaveBeenCalled();
    expect(harness.savingChanges).toEqual([true]);

    flush.resolve();
    await Promise.all([first, repeated]);

    expect(commandMocks.updateSettings).toHaveBeenCalledTimes(1);
    expect(harness.savingChanges).toEqual([true, false]);
    expect(get(showSettings)).toBe(false);
  });
});
