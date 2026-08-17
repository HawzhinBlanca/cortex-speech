/** Reveal the completion actions exactly when a review queue becomes complete. */
export function revealReviewCompletion(
  scroller: Pick<HTMLElement, 'scrollTop'> | null,
  completedNow: boolean,
  completedBefore: boolean,
): boolean {
  if (completedNow && !completedBefore && scroller) {
    scroller.scrollTop = 0;
  }
  return completedNow;
}
