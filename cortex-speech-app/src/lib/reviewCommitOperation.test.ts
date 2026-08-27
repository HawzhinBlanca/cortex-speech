import { describe, expect, it, vi } from 'vitest';
import { ReviewCommitOperationLedger, type ReviewCommitIntent } from './reviewCommitOperation';

const intent: ReviewCommitIntent = {
  segmentId: 'segment-a',
  baseRevision: 7,
  decision: 'edit',
  transcript: 'دەقی ڕاست',
  reasonCode: null,
  playbackReceiptId: 'receipt-a',
};

describe('ReviewCommitOperationLedger', () => {
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
});
