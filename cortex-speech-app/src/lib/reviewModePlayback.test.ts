import { invoke } from '@tauri-apps/api/core';
import { render } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { ReviewAudioAuthority } from './reviewModePlayback.svelte';
import type { ReviewModePlaybackController } from './reviewModePlayback.svelte';
import ReviewModePlaybackHarness from './reviewModePlaybackHarness.test.svelte';
import { REVIEW_OPERATION_TIMEOUT_MS } from './reviewOperationTimeout';
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
    id: 'playback-segment',
    audioPath: 'C:\\audio\\playback.wav',
    rawTranscript: 'دەقی تاقیکردنەوە',
    normalizedTranscript: null,
    annotatedTranscript: null,
    alignmentJson: null,
    durationMs: 1_000,
    speakerId: null,
    verified: false,
  };
}

function audioAuthority(
  playbackReceiptId: string,
  mediaGrantId: string,
  baseRevision = 7,
): ReviewAudioAuthority {
  return {
    pauseAndSnapshot: vi.fn(() => ({
      segmentId: 'playback-segment',
      segmentRevision: baseRevision,
      playbackReceiptId,
      mediaGrantId,
      clipDurationMs: 1_000,
      intervals: [{ startMs: 0, endMs: 900 }],
    })),
    restartPlaybackAuthority: vi.fn(),
  };
}

function finalizedReceipt(playbackReceiptId: string) {
  return {
    playbackReceiptId,
    segmentId: 'playback-segment',
    segmentRevision: 7,
    uniquePlayedMs: 900,
    clipDurationMs: 1_000,
    coverageRatio: 0.9,
  };
}

const invokeMock = vi.mocked(invoke);
const teardownViews: Array<() => void> = [];

function controller() {
  let created!: ReviewModePlaybackController;
  const view = render(ReviewModePlaybackHarness, {
    props: { onReady: (value: ReviewModePlaybackController) => (created = value) },
  });
  teardownViews.push(view.unmount);
  return created;
}

describe('review playback finalization replay authority', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    invokeMock.mockReset();
  });

  afterEach(() => {
    while (teardownViews.length > 0) teardownViews.pop()?.();
    vi.useRealTimers();
  });

  it.each(['absent', 'changed'] as const)(
    'replays the original receipt, grant, and intervals after timeout when the player is %s',
    async (playerState) => {
      const firstNativeCall = deferred<unknown>();
      invokeMock
        .mockReturnValueOnce(firstNativeCall.promise)
        .mockResolvedValueOnce(finalizedReceipt('receipt-original'));
      const playback = controller();
      const originalPlayer = audioAuthority('receipt-original', 'grant-original');
      const changedPlayer = audioAuthority('receipt-new', 'grant-new');
      playback.state.player = originalPlayer;

      const firstFinalization = playback.finalize(segment(), 7);
      const firstRejection = expect(firstFinalization).rejects.toThrow(
        'E_PLAYBACK_FINALIZATION_TIMEOUT',
      );
      await Promise.resolve();
      await Promise.resolve();
      expect(invokeMock).toHaveBeenCalledOnce();
      playback.state.player = playerState === 'absent' ? undefined : changedPlayer;

      await vi.advanceTimersByTimeAsync(REVIEW_OPERATION_TIMEOUT_MS);
      await firstRejection;

      await expect(playback.finalize(segment(), 7)).resolves.toBe('receipt-original');
      expect(invokeMock).toHaveBeenCalledTimes(2);
      expect(invokeMock.mock.calls[0]).toEqual([
        'finalize_desktop_playback_session_v1',
        {
          playbackReceiptId: 'receipt-original',
          mediaGrantId: 'grant-original',
          intervals: [{ startMs: 0, endMs: 900 }],
        },
      ]);
      expect(invokeMock.mock.calls[1]).toEqual(invokeMock.mock.calls[0]);
      expect(originalPlayer.pauseAndSnapshot).toHaveBeenCalledOnce();
      expect(changedPlayer.pauseAndSnapshot).not.toHaveBeenCalled();

      firstNativeCall.resolve(finalizedReceipt('wrong-late-receipt'));
      await Promise.resolve();
      await Promise.resolve();
      await Promise.resolve();

      await expect(playback.finalize(segment(), 7)).resolves.toBe('receipt-original');
      expect(invokeMock).toHaveBeenCalledTimes(2);
      expect(changedPlayer.pauseAndSnapshot).not.toHaveBeenCalled();
    },
  );

  it('retires only a typed proven non-commit and then requires a fresh player authority', async () => {
    const refusal = {
      schema: 1,
      code: 'PLAYBACK_REVISION_CHANGED',
      message: 'clip revision changed before finalization',
      retryable: true,
      suggestedAction: 'reloadClip',
      operationId: null,
    };
    invokeMock
      .mockRejectedValueOnce(refusal)
      .mockResolvedValueOnce(finalizedReceipt('receipt-fresh'));
    const playback = controller();
    const originalPlayer = audioAuthority('receipt-expired', 'grant-expired');
    const freshPlayer = audioAuthority('receipt-fresh', 'grant-fresh');
    playback.state.player = originalPlayer;

    await expect(playback.finalize(segment(), 7)).rejects.toBe(refusal);
    expect(originalPlayer.restartPlaybackAuthority).toHaveBeenCalledOnce();

    playback.state.player = freshPlayer;
    await expect(playback.finalize(segment(), 7)).resolves.toBe('receipt-fresh');

    expect(originalPlayer.pauseAndSnapshot).toHaveBeenCalledOnce();
    expect(freshPlayer.pauseAndSnapshot).toHaveBeenCalledOnce();
    expect(invokeMock.mock.calls[0]?.[1]).toMatchObject({
      playbackReceiptId: 'receipt-expired',
      mediaGrantId: 'grant-expired',
    });
    expect(invokeMock.mock.calls[1]?.[1]).toMatchObject({
      playbackReceiptId: 'receipt-fresh',
      mediaGrantId: 'grant-fresh',
    });
  });

  it('keeps a malformed-success identity frozen and ignores a changed player on retry', async () => {
    invokeMock
      .mockResolvedValueOnce({
        ...finalizedReceipt('wrong-receipt'),
        segmentId: 'wrong-segment',
        segmentRevision: 99,
      })
      .mockResolvedValueOnce(finalizedReceipt('receipt-original'));
    const playback = controller();
    const originalPlayer = audioAuthority('receipt-original', 'grant-original');
    const changedPlayer = audioAuthority('receipt-new', 'grant-new');
    playback.state.player = originalPlayer;

    await expect(playback.finalize(segment(), 7)).rejects.toThrow(
      'playback receipt response identity mismatch',
    );
    expect(originalPlayer.restartPlaybackAuthority).not.toHaveBeenCalled();

    playback.state.player = changedPlayer;
    await expect(playback.finalize(segment(), 7)).resolves.toBe('receipt-original');

    expect(invokeMock).toHaveBeenCalledTimes(2);
    expect(invokeMock.mock.calls[1]).toEqual(invokeMock.mock.calls[0]);
    expect(originalPlayer.pauseAndSnapshot).toHaveBeenCalledOnce();
    expect(changedPlayer.pauseAndSnapshot).not.toHaveBeenCalled();
    expect(changedPlayer.restartPlaybackAuthority).not.toHaveBeenCalled();
  });
});
