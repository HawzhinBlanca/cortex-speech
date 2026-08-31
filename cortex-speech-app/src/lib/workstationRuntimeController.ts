import { get } from 'svelte/store';
import * as api from './commands';
import { registerDurableCloseGuard } from './closeGuard';
import {
  setBatchWorkerSettledHandler,
  setImportCompleteHandler,
  setImportEnrichmentCompleteHandler,
  setImportWorkerSettledHandler,
  startEventListeners,
  stopEventListeners,
} from './events';
import { formatPublicErrorReference } from './errorText';
import { t } from './i18n';
import { globalKeyboardManager, initKeyboardManager } from './keyboard';
import { flushReviewDrafts } from './reviewDraftFlush';
import { sharedDurableReviewUndo } from './durableReviewUndo.svelte';
import { isTauriRuntime } from './runtime';
import { notifications } from './stores/notificationStore';
import { segmentStats, segments } from './stores/segmentStore';
import { showReviewInbox, statusMessage } from './stores/uiStore';
import type { createBatchOperationCoordinator } from './batchOperationCoordinator';
import type { createImportOperationCoordinator } from './importOperationCoordinator';

type ImportCoordinator = ReturnType<typeof createImportOperationCoordinator>;
type BatchCoordinator = ReturnType<typeof createBatchOperationCoordinator>;

type RuntimeDependencies = {
  importCoordinator: ImportCoordinator;
  batchCoordinator: BatchCoordinator;
  registerShortcuts: (keyboard: ReturnType<typeof initKeyboardManager>) => void;
  getViewMode: () => 'curate' | 'insights' | 'review';
  setTauriAvailable: (available: boolean) => void;
  setSegmentsLoading: (loading: boolean) => void;
  setQuarantineNotice: (notice: api.QuarantineNotice | null) => void;
  loadSegments: () => Promise<void>;
  loadLatestAgentHistory: () => Promise<void>;
  loadSettings: () => Promise<void>;
  restoreAndApplySession: () => Promise<void>;
  reconcileImportRecovery: () => Promise<void>;
  resolveImportRecoveryWithoutBackend: () => void;
  flushAutosave: () => void;
  flushAutosaveAsync: () => Promise<void>;
  clearSessionTimer: () => void;
};

