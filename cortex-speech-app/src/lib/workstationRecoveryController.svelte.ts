import * as api from './commands';
import { get } from 'svelte/store';
import { createBatchOperationCoordinator } from './batchOperationCoordinator';
import {
  createImportRecoveryController,
  type ImportRecoveryAuthorityState,
} from './importRecoveryController';
import { createImportOperationCoordinator } from './importOperationCoordinator';
import { t } from './i18n';
import { notifications } from './stores/notificationStore';
import { segments } from './stores/segmentStore';
import { historyStore } from './stores/historyStore';
import type { ImportEventContext } from './events';

type RecoveryDependencies = {
  requireDesktopRuntime: () => boolean;
  loadSegments: (isCurrent?: () => boolean) => Promise<void>;
  loadLatestAgentHistory: (expectedRunId?: string, context?: ImportEventContext) => Promise<void>;
  clearAgentEvidence: () => void;
  setSegmentsLoading: (loading: boolean) => void;
};

export function createWorkstationRecoveryController(dependencies: RecoveryDependencies) {
  let interruptedImport = $state<api.ImportJob | null>(null);
  let importRecoveryBusy = $state(false);
  let importRecoveryAuthority = $state<ImportRecoveryAuthorityState>('checking');
  let importStarting = $state(false);
  let batchStarting = $state(false);
  let quarantineNotice = $state<api.QuarantineNotice | null>(null);

  function requireClearImportRecoveryAuthority(): boolean {
    if (importRecoveryAuthority === 'known' && !interruptedImport) return true;
    const translate = get(t);
    notifications.warning(translate('import.recoveryBlocksNew'), {
      publicDetail:
        importRecoveryAuthority === 'known'
          ? translate('import.recoveryBlocksNewJournalDetail')
          : translate('import.recoveryBlocksNewUnknownDetail'),
    });
    return false;
  }

  const importRecovery = createImportRecoveryController({
    currentJob: () => interruptedImport,
    setBusy: (busy) => (importRecoveryBusy = busy),
    clearIfCurrent: (id) => {
      if (interruptedImport?.id === id) interruptedImport = null;
    },
    load: api.getInterruptedImport,
    replaceCurrent: (job) => (interruptedImport = job),
    resume: (jobId) => importCoordinator.resume(jobId),
    discard: api.discardInterruptedImport,
    onResumeSuccess: () => notifications.success(get(t)('import.resumeStarted')),
    onResumeFailure: (error) => {
      if (!importCoordinator.shouldSuppressResumeFailure()) {
        notifications.error(get(t)('import.resumeFailed'), { cause: error });
      }
    },
    onDiscardFailure: (error) =>
      notifications.error(get(t)('import.discardFailed'), { cause: error }),
    onLoadFailure: (error) =>
      notifications.error(get(t)('import.recoveryCheckFailed'), { cause: error }),
    setAuthorityState: (state) => (importRecoveryAuthority = state),
  });

  const importCoordinator = createImportOperationCoordinator({
    requireDesktopRuntime: dependencies.requireDesktopRuntime,
    canStartImport: requireClearImportRecoveryAuthority,
    setStarting: (starting) => (importStarting = starting),
    loadSegments: dependencies.loadSegments,
    loadLatestAgentHistory: dependencies.loadLatestAgentHistory,
    reconcileRecovery: () => importRecovery.reconcile(),
    clearAgentEvidence: dependencies.clearAgentEvidence,
  });

  const batchCoordinator = createBatchOperationCoordinator({
    loadSegments: dependencies.loadSegments,
    invalidateSegmentLoad: () => {
      segments.bumpLoadGeneration();
      dependencies.setSegmentsLoading(false);
    },
    refreshHistory: () => historyStore.refresh(),
  });

  async function acknowledgeQuarantine(): Promise<void> {
    try {
      const moved = await api.acknowledgeQuarantine();
      notifications.success(get(t)('db.quarantineAcknowledged', { count: String(moved) }));
      quarantineNotice = null;
    } catch (error) {
      notifications.error(get(t)('db.quarantineAcknowledgeFailed'), { cause: error });
    }
  }

  return {
    get interruptedImport() {
      return interruptedImport;
    },
    get importRecoveryBusy() {
      return importRecoveryBusy;
    },
    get importRecoveryAuthority() {
      return importRecoveryAuthority;
    },
    // Browser mode has no backend and therefore no durable import journal to reconcile — authority
    // is vacuously known-empty. Without this, the mount early-return left it 'checking' forever and
    // the recovery region rendered a permanently busy resume banner in a mode that cannot resume.
    resolveImportRecoveryWithoutBackend() {
      interruptedImport = null;
      importRecoveryAuthority = 'known';
    },
    get importStarting() {
      return importStarting;
    },
    get batchStarting() {
      return batchStarting;
    },
    set batchStarting(value: boolean) {
      batchStarting = value;
    },
    get quarantineNotice() {
      return quarantineNotice;
    },
    set quarantineNotice(value: api.QuarantineNotice | null) {
      quarantineNotice = value;
    },
    acknowledgeQuarantine,
    batchCoordinator,
    importCoordinator,
    importRecovery,
  };
}
