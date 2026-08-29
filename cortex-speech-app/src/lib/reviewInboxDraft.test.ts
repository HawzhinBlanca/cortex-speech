import { beforeEach, describe, expect, it, vi } from 'vitest';

const api = vi.hoisted(() => ({
  getReviewDraftV1: vi.fn(),
  saveReviewDraftV1: vi.fn(),
  deleteReviewDraftV1: vi.fn(),
  reviewErrorMessage: vi.fn((_error: unknown, fallback: string) => fallback),
}));

vi.mock('./commands', () => api);

import { createReviewInboxDraftController } from './reviewInboxDraft.svelte';
import type { ReviewInboxDraftController } from './reviewInboxDraft.svelte';
import type { SpeechSegment } from './types';

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

function segment(): SpeechSegment {
  return {
    id: 'inbox-draft-segment',
    audioPath: 'C:\\audio\\draft.wav',
    rawTranscript: 'authoritative baseline',
    normalizedTranscript: null,
    annotatedTranscript: null,
    alignmentJson: null,
    durationMs: 1_000,
    speakerId: null,
    verified: false,
  };
}

async function readyController(): Promise<ReviewInboxDraftController> {
  const current = segment();
  const controller = createReviewInboxDraftController({
    current: () => current,
    currentRevision: () => 7,
    resetSelectionAuthority: vi.fn(),
    setStatus: vi.fn(),
  });
  controller.syncSelection();
  await vi.waitFor(() => expect(api.getReviewDraftV1).toHaveBeenCalledWith(current.id));
  await vi.waitFor(() => expect(controller.state.readyId).toBe(current.id));
  await controller.startEdit();
  return controller;
}

function savedDraft(text: string) {
  return {
    segmentId: 'inbox-draft-segment',
    baseRevision: 7,
    text,
    updatedAt: '2026-08-28T12:00:00.000Z',
  };
}

describe('ReviewInbox draft baseline deletion authority', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    api.getReviewDraftV1.mockResolvedValue(null);
    api.saveReviewDraftV1.mockImplementation(
      async (segmentId: string, baseRevision: number, text: string) => ({
        segmentId,
        baseRevision,
        text,
        updatedAt: '2026-08-28T12:00:00.000Z',
      }),
    );
    api.deleteReviewDraftV1.mockResolvedValue(true);
  });

  it('deletes a durable correction when the owner reverts exactly to the authoritative baseline', async () => {
    const controller = await readyController();

    controller.handleInput('durable correction');
    await controller.flush();
    expect(api.saveReviewDraftV1).toHaveBeenCalledWith(
      'inbox-draft-segment',
      7,
      'durable correction',
    );

    controller.handleInput('authoritative baseline');
    expect(controller.state.pending).toBe(false);
    await controller.flush();

    expect(api.deleteReviewDraftV1).toHaveBeenCalledOnce();
    expect(api.deleteReviewDraftV1).toHaveBeenCalledWith('inbox-draft-segment', 7);
    expect(api.saveReviewDraftV1).toHaveBeenCalledTimes(1);
  });

  it('serializes an in-flight save before the baseline delete so the old edit cannot win', async () => {
    const pendingSave = deferred<ReturnType<typeof savedDraft>>();
    api.saveReviewDraftV1.mockReturnValueOnce(pendingSave.promise);
    const controller = await readyController();

    controller.handleInput('edit still being saved');
    const saveFlush = controller.flush();
    await vi.waitFor(() => expect(api.saveReviewDraftV1).toHaveBeenCalledOnce());

    controller.handleInput('authoritative baseline');
    const revertFlush = controller.flush();
    expect(api.deleteReviewDraftV1).not.toHaveBeenCalled();

    pendingSave.resolve(savedDraft('edit still being saved'));
    await Promise.all([saveFlush, revertFlush]);

    expect(api.saveReviewDraftV1).toHaveBeenCalledTimes(1);
    expect(api.deleteReviewDraftV1).toHaveBeenCalledOnce();
    expect(api.saveReviewDraftV1.mock.invocationCallOrder[0]).toBeLessThan(
      api.deleteReviewDraftV1.mock.invocationCallOrder[0],
    );
    expect(api.deleteReviewDraftV1).toHaveBeenCalledWith('inbox-draft-segment', 7);
  });

  it('retains an exact timed-out delete and clears it only after an exact retry succeeds', async () => {
    const timeout = new Error('E_REVIEW_DRAFT_DELETE_TIMEOUT');
    api.deleteReviewDraftV1.mockRejectedValueOnce(timeout).mockResolvedValueOnce(true);
    const controller = await readyController();

    controller.handleInput('durable correction');
    await controller.flush();
    controller.handleInput('authoritative baseline');

    await expect(controller.flush()).rejects.toBe(timeout);
    expect(controller.state.saveFailed).toBe(true);
    await expect(controller.flush()).resolves.toBeUndefined();

    expect(api.deleteReviewDraftV1.mock.calls).toEqual([
      ['inbox-draft-segment', 7],
      ['inbox-draft-segment', 7],
    ]);
    expect(controller.state.saveFailed).toBe(false);
  });

  it('retires a disposed draft read before reverse completion can delete a replacement edit', async () => {
    const staleRead = deferred<ReturnType<typeof savedDraft> | null>();
    api.getReviewDraftV1.mockReturnValueOnce(staleRead.promise).mockResolvedValueOnce(null);
    const current = segment();
    const staleStatus = vi.fn();
    const stale = createReviewInboxDraftController({
      current: () => current,
      currentRevision: () => 7,
      resetSelectionAuthority: vi.fn(),
      setStatus: staleStatus,
    });

    const staleLoad = stale.syncSelection();
    await vi.waitFor(() => expect(api.getReviewDraftV1).toHaveBeenCalledTimes(1));
    stale.dispose();
    const stateAtDispose = {
      readyId: stale.state.readyId,
      editText: stale.state.editText,
      editing: stale.state.editing,
      pending: stale.state.pending,
      conflict: stale.state.conflict,
      loadError: stale.state.loadError,
    };

    const replacement = createReviewInboxDraftController({
      current: () => current,
      currentRevision: () => 7,
      resetSelectionAuthority: vi.fn(),
      setStatus: vi.fn(),
    });
    await replacement.syncSelection();
    await replacement.startEdit();
    replacement.handleInput('replacement correction');
    await replacement.flush();
    expect(api.saveReviewDraftV1).toHaveBeenCalledWith(current.id, 7, 'replacement correction');

    // The old read resolves last with a now-redundant snapshot. Before the lifecycle fence it queued
    // a delete after the replacement save and erased that newer same-revision correction.
    staleRead.resolve(savedDraft('authoritative baseline'));
    await staleLoad;

    expect(api.deleteReviewDraftV1).not.toHaveBeenCalled();
    expect(staleStatus).not.toHaveBeenCalled();
    expect({
      readyId: stale.state.readyId,
      editText: stale.state.editText,
      editing: stale.state.editing,
      pending: stale.state.pending,
      conflict: stale.state.conflict,
      loadError: stale.state.loadError,
    }).toEqual(stateAtDispose);
  });
});
