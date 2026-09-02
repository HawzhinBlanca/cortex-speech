export interface CommittedReviewIdentity {
  segmentId: string;
  committedRevision: number;
}

/** A commit response may update renderer state only when it advances the exact submitted row. */
export function isCommittedReviewFor(
  commit: CommittedReviewIdentity,
  segmentId: string,
  baseRevision: number,
): boolean {
  return (
    commit.segmentId === segmentId &&
    Number.isSafeInteger(baseRevision) &&
    Number.isSafeInteger(commit.committedRevision) &&
    commit.committedRevision === baseRevision + 1
  );
}
