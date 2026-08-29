import { get } from 'svelte/store';
import * as api from './commands';
import { t } from './i18n';
import { ApiKeySaveFailure } from './settingsKeyController.svelte';
import { notifications } from './stores/notificationStore';
import { settings, showSettings, type AppSettings } from './stores/settingsStore';

type SettingsPersistenceDependencies = {
  tauriAvailable: boolean;
  getLocal: () => AppSettings;
  setLocal: (value: AppSettings) => void;
  flushPendingKeys: () => Promise<void>;
  onSavingChange: (saving: boolean) => void;
};

const CONSENT_FIELDS = ['cloudLlmOptIn', 'juryCloudOptIn'] as const;

export function createSettingsPersistenceController({
  tauriAvailable,
  getLocal,
  setLocal,
  flushPendingKeys,
  onSavingChange,
}: SettingsPersistenceDependencies) {
  let persistQueue: Promise<void> = Promise.resolve();
  let persistBusy = false;
  let persistPending = 0;
  let lastPersisted: AppSettings = { ...get(settings) };
  let saving = false;

  function enqueuePersist(job: () => Promise<void>): Promise<void> {
    const run = persistBusy ? persistQueue.then(job) : job();
    persistBusy = true;
    persistPending += 1;
    const settled = run.catch(() => {});
    persistQueue = settled;
    void settled.then(() => {
      persistPending -= 1;
      if (persistQueue === settled) persistBusy = false;
    });
    return run;
  }

  function coerceSettingsForRuntime(): AppSettings {
    const local = getLocal();
    const persisted = get(settings);
    const finite = (value: number, fallback: number): number =>
      Number.isFinite(value) ? value : fallback;
    const coerced: AppSettings = {
      ...local,
      asrModel: 'wsl-7b',
      useFinetuned: false,
      vadThreshold: finite(local.vadThreshold, persisted.vadThreshold),
      minSegmentSec: finite(local.minSegmentSec, persisted.minSegmentSec),
      maxSegmentSec: finite(local.maxSegmentSec, persisted.maxSegmentSec),
      maxSpeakers: finite(local.maxSpeakers, persisted.maxSpeakers),
      maxWerThreshold: finite(local.maxWerThreshold, persisted.maxWerThreshold),
      maxCerThreshold: finite(local.maxCerThreshold, persisted.maxCerThreshold),
      jurySelfConsistencyN: finite(local.jurySelfConsistencyN, persisted.jurySelfConsistencyN),
    };
    setLocal(coerced);
    return coerced;
  }

  function rollbackTo(previous: AppSettings, error?: unknown): void {
    const authoritative =
      error && typeof error === 'object' && 'authoritativeSettings' in error
        ? ((error as { authoritativeSettings?: AppSettings | null }).authoritativeSettings ?? null)
        : null;
    if (authoritative) lastPersisted = { ...authoritative };
    const restored = { ...(authoritative ?? previous) };
    setLocal(restored);
    settings.set({ ...restored });
  }

  function autosavable(local: AppSettings): AppSettings {
    const clamped = { ...local };
    for (const field of CONSENT_FIELDS) {
      clamped[field] = local[field] && lastPersisted[field];
    }
    return clamped;
  }

  function saveQuietly(): Promise<void> {
    return enqueuePersist(async () => {
      const payload = autosavable(coerceSettingsForRuntime());
      if (!tauriAvailable) {
        settings.set(payload);
        return;
      }
      const previous = { ...lastPersisted };
      settings.set({ ...payload });
      try {
        await api.updateSettings(payload);
        lastPersisted = { ...payload };
      } catch (error) {
        console.error('Auto-save settings failed:', error);
        if (persistPending > 1) return;
        rollbackTo(previous, error);
        notifications.error(get(t)('settingsSaveFailed'), { cause: error });
      }
    });
  }

  function consentToggled(field: (typeof CONSENT_FIELDS)[number]): void {
    const wasGranted = get(settings)[field];
    if (wasGranted && !getLocal()[field]) void saveQuietly();
  }

  async function save(): Promise<void> {
    if (saving) return;
    saving = true;
    onSavingChange(true);
    try {
      await enqueuePersist(async () => {
        const previous = { ...lastPersisted };
        try {
          const local = coerceSettingsForRuntime();
          settings.set({ ...local });
          if (!tauriAvailable) {
            notifications.info(get(t)('settingsPreviewOnly'));
            showSettings.set(false);
            return;
          }
          await flushPendingKeys();
          await api.updateSettings(local);
          lastPersisted = { ...local };
          notifications.success(get(t)('settingsSaved'));
          showSettings.set(false);
        } catch (error) {
          console.error('Save settings failed:', error);
          if (persistPending > 1) return;
          rollbackTo(previous, error);
          if (!(error instanceof ApiKeySaveFailure)) {
            notifications.error(get(t)('settingsSaveFailed'), { cause: error });
          }
        }
      });
    } finally {
      saving = false;
      onSavingChange(false);
    }
  }

  function saveOnDestroy(): void {
    if (JSON.stringify(getLocal()) !== JSON.stringify(get(settings))) {
      void saveQuietly();
    }
  }

  return { consentToggled, save, saveOnDestroy, saveQuietly };
}
