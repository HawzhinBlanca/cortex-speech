import { get } from 'svelte/store';
import * as api from './commands';
import { parseActionableError } from './errors';
import {
  beginBatchEventScope,
  closeBatchEventScope,
  createBatchOperationId,
  markBatchEventSettled,
  publicBatchHaltDetail,
  type BatchEventContext,
  type BatchOperationKind,
  type BatchWorkerSettled,
} from './events';
import {
  boundedBatchCommandResponse,
  reconcileBatchStartResponse,
  type BatchRunOutcomeWire,
} from './batchStartReconciliation';
import { createBatchRunAdoption } from './batchRunAdoption';
import { acknowledgeBatchRunWithRetry } from './batchRunAcknowledgement';
import {
  boundedBatchRefresh,
  sameBatchOutcome,
  terminalEventMatchesOutcome,
} from './batchSettlement';
import { t, type TranslationKey } from './i18n';
import { endOperation, startOperation } from './invoke';
import { notifications } from './stores/notificationStore';
import { segments } from './stores/segmentStore';
import {
  agentPipelineStages,
  batchProgress,
  isProcessing,
  pipelinePhase,
  statusMessage,
} from './stores/uiStore';

type CurrentBatchOperation = {
  operationId: string;
  operation: BatchOperationKind;
  activityId: string;
  context: BatchEventContext;
  responseLossNotified: boolean;
  outcomeLossNotified: boolean;
  commandResponsePending: boolean;
  rejectionObservedAt: number | null;
  terminalOutcome: BatchRunOutcomeWire | null;
  outcomePresented: boolean;
  refreshComplete: boolean;
  refreshNoticeId: string | null;
  acknowledgementNoticeId: string | null;
};

export type BatchOperationCoordinatorOptions = {
  loadSegments: (isCurrent?: () => boolean) => Promise<void>;
  invalidateSegmentLoad: () => void;
  refreshHistory: () => Promise<void>;
};

function tr(key: TranslationKey, params?: Record<string, string>): string {
  return get(t)(key, params);
}

function failureKey(operation: BatchOperationKind): TranslationKey {
  return operation === 'transcribe' ? 'batchTranscribe.failed' : 'batchNormalize.failed';
}

function progressKey(operation: BatchOperationKind): TranslationKey {
  return operation === 'transcribe' ? 'batchTranscribe.progress' : 'batchNormalize.progress';
}

const batchStartFailureKeys: Partial<Record<string, readonly [TranslationKey, TranslationKey]>> = {
  BATCH_START_CANCELLED: ['batch.startCancelled', 'batch.startCancelledDetail'],
  BATCH_START_AUTHORITY_LOST: ['batch.startAuthorityLost', 'batch.startAuthorityLostDetail'],
  RESTORE_GENERATION_CHANGED: [
    'batch.restoreGenerationChanged',
    'batch.restoreGenerationChangedDetail',
  ],
};

function unexpectedBatchRejection() {
  return {
    schema: 1,
    code: 'BATCH_RUN_REJECTED',
    message: 'The accepted batch operation was later rejected.',
    retryable: true,
    suggestedAction: 'retry' as const,
  };
}