export function createWorkstationRuntimeController(dependencies: RuntimeDependencies) {
  let closeUnlisten: (() => void) | undefined;
  let healthInterval: ReturnType<typeof setInterval> | undefined;

  async function checkHealthAndWarn(): Promise<void> {
    const translate = get(t);
    try {
      const health = await api.appHealth();
      if (!health) return;
      const gibibyte = 1024 ** 3;
      if ((health.snapshot_consecutive_failures ?? 0) >= 3) {
        notifications.error(
          translate('notifications.snapshotFailing', {
            count: String(health.snapshot_consecutive_failures),
          }),
        );
      }
      const lastSuccess = health.snapshot_last_success_epoch_secs;
      if (
        lastSuccess != null &&
        Date.now() / 1000 - lastSuccess > 3 * 600 &&
        get(segmentStats).total > 0
      ) {
        notifications.error(
          translate('notifications.snapshotStale', {
            minutes: String(Math.round((Date.now() / 1000 - lastSuccess) / 60)),
          }),
        );
      }
      if (health.free_disk_bytes != null && health.free_disk_bytes < 2 * gibibyte) {
        notifications.error(
          translate('notifications.lowDisk', {
            gb: (health.free_disk_bytes / gibibyte).toFixed(1),
          }),
        );
      }
      if ((health.missing_models?.length ?? 0) > 0) {
        notifications.error(
          translate('notifications.missingModels', {
            models: health.missing_models.join(', '),
          }),
        );
      }
    } catch (error) {
      console.error('health check failed', error);
    }
  }

  async function mount(): Promise<void> {
    const available = isTauriRuntime();
    dependencies.setTauriAvailable(available);
    const keyboard = initKeyboardManager();
    keyboard.setReviewSurfaceProbe(
      () => dependencies.getViewMode() === 'review' || get(showReviewInbox),
    );
    dependencies.registerShortcuts(keyboard);
    setImportCompleteHandler(dependencies.importCoordinator.handleComplete);
    setImportEnrichmentCompleteHandler(dependencies.importCoordinator.handleEnrichment);
    setImportWorkerSettledHandler(dependencies.importCoordinator.settleFromWorker);
    setBatchWorkerSettledHandler(dependencies.batchCoordinator.settleFromWorker);

    if (!available) {
      segments.set([]);
      dependencies.setSegmentsLoading(false);
      dependencies.resolveImportRecoveryWithoutBackend();
      statusMessage.set(get(t)('ready'));
      return;
    }

    try {
      await startEventListeners();
    } catch (error) {
      notifications.error(get(t)('eventListenersFailed'), { cause: error });
    }
    await dependencies.batchCoordinator.adoptActive();
    await dependencies.loadSegments();
    await dependencies.loadLatestAgentHistory();
    await dependencies.loadSettings();
    await dependencies.restoreAndApplySession();
    await dependencies.reconcileImportRecovery();
    dependencies.setQuarantineNotice(
      await api
        .getQuarantineNotice()
        .then((notice) => (notice.quarantinedFileCount > 0 ? notice : null))
        .catch((error) => {
          notifications.error(get(t)('db.quarantineCheckFailed'), { cause: error });
          return null;
        }),
    );
    void checkHealthAndWarn();
    healthInterval = setInterval(() => void checkHealthAndWarn(), 5 * 60 * 1000);
    void api
      .takeLastCrash()
      .then((crash) => {
        if (!crash) return;
        notifications.error(
          get(t)('notifications.previousCrash', {
            summary: formatPublicErrorReference(crash) ?? get(t)('errors.unknown'),
          }),
          { cause: crash },
        );
      })
      .catch((error) => console.error('crash check failed', error));
    try {
      closeUnlisten = await registerDurableCloseGuard({
        flush: async () => {
          if (
            sharedDurableReviewUndo.blocksSurfaceTransition() &&
            !sharedDurableReviewUndo.state.truthWriteAmbiguous
          ) {
            throw new Error(
              sharedDurableReviewUndo.state.truthWriteAmbiguous
                ? 'E_REVIEW_TRUTH_OUTCOME_UNKNOWN_RESTART_REQUIRED'
                : 'E_REVIEW_TRUTH_OPERATION_IN_FLIGHT',
            );
          }
          await Promise.all([flushReviewDrafts(), dependencies.flushAutosaveAsync()]);
          if (
            sharedDurableReviewUndo.blocksSurfaceTransition() &&
            !sharedDurableReviewUndo.state.truthWriteAmbiguous
          ) {
            throw new Error('E_REVIEW_TRUTH_OPERATION_IN_FLIGHT');
          }
        },
        timeoutMs: 10_000,
        onFlushError: (error) => {
          notifications.error(get(t)('review.closeDraftFailed'), {
            cause: error,
            publicDetail: get(t)('review.closeDraftFailedHint'),
          });
        },
        onCloseError: (error) => {
          notifications.error(get(t)('review.closeFailed'), {
            cause: error,
            publicDetail: get(t)('review.closeFailedHint'),
          });
        },
      });
    } catch (error) {
      console.error('Failed to register close-request autosave flush:', error);
    }
  }

  function destroy(): void {
    stopEventListeners();
    globalKeyboardManager?.destroy();
    closeUnlisten?.();
    if (healthInterval) clearInterval(healthInterval);
    dependencies.importCoordinator.destroy();
    dependencies.batchCoordinator.destroy();
    dependencies.flushAutosave();
    dependencies.clearSessionTimer();
  }

  return { destroy, mount };
}
