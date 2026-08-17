import { describe, expect, it } from 'vitest';
import { revealReviewCompletion } from '../../src/lib/reviewCompletion';

describe('revealReviewCompletion', () => {
  it('scrolls to the newly-visible completion actions once', () => {
    const scroller = { scrollTop: 573 };

    let wasComplete = revealReviewCompletion(scroller, true, false);
    expect(scroller.scrollTop).toBe(0);

    scroller.scrollTop = 120;
    wasComplete = revealReviewCompletion(scroller, true, wasComplete);
    expect(scroller.scrollTop).toBe(120);
    expect(wasComplete).toBe(true);
  });
});
