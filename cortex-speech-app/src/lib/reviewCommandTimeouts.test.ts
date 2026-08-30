import { invoke } from '@tauri-apps/api/core';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  deleteReviewDraftV1,
  getReviewDraftV1,
  recordPlaybackReceipt,
  saveReviewDraftV1,
} from './commands';
import { REVIEW_OPERATION_TIMEOUT_MS } from './reviewOperationTimeout';

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
const DRAFT_OPERATION_ID = '10000000-0000-4000-8000-000000000017';

const singleInvokeTimeoutCases = [
  {
    label: 'draft load',
    code: 'E_REVIEW_DRAFT_LOAD_TIMEOUT',
    call: () => getReviewDraftV1('draft-segment'),
    expectedInvoke: ['get_review_draft_v1', { segmentId: 'draft-segment' }],
    lateValue: null,
  },
  {
    label: 'playback finalization',
    code: 'E_PLAYBACK_FINALIZATION_TIMEOUT',
    call: () =>
      recordPlaybackReceipt({
        playbackReceiptId: 'receipt-original',
        mediaGrantId: 'grant-original',
        intervals: [{ startMs: 0, endMs: 900 }],
      }),
    expectedInvoke: [
      'finalize_desktop_playback_session_v1',
      {
        playbackReceiptId: 'receipt-original',
        mediaGrantId: 'grant-original',
        intervals: [{ startMs: 0, endMs: 900 }],
      },
    ],
    lateValue: {
      playbackReceiptId: 'receipt-original',
      segmentId: 'playback-segment',
      segmentRevision: 17,
      uniquePlayedMs: 900,
      clipDurationMs: 1_000,
      coverageRatio: 0.9,
    },
  },
] as const;

const draftMutationCases = [
  {
    label: 'draft save',
    code: 'E_REVIEW_DRAFT_SAVE_TIMEOUT',
    call: () => saveReviewDraftV1('draft-segment', 17, 'exact owner draft'),
    mutationInvoke: [
      'save_review_draft_v1',
      {
        segmentId: 'draft-segment',
        baseRevision: 17,
        text: 'exact owner draft',
        operationId: DRAFT_OPERATION_ID,
      },
    ],
    lateValue: {
      segmentId: 'draft-segment',
      baseRevision: 17,
      text: 'exact owner draft',
      updatedAt: '2026-08-28T12:00:00.000Z',
    },
  },
  {
    label: 'draft delete',
    code: 'E_REVIEW_DRAFT_DELETE_TIMEOUT',
    call: () => deleteReviewDraftV1('draft-segment', 17),
    mutationInvoke: [
      'delete_review_draft_v1',
      { segmentId: 'draft-segment', baseRevision: 17, operationId: DRAFT_OPERATION_ID },
    ],
    lateValue: true,
  },
] as const;

