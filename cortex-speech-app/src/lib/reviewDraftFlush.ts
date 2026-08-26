export type ReviewDraftFlusher = () => Promise<void>;

const activeFlushers = new Set<ReviewDraftFlusher>();

/** Register one mounted review session's durable-draft barrier. */
export function registerReviewDraftFlusher(flusher: ReviewDraftFlusher): () => void {
  activeFlushers.add(flusher);
  return () => activeFlushers.delete(flusher);
}

/**
 * Wait until every mounted review surface has durably persisted its latest visible draft.
 *
 * A failure is intentionally propagated: native close must stay open and tell the reviewer, because
 * closing successfully after a failed draft write converts a visible human correction into data loss.
 */
export async function flushReviewDrafts(): Promise<void> {
  await Promise.all([...activeFlushers].map((flush) => flush()));
}

export function registeredReviewDraftFlushers(): number {
  return activeFlushers.size;
}
