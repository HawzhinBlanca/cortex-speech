import { get } from 'svelte/store';
import * as api from './commands';
import type { AgentOrchestrationStage } from './commands';
import { chooseDirectory, saveFile } from './fileDialogs';
import { t } from './i18n';
import { endOperation, startOperation } from './invoke';
import { isVerifiedGood } from './segmentQuality';
import { notifications } from './stores/notificationStore';
import { segmentStats, segments } from './stores/segmentStore';
import { settings } from './stores/settingsStore';
import { batchProgress, isProcessing, statusMessage } from './stores/uiStore';

type WorkstationExportDependencies = {
  requireDesktopRuntime: () => boolean;
  getPromotionStage: () => AgentOrchestrationStage | null;
  isTrainingExportBlocked: () => boolean;
  trainingExportBlockDetail: (stage: AgentOrchestrationStage | null) => string | undefined;
};

export function createWorkstationExportActions({
  requireDesktopRuntime,
  getPromotionStage,
  isTrainingExportBlocked,
  trainingExportBlockDetail,
}: WorkstationExportDependencies) {
  async function exportDataset(): Promise<void> {
    if (!requireDesktopRuntime()) return;
    const translate = get(t);
    try {
      const format = get(settings).exportFormat;
      const extension = format === 'parquet' ? 'parquet' : format;
      const path = await saveFile({
        filters: [
          { name: 'JSON', extensions: ['json'] },
          { name: 'JSONL', extensions: ['jsonl'] },
          { name: 'CSV', extensions: ['csv'] },
          { name: 'Parquet', extensions: ['parquet'] },
        ],
        defaultPath: `cortex-dataset.${extension}`,
      });
      if (!path) return;
      const lower = path.toLowerCase();
      const resolvedFormat = lower.endsWith('.parquet')
        ? 'parquet'
        : lower.endsWith('.csv')
          ? 'csv'
          : lower.endsWith('.jsonl')
            ? 'jsonl'
            : 'json';
      await api.exportDataset(path, resolvedFormat);
      notifications.success(translate('exportDataset.success'), { detail: path });
    } catch (error) {
      notifications.error(translate('exportDataset.failed'), { cause: error });
    }
  }

  async function exportTranscript(): Promise<void> {
    if (get(isProcessing) || get(segmentStats).total === 0) return;
    if (!requireDesktopRuntime()) return;
    const translate = get(t);
    try {
      const path = await saveFile({
        filters: [
          { name: 'SubRip subtitles', extensions: ['srt'] },
          { name: 'WebVTT subtitles', extensions: ['vtt'] },
          { name: 'Plain text', extensions: ['txt'] },
        ],
        defaultPath: 'cortex-transcript.srt',
      });
      if (!path) return;
      const lower = path.toLowerCase();
      const format: 'txt' | 'srt' | 'vtt' = lower.endsWith('.vtt')
        ? 'vtt'
        : lower.endsWith('.txt')
          ? 'txt'
          : 'srt';
      await api.exportTranscript(path, format);
      notifications.success(translate('exportTranscript.success'), { detail: path });
    } catch (error) {
      notifications.error(translate('exportTranscript.failed'), { cause: error });
    }
  }

  async function exportHuggingface(): Promise<void> {
    if (get(isProcessing) || get(segmentStats).total === 0) return;
    if (!requireDesktopRuntime()) return;
    const translate = get(t);
    const stage = getPromotionStage();
    if (isTrainingExportBlocked()) {
      notifications.warning(translate('exportHuggingface.blocked'), {
        detail: trainingExportBlockDetail(stage),
      });
      return;
    }
    if (stage?.status === 'needs_review') {
      notifications.warning(translate('exportHuggingface.needsReview'), {
        detail: trainingExportBlockDetail(stage),
      });
    }
    try {
      const directory = await chooseDirectory();
      if (!directory) return;
      await api.exportHuggingfaceDataset(directory);
      notifications.success(translate('exportHuggingface.success'), { detail: directory });
    } catch (error) {
      notifications.error(translate('exportHuggingface.failed'), { cause: error });
    }
  }

  async function exportAudio(): Promise<void> {
    if (!requireDesktopRuntime()) return;
    const translate = get(t);
    const verifiedIds = get(segments)
      .filter((segment) => isVerifiedGood(segment))
      .map((segment) => segment.id);
    if (verifiedIds.length === 0) {
      notifications.warning(translate('exportAudio.noVerified'));
      return;
    }
    try {
      const directory = await chooseDirectory();
      if (!directory) return;
      startOperation('export-audio');
      isProcessing.set(true);
      statusMessage.set(translate('exportAudio.progress'));
      const result = await api.exportAudio(verifiedIds, {
        output_dir: directory,
        format: api.AudioExportFormat.Wav,
        sample_rate: 16000,
        include_metadata: true,
      });
      if (result.failed > 0) {
        notifications.warning(
          translate('exportAudio.partial', {
            succeeded: String(result.succeeded),
            failed: String(result.failed),
          }),
          { detail: result.output_dir },
        );
      } else {
        notifications.success(
          translate('exportAudio.success', { count: String(result.succeeded) }),
          { detail: result.output_dir },
        );
      }
    } catch (error) {
      notifications.error(translate('exportAudio.failed'), { cause: error });
    } finally {
      isProcessing.set(false);
      batchProgress.set({ status: 'idle', completed: 0, total: 0, percent: 0 });
      statusMessage.set(translate('ready'));
      endOperation('export-audio');
    }
  }

  return { exportAudio, exportDataset, exportHuggingface, exportTranscript };
}