describe('owner review command deadlines', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    invokeMock.mockReset();
  });

  afterEach(() => {
    vi.restoreAllMocks();
    vi.useRealTimers();
  });

  for (const timeoutCase of singleInvokeTimeoutCases) {
    it.each(['resolve', 'reject'] as const)(
      `${timeoutCase.label} rejects with ${timeoutCase.code} and absorbs a late source %s`,
      async (lateOutcome) => {
        const nativeCall = deferred<unknown>();
        invokeMock.mockReturnValueOnce(nativeCall.promise);
        const resolved = vi.fn();
        const rejected = vi.fn();

        void timeoutCase.call().then(resolved, rejected);
        expect(invokeMock).toHaveBeenCalledOnce();
        expect(invokeMock.mock.calls[0]).toEqual(timeoutCase.expectedInvoke);

        await vi.advanceTimersByTimeAsync(REVIEW_OPERATION_TIMEOUT_MS);

        expect(resolved).not.toHaveBeenCalled();
        expect(rejected).toHaveBeenCalledOnce();
        expect(rejected.mock.calls[0]?.[0]).toMatchObject({
          name: 'Error',
          message: timeoutCase.code,
        });
        expect(vi.getTimerCount()).toBe(0);

        if (lateOutcome === 'resolve') nativeCall.resolve(timeoutCase.lateValue);
        else nativeCall.reject(new Error(`late ${timeoutCase.label} failure`));
        await Promise.resolve();
        await Promise.resolve();
        await Promise.resolve();

        expect(resolved).not.toHaveBeenCalled();
        expect(rejected).toHaveBeenCalledOnce();
        expect(rejected.mock.calls[0]?.[0]).toMatchObject({ message: timeoutCase.code });
      },
    );
  }

  for (const draftCase of draftMutationCases) {
    it.each(['resolve', 'reject'] as const)(
      `${draftCase.label} reserve timeout absorbs a late source %s and dispatches no mutation`,
      async (lateOutcome) => {
        vi.spyOn(globalThis.crypto, 'randomUUID').mockReturnValue(DRAFT_OPERATION_ID);
        const nativeReservation = deferred<unknown>();
        invokeMock.mockReturnValueOnce(nativeReservation.promise);
        const resolved = vi.fn();
        const rejected = vi.fn();

        void draftCase.call().then(resolved, rejected);
        expect(invokeMock.mock.calls).toEqual([
          [
            'reserve_review_draft_write_v1',
            { segmentId: 'draft-segment', operationId: DRAFT_OPERATION_ID },
          ],
        ]);

        await vi.advanceTimersByTimeAsync(REVIEW_OPERATION_TIMEOUT_MS);

        expect(resolved).not.toHaveBeenCalled();
        expect(rejected).toHaveBeenCalledOnce();
        expect(rejected.mock.calls[0]?.[0]).toMatchObject({
          name: 'Error',
          message: 'E_REVIEW_DRAFT_RESERVE_TIMEOUT',
        });

        if (lateOutcome === 'resolve') nativeReservation.resolve(null);
        else nativeReservation.reject(new Error(`late ${draftCase.label} reservation failure`));
        await Promise.resolve();
        await Promise.resolve();
        await Promise.resolve();

        expect(invokeMock).toHaveBeenCalledOnce();
        expect(resolved).not.toHaveBeenCalled();
        expect(rejected).toHaveBeenCalledOnce();
      },
    );

    it.each(['resolve', 'reject'] as const)(
      `${draftCase.label} mutation timeout reuses its reservation and absorbs a late source %s`,
      async (lateOutcome) => {
        vi.spyOn(globalThis.crypto, 'randomUUID').mockReturnValue(DRAFT_OPERATION_ID);
        const nativeMutation = deferred<unknown>();
        invokeMock.mockResolvedValueOnce(null).mockReturnValueOnce(nativeMutation.promise);
        const resolved = vi.fn();
        const rejected = vi.fn();

        void draftCase.call().then(resolved, rejected);
        await Promise.resolve();
        await Promise.resolve();
        await Promise.resolve();

        expect(invokeMock.mock.calls).toEqual([
          [
            'reserve_review_draft_write_v1',
            { segmentId: 'draft-segment', operationId: DRAFT_OPERATION_ID },
          ],
          draftCase.mutationInvoke,
        ]);

        await vi.advanceTimersByTimeAsync(REVIEW_OPERATION_TIMEOUT_MS);

        expect(resolved).not.toHaveBeenCalled();
        expect(rejected).toHaveBeenCalledOnce();
        expect(rejected.mock.calls[0]?.[0]).toMatchObject({
          name: 'Error',
          message: draftCase.code,
        });

        if (lateOutcome === 'resolve') nativeMutation.resolve(draftCase.lateValue);
        else nativeMutation.reject(new Error(`late ${draftCase.label} mutation failure`));
        await Promise.resolve();
        await Promise.resolve();
        await Promise.resolve();

        expect(invokeMock).toHaveBeenCalledTimes(2);
        expect(resolved).not.toHaveBeenCalled();
        expect(rejected).toHaveBeenCalledOnce();
        expect(rejected.mock.calls[0]?.[0]).toMatchObject({ message: draftCase.code });
      },
    );
  }
});
