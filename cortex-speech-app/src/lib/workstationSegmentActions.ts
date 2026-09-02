import { get } from 'svelte/store';
import * as api from './commands';
import { effectiveTranscript } from './segmentQuality';
import { t } from './i18n';
import { endOperation, startOperation } from './invoke';
import { notifications } from './stores/notificationStore';
import {
  segments,
  selectedSegment,
  selectedSegmentId,
  wordTimestamps,
} from './stores/segmentStore';
import { isProcessing, pipelinePhase, showConfirmDialog, statusMessage } from './stores/uiStore';
import { historyStore } from './stores/historyStore';
import { segmentSourceFilename, truncateFilename } from './alignment';
import type { HistoryRecorder } from './historyAction';

type SegmentActionDependencies = {
  requireDesktopRuntime: () => boolean;
  loadSegments: () => Promise<void>;
  notifyActionableError: (error: unknown, fallback: string) => void;
  pendingAutosaveId: () => string | null;
  flushAutosave: () => Promise<void>;
  flushAutosaveIds: (ids: string[]) => Promise<boolean>;
  saveMetadata: (segmentId: string, fields: api.SegmentMetadataFields) => Promise<unknown>;
  getHistoryPanel: () => HistoryRecorder | null;
};

export function createWorkstationSegmentActions({
  requireDesktopRuntime,
  loadSegments,
  notifyActionableError,
  pendingAutosaveId,
  flushAutosave,
  flushAutosaveIds,
  saveMetadata,
  getHistoryPanel,
}: SegmentActionDependencies) {
  function promptChampionRetry(retryChampion: () => void): void {
    const translate = get(t);
    showConfirmDialog.set({
      title: translate('asr.championUnavailableTitle'),
      message: translate('asr.championUnavailableMessage'),
      confirmLabel: translate('asr.tryAgain'),
      danger: false,
      onConfirm: retryChampion,
    });
  }

  async function transcribe(): Promise<void> {
    const segment = get(selectedSegment);
    if (!segment || get(isProcessing) || !requireDesktopRuntime()) return;
    const translate = get(t);
    if (segment.verified || segment.humanDecision) {
      notifications.info(translate('asr.reopenBeforeRetranscribe'));
      return;
    }
    startOperation('transcribe');
    isProcessing.set(true);
    pipelinePhase.set('transcribing');
    statusMessage.set(translate('transcribing'));
    try {
      await api.transcribeSegment(segment.audioPath, segment.alignmentJson, segment.id);
      await loadSegments();
      notifications.success(translate('notifications.transcriptionComplete'));
    } catch (error) {
      if (api.is7bUnavailableError(error)) promptChampionRetry(transcribe);
      else notifyActionableError(error, translate('errors.transcriptionFailed'));
    } finally {
      isProcessing.set(false);
      pipelinePhase.set('idle');
      statusMessage.set(translate('ready'));
      endOperation('transcribe');
    }
  }

  async function saveSpeaker(): Promise<void> {
    const segment = get(selectedSegment);
    if (!segment || !requireDesktopRuntime()) return;
    const hadPendingSave = pendingAutosaveId() === segment.id;
    try {
      await flushAutosave();
      if (!hadPendingSave) {
        await saveMetadata(segment.id, { speakerId: segment.speakerId });
      }
      notifications.success(get(t)('speaker.saved'));
    } catch (error) {
      if (!hadPendingSave) {
        notifications.error(get(t)('notifications.saveFailed'), { cause: error });
      }
    }
  }

  function deleteWithConfirm(): void {
    const segment = get(selectedSegment);
    if (!segment || !requireDesktopRuntime()) return;
    const translate = get(t);
    showConfirmDialog.set({
      title: translate('deleteSegment'),
      message: translate('deleteSegmentConfirm').replace(
        '{name}',
        segment.audioPath.split(/[/\\]/).pop() ?? '',
      ),
      onConfirm: deleteSegment,
    });
  }

  async function deleteSegment(): Promise<void> {
    const segment = get(selectedSegment);
    if (!segment || !requireDesktopRuntime()) return;
    if (!(await flushAutosaveIds([segment.id]))) return;

    const originalSegments = get(segments);
    const segmentName = truncateFilename(segmentSourceFilename(segment.audioPath));
    segments.update((rows) => rows.filter((row) => row.id !== segment.id));
    selectedSegmentId.set(null);
    wordTimestamps.set([]);
    getHistoryPanel()?.recordAction(`Deleted segment: ${segmentName}`, 'delete');

    try {
      await api.deleteSegment(segment.id);
      await historyStore.refresh();
      notifications.info(get(t)('notifications.segmentDeleted'));
    } catch (error) {
      segments.set(originalSegments);
      selectedSegmentId.set(segment.id);
      notifications.error(get(t)('notifications.deleteFailed'), { cause: error });
    }
  }

  async function align(): Promise<void> {
    const segment = get(selectedSegment);
    if (!segment || !requireDesktopRuntime()) return;
    const text = effectiveTranscript(segment);
    if (!text) return;
    const translate = get(t);
    startOperation('align');
    isProcessing.set(true);
    pipelinePhase.set('detecting');
    statusMessage.set(translate('pipeline.detecting'));
    try {
      const timestamps = await api.alignSegment(
        segment.audioPath,
        text,
        segment.alignmentJson,
        segment.id,
      );
      wordTimestamps.set(timestamps);
      await loadSegments();
      notifications.success(translate('notifications.alignmentComplete'));
    } catch (error) {
      notifications.error(translate('notifications.alignmentFailed'), { cause: error });
    } finally {
      isProcessing.set(false);
      pipelinePhase.set('idle');
      statusMessage.set(translate('ready'));
      endOperation('align');
    }
  }

  return { align, deleteWithConfirm, saveSpeaker, transcribe };
}
