import { get } from 'svelte/store';
import * as api from './commands';
import type { AgenticReadiness } from './commands';
import { publicErrorReference } from './errorText';
import { parseActionableError } from './errors';
import {
  beginImportEventScope,
  closeImportEventScope,
  createImportRunId,
  markImportEventSettled,
  type ImportComplete,
  type ImportEnrichmentComplete,
  type ImportEventContext,
  type ImportWorkerSettled,
} from './events';
import { t, type TranslationKey } from './i18n';
import {
  boundedImportCommandResponse,
  reconcileImportStartResponse,
} from './importStartReconciliation';
import { endOperation, startOperation } from './invoke';
import { notifications } from './stores/notificationStore';
import { segments, selectedSegmentId } from './stores/segmentStore';
import {
  agentPipelineStages,
  filesProcessed,
  isProcessing,
  pipelineCurrentFile,
  pipelinePhase,
  pipelineStatus,
  pipelineTotal,
  statusMessage,
} from './stores/uiStore';

type ImportStartKind = 'file' | 'directory' | 'resume';

type CurrentImportOperation = {
  runId: string;
  operationId: string;
  source: 'file' | 'directory';
  kind: ImportStartKind;
  context: ImportEventContext;
  responseLossNotified: boolean;
  commandResponsePending: boolean;
  rejectionObservedAt: number | null;
};

export type ImportOperationCoordinatorOptions = {
  requireDesktopRuntime: () => boolean;
  canStartImport: () => boolean;
  setStarting: (starting: boolean) => void;
  loadSegments: (isCurrent?: () => boolean) => Promise<void>;
  loadLatestAgentHistory: (expectedRunId?: string, context?: ImportEventContext) => Promise<void>;
  reconcileRecovery: () => Promise<void>;
  clearAgentEvidence: () => void;
};

function tr(key: TranslationKey, params?: Record<string, string>): string {
  return get(t)(key, params);
}

function unexpectedImportRejection() {
  return {
    schema: 1,
    code: 'IMPORT_RUN_REJECTED',
    message: 'The accepted import run was later rejected.',
    retryable: true,
    suggestedAction: 'retry' as const,
  };
}

/**
 * Exact-run import orchestration for the owner workstation. The coordinator owns transport-loss
 * reconciliation, terminal refresh, picker liveness and operation cleanup; the Svelte workspace
 * supplies only local view callbacks. No human or transcript truth is held here.
 */
