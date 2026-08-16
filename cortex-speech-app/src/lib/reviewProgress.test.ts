import { describe, it, expect } from 'vitest';
import { reviewProgress } from './reviewProgress';

const clips = (verifiedFlags: boolean[]) => verifiedFlags.map((verified) => ({ verified }));

describe('reviewProgress', () => {
  it('counts done and total from the SAME set so the denominators cannot diverge', () => {
    // The audit's actual defect: the position counter counted the queue, the progress text counted
    // the corpus. Under a search those are different lengths and the two lines contradicted.
    const corpus = clips([...Array(144)].map((_, i) => i < 67));
    const queue = corpus.slice(0, 12); // search-scoped view
    const p = reviewProgress(queue, corpus);
    expect(p.total).toBe(queue.length);
    expect(p.done).toBe(12); // all 12 happen to be verified in this slice
    expect(p.percent).toBe(100);
  });

  it('does NOT claim the corpus is reviewed when only a filtered subset is', () => {
    // The honesty-critical rule. A fully-verified search subset inside an unfinished corpus must not
    // fire the "you're done" banner.
    const corpus = clips([true, true, false, false]);
    const queue = clips([true, true]);
    const p = reviewProgress(queue, corpus);
    expect(p.percent).toBe(100); // the VIEW is complete
    expect(p.allReviewed).toBe(false); // the DATASET is not
  });

  it('claims all-reviewed only when every clip in the corpus is verified', () => {
    const corpus = clips([true, true, true]);
    expect(reviewProgress(corpus, corpus).allReviewed).toBe(true);
  });

  it('an empty corpus is not "all reviewed"', () => {
    // Zero clips means nothing has been done, not that everything has.
    expect(reviewProgress([], []).allReviewed).toBe(false);
  });

  it('an empty queue reports 0% instead of dividing by zero', () => {
    const p = reviewProgress([], clips([false]));
    expect(p.percent).toBe(0);
    expect(p.total).toBe(0);
    expect(p.done).toBe(0);
  });

  it('rounds the bar percentage', () => {
    expect(reviewProgress(clips([true, false, false]), clips([true, false, false])).percent).toBe(
      33,
    );
    expect(reviewProgress(clips([true, true, false]), clips([true, true, false])).percent).toBe(67);
  });
});
