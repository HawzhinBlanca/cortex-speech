import { get } from 'svelte/store';
import * as api from './commands';
import { chooseDirectory } from './fileDialogs';
import { t } from './i18n';
import { endOperation, startOperation } from './invoke';
import { isVerifiedGood } from './segmentQuality';
import { notifications } from './stores/notificationStore';
import { segments } from './stores/segmentStore';
import { batchProgress, isProcessing, statusMessage } from './stores/uiStore';

export async function exportVerifiedAudioFromSettings(
  tauriAvailable: boolean,
  onBusyChange: (busy: boolean) => void,
): Promise<void> {
  const translate = get(t);
  if (!tauriAvailable) {
    notifications.info(translate('desktopRuntimeRequired'));
    return;
  }
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

    onBusyChange(true);
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
      notifications.success(translate('exportAudio.success', { count: String(result.succeeded) }), {
        detail: result.output_dir,
      });
    }
  } catch (error) {
    notifications.error(translate('exportAudio.failed'), { cause: error });
  } finally {
    onBusyChange(false);
    isProcessing.set(false);
    batchProgress.set({ status: 'idle', completed: 0, total: 0, percent: 0 });
    statusMessage.set(translate('ready'));
    endOperation('export-audio');
  }
}