export function createImportOperationCoordinator(options: ImportOperationCoordinatorOptions) {
  let current: CurrentImportOperation | null = null;
  let statusMonitor: ReturnType<typeof setTimeout> | null = null;
  let ambiguousStart: { runId: string; error: unknown } | null = null;
  let completionEventObservedRunId: string | null = null;
  let settlementRunId: string | null = null;
  let starting = false;

  function setStarting(value: boolean) {
    starting = value;
    options.setStarting(value);
  }

  function clearStatusMonitor() {
    if (statusMonitor) clearTimeout(statusMonitor);
    statusMonitor = null;
  }

  function resetProgress(message: string) {
    isProcessing.set(false);
    pipelinePhase.set('idle');
    pipelineCurrentFile.set('');
    pipelineStatus.set('');
    pipelineTotal.set(0);
    filesProcessed.set(0);
    agentPipelineStages.set([]);
    statusMessage.set(message);
  }

  function end(runId: string, preserveEventScope = false) {
    const operation = current;
    if (operation?.runId !== runId) return;
    clearStatusMonitor();
    endOperation(operation.operationId);
    if (!preserveEventScope) closeImportEventScope(operation.context);
    current = null;
    if (ambiguousStart?.runId === runId) ambiguousStart = null;
    if (completionEventObservedRunId === runId) completionEventObservedRunId = null;
  }

  function begin(kind: ImportStartKind) {
    const runId = createImportRunId();
    if (current) end(current.runId);
    clearStatusMonitor();
    ambiguousStart = null;
    completionEventObservedRunId = null;
    settlementRunId = null;
    const source = kind === 'file' ? 'file' : 'directory';
    const operationId = `${source === 'file' ? 'open-file' : 'import'}:${runId}`;
    const context = beginImportEventScope(runId, source);
    current = {
      runId,
      operationId,
      source,
      kind,
      context,
      responseLossNotified: false,
      commandResponsePending: true,
      rejectionObservedAt: null,
    };
    startOperation(operationId);
    segments.bumpLoadGeneration();
    options.clearAgentEvidence();
    return { runId, context };
  }

  function markResponseFinished(runId: string) {
    if (current?.runId === runId) current.commandResponsePending = false;
  }

  function failureLabel(operation: CurrentImportOperation): string {
    if (operation.kind === 'file') return tr('openFile.failed');
    if (operation.kind === 'resume') return tr('import.resumeFailed');
    return tr('importFailed');
  }

  function actionableError(error: unknown, fallback: string) {
    const parsed = parseActionableError(error, fallback);
    notifications.error(parsed.message, {
      cause: error,
      publicDetail: parsed.detail,
      action: parsed.action,
    });
  }

  function resetRejectedStart(runId: string) {
    if (current?.runId !== runId) return;
    resetProgress(tr('ready'));
    end(runId);
  }

  function notifyResponseLoss(operation: CurrentImportOperation, uncertain: boolean) {
    if (operation.responseLossNotified) return;
    operation.responseLossNotified = true;
    statusMessage.set(tr('import.responseLostStatus'));
    notifications.warning(tr('import.responseLost'), {
      publicDetail: tr(
        uncertain ? 'import.responseLostUncertainDetail' : 'import.responseLostRunningDetail',
      ),
    });
  }

  async function settle(runId: string, source: 'file' | 'directory', context: ImportEventContext) {
    const operation = current;
    if (
      !context.isCurrent() ||
      operation?.runId !== runId ||
      operation.source !== source ||
      settlementRunId === runId
    ) {
      return;
    }
    // Status reconciliation is as authoritative as the physical event. Seal the primary lane here
    // as well so a delayed same-run progress event cannot revive a settled operation.
    if (!context.hasTerminalEvent() && !markImportEventSettled(context)) return;
    const completionEventObserved = completionEventObservedRunId === runId;
    settlementRunId = runId;
    // Start journal reconciliation synchronously before reopening controls. UI cleanup does not
    // await it, so a lost read response cannot keep a settled backend operation permanently busy.
    const recovery = source === 'directory' ? options.reconcileRecovery() : Promise.resolve();
    resetProgress(tr('ready'));
    end(runId, true);
    try {
      // Event arrival is not refresh success. Always start a newer, generation-guarded read after
      // worker settlement; this supersedes a hung/failed completion callback without blocking UI.
      await options.loadSegments(context.isCurrent);
      if (!context.isCurrent()) return;
      if (source === 'directory') await options.loadLatestAgentHistory(runId, context);
      if (!context.isCurrent()) return;
      if (!completionEventObserved) notifications.info(tr('import.settledRecovered'));
      await recovery;
    } catch (error) {
      if (context.isCurrent()) {
        notifications.error(tr('notify.refreshFailedImport'), { cause: error });
      }
    } finally {
      if (settlementRunId === runId) settlementRunId = null;
    }
  }

  function scheduleStatusMonitor(runId: string, error: unknown, delayMs = 5_000) {
    if (current?.runId !== runId) return;
    clearStatusMonitor();
    statusMonitor = setTimeout(() => {
      statusMonitor = null;
      void (async () => {
        const operation = current;
        if (!operation || operation.runId !== runId || !operation.context.isCurrent()) return;
        const disposition = await reconcileImportStartResponse({
          context: operation.context,
          getStatus: api.getImportRunStatus,
        });
        if (current?.runId !== runId || !operation.context.isCurrent()) return;
        if (disposition === 'settled') {
          await settle(runId, operation.source, operation.context);
          return;
        }
        if (disposition === 'rejected') {
          if (operation.commandResponsePending) {
            const now = Date.now();
            operation.rejectionObservedAt ??= now;
            if (now - operation.rejectionObservedAt < 2_000) {
              scheduleStatusMonitor(runId, error, 500);
              return;
            }
          }
          resetRejectedStart(runId);
          actionableError(error, failureLabel(operation));
          if (operation.kind !== 'file') await options.reconcileRecovery();
          return;
        }
        if (disposition === 'running' || disposition === 'uncertain') {
          scheduleStatusMonitor(runId, error, 10_000);
        }
      })();
    }, delayMs);
  }

  async function reconcileRejectedStart(
    runId: string,
    error: unknown,
  ): Promise<'accepted' | 'uncertain' | 'rejected' | 'stale'> {
    const operation = current;
    if (!operation || operation.runId !== runId || !operation.context.isCurrent()) return 'stale';
    const disposition = await reconcileImportStartResponse({
      context: operation.context,
      getStatus: api.getImportRunStatus,
    });
    if (current?.runId !== runId || !operation.context.isCurrent()) return 'stale';
    if (disposition === 'settled') {
      await settle(runId, operation.source, operation.context);
      return 'accepted';
    }
    if (disposition === 'running') {
      notifyResponseLoss(operation, false);
      scheduleStatusMonitor(runId, error);
      return 'accepted';
    }
    if (disposition === 'uncertain') {
      ambiguousStart = { runId, error };
      notifyResponseLoss(operation, true);
      scheduleStatusMonitor(runId, error);
      return 'uncertain';
    }
    return disposition === 'stale' ? 'stale' : 'rejected';
  }

  function beginProgress(kind: ImportStartKind) {
    const operation = begin(kind);
    isProcessing.set(true);
    pipelinePhase.set('importing');
    filesProcessed.set(0);
    pipelineTotal.set(0);
    pipelineCurrentFile.set('');
    pipelineStatus.set('');
    statusMessage.set(tr('pipeline.importing'));
    scheduleStatusMonitor(operation.runId, unexpectedImportRejection());
    return operation;
  }

  function readinessDetail(readiness: AgenticReadiness): string {
    const blockers = readiness.checks.filter((check) =>
      ['blocked', 'failed', 'degraded', 'unknown'].includes(check.status),
    ).length;
    return tr('agentReport.blockerCount', { count: String(blockers) });
  }

  async function warnReadiness() {
    try {
      const readiness = await boundedImportCommandResponse(api.checkAgenticReadiness(), 15_000);
      if (readiness.status === 'blocked') {
        notifications.warning(tr('agenticReadiness.blocked'), {
          publicDetail: readinessDetail(readiness),
        });
      } else if (readiness.status === 'degraded') {
        notifications.info(tr('agenticReadiness.degraded'), {
          detail: readinessDetail(readiness),
        });
      }
    } catch (error) {
      notifications.warning(tr('agenticReadiness.checkFailed'), { cause: error });
    }
  }

  async function openFile() {
    if (get(isProcessing) || starting) return;
    if (!options.requireDesktopRuntime() || !options.canStartImport()) return;
    setStarting(true);
    let runId: string | null = null;
    try {
      isProcessing.set(true);
      pipelinePhase.set('importing');
      filesProcessed.set(0);
      pipelineTotal.set(0);
      pipelineCurrentFile.set('');
      pipelineStatus.set('');
      statusMessage.set(tr('openFile.choosing'));
      const path = await api.openAudioFile();
      if (!path) {
        resetProgress(tr('ready'));
        return;
      }
      // The native picker is the only cancellable work before a run exists. Once it returns,
      // stop advertising a Cancel action while the advisory readiness probe is in flight.
      resetProgress(tr('ready'));
      await warnReadiness();
      if (get(isProcessing)) {
        notifications.warning(tr('import.startAbortedBusy'));
        return;
      }
      ({ runId } = beginProgress('file'));
      setStarting(false);
      await boundedImportCommandResponse(api.importAudioFile(path, runId));
      markResponseFinished(runId);
    } catch (error) {
      if (runId) markResponseFinished(runId);
      if (runId) {
        const disposition = await reconcileRejectedStart(runId, error);
        if (disposition !== 'rejected') return;
      }
      const cancelled = !runId && publicErrorReference(error).code === 'E_FILE_PICKER_CANCELLED';
      if (!cancelled) actionableError(error, tr('openFile.failed'));
      resetProgress(tr('ready'));
      if (runId) end(runId);
    } finally {
      setStarting(false);
    }
  }

  async function importDirectory() {
    if (get(isProcessing) || starting) return;
    if (!options.requireDesktopRuntime() || !options.canStartImport()) return;
    setStarting(true);
    let runId: string | null = null;
    try {
      await warnReadiness();
      if (get(isProcessing)) {
        notifications.warning(tr('import.startAbortedBusy'));
        return;
      }
      ({ runId } = beginProgress('directory'));
      setStarting(false);
      await api.importDirectory(runId);
      markResponseFinished(runId);
    } catch (error) {
      if (runId) markResponseFinished(runId);
      if (runId) {
        const disposition = await reconcileRejectedStart(runId, error);
        if (disposition !== 'rejected') return;
      }
      const cancelled = publicErrorReference(error).code === 'E_DIRECTORY_PICKER_CANCELLED';
      if (cancelled) await options.loadLatestAgentHistory();
      else actionableError(error, tr('importFailed'));
      resetProgress(cancelled ? tr('ready') : tr('importFailed'));
      if (runId) end(runId);
    } finally {
      setStarting(false);
    }
  }

  async function resume(jobId: string): Promise<unknown> {
    const { runId } = beginProgress('resume');
    try {
      const response = await boundedImportCommandResponse(
        api.resumeInterruptedImport(jobId, runId),
      );
      markResponseFinished(runId);
      return response;
    } catch (error) {
      markResponseFinished(runId);
      const disposition = await reconcileRejectedStart(runId, error);
      if (disposition === 'accepted' || disposition === 'stale') return;
      if (disposition === 'rejected') resetRejectedStart(runId);
      throw error;
    }
  }

  async function handleComplete(payload: ImportComplete, context: ImportEventContext) {
    completionEventObservedRunId = payload.runId;
    try {
      await options.loadSegments(context.isCurrent);
      if (!context.isCurrent()) return;
      if (payload.source !== 'file') {
        await options.loadLatestAgentHistory(payload.runId, context);
        if (!context.isCurrent()) return;
      }
      if (payload.segmentIds?.length) selectedSegmentId.set(payload.segmentIds[0]);
      statusMessage.set(tr('ready'));
      if (payload.source === 'file') {
        if (payload.failed > 0) notifications.error(tr('openFile.failed'));
        else if (payload.segmentCount && payload.segmentCount > 1) {
          notifications.success(tr('openFile.multiChunk', { count: String(payload.segmentCount) }));
        } else if (payload.succeeded > 0) notifications.success(tr('openFile.imported'));
      } else {
        await options.reconcileRecovery();
        if (!context.isCurrent()) return;
        statusMessage.set(tr('importComplete'));
      }
    } catch (error) {
      if (context.isCurrent()) {
        console.error('Import complete handler error:', error);
        notifications.error(tr('notify.refreshFailedImport'), { cause: error });
      }
    }
  }

  async function handleEnrichment(payload: ImportEnrichmentComplete, context: ImportEventContext) {
    await options.loadSegments(context.isCurrent);
    if (!context.isCurrent()) return;
    await options.loadLatestAgentHistory(payload.runId, context);
  }

  return {
    openFile,
    importDirectory,
    resume,
    handleComplete,
    handleEnrichment,
    settleFromWorker: (payload: ImportWorkerSettled, context: ImportEventContext) =>
      settle(payload.runId, payload.source, context),
    shouldSuppressResumeFailure: () =>
      ambiguousStart !== null && current !== null && ambiguousStart.runId === current.runId,
    destroy: () => {
      clearStatusMonitor();
      if (current) end(current.runId);
    },
  };
}