export function createBatchOperationCoordinator(options: BatchOperationCoordinatorOptions) {
  let current: CurrentBatchOperation | null = null;
  let statusMonitor: ReturnType<typeof setTimeout> | null = null;
  let destroyed = false;
  let settlingOperationId: string | null = null;

  function clearStatusMonitor() {
    if (statusMonitor) clearTimeout(statusMonitor);
    statusMonitor = null;
  }

  function resetProgress() {
    isProcessing.set(false);
    pipelinePhase.set('idle');
    batchProgress.set({ status: 'idle', completed: 0, total: 0, percent: 0 });
    agentPipelineStages.set([]);
    statusMessage.set(tr('ready'));
  }

  function close(operationId: string) {
    const operation = current;
    if (!operation || operation.operationId !== operationId) return;
    clearStatusMonitor();
    if (operation.refreshNoticeId) notifications.dismiss(operation.refreshNoticeId);
    if (operation.acknowledgementNoticeId) notifications.dismiss(operation.acknowledgementNoticeId);
    endOperation(operation.activityId);
    closeBatchEventScope(operation.context);
    current = null;
  }

  function actionableError(error: unknown, operation: BatchOperationKind) {
    const parsed = parseActionableError(error, tr(failureKey(operation)));
    const localized = parsed.code ? batchStartFailureKeys[parsed.code] : undefined;
    notifications.error(localized ? tr(localized[0]) : parsed.message, {
      cause: error,
      publicDetail: localized ? tr(localized[1]) : parsed.detail,
      action: parsed.action,
    });
  }

  function notifyResponseLoss(operation: CurrentBatchOperation, uncertain: boolean) {
    if (operation.responseLossNotified) return;
    operation.responseLossNotified = true;
    statusMessage.set(tr('batch.responseLostStatus'));
    notifications.warning(tr('batch.responseLost'), {
      publicDetail: tr(
        uncertain ? 'batch.responseLostUncertainDetail' : 'batch.responseLostRunningDetail',
      ),
    });
  }

  function notifyRecoveredOutcome(operation: BatchOperationKind, outcome: BatchRunOutcomeWire) {
    if (outcome.disposition === 'panicked' || outcome.disposition === 'halted') {
      const code = outcome.errorCode ?? 'BATCH_WORKER_PANICKED';
      notifications.error(tr(failureKey(operation)), {
        publicDetail: publicBatchHaltDetail({ code }),
      });
      return;
    }
    if (outcome.disposition === 'cancelled') {
      notifications.warning(tr('events.batchCancelled'));
      return;
    }
    const partialKey =
      operation === 'transcribe' ? 'events.batchTranscribePartial' : 'events.batchNormalizePartial';
    const successKey = operation === 'transcribe' ? 'events.transcribed' : 'events.normalized';
    if (outcome.failed > 0) {
      notifications.warning(
        tr(partialKey, {
          ok: String(outcome.succeeded),
          failed: String(outcome.failed),
        }),
      );
    } else if (outcome.succeeded > 0) {
      notifications.success(tr(successKey, { n: String(outcome.succeeded) }));
    } else {
      notifications.info(tr('batch.settledRecovered'));
    }
  }

  function notifyOutcomeUnknown(operation: CurrentBatchOperation) {
    if (operation.outcomeLossNotified) return;
    operation.outcomeLossNotified = true;
    statusMessage.set(tr('batch.responseLostStatus'));
    notifications.error(tr('batch.outcomeUnavailable'), {
      publicDetail: tr('batch.outcomeUnavailableDetail'),
    });
  }

  function notifyAcknowledgementLoss(operation: CurrentBatchOperation) {
    if (operation.acknowledgementNoticeId) return;
    statusMessage.set(tr('batch.acknowledgementPending'));
    operation.acknowledgementNoticeId = notifications.error(tr('batch.acknowledgementFailed'), {
      publicDetail: tr('batch.acknowledgementFailedDetail'),
      action: {
        label: tr('retry'),
        handler: () =>
          scheduleStatusMonitor(operation.operationId, unexpectedBatchRejection(), 0, true),
      },
    });
  }

  async function settle(
    operationId: string,
    context: BatchEventContext,
    knownOutcome?: BatchRunOutcomeWire,
  ) {
    const operation = current;
    if (
      !operation ||
      operation.operationId !== operationId ||
      operation.context.operation !== context.operation ||
      operation.context.generation !== context.generation ||
      !context.isCurrent() ||
      settlingOperationId === operationId
    ) {
      return;
    }
    if (!context.hasSettledEvent() && !markBatchEventSettled(context)) return;
    settlingOperationId = operationId;
    clearStatusMonitor();
    let mayClose = false;
    try {
      let outcome = operation.terminalOutcome ?? knownOutcome;
      if (
        operation.terminalOutcome &&
        knownOutcome &&
        !sameBatchOutcome(operation.terminalOutcome, knownOutcome)
      ) {
        notifications.error(tr('batch.eventOutcomeMismatch'), {
          publicDetail: tr('batch.eventOutcomeMismatchDetail'),
        });
        scheduleStatusMonitor(operationId, unexpectedBatchRejection(), 2_000);
        return;
      }
      if (!outcome) {
        const reconciled = await reconcileBatchStartResponse({
          context,
          getStatus: api.getBatchRunStatus,
        });
        if (!context.isCurrent() || current?.operationId !== operationId) return;
        if (reconciled.disposition !== 'settled' || !reconciled.outcome) {
          notifyOutcomeUnknown(operation);
          scheduleStatusMonitor(operationId, unexpectedBatchRejection(), 2_000);
          return;
        }
        outcome = reconciled.outcome;
      }
      operation.terminalOutcome = outcome;
      if (!operation.outcomePresented) {
        if (!terminalEventMatchesOutcome(context.terminalEvent(), outcome)) {
          notifications.error(tr('batch.eventOutcomeMismatch'), {
            publicDetail: tr('batch.eventOutcomeMismatchDetail'),
          });
        }
        notifyRecoveredOutcome(operation.operation, outcome);
        operation.outcomePresented = true;
      }
      if (!operation.refreshComplete) {
        try {
          await boundedBatchRefresh(
            options.loadSegments(context.isCurrent),
            15_000,
            options.invalidateSegmentLoad,
          );
          if (!context.isCurrent()) return;
          await boundedBatchRefresh(options.refreshHistory());
          if (!context.isCurrent()) return;
          operation.refreshComplete = true;
          if (operation.refreshNoticeId) {
            notifications.dismiss(operation.refreshNoticeId);
            operation.refreshNoticeId = null;
          }
        } catch (error) {
          if (context.isCurrent()) {
            operation.refreshNoticeId ??= notifications.error(tr('events.batchRefreshFailed'), {
              cause: error,
            });
            scheduleStatusMonitor(operationId, error, 2_000, true);
          }
          return;
        }
      }
      const acknowledgement = await acknowledgeBatchRunWithRetry({
        operationId,
        acknowledge: api.acknowledgeBatchRun,
        isCurrent: context.isCurrent,
      });
      if (acknowledgement === 'stale') return;
      if (acknowledgement !== 'acknowledged') {
        notifyAcknowledgementLoss(operation);
        scheduleStatusMonitor(operationId, unexpectedBatchRejection(), 2_000, true);
        return;
      }
      mayClose = true;
    } catch (error) {
      if (context.isCurrent()) {
        notifications.error(tr('events.batchRefreshFailed'), { cause: error });
      }
    } finally {
      if (mayClose && context.isCurrent() && current?.operationId === operationId) {
        resetProgress();
        close(operationId);
      }
      if (settlingOperationId === operationId) settlingOperationId = null;
    }
  }

  function scheduleStatusMonitor(
    operationId: string,
    error: unknown,
    delayMs = 5_000,
    exactOutcomeOnly = false,
  ) {
    if (current?.operationId !== operationId) return;
    clearStatusMonitor();
    statusMonitor = setTimeout(() => {
      statusMonitor = null;
      void (async () => {
        const operation = current;
        if (!operation || operation.operationId !== operationId || !operation.context.isCurrent()) {
          return;
        }
        if (exactOutcomeOnly && operation.terminalOutcome) {
          await settle(operationId, operation.context, operation.terminalOutcome);
          return;
        }
        const reconciled = await reconcileBatchStartResponse({
          context: operation.context,
          getStatus: api.getBatchRunStatus,
        });
        if (current?.operationId !== operationId || !operation.context.isCurrent()) return;
        if (reconciled.disposition === 'settled') {
          await settle(operationId, operation.context, reconciled.outcome);
          return;
        }
        if (reconciled.disposition === 'outcome-unknown') {
          notifyOutcomeUnknown(operation);
          scheduleStatusMonitor(operationId, error, 2_000);
          return;
        }
        if (reconciled.disposition === 'rejected') {
          if (operation.commandResponsePending) {
            const now = Date.now();
            operation.rejectionObservedAt ??= now;
            if (now - operation.rejectionObservedAt < 2_000) {
              scheduleStatusMonitor(operationId, error, 500);
              return;
            }
          }
          resetProgress();
          close(operationId);
          actionableError(error, operation.operation);
          return;
        }
        if (['starting', 'running', 'uncertain'].includes(reconciled.disposition)) {
          scheduleStatusMonitor(operationId, error, 10_000);
        }
      })();
    }, delayMs);
  }

  const adoption = createBatchRunAdoption({
    query: api.getActiveBatchRun,
    isOccupied: () => current !== null || get(isProcessing),
    setDiscoveryLock: (locked) => {
      if (locked) {
        isProcessing.set(true);
        statusMessage.set(tr('batch.adoptionChecking'));
      } else if (!current) {
        resetProgress();
      }
    },
    activate: (active) => {
      const context = beginBatchEventScope(active.operationId, active.operation, active.total);
      const activityId = `batch-${active.operation}:${active.operationId}`;
      try {
        current = {
          operationId: active.operationId,
          operation: active.operation,
          activityId,
          context,
          responseLossNotified: false,
          outcomeLossNotified: false,
          commandResponsePending: false,
          rejectionObservedAt: null,
          terminalOutcome: null,
          outcomePresented: false,
          refreshComplete: false,
          refreshNoticeId: null,
          acknowledgementNoticeId: null,
        };
        startOperation(activityId);
        segments.bumpLoadGeneration();
        isProcessing.set(true);
        pipelinePhase.set(active.operation === 'transcribe' ? 'transcribing' : 'idle');
        batchProgress.set({ status: 'running', completed: 0, total: active.total, percent: 0 });
        statusMessage.set(tr(progressKey(active.operation), { n: String(active.total) }));
        if (active.status === 'settled') {
          void settle(active.operationId, context, active.outcome);
        } else {
          notifications.info(tr('batch.adoptionRunning', { n: String(active.total) }));
          scheduleStatusMonitor(active.operationId, unexpectedBatchRejection(), 0);
        }
      } catch (error) {
        current = null;
        endOperation(activityId);
        closeBatchEventScope(context);
        throw error;
      }
    },
  });

  async function reconcileRejectedStart(operationId: string, error: unknown) {
    const operation = current;
    if (!operation || operation.operationId !== operationId || !operation.context.isCurrent()) {
      return 'stale' as const;
    }
    const reconciled = await reconcileBatchStartResponse({
      context: operation.context,
      getStatus: api.getBatchRunStatus,
    });
    if (current?.operationId !== operationId || !operation.context.isCurrent()) {
      return 'stale' as const;
    }
    if (reconciled.disposition === 'settled') {
      await settle(operationId, operation.context, reconciled.outcome);
      return 'accepted' as const;
    }
    if (reconciled.disposition === 'outcome-unknown') {
      notifyOutcomeUnknown(operation);
      scheduleStatusMonitor(operationId, error, 2_000);
      return 'uncertain' as const;
    }
    if (reconciled.disposition === 'starting' || reconciled.disposition === 'running') {
      notifyResponseLoss(operation, false);
      scheduleStatusMonitor(operationId, error);
      return 'accepted' as const;
    }
    if (reconciled.disposition === 'uncertain') {
      notifyResponseLoss(operation, true);
      scheduleStatusMonitor(operationId, error);
      return 'uncertain' as const;
    }
    return reconciled.disposition;
  }

  async function start(operation: BatchOperationKind, ids: string[]) {
    if (destroyed || current || adoption.blocksStart() || get(isProcessing)) return;
    const operationId = createBatchOperationId();
    const context = beginBatchEventScope(operationId, operation, ids.length);
    const activityId = `batch-${operation}:${operationId}`;
    current = {
      operationId,
      operation,
      activityId,
      context,
      responseLossNotified: false,
      outcomeLossNotified: false,
      commandResponsePending: true,
      rejectionObservedAt: null,
      terminalOutcome: null,
      outcomePresented: false,
      refreshComplete: false,
      refreshNoticeId: null,
      acknowledgementNoticeId: null,
    };
    startOperation(activityId);
    segments.bumpLoadGeneration();
    isProcessing.set(true);
    pipelinePhase.set(operation === 'transcribe' ? 'transcribing' : 'idle');
    batchProgress.set({ status: 'running', completed: 0, total: ids.length, percent: 0 });
    statusMessage.set(tr(progressKey(operation), { n: String(ids.length) }));
    scheduleStatusMonitor(operationId, unexpectedBatchRejection());

    try {
      const command =
        operation === 'transcribe'
          ? api.batchTranscribe(ids, operationId)
          : api.batchNormalize(ids, operationId);
      await boundedBatchCommandResponse(command, context);
      if (current?.operationId === operationId) current.commandResponsePending = false;
    } catch (error) {
      if (current?.operationId === operationId) current.commandResponsePending = false;
      const disposition = await reconcileRejectedStart(operationId, error);
      if (disposition !== 'rejected') return;
      resetProgress();
      close(operationId);
      actionableError(error, operation);
    }
  }

  return {
    adoptActive: () => (current ? Promise.resolve(true) : adoption.adoptActive()),
    startTranscription: (ids: string[]) => start('transcribe', ids),
    startNormalization: (ids: string[]) => start('normalize', ids),
    settleFromWorker: (payload: BatchWorkerSettled, context: BatchEventContext) =>
      settle(payload.operationId, context),
    destroy: () => {
      if (destroyed) return;
      destroyed = true;
      clearStatusMonitor();
      adoption.destroy();
      if (current) {
        resetProgress();
        close(current.operationId);
      }
    },
  };
}
