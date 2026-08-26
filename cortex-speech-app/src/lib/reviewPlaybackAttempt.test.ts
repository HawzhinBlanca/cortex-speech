import { describe, expect, it } from 'vitest';
import {
  ReviewPlaybackAttemptLedger,
  hasSufficientReviewPlayback,
  isProvenUncommittedPlaybackFinalization,
  uniquePlaybackMs,
} from './reviewPlaybackAttempt';
import { ReviewCommitOperationLedger } from './reviewCommitOperation';

describe('review playback attempt authority', () => {
  it('retains the exact first finalized payload across a lost response', () => {
    const ledger = new ReviewPlaybackAttemptLedger();
    const first = ledger.snapshot({
      segmentId: 'seg-1',
      baseRevision: 7,
      playbackReceiptId: 'receipt-original',
      mediaGrantId: 'grant-original',
      intervals: [{ startMs: 0, endMs: 850 }],
    });
    const retry = ledger.snapshot({
      segmentId: 'seg-1',
      baseRevision: 7,
      playbackReceiptId: 'receipt-new',
      mediaGrantId: 'grant-new',
      intervals: [{ startMs: 0, endMs: 1_000 }],
    });

    expect(retry).toBe(first);
    expect(retry).toEqual({
      segmentId: 'seg-1',
      baseRevision: 7,
      playbackReceiptId: 'receipt-original',
      mediaGrantId: 'grant-original',
      intervals: [{ startMs: 0, endMs: 850 }],
    });
    expect(Object.isFrozen(retry)).toBe(true);
    expect(Object.isFrozen(retry.intervals)).toBe(true);
  });

  it('issues a fresh snapshot only after the prior authority is resolved', () => {
    const ledger = new ReviewPlaybackAttemptLedger();
    const base = {
      segmentId: 'seg-1',
      baseRevision: 7,
      playbackReceiptId: 'receipt-1',
      mediaGrantId: 'grant-1',
      intervals: [{ startMs: 0, endMs: 850 }],
    } as const;
    ledger.snapshot(base);
    ledger.resolve('seg-1', 7);
    expect(
      ledger.snapshot({ ...base, playbackReceiptId: 'receipt-2', mediaGrantId: 'grant-2' }),
    ).toMatchObject({ playbackReceiptId: 'receipt-2', mediaGrantId: 'grant-2' });
  });

  it('remembers a verified finalization after the live media grant expires', () => {
    const ledger = new ReviewPlaybackAttemptLedger();
    ledger.snapshot({
      segmentId: 'seg-finalized',
      baseRevision: 3,
      playbackReceiptId: 'receipt-finalized',
      mediaGrantId: 'grant-about-to-expire',
      intervals: [{ startMs: 0, endMs: 900 }],
    });
    expect(ledger.finalizedReceipt('seg-finalized', 3)).toBeNull();
    ledger.markFinalized('seg-finalized', 3, 'receipt-finalized');
    expect(ledger.finalizedReceipt('seg-finalized', 3)).toBe('receipt-finalized');
    expect(() => ledger.markFinalized('seg-finalized', 3, 'another-receipt')).toThrow();
  });

  it('fails closed on malformed unions and requires the same 85 percent as the backend', () => {
    expect(
      uniquePlaybackMs([
        { startMs: 0, endMs: 500 },
        { startMs: 500, endMs: 900 },
      ]),
    ).toBe(0);
    expect(hasSufficientReviewPlayback([{ startMs: 0, endMs: 849 }], 1_000)).toBe(false);
    expect(hasSufficientReviewPlayback([{ startMs: 0, endMs: 850 }], 1_000)).toBe(true);
    expect(hasSufficientReviewPlayback([], Number.NaN)).toBe(false);
  });

  it('retires only typed server-attested non-commits and keeps every ambiguous outcome frozen', () => {
    for (const code of [
      'INVALID_MEDIA_GRANT',
      'PLAYBACK_COVERAGE_INSUFFICIENT',
      'PLAYBACK_TIME_IMPLAUSIBLE',
      'PLAYBACK_REVISION_CHANGED',
      'PLAYBACK_EVIDENCE_CHANGED',
      'PLAYBACK_SESSION_EXPIRED',
      'PLAYBACK_MEDIA_GRANT_UNAVAILABLE',
    ]) {
      expect(isProvenUncommittedPlaybackFinalization({ schema: 1, code })).toBe(true);
    }
    for (const error of [
      new Error('transport lost'),
      { schema: 1, code: 'DATABASE_BUSY' },
      { schema: 1, code: 'PLAYBACK_PROOF_FAILED' },
      { schema: 1, code: 'PLAYBACK_AUTHORITY_MISMATCH' },
      { schema: 1, code: 'COMMIT_OUTCOME_UNKNOWN' },
      { schema: 2, code: 'PLAYBACK_SESSION_EXPIRED' },
    ]) {
      expect(isProvenUncommittedPlaybackFinalization(error)).toBe(false);
    }
  });

  it('replays one frozen receipt and one operation id after both responses are lost', () => {
    let sequence = 0;
    const playback = new ReviewPlaybackAttemptLedger();
    const operations = new ReviewCommitOperationLedger(() => `operation-${++sequence}`);
    const firstProof = playback.snapshot({
      segmentId: 'seg-loss',
      baseRevision: 11,
      playbackReceiptId: 'receipt-loss',
      mediaGrantId: 'grant-loss',
      intervals: [{ startMs: 0, endMs: 900 }],
    });
    const firstIntent = {
      segmentId: 'seg-loss',
      baseRevision: 11,
      decision: 'edit' as const,
      transcript: 'exact human text',
      reasonCode: null,
      playbackReceiptId: firstProof.playbackReceiptId,
    };
    const operationId = operations.idFor(firstIntent);

    const retriedProof = playback.snapshot({
      ...firstProof,
      intervals: [{ startMs: 0, endMs: 1_000 }],
    });
    const retriedIntent = { ...firstIntent, playbackReceiptId: retriedProof.playbackReceiptId };
    expect(retriedProof).toBe(firstProof);
    expect(operations.idFor(retriedIntent)).toBe(operationId);

    operations.resolve(retriedIntent);
    playback.resolve('seg-loss', 11);
    expect(operations.idFor(retriedIntent)).toBe('operation-2');
  });

  it('refuses a 257th distinct attempt without evicting any unresolved receipt', () => {
    const ledger = new ReviewPlaybackAttemptLedger();
    for (let index = 0; index < 256; index += 1) {
      ledger.snapshot({
        segmentId: `segment-${index}`,
        baseRevision: index,
        playbackReceiptId: `receipt-${index}`,
        mediaGrantId: `grant-${index}`,
        intervals: [{ startMs: 0, endMs: 850 }],
      });
    }

    expect(() =>
      ledger.snapshot({
        segmentId: 'segment-256',
        baseRevision: 256,
        playbackReceiptId: 'receipt-256',
        mediaGrantId: 'grant-256',
        intervals: [{ startMs: 0, endMs: 850 }],
      }),
    ).toThrow('playback retry ledger capacity exhausted');
    expect(
      ledger.snapshot({
        segmentId: 'segment-0',
        baseRevision: 0,
        playbackReceiptId: 'changed-receipt-must-not-win',
        mediaGrantId: 'changed-grant-must-not-win',
        intervals: [{ startMs: 0, endMs: 1_000 }],
      }),
    ).toMatchObject({ playbackReceiptId: 'receipt-0', mediaGrantId: 'grant-0' });

    ledger.resolve('segment-42', 42);
    expect(
      ledger.snapshot({
        segmentId: 'segment-256',
        baseRevision: 256,
        playbackReceiptId: 'receipt-256',
        mediaGrantId: 'grant-256',
        intervals: [{ startMs: 0, endMs: 850 }],
      }),
    ).toMatchObject({ playbackReceiptId: 'receipt-256' });
  });
});
