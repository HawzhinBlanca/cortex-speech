import { invoke } from '@tauri-apps/api/core';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { historyActionTranslationKey } from './historyAction';
import { historyStore } from './stores/historyStore';
import type { HistoryMutationResultV1 } from './commands';

const invokeMock = vi.mocked(invoke);

describe('typed history command boundary', () => {
  beforeEach(() => {
    invokeMock.mockReset();
    window.__TAURI_INTERNALS__ = {};
  });

  afterEach(() => {
    delete window.__TAURI_INTERNALS__;
  });

  it('maps every generated action identity to locale-owned copy', () => {
    expect(historyActionTranslationKey('updateSegment')).toBe('history.action.updateSegment');
    expect(historyActionTranslationKey('deleteSegments')).toBe('history.action.deleteSegments');
    expect(historyActionTranslationKey('batchTranscribe')).toBe('history.action.batchTranscribe');
    expect(historyActionTranslationKey('batchNormalize')).toBe('history.action.batchNormalize');
    expect(historyActionTranslationKey('speakerAssignment')).toBe(
      'history.action.speakerAssignment',
    );
    expect(historyActionTranslationKey('futureAction' as never)).toBe('history.action.unknown');
  });

  it('single-flights repeated Undo while the durable result is pending', async () => {
    let resolveUndo!: (result: HistoryMutationResultV1) => void;
    invokeMock.mockImplementationOnce(
      () =>
        new Promise<HistoryMutationResultV1>((resolve) => {
          resolveUndo = resolve;
        }),
    );

    const first = historyStore.undo();
    const repeated = historyStore.undo();
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith('undo');

    const result: HistoryMutationResultV1 = {
      action: 'deleteSegments',
      status: { undoAction: null, redoAction: 'deleteSegments' },
    };
    resolveUndo(result);
    await expect(first).resolves.toEqual(result);
    await expect(repeated).resolves.toEqual(result);
  });
});
