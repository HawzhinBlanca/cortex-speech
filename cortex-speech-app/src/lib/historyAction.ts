import type { HistoryActionV1 } from './commands';
import type { Translate, TranslationKey } from './i18n';

export interface HistoryRecorder {
  recordAction(description: string, type: 'edit' | 'verify' | 'delete' | 'import'): void;
}

const actionKeys = {
  updateSegment: 'history.action.updateSegment',
  deleteSegments: 'history.action.deleteSegments',
  batchTranscribe: 'history.action.batchTranscribe',
  batchNormalize: 'history.action.batchNormalize',
  speakerAssignment: 'history.action.speakerAssignment',
} satisfies Record<HistoryActionV1, TranslationKey>;

/** Map server action identity to locale-owned copy. The fallback keeps malformed/forward-version
 * runtime data total without rendering the backend token or throwing inside an error path. */
export function historyActionTranslationKey(action: HistoryActionV1): TranslationKey {
  return (
    (actionKeys as Partial<Record<string, TranslationKey>>)[action] ?? 'history.action.unknown'
  );
}

export function historyMutationMessage(
  translate: Translate,
  action: HistoryActionV1 | null,
  direction: 'undo' | 'redo',
): string {
  if (!action) {
    return translate(direction === 'undo' ? 'history.nothingToUndo' : 'history.nothingToRedo');
  }
  return translate(direction === 'undo' ? 'notifications.undone' : 'notifications.redone', {
    what: translate(historyActionTranslationKey(action)),
  });
}
