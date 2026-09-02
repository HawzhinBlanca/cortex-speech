import { describe, expect, it, vi } from 'vitest';
import {
  ReviewCommitOperationLedger,
  reviewCommitFailureDisposition,
  type ReviewCommitIntent,
} from './reviewCommitOperation';

const intent: ReviewCommitIntent = {
  segmentId: 'segment-a',
  baseRevision: 7,
  decision: 'edit',
  transcript: 'دەقی ڕاست',
  reasonCode: null,
  playbackReceiptId: 'receipt-a',
};

describe('ReviewCommitOperationLedger', () => {
  it('uses a real UUID when no test identity provider is supplied', () => {
    const operationId = new ReviewCommitOperationLedger().idFor(intent);
    expect(operationId).toMatch(
      /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/u,
    );
  });

  it('reuses the exact operation after an ambiguous lost response', () => {
    const makeId = vi.fn().mockReturnValueOnce('operation-1').mockReturnValueOnce('operation-2');
    const ledger = new ReviewCommitOperationLedger(makeId);
    expect(ledger.idFor(intent)).toBe('operation-1');
    expect(ledger.idFor({ ...intent })).toBe('operation-1');
    expect(makeId).toHaveBeenCalledTimes(1);
  });

  it('mints a new operation only after success/conflict resolves the old attempt', () => {
    const makeId = vi.fn().mockReturnValueOnce('operation-1').mockReturnValueOnce('operation-2');
    const ledger = new ReviewCommitOperationLedger(makeId);
    expect(ledger.idFor(intent)).toBe('operation-1');
    ledger.resolve(intent);
    expect(ledger.idFor(intent)).toBe('operation-2');
  });

  it('does not alias a changed human decision or transcript', () => {
    let sequence = 0;
    const ledger = new ReviewCommitOperationLedger(() => `operation-${++sequence}`);
    expect(ledger.idFor(intent)).toBe('operation-1');
    expect(ledger.idFor({ ...intent, transcript: 'دەقی تر' })).toBe('operation-2');
    expect(ledger.idFor({ ...intent, decision: 'reject', transcript: null })).toBe('operation-3');
  });

  it('refuses a 257th distinct operation without evicting any unresolved id', () => {
    let sequence = 0;
    const ledger = new ReviewCommitOperationLedger(() => `operation-${++sequence}`);
    const pending = Array.from({ length: 256 }, (_, index) => ({
      ...intent,
      segmentId: `segment-${index}`,
      baseRevision: index,
      playbackReceiptId: `receipt-${index}`,
    }));
    for (const entry of pending) ledger.idFor(entry);

    const overflow = {
      ...intent,
      segmentId: 'segment-256',
      baseRevision: 256,
      playbackReceiptId: 'receipt-256',
    };
    expect(() => ledger.idFor(overflow)).toThrow(
      'review operation retry ledger capacity exhausted',
    );
    expect(ledger.idFor(pending[0])).toBe('operation-1');
    expect(sequence).toBe(256);

    ledger.resolve(pending[42]);
    expect(ledger.idFor(overflow)).toBe('operation-257');
    expect(ledger.idFor(pending[0])).toBe('operation-1');
  });

  it('retires only server-proven non-commits and preserves ambiguous exact retries', () => {
    for (const code of ['NO_PLAYBACK_EVIDENCE', 'PLAYBACK_EVIDENCE_CHANGED', 'STALE_REVISION']) {
      expect(reviewCommitFailureDisposition({ schema: 1, code })).toBe('restart-playback');
    }
    expect(reviewCommitFailureDisposition({ schema: 1, code: 'OPERATION_ID_CONFLICT' })).toBe(
      'retry-with-new-operation',
    );
    for (const error of [
      new Error('response lost after commit'),
      { schema: 1, code: 'COMMIT_OUTCOME_UNKNOWN' },
      { schema: 1, code: 'DATABASE_BUSY' },
      { schema: 1, code: { untrusted: true } },
      { schema: 2, code: 'NO_PLAYBACK_EVIDENCE' },
      null,
    ]) {
      expect(reviewCommitFailureDisposition(error)).toBe('retain-exact-retry');
    }

    const callable = Object.assign(() => undefined, {
      schema: 1,
      code: 'NO_PLAYBACK_EVIDENCE',
    });
    expect(reviewCommitFailureDisposition(callable)).toBe('retain-exact-retry');

    let reads = 0;
    const oneShotCode = {
      schema: 1,
      get code() {
        reads += 1;
        return 'COMMIT_OUTCOME_UNKNOWN';
      },
    };
    expect(reviewCommitFailureDisposition(oneShotCode)).toBe('retain-exact-retry');
    expect(reads).toBe(1);

    const hostile = new Proxy(
      {},
      {
        get() {
          throw new Error('untrusted accessor');
        },
      },
    );
    expect(reviewCommitFailureDisposition(hostile)).toBe('retain-exact-retry');
  });

  it('does not exhaust capacity after one thousand proven-uncommitted retries are resolved', () => {
    let sequence = 0;
    const ledger = new ReviewCommitOperationLedger(() => `operation-${++sequence}`);
    for (let index = 0; index < 1_000; index += 1) {
      const refused = {
        ...intent,
        segmentId: `segment-${index}`,
        baseRevision: index,
        playbackReceiptId: `receipt-${index}`,
      };
      expect(ledger.idFor(refused)).toBe(`operation-${index + 1}`);
      ledger.resolve(refused);
    }
    expect(
      ledger.idFor({
        ...intent,
        segmentId: 'after-refusals',
        playbackReceiptId: 'fresh-receipt',
      }),
    ).toBe('operation-1001');
  });
});
