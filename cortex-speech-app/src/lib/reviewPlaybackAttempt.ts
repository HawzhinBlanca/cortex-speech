import type { PlaybackInterval } from './playbackCoverage';

export const MIN_REVIEW_PLAYBACK_COVERAGE = 0.85;

const PROVEN_UNCOMMITTED_FINALIZATION_CODES = new Set([
  // Public command validation occurs before any database call.
  'INVALID_PLAYBACK_RECEIPT',
  'INVALID_MEDIA_GRANT',
  // These typed outcomes are emitted by validation branches whose native contract proves that no
  // older timed-out invocation can subsequently commit the same receipt.
  'NO_PLAYBACK_EVIDENCE',
  'PLAYBACK_COVERAGE_INSUFFICIENT',
  'PLAYBACK_REVISION_CHANGED',
  'PLAYBACK_EVIDENCE_CHANGED',
]);

/**
 * True only for server contracts that attest receipt finalization did not commit.
 *
 * Transport errors, generic proof failures, database-busy errors, malformed success payloads and
 * authority mismatches remain ambiguous: the server can durably finalize before a response-stage
 * failure, and an exact immutable replay is the only safe reconciliation.
 */
export function isProvenUncommittedPlaybackFinalization(error: unknown): boolean {
  if (!error || typeof error !== 'object') return false;
  const candidate = error as { schema?: unknown; code?: unknown };
  return (
    candidate.schema === 1 &&
    typeof candidate.code === 'string' &&
    PROVEN_UNCOMMITTED_FINALIZATION_CODES.has(candidate.code)
  );
}

export interface ReviewPlaybackAttempt {
  segmentId: string;
  baseRevision: number;
  playbackReceiptId: string;
  mediaGrantId: string;
  intervals: readonly PlaybackInterval[];
}

function attemptKey(segmentId: string, baseRevision: number): string {
  return JSON.stringify([segmentId, baseRevision]);
}

function immutableAttempt(attempt: ReviewPlaybackAttempt): ReviewPlaybackAttempt {
  const intervals = Object.freeze(
    attempt.intervals.map(({ startMs, endMs }) => Object.freeze({ startMs, endMs })),
  );
  return Object.freeze({ ...attempt, intervals });
}

/** Exact duration of a canonical playback interval union, or zero for malformed evidence. */
export function uniquePlaybackMs(intervals: readonly PlaybackInterval[]): number {
  let total = 0;
  let previousEnd = -1;
  for (const interval of intervals) {
    if (
      !Number.isSafeInteger(interval.startMs) ||
      !Number.isSafeInteger(interval.endMs) ||
      interval.startMs < 0 ||
      interval.endMs <= interval.startMs ||
      interval.startMs <= previousEnd
    ) {
      return 0;
    }
    total += interval.endMs - interval.startMs;
    if (!Number.isSafeInteger(total)) return 0;
    previousEnd = interval.endMs;
  }
  return total;
}

export function hasSufficientReviewPlayback(
  intervals: readonly PlaybackInterval[],
  clipDurationMs: number,
): boolean {
  if (!Number.isFinite(clipDurationMs) || clipDurationMs <= 0) return false;
  const requiredMs = Math.ceil(Math.floor(clipDurationMs) * MIN_REVIEW_PLAYBACK_COVERAGE);
  return uniquePlaybackMs(intervals) >= requiredMs;
}

/**
 * Freezes the exact policy-4 receipt payload across a lost/ambiguous response.
 *
 * A finalized receipt is immutable server-side. If the renderer retried with the intervals that
 * accrued after the first request, the altered replay would correctly be rejected and the reviewer
 * could not recover the already-durable decision. The first sufficient snapshot therefore remains
 * authoritative until a verified commit response or an authoritative revision conflict.
 */
export class ReviewPlaybackAttemptLedger {
  private readonly pending = new Map<
    string,
    { attempt: ReviewPlaybackAttempt; finalizedReceiptId: string | null }
  >();

  snapshot(attempt: ReviewPlaybackAttempt): ReviewPlaybackAttempt {
    const key = attemptKey(attempt.segmentId, attempt.baseRevision);
    const existing = this.pending.get(key);
    if (existing) return existing.attempt;
    if (this.pending.size >= 256) {
      throw new Error(
        'playback retry ledger capacity exhausted; resolve pending attempts before starting another',
      );
    }
    const frozen = immutableAttempt(attempt);
    this.pending.set(key, { attempt: frozen, finalizedReceiptId: null });
    return frozen;
  }

  /** Once the server confirms finalization, retries can commit without a now-expired media grant. */
  markFinalized(segmentId: string, baseRevision: number, playbackReceiptId: string): void {
    const entry = this.pending.get(attemptKey(segmentId, baseRevision));
    if (!entry || entry.attempt.playbackReceiptId !== playbackReceiptId) {
      throw new Error('playback finalization does not match the pending review attempt');
    }
    entry.finalizedReceiptId = playbackReceiptId;
  }

  finalizedReceipt(segmentId: string, baseRevision: number): string | null {
    return this.pending.get(attemptKey(segmentId, baseRevision))?.finalizedReceiptId ?? null;
  }

  /** Return the immutable payload retained after an ambiguous or lost finalization response. */
  pendingAttempt(segmentId: string, baseRevision: number): ReviewPlaybackAttempt | null {
    return this.pending.get(attemptKey(segmentId, baseRevision))?.attempt ?? null;
  }

  resolve(segmentId: string, baseRevision: number): void {
    this.pending.delete(attemptKey(segmentId, baseRevision));
  }
}
