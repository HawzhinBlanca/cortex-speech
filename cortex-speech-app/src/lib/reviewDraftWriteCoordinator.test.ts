import { invoke } from '@tauri-apps/api/core';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { deleteReviewDraftV1, saveReviewDraftV1 } from './commands';
import {
  ReviewDraftWriteCoordinator,
  ReviewDraftWriteIdentityError,
  type ReviewDraftWriteCoordinatorOptions,
} from './reviewDraftWriteCoordinator';
import { REVIEW_OPERATION_TIMEOUT_MS } from './reviewOperationTimeout';

function echo(segmentId: string, baseRevision: number, text: string) {
  return { segmentId, baseRevision, text };
}

function coordinator(overrides: Partial<ReviewDraftWriteCoordinatorOptions> = {}) {
  return new ReviewDraftWriteCoordinator({
    save: vi.fn(async (segmentId, baseRevision, text) => echo(segmentId, baseRevision, text)),
    delete: vi.fn(async () => true),
    ...overrides,
  });
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

const invokeMock = vi.mocked(invoke);
const FIRST_DRAFT_OPERATION_ID = '20000000-0000-4000-8000-000000000001';
const SECOND_DRAFT_OPERATION_ID = '20000000-0000-4000-8000-000000000002';

describe('revision-bound review draft write coordinator', () => {
  it('retries an off-screen A failure during close after navigation to B', async () => {
    const save = vi
      .fn<ReviewDraftWriteCoordinatorOptions['save']>()
      .mockRejectedValueOnce(new Error('disk full'))
      .mockImplementation(async (segmentId, baseRevision, text) =>
        echo(segmentId, baseRevision, text),
      );
    const writes = coordinator({ save });

    await expect(
      writes.request({ kind: 'save', segmentId: 'A', baseRevision: 7, text: 'exact A' }),
    ).rejects.toThrow('disk full');
    await writes.request({ kind: 'save', segmentId: 'B', baseRevision: 9, text: 'exact B' });

    expect(writes.hasDesired('A')).toBe(true);
    await writes.flushAll();
    expect(save.mock.calls).toEqual([
      ['A', 7, 'exact A'],
      ['B', 9, 'exact B'],
      ['A', 7, 'exact A'],
    ]);
    expect(writes.hasDesired()).toBe(false);
  });

  it('keeps a fail-once intent dirty and retries the exact request', async () => {
    const save = vi
      .fn<ReviewDraftWriteCoordinatorOptions['save']>()
      .mockRejectedValueOnce(new Error('transport lost'))
      .mockImplementation(async (segmentId, baseRevision, text) =>
        echo(segmentId, baseRevision, text),
      );
    const writes = coordinator({ save });
    const intent = {
      kind: 'save' as const,
      segmentId: 'clip',
      baseRevision: 12,
      text: 'human text',
    };

    await expect(writes.request(intent)).rejects.toThrow('transport lost');
    expect(writes.desiredIntent('clip')).toEqual(intent);
    await writes.flushSegment('clip');

    expect(save).toHaveBeenNthCalledWith(1, 'clip', 12, 'human text');
    expect(save).toHaveBeenNthCalledWith(2, 'clip', 12, 'human text');
    expect(writes.isDurable(intent)).toBe(true);
  });

  it('propagates a repeated retry failure so the close barrier stays blocked', async () => {
    const save = vi.fn<ReviewDraftWriteCoordinatorOptions['save']>(async () => {
      throw new Error('storage remains unavailable');
    });
    const writes = coordinator({ save });
    const intent = { kind: 'save' as const, segmentId: 'clip', baseRevision: 12, text: 'keep me' };

    await expect(writes.request(intent)).rejects.toThrow('storage remains unavailable');
    await expect(writes.flushAll()).rejects.toThrow('storage remains unavailable');

    expect(save).toHaveBeenCalledTimes(2);
    expect(writes.desiredIntent('clip')).toEqual(intent);
  });

  it('does not let a stale success clear a newer draft', async () => {
    let releaseOld!: () => void;
    const oldPending = new Promise<void>((resolve) => (releaseOld = resolve));
    const save = vi.fn<ReviewDraftWriteCoordinatorOptions['save']>(
      async (segmentId, baseRevision, text) => {
        if (text === 'older') await oldPending;
        return echo(segmentId, baseRevision, text);
      },
    );
    const writes = coordinator({ save });

    const oldWrite = writes.request({
      kind: 'save',
      segmentId: 'clip',
      baseRevision: 3,
      text: 'older',
    });
    const newWrite = writes.request({
      kind: 'save',
      segmentId: 'clip',
      baseRevision: 3,
      text: 'newer',
    });
    releaseOld();
    await Promise.all([oldWrite, newWrite]);

    expect(save.mock.calls).toEqual([
      ['clip', 3, 'older'],
      ['clip', 3, 'newer'],
    ]);
    expect(
      writes.isDurable({ kind: 'save', segmentId: 'clip', baseRevision: 3, text: 'newer' }),
    ).toBe(true);
  });

  it.each([
    ['segment', echo('other', 4, 'exact')],
    ['revision', echo('clip', 5, 'exact')],
    ['text', echo('clip', 4, 'wrong')],
  ])('rejects a wrong %s echo and leaves the exact intent dirty', async (_label, response) => {
    const save = vi.fn(async () => response);
    const writes = coordinator({ save });
    const intent = { kind: 'save' as const, segmentId: 'clip', baseRevision: 4, text: 'exact' };

    await expect(writes.request(intent)).rejects.toBeInstanceOf(ReviewDraftWriteIdentityError);
    expect(writes.desiredIntent('clip')).toEqual(intent);
  });
});

describe('review draft coordinator over the bounded native command bridge', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    invokeMock.mockReset();
  });

  afterEach(() => {
    vi.restoreAllMocks();
    vi.useRealTimers();
  });

  it('times out a hung save, keeps the exact intent dirty, and retries behind a fresh fence', async () => {
    const firstNativeCall = deferred<unknown>();
    const onWriteSucceeded = vi.fn();
    const onWriteFailed = vi.fn();
    vi.spyOn(globalThis.crypto, 'randomUUID')
      .mockReturnValueOnce(FIRST_DRAFT_OPERATION_ID)
      .mockReturnValueOnce(SECOND_DRAFT_OPERATION_ID);
    invokeMock
      .mockResolvedValueOnce(null)
      .mockReturnValueOnce(firstNativeCall.promise)
      .mockResolvedValueOnce(null)
      .mockResolvedValueOnce({
        segmentId: 'clip-save',
        baseRevision: 23,
        text: 'exact desired text',
        updatedAt: '2026-08-28T12:00:00.000Z',
      });
    const writes = new ReviewDraftWriteCoordinator({
      save: saveReviewDraftV1,
      delete: deleteReviewDraftV1,
      onWriteSucceeded,
      onWriteFailed,
    });
    const intent = {
      kind: 'save' as const,
      segmentId: 'clip-save',
      baseRevision: 23,
      text: 'exact desired text',
    };

    const firstRequest = writes.request(intent);
    const firstRejection = expect(firstRequest).rejects.toThrow('E_REVIEW_DRAFT_SAVE_TIMEOUT');
    expect(writes.isWriting(intent.segmentId)).toBe(true);

    await vi.advanceTimersByTimeAsync(REVIEW_OPERATION_TIMEOUT_MS);
    await firstRejection;

    expect(writes.isWriting(intent.segmentId)).toBe(false);
    expect(writes.desiredIntent(intent.segmentId)).toEqual(intent);
    expect(onWriteFailed).toHaveBeenCalledOnce();
    expect(onWriteSucceeded).not.toHaveBeenCalled();

    await expect(writes.flushAll()).resolves.toBeUndefined();
    expect(invokeMock.mock.calls).toEqual([
      [
        'reserve_review_draft_write_v1',
        { segmentId: 'clip-save', operationId: FIRST_DRAFT_OPERATION_ID },
      ],
      [
        'save_review_draft_v1',
        {
          segmentId: 'clip-save',
          baseRevision: 23,
          text: 'exact desired text',
          operationId: FIRST_DRAFT_OPERATION_ID,
        },
      ],
      [
        'reserve_review_draft_write_v1',
        { segmentId: 'clip-save', operationId: SECOND_DRAFT_OPERATION_ID },
      ],
      [
        'save_review_draft_v1',
        {
          segmentId: 'clip-save',
          baseRevision: 23,
          text: 'exact desired text',
          operationId: SECOND_DRAFT_OPERATION_ID,
        },
      ],
    ]);
    expect(writes.isDurable(intent)).toBe(true);
    expect(writes.hasDesired()).toBe(false);
    expect(onWriteSucceeded).toHaveBeenCalledOnce();

    firstNativeCall.resolve({
      segmentId: 'wrong-late-segment',
      baseRevision: 999,
      text: 'late stale response',
      updatedAt: '2026-08-28T12:01:00.000Z',
    });
    await Promise.resolve();
    await Promise.resolve();

    expect(writes.isDurable(intent)).toBe(true);
    expect(writes.hasDesired()).toBe(false);
    expect(onWriteSucceeded).toHaveBeenCalledOnce();
    expect(onWriteFailed).toHaveBeenCalledOnce();
  });

  it('times out a hung delete and retries the same revision behind a fresh close fence', async () => {
    const firstNativeCall = deferred<unknown>();
    const onWriteSucceeded = vi.fn();
    vi.spyOn(globalThis.crypto, 'randomUUID')
      .mockReturnValueOnce(FIRST_DRAFT_OPERATION_ID)
      .mockReturnValueOnce(SECOND_DRAFT_OPERATION_ID);
    invokeMock
      .mockResolvedValueOnce(null)
      .mockReturnValueOnce(firstNativeCall.promise)
      .mockResolvedValueOnce(null)
      .mockResolvedValueOnce(true);
    const writes = new ReviewDraftWriteCoordinator({
      save: saveReviewDraftV1,
      delete: deleteReviewDraftV1,
      onWriteSucceeded,
    });
    const intent = {
      kind: 'delete' as const,
      segmentId: 'clip-delete',
      baseRevision: 31,
    };

    const firstRequest = writes.request(intent);
    const firstRejection = expect(firstRequest).rejects.toThrow('E_REVIEW_DRAFT_DELETE_TIMEOUT');
    await vi.advanceTimersByTimeAsync(REVIEW_OPERATION_TIMEOUT_MS);
    await firstRejection;

    expect(writes.isWriting(intent.segmentId)).toBe(false);
    expect(writes.desiredIntent(intent.segmentId)).toEqual(intent);

    await expect(writes.flushAll()).resolves.toBeUndefined();
    expect(invokeMock.mock.calls).toEqual([
      [
        'reserve_review_draft_write_v1',
        { segmentId: 'clip-delete', operationId: FIRST_DRAFT_OPERATION_ID },
      ],
      [
        'delete_review_draft_v1',
        {
          segmentId: 'clip-delete',
          baseRevision: 31,
          operationId: FIRST_DRAFT_OPERATION_ID,
        },
      ],
      [
        'reserve_review_draft_write_v1',
        { segmentId: 'clip-delete', operationId: SECOND_DRAFT_OPERATION_ID },
      ],
      [
        'delete_review_draft_v1',
        {
          segmentId: 'clip-delete',
          baseRevision: 31,
          operationId: SECOND_DRAFT_OPERATION_ID,
        },
      ],
    ]);
    expect(writes.isDurable(intent)).toBe(true);
    expect(onWriteSucceeded).toHaveBeenCalledOnce();

    firstNativeCall.reject(new Error('late stale delete rejection'));
    await Promise.resolve();
    await Promise.resolve();

    expect(writes.isDurable(intent)).toBe(true);
    expect(writes.hasDesired()).toBe(false);
    expect(onWriteSucceeded).toHaveBeenCalledOnce();
  });
});
