export type RevisionBoundReviewDraftIntent =
  | {
      kind: 'save';
      segmentId: string;
      baseRevision: number;
      text: string;
    }
  | {
      kind: 'delete';
      segmentId: string;
      baseRevision: number;
    };

export interface ReviewDraftWriteEcho {
  segmentId: string;
  baseRevision: number;
  text: string;
}

interface PendingIntent {
  intent: RevisionBoundReviewDraftIntent;
  sequence: number;
}

export interface ReviewDraftWriteCoordinatorOptions {
  save: (segmentId: string, baseRevision: number, text: string) => Promise<ReviewDraftWriteEcho>;
  delete?: (segmentId: string, baseRevision: number) => Promise<boolean>;
  onStateChange?: (segmentId: string) => void;
  onWriteSucceeded?: (intent: RevisionBoundReviewDraftIntent) => void;
  onWriteFailed?: (intent: RevisionBoundReviewDraftIntent, error: unknown) => void;
}

/** A successful write response that cannot prove its exact request is not an acknowledgement. */
export class ReviewDraftWriteIdentityError extends Error {
  constructor() {
    super('Review draft write response did not match the exact segment, revision, and text');
    this.name = 'ReviewDraftWriteIdentityError';
  }
}

function intentKey(intent: RevisionBoundReviewDraftIntent): string {
  return intent.kind === 'save'
    ? `save\0${intent.baseRevision}\0${intent.text}`
    : `delete\0${intent.baseRevision}`;
}

function sameIntent(
  left: RevisionBoundReviewDraftIntent,
  right: RevisionBoundReviewDraftIntent,
): boolean {
  return left.segmentId === right.segmentId && intentKey(left) === intentKey(right);
}

/**
 * Serialize revision-bound draft writes without coupling durability to a Promise's lifetime.
 *
 * The desired map is the authority: a rejected attempt remains there after its Promise settles, so
 * navigation cannot make an off-screen edit invisible to the native-close barrier. An older success
 * records only what it actually proved and can never clear a newer desired value for the same clip.
 */
export class ReviewDraftWriteCoordinator {
  private readonly desired = new Map<string, PendingIntent>();
  private readonly inFlight = new Map<string, Promise<void>>();
  private readonly durable = new Map<string, string>();
  private sequence = 0;

  constructor(private readonly options: ReviewDraftWriteCoordinatorOptions) {}

  isWriting(segmentId: string): boolean {
    return this.inFlight.has(segmentId);
  }

  hasDesired(segmentId?: string): boolean {
    return segmentId === undefined ? this.desired.size > 0 : this.desired.has(segmentId);
  }

  desiredIntent(segmentId: string): RevisionBoundReviewDraftIntent | null {
    return this.desired.get(segmentId)?.intent ?? null;
  }

  isDurable(intent: RevisionBoundReviewDraftIntent): boolean {
    return (
      !this.desired.has(intent.segmentId) &&
      this.durable.get(intent.segmentId) === intentKey(intent)
    );
  }

  /** Adopt an exact authoritative read/commit result without issuing another renderer write. */
  acknowledge(intent: RevisionBoundReviewDraftIntent): void {
    this.durable.set(intent.segmentId, intentKey(intent));
    const desired = this.desired.get(intent.segmentId);
    if (desired && sameIntent(desired.intent, intent)) {
      this.desired.delete(intent.segmentId);
    }
    this.options.onStateChange?.(intent.segmentId);
  }

  request(intent: RevisionBoundReviewDraftIntent): Promise<void> {
    const current = this.desired.get(intent.segmentId);
    if (!current && this.durable.get(intent.segmentId) === intentKey(intent)) {
      return Promise.resolve();
    }
    if (!current || !sameIntent(current.intent, intent)) {
      this.desired.set(intent.segmentId, { intent, sequence: ++this.sequence });
      this.options.onStateChange?.(intent.segmentId);
    }
    return this.start(intent.segmentId);
  }

  /** Retry and await every still-desired write for one clip. */
  async flushSegment(segmentId: string): Promise<void> {
    while (this.desired.has(segmentId) || this.inFlight.has(segmentId)) {
      await this.start(segmentId);
    }
  }

  /** Retry all desired entries, including failures whose original Promises already settled. */
  async flushAll(): Promise<void> {
    while (this.desired.size > 0 || this.inFlight.size > 0) {
      const segmentIds = new Set([...this.desired.keys(), ...this.inFlight.keys()]);
      const results = await Promise.allSettled(
        [...segmentIds].map((segmentId) => this.flushSegment(segmentId)),
      );
      const failure = results.find(
        (result): result is PromiseRejectedResult => result.status === 'rejected',
      );
      if (failure) throw failure.reason;
    }
  }

  private start(segmentId: string): Promise<void> {
    const existing = this.inFlight.get(segmentId);
    if (existing) return existing;
    if (!this.desired.has(segmentId)) return Promise.resolve();

    let task!: Promise<void>;
    task = this.run(segmentId).finally(() => {
      if (this.inFlight.get(segmentId) === task) this.inFlight.delete(segmentId);
      this.options.onStateChange?.(segmentId);
    });
    this.inFlight.set(segmentId, task);
    this.options.onStateChange?.(segmentId);
    return task;
  }

  private async run(segmentId: string): Promise<void> {
    while (true) {
      const pending = this.desired.get(segmentId);
      if (!pending) return;
      try {
        await this.persist(pending.intent);
      } catch (error) {
        this.options.onWriteFailed?.(pending.intent, error);
        throw error;
      }

      this.durable.set(segmentId, intentKey(pending.intent));
      const latest = this.desired.get(segmentId);
      if (latest?.sequence === pending.sequence) this.desired.delete(segmentId);
      this.options.onStateChange?.(segmentId);
      this.options.onWriteSucceeded?.(pending.intent);
      // A newer value may have arrived while this request was in flight. Persist it in the same
      // serialized chain; the older response proves only the older value and cannot clear it.
      if (!this.desired.has(segmentId)) return;
    }
  }

  private async persist(intent: RevisionBoundReviewDraftIntent): Promise<void> {
    if (intent.kind === 'delete') {
      if (!this.options.delete) {
        throw new Error('Review draft deletion is not configured for this write coordinator');
      }
      const deleted = await this.options.delete(intent.segmentId, intent.baseRevision);
      if (typeof deleted !== 'boolean') throw new ReviewDraftWriteIdentityError();
      return;
    }

    const saved = await this.options.save(intent.segmentId, intent.baseRevision, intent.text);
    if (
      saved.segmentId !== intent.segmentId ||
      saved.baseRevision !== intent.baseRevision ||
      saved.text !== intent.text
    ) {
      throw new ReviewDraftWriteIdentityError();
    }
  }
}
