export type ReviewCommitIntent = {
  segmentId: string;
  baseRevision: number;
  decision: 'accept' | 'edit' | 'reject';
  transcript: string | null;
  reasonCode: string | null;
  playbackReceiptId: string;
};

function intentKey(intent: ReviewCommitIntent): string {
  // JSON over a fixed tuple is collision-free for these strings and preserves the exact human text.
  // Do not normalize here: the backend payload hash must see precisely the request being retried.
  return JSON.stringify([
    intent.segmentId,
    intent.baseRevision,
    intent.decision,
    intent.transcript,
    intent.reasonCode,
    intent.playbackReceiptId,
  ]);
}

/**
 * Keeps one operation UUID stable across an ambiguous/lost response. A new click with the same exact
 * intent must replay the server's durable result, not submit a second decision under a fresh UUID.
 * Entries are removed only after a verified response or an authoritative revision conflict.
 */
export class ReviewCommitOperationLedger {
  private readonly pending = new Map<string, string>();

  constructor(private readonly makeId: () => string = () => crypto.randomUUID()) {}

  idFor(intent: ReviewCommitIntent): string {
    const key = intentKey(intent);
    const existing = this.pending.get(key);
    if (existing) return existing;
    if (this.pending.size >= 256) {
      throw new Error(
        'review operation retry ledger capacity exhausted; resolve pending operations before starting another',
      );
    }
    const operationId = this.makeId();
    this.pending.set(key, operationId);
    return operationId;
  }

  resolve(intent: ReviewCommitIntent): void {
    this.pending.delete(intentKey(intent));
  }
}
