export type ReviewCommitIntent = {
  segmentId: string;
  baseRevision: number;
  decision: 'accept' | 'edit' | 'reject';
  transcript: string | null;
  reasonCode: string | null;
  playbackReceiptId: string;
};

export type ReviewCommitFailureDisposition =
  'retain-exact-retry' | 'retry-with-new-operation' | 'restart-playback';

/**
 * Classify only typed server outcomes whose contract proves this exact decision did not commit.
 *
 * Transport loss, database-busy errors, malformed success payloads, and the explicit
 * COMMIT_OUTCOME_UNKNOWN result remain ambiguous and therefore retain both immutable identities.
 * A stale/changed playback authority cannot be reused; an operation-id collision needs only a new
 * operation UUID because the already-finalized receipt itself was not consumed by this request.
 */
export function reviewCommitFailureDisposition(error: unknown): ReviewCommitFailureDisposition {
  if (!error || typeof error !== 'object') return 'retain-exact-retry';
  const candidate = error as { schema?: unknown; code?: unknown };
  let schema: unknown;
  let code: unknown;
  try {
    // Read unknown accessors once. A hostile Proxy/getter is transport noise, not proof that the
    // exact decision failed before commit, and therefore must retain the immutable retry identity.
    schema = candidate.schema;
    code = candidate.code;
  } catch {
    // Leave both values unknown; the schema guard below retains the exact retry identity.
  }
  if (schema !== 1) return 'retain-exact-retry';
  if (code === 'OPERATION_ID_CONFLICT') return 'retry-with-new-operation';
  if (
    code === 'NO_PLAYBACK_EVIDENCE' ||
    code === 'PLAYBACK_EVIDENCE_CHANGED' ||
    code === 'STALE_REVISION'
  ) {
    return 'restart-playback';
  }
  return 'retain-exact-retry';
}

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
