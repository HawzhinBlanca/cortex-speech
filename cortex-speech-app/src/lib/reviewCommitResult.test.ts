import { describe, expect, it } from 'vitest';
import { isCommittedReviewFor } from './reviewCommitResult';

describe('isCommittedReviewFor', () => {
  it('accepts only an integer revision advance for the submitted segment', () => {
    expect(isCommittedReviewFor({ segmentId: 'a', committedRevision: 8 }, 'a', 7)).toBe(true);
    expect(isCommittedReviewFor({ segmentId: 'b', committedRevision: 8 }, 'a', 7)).toBe(false);
    expect(isCommittedReviewFor({ segmentId: 'a', committedRevision: 7 }, 'a', 7)).toBe(false);
    expect(isCommittedReviewFor({ segmentId: 'a', committedRevision: 6 }, 'a', 7)).toBe(false);
    expect(isCommittedReviewFor({ segmentId: 'a', committedRevision: 7.5 }, 'a', 7)).toBe(false);
  });

  it('rejects a revision jump even when it advances the submitted row', () => {
    expect(isCommittedReviewFor({ segmentId: 'seg-1', committedRevision: 9 }, 'seg-1', 7)).toBe(
      false,
    );
  });
});
