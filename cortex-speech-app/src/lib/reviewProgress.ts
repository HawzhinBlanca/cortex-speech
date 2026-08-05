// Pure review-progress math. Kept out of the Svelte component so the one honesty-critical rule here
// — a filtered queue must NEVER claim the corpus is finished — is unit-tested in isolation.
//
// Audit 2026-08-05: review mode showed three unlabelled fractions at once ("Clip 1 of 144",
// "61/144", "67 of 144 reviewed") that a reviewer read as three different positions. Two were
// genuinely inconsistent: the position counter counted the QUEUE while the progress text counted the
// whole CORPUS, so an active search silently split the denominator. Both now come from one call.

/** Anything with a verified flag — deliberately narrower than SpeechSegment so the math stays pure. */
export interface ReviewableClip {
  verified: boolean;
}

export interface ReviewProgress {
  /** Verified clips within the queue on screen — pairs with `total`, never a different set. */
  done: number;
  /** Clips in the queue on screen. Same denominator the "Clip n of N" position counter uses. */
  total: number;
  /** 0–100 for the bar, rounded. 0 when the queue is empty. */
  percent: number;
  /**
   * True only when every clip in the WHOLE corpus is verified. A fully-reviewed search subset does
   * NOT set this: the completion banner is a claim about the dataset, and firing it on a filtered
   * view would tell the reviewer they are done when work remains.
   */
  allReviewed: boolean;
}

export function reviewProgress(
  queue: readonly ReviewableClip[],
  corpus: readonly ReviewableClip[],
): ReviewProgress {
  const total = queue.length;
  const done = queue.reduce((n, c) => n + (c.verified ? 1 : 0), 0);
  const corpusDone = corpus.reduce((n, c) => n + (c.verified ? 1 : 0), 0);
  return {
    done,
    total,
    percent: total > 0 ? Math.round((done / total) * 100) : 0,
    allReviewed: corpus.length > 0 && corpusDone === corpus.length,
  };
}
