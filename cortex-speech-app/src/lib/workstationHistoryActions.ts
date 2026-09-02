import { get } from 'svelte/store';
import { historyMutationMessage, type HistoryRecorder } from './historyAction';
import { t } from './i18n';
import { historyStore } from './stores/historyStore';
import { notifications } from './stores/notificationStore';
import { showReviewInbox } from './stores/uiStore';

type WorkstationHistoryDependencies = {
  requireDesktopRuntime: () => boolean;
  getViewMode: () => 'curate' | 'insights' | 'review';
  getHistoryPanel: () => HistoryRecorder | null;
  loadSegments: () => Promise<void>;
};

export function createWorkstationHistoryActions({
  requireDesktopRuntime,
  getViewMode,
  getHistoryPanel,
  loadSegments,
}: WorkstationHistoryDependencies) {
  async function undo(): Promise<void> {
    if (!requireDesktopRuntime() || get(historyStore).processing) return;
    const translate = get(t);
    if (getViewMode() === 'review' || get(showReviewInbox)) {
      notifications.info(translate('notifications.undoInReview'));
      return;
    }
    try {
      const result = await historyStore.undo();
      const message = historyMutationMessage(translate, result.action, 'undo');
      notifications.info(message);
      if (!result.action) return;
      await loadSegments();
      getHistoryPanel()?.recordAction(message, 'edit');
    } catch (error) {
      notifications.error(translate('notifications.undoFailed'), { cause: error });
    }
  }

  async function redo(): Promise<void> {
    if (!requireDesktopRuntime() || get(historyStore).processing) return;
    if (getViewMode() === 'review' || get(showReviewInbox)) return;
    const translate = get(t);
    try {
      const result = await historyStore.redo();
      const message = historyMutationMessage(translate, result.action, 'redo');
      notifications.info(message);
      if (!result.action) return;
      await loadSegments();
      getHistoryPanel()?.recordAction(message, 'edit');
    } catch (error) {
      notifications.error(translate('notifications.redoFailed'), { cause: error });
    }
  }

  return { redo, undo };
}
