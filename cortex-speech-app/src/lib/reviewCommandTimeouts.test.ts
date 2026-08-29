import { invoke } from '@tauri-apps/api/core';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { recordPlaybackReceipt } from './commands';
import { REVIEW_OPERATION_TIMEOUT_MS } from './reviewOperationTimeout';

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

const invokeMock = vi.mocked(invoke);

describe('owner review command deadlines', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    invokeMock.mockReset();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('bounds playback finalization and absorbs a late native completion', async () => {
    const nativeCall = deferred<unknown>();
    invokeMock.mockReturnValueOnce(nativeCall.promise);
    const resolved = vi.fn();
    const rejected = vi.fn();

    void recordPlaybackReceipt({
      playbackReceiptId: 'receipt-original',
      mediaGrantId: 'grant-original',
      intervals: [{ startMs: 0, endMs: 900 }],
    }).then(resolved, rejected);

    expect(invokeMock.mock.calls).toEqual([
      [
        'finalize_desktop_playback_session_v1',
        {
          playbackReceiptId: 'receipt-original',
          mediaGrantId: 'grant-original',
          intervals: [{ startMs: 0, endMs: 900 }],
        },
      ],
    ]);

    await vi.advanceTimersByTimeAsync(REVIEW_OPERATION_TIMEOUT_MS);
    expect(resolved).not.toHaveBeenCalled();
    expect(rejected).toHaveBeenCalledOnce();
    expect(rejected.mock.calls[0]?.[0]).toMatchObject({
      message: 'E_PLAYBACK_FINALIZATION_TIMEOUT',
    });

    nativeCall.resolve({
      playbackReceiptId: 'receipt-original',
      segmentId: 'segment-original',
      segmentRevision: 1,
      uniquePlayedMs: 900,
      clipDurationMs: 1_000,
      coverageRatio: 0.9,
    });
    await Promise.resolve();
    await Promise.resolve();
    expect(resolved).not.toHaveBeenCalled();
    expect(rejected).toHaveBeenCalledOnce();
    expect(invokeMock).toHaveBeenCalledOnce();
  });
});
