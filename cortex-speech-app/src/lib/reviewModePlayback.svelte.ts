import { get } from 'svelte/store';
import * as api from './commands';
import { formatPublicErrorReference } from './errorText';
import { t } from './i18n';
import {
  ReviewPlaybackAttemptLedger,
  hasSufficientReviewPlayback,
  isProvenUncommittedPlaybackFinalization,
  type ReviewPlaybackAttempt,
} from './reviewPlaybackAttempt';
import { notifications } from './stores/notificationStore';
import type { SpeechSegment } from './types';

export interface ReviewAudioAuthority {
  pauseAndSnapshot: () => {
    segmentId: string | null;
    segmentRevision: number | null;
    playbackReceiptId: string | null;
    mediaGrantId: string | null;
    clipDurationMs: number | null;
    intervals: readonly api.PlaybackIntervalV1[];
  };
  restartPlaybackAuthority: () => void;
}

interface PlaybackDependencies {
  inboxOpen: () => boolean;
  onPlaybackRequired?: () => void;
}

export function createReviewModePlaybackController(deps: PlaybackDependencies) {
  const state = $state({
    waveformData: [] as number[],
    waveformError: null as string | null,
    currentTime: 0,
    playerDuration: 0,
    playing: false,
    audioError: null as string | null,
    heardMs: 0,
    playbackReceiptId: null as string | null,
    playbackMediaGrantId: null as string | null,
    playbackClipDurationMs: null as number | null,
    heardIntervals: [] as readonly api.PlaybackIntervalV1[],
    player: undefined as ReviewAudioAuthority | undefined,
  });
  const attempts = new ReviewPlaybackAttemptLedger();
  let waveformLoadSequence = 0;

  function playbackRequired() {
    if (deps.onPlaybackRequired) deps.onPlaybackRequired();
    else notifications.error(get(t)('review.mustListen'));
  }

  $effect(() => {
    if (!deps.inboxOpen()) return;
    state.playing = false;
    state.playbackReceiptId = null;
    state.playbackMediaGrantId = null;
    state.playbackClipDurationMs = null;
    state.heardIntervals = [];
    state.heardMs = 0;
  });

  async function finalize(segment: SpeechSegment, baseRevision: number): Promise<string | null> {
    const finalizedReceipt = attempts.finalizedReceipt(segment.id, baseRevision);
    if (finalizedReceipt) {
      state.playing = false;
      return finalizedReceipt;
    }

    const pendingAttempt = attempts.pendingAttempt(segment.id, baseRevision);
    let attempt: ReviewPlaybackAttempt;
    if (pendingAttempt) {
      // A lost response may have durably finalized this frozen receipt. Its replay must not depend
      // on a recreated player, an expired grant, or listening to the same clip a second time.
      state.playing = false;
      attempt = pendingAttempt;
    } else {
      const authority = await state.player?.pauseAndSnapshot();
      state.playing = false;
      if (
        !authority ||
        authority.segmentId !== segment.id ||
        authority.segmentRevision !== baseRevision
      ) {
        playbackRequired();
        return null;
      }
      const { playbackReceiptId, mediaGrantId, clipDurationMs } = authority;
      const intervals = authority.intervals.map(({ startMs, endMs }) => ({ startMs, endMs }));
      if (
        !playbackReceiptId ||
        !mediaGrantId ||
        !Number.isSafeInteger(clipDurationMs) ||
        (clipDurationMs ?? 0) <= 0 ||
        !hasSufficientReviewPlayback(intervals, clipDurationMs as number)
      ) {
        playbackRequired();
        return null;
      }
      attempt = attempts.snapshot({
        segmentId: segment.id,
        baseRevision,
        playbackReceiptId,
        mediaGrantId,
        intervals,
      });
    }
    try {
      const finalized = await api.recordPlaybackReceipt({
        playbackReceiptId: attempt.playbackReceiptId,
        mediaGrantId: attempt.mediaGrantId,
        intervals: attempt.intervals,
      });
      if (
        finalized.playbackReceiptId !== attempt.playbackReceiptId ||
        finalized.segmentId !== segment.id ||
        finalized.segmentRevision !== baseRevision
      ) {
        throw new Error('playback receipt response identity mismatch');
      }
      attempts.markFinalized(segment.id, baseRevision, finalized.playbackReceiptId);
      return finalized.playbackReceiptId;
    } catch (error) {
      if (isProvenUncommittedPlaybackFinalization(error)) {
        attempts.resolve(segment.id, baseRevision);
        state.player?.restartPlaybackAuthority();
      }
      throw error;
    }
  }

  async function loadWaveform(segment: SpeechSegment) {
    const sequence = ++waveformLoadSequence;
    try {
      const data = await api.getWaveform(segment.audioPath, 240, segment.alignmentJson);
      if (sequence !== waveformLoadSequence) return;
      state.waveformData = data;
      state.waveformError = null;
    } catch (error) {
      if (sequence !== waveformLoadSequence) return;
      state.waveformData = [];
      state.waveformError = formatPublicErrorReference(error) ?? get(t)('errors.unknown');
      notifications.error(get(t)('review.waveformFailed'), { cause: error });
    }
  }

  function resolve(segmentId: string, baseRevision: number) {
    attempts.resolve(segmentId, baseRevision);
  }

  /** Retire server-refused authority and force the next decision to mint and hear a fresh attempt. */
  function restartAfterProvenNonCommit(segmentId: string, baseRevision: number) {
    attempts.resolve(segmentId, baseRevision);
    state.playbackReceiptId = null;
    state.playbackMediaGrantId = null;
    state.playbackClipDurationMs = null;
    state.heardIntervals = [];
    state.heardMs = 0;
    state.playing = false;
    state.player?.restartPlaybackAuthority();
  }

  function resetForSelection() {
    state.playing = false;
    state.currentTime = 0;
    state.playbackReceiptId = null;
    state.playbackMediaGrantId = null;
    state.playbackClipDurationMs = null;
    state.heardIntervals = [];
    state.heardMs = 0;
    state.audioError = null;
  }

  return {
    state,
    finalize,
    loadWaveform,
    resolve,
    restartAfterProvenNonCommit,
    resetForSelection,
  };
}

export type ReviewModePlaybackController = ReturnType<typeof createReviewModePlaybackController>;
export const createReviewPlaybackController = createReviewModePlaybackController;
export type ReviewPlaybackController = ReviewModePlaybackController;
