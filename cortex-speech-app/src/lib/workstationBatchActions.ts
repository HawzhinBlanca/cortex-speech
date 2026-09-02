import { get } from 'svelte/store';
import * as api from './commands';
import { parseActionableError } from './errors';
import { t } from './i18n';
import { endOperation, startOperation } from './invoke';
import { historyStore } from './stores/historyStore';
import { notifications } from './stores/notificationStore';
import {
  filterVerified,
  searchQuery,
  selectedSegmentId,
  wordTimestamps,
} from './stores/segmentStore';
import {
  batchProgress,
  isProcessing,
  pipelinePhase,
  showConfirmDialog,
  statusMessage,
} from './stores/uiStore';
import type { createBatchOperationCoordinator } from './batchOperationCoordinator';

type BatchCoordinator = ReturnType<typeof createBatchOperationCoordinator>;

type WorkstationBatchDependencies = {
  requireDesktopRuntime: () => boolean;
  batchCoordinator: BatchCoordinator;
  getBatchStarting: () => boolean;
  setBatchStarting: (starting: boolean) => void;
  getBatchSpeakerId: () => string;
  loadSegments: () => Promise<void>;
  flushAutosave: (ids: string[]) => Promise<boolean>;
};

export function createWorkstationBatchActions({
  requireDesktopRuntime,
  batchCoordinator,
  getBatchStarting,
  setBatchStarting,
  getBatchSpeakerId,
  loadSegments,
  flushAutosave,
}: WorkstationBatchDependencies) {
  async function resolveViewIds(
    transcriptState: 'any' | 'real' | 'missing' = 'any',
    verified: boolean | null = get(filterVerified),
    query: string | null = get(searchQuery).trim() || null,
  ): Promise<string[] | null> {
    try {
      return await api.getSegmentIdsForView({ verified, query, transcriptState });
    } catch (error) {
      notifications.error(get(t)('notifications.loadSegmentsFailed'), { cause: error });
      return null;
    }
  }

  function notifyActionableError(error: unknown, fallback: string): void {
    const parsed = parseActionableError(error, fallback);
    notifications.error(parsed.message, {
      cause: error,
      publicDetail: parsed.detail,
      action: parsed.action,
    });
  }

  async function transcribe(mode: 'empty' | 'selected' | 'filtered'): Promise<void> {
    if (get(isProcessing) || getBatchStarting()) return;
    if (!requireDesktopRuntime()) return;
    setBatchStarting(true);
    const translate = get(t);
    try {
      const selectedId = get(selectedSegmentId);
      const ids =
        mode === 'empty'
          ? await resolveViewIds('missing', null, null)
          : mode === 'selected'
            ? selectedId
              ? [selectedId]
              : []
            : await resolveViewIds();
      if (ids === null) return;
      if (mode === 'selected' && !selectedId) {
        notifications.warning(translate('batchTranscribe.noSelection'));
        return;
      }
      if (ids.length === 0) {
        notifications.info(translate('batchTranscribe.nothingToTranscribe'));
        return;
      }
      await batchCoordinator.startTranscription(ids);
    } catch (error) {
      notifyActionableError(error, translate('batchTranscribe.failed'));
    } finally {
      setBatchStarting(false);
    }
  }

  async function assignSpeaker(): Promise<void> {
    if (get(isProcessing) || getBatchStarting()) return;
    if (!requireDesktopRuntime()) return;
    const translate = get(t);
    const speaker = getBatchSpeakerId().trim();
    if (!speaker) {
      notifications.warning(translate('batchAssignSpeaker.noSpeaker'));
      return;
    }
    const ids = await resolveViewIds();
    if (ids === null || get(isProcessing) || getBatchStarting()) return;
    if (ids.length === 0) {
      notifications.info(translate('batchAssignSpeaker.nothingToAssign'));
      return;
    }
    startOperation('batch-assign-speaker');
    isProcessing.set(true);
    batchProgress.set({ status: 'running', completed: 0, total: ids.length, percent: 0 });
    statusMessage.set(translate('batchAssignSpeaker.progress', { n: String(ids.length) }));
    try {
      const result = await api.assignSpeakersV1({ ids, targetSpeakerId: speaker });
      notifications.success(
        translate('events.speakerAssigned', { n: String(result.changedCount) }),
      );
      await historyStore.refresh();
      await loadSegments();
    } catch (error) {
      notifications.error(translate('batchAssignSpeaker.failed'), { cause: error });
    } finally {
      isProcessing.set(false);
      batchProgress.set({ status: 'idle', completed: 0, total: 0, percent: 0 });
      statusMessage.set(translate('ready'));
      endOperation('batch-assign-speaker');
    }
  }

  async function normalize(): Promise<void> {
    if (get(isProcessing) || getBatchStarting()) return;
    if (!requireDesktopRuntime()) return;
    setBatchStarting(true);
    const translate = get(t);
    try {
      const ids = await resolveViewIds('real');
      if (ids === null) return;
      if (ids.length === 0) {
        notifications.info(translate('batchNormalize.nothingToNormalize'));
        return;
      }
      await batchCoordinator.startNormalization(ids);
    } catch (error) {
      notifications.error(translate('batchNormalize.failed'), { cause: error });
    } finally {
      setBatchStarting(false);
    }
  }

  async function rediarize(mode: 'selected' | 'filtered'): Promise<void> {
    if (get(isProcessing) || getBatchStarting()) return;
    if (!requireDesktopRuntime()) return;
    const selectedId = get(selectedSegmentId);
    const ids = mode === 'selected' ? (selectedId ? [selectedId] : []) : await resolveViewIds();
    if (ids === null || get(isProcessing) || getBatchStarting()) return;
    const translate = get(t);
    if (mode === 'selected' && !selectedId) {
      notifications.warning(translate('rediarize.noSelection'));
      return;
    }
    if (ids.length === 0) {
      notifications.info(translate('rediarize.nothingToRediarize'));
      return;
    }
    startOperation('rediarize');
    isProcessing.set(true);
    statusMessage.set(translate('rediarize.progress', { n: String(ids.length) }));
    try {
      const updated = await api.rediarizeSegments(ids);
      await loadSegments();
      notifications.success(translate('rediarize.success', { n: String(updated) }));
    } catch (error) {
      notifications.error(translate('rediarize.failed'), { cause: error });
    } finally {
      isProcessing.set(false);
      pipelinePhase.set('idle');
      statusMessage.set(translate('ready'));
      endOperation('rediarize');
    }
  }

  async function deleteFilteredWithConfirm(): Promise<void> {
    if (get(isProcessing) || !requireDesktopRuntime()) return;
    const ids = await resolveViewIds();
    if (ids === null) return;
    const translate = get(t);
    if (ids.length === 0) {
      notifications.info(translate('batchDelete.nothingToDelete'));
      return;
    }
    showConfirmDialog.set({
      title: translate('batchDelete.confirmTitle'),
      message: translate('batchDelete.confirmMessage', { n: String(ids.length) }),
      onConfirm: () => deleteFiltered(ids),
    });
  }

  async function deleteFiltered(ids: string[]): Promise<void> {
    if (get(isProcessing) || !requireDesktopRuntime()) return;
    if (!(await flushAutosave(ids))) return;
    const translate = get(t);
    startOperation('batch-delete');
    isProcessing.set(true);
    statusMessage.set(translate('batchDelete.progress', { n: String(ids.length) }));
    try {
      await api.deleteSegmentsBatch(ids);
      const selectedId = get(selectedSegmentId);
      if (selectedId && ids.includes(selectedId)) {
        selectedSegmentId.set(null);
        wordTimestamps.set([]);
      }
      await loadSegments();
      notifications.success(translate('batchDelete.success', { n: String(ids.length) }));
    } catch (error) {
      notifications.error(translate('batchDelete.failed'), { cause: error });
    } finally {
      isProcessing.set(false);
      statusMessage.set(translate('ready'));
      endOperation('batch-delete');
    }
  }

  return { assignSpeaker, deleteFilteredWithConfirm, normalize, rediarize, transcribe };
}
