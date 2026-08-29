/**
 * Tracks whether a rendered database projection is both current and fully settled.
 *
 * An epoch is minted before every asynchronous projection operation. A receipt exists only when
 * every started operation has finished and the newest one succeeded. Synchronous local projection
 * changes also mint a settled epoch, so a receipt captured before an optimistic/local mutation is
 * invalidated immediately.
 */
export class ProjectionEpoch {
  private epoch = 0;
  private readonly pending = new Set<number>();
  private latestSucceeded = true;

  begin(): number {
    const epoch = ++this.epoch;
    // Every async projection writer must check `isLatest` before applying. A newer read therefore
    // retires older (even hung) reads instead of letting one abandoned promise block recovery.
    this.pending.clear();
    this.pending.add(epoch);
    this.latestSucceeded = false;
    return epoch;
  }

  finish(epoch: number, succeeded: boolean): void {
    this.pending.delete(epoch);
    if (epoch === this.epoch) this.latestSucceeded = succeeded;
  }

  settle(epoch: number, succeeded: boolean): number | null {
    this.finish(epoch, succeeded);
    return this.receipt() === epoch ? epoch : null;
  }

  mutate(): number {
    const epoch = ++this.epoch;
    this.pending.clear();
    this.latestSucceeded = true;
    return epoch;
  }

  isLatest(epoch: number): boolean {
    return epoch === this.epoch;
  }

  receipt(): number | null {
    return this.pending.size === 0 && this.latestSucceeded ? this.epoch : null;
  }
}
