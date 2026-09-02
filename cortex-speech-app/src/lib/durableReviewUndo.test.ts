import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const commandMocks = vi.hoisted(() => ({
  getAvailability: vi.fn(),
}));

vi.mock('./commands', () => ({
  getDesktopReviewUndoAvailabilityV1: commandMocks.getAvailability,
}));

import {
  createDurableReviewUndoController,
  sharedDurableReviewUndo,
  validatedDesktopReviewUndoOutcome,
} from './durableReviewUndo.svelte';
import { ProjectionEpoch } from './projectionEpoch';
import { REVIEW_OPERATION_TIMEOUT_MS } from './reviewOperationTimeout';

type DecisionUndoTarget = {
  kind: 'decision';
  effectEventId: number;
  segmentId: string;
  decision: 'accept' | 'edit' | 'reject';
  sourceOperationId: string;
  sourcePayloadHash: string;
  databaseGeneration: number;
};

type FlagUndoTarget = {
  kind: 'flag';
  effectEventId: number;
  segmentId: string;
  sourceOperationId: string;
  sourcePayloadHash: string;
  priorRevision: number;
  flagRevision: number;
  flagKind:
    | { kind: 'generic' }
    | {
        kind: 'technicalUnusable';
        reason: 'decodeFailed' | 'missingFile' | 'permissionDenied' | 'corruptContainer';
      };
  databaseGeneration: number;
};

type UndoTarget = DecisionUndoTarget | FlagUndoTarget;

const FIRST_UNDO_OPERATION = '00000000-0000-4000-8000-000000000001';
const SECOND_UNDO_OPERATION = '00000000-0000-4000-8000-000000000002';

function undoTarget(overrides: Partial<DecisionUndoTarget> = {}): DecisionUndoTarget {
  return {
    kind: 'decision',
    effectEventId: 41,
    segmentId: 'segment-41',
    decision: 'edit',
    sourceOperationId: '10000000-0000-4000-8000-000000000041',
    sourcePayloadHash: 'a'.repeat(64),
    databaseGeneration: 9,
    ...overrides,
  };
}

function flagUndoTarget(overrides: Partial<FlagUndoTarget> = {}): FlagUndoTarget {
  return {
    kind: 'flag',
    effectEventId: 51,
    segmentId: 'segment-51',
    sourceOperationId: '10000000-0000-4000-8000-000000000051',
    sourcePayloadHash: 'b'.repeat(64),
    priorRevision: 5,
    flagRevision: 6,
    flagKind: { kind: 'generic' },
    databaseGeneration: 9,
    ...overrides,
  };
}

function available(target: UndoTarget) {
  return { status: 'available' as const, target };
}

function expectedUndo(target: UndoTarget) {
  if (target.kind === 'flag') {
    return {
      kind: 'flag' as const,
      effectEventId: target.effectEventId,
      segmentId: target.segmentId,
      sourceOperationId: target.sourceOperationId,
      flagKind: target.flagKind,
    };
  }
  return {
    kind: 'decision' as const,
    effectEventId: target.effectEventId,
    segmentId: target.segmentId,
    decision: target.decision,
    sourceOperationId: target.sourceOperationId,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

function settledProjectionAuthority(label?: string, trace?: string[]) {
  const projection = new ProjectionEpoch();
  const authority = {
    reloadProjection: vi.fn(async () => {
      if (label && trace) trace.push(label);
      const epoch = projection.begin();
      return projection.settle(epoch, true);
    }),
    projectionReceipt: vi.fn(() => projection.receipt()),
  };
  return { projection, authority };
}

describe('durable desktop review Undo authority', () => {
  beforeEach(() => {
    vi.restoreAllMocks();
    commandMocks.getAvailability.mockReset();
    vi.spyOn(globalThis.crypto, 'randomUUID').mockReturnValue(FIRST_UNDO_OPERATION);
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('hydrates startup availability without minting an operation id before the first Undo click', async () => {
    const target = undoTarget();
    commandMocks.getAvailability.mockResolvedValueOnce(available(target));
    const controller = createDurableReviewUndoController();

    expect(controller.state).toMatchObject({
      status: 'loading',
      target: null,
      operationId: null,
      blockedReason: null,
    });

    await expect(controller.refresh()).resolves.toBe(true);
    expect(controller.state).toMatchObject({
      status: 'ready',
      target,
      operationId: null,
      blockedReason: null,
    });
    expect(globalThis.crypto.randomUUID).not.toHaveBeenCalled();

    expect(controller.beginRequest()).toEqual({ target, operationId: FIRST_UNDO_OPERATION });
    expect(controller.state.operationId).toBe(FIRST_UNDO_OPERATION);
    expect(controller.state).toMatchObject({ status: 'reconciling', inFlight: true });
    expect(globalThis.crypto.randomUUID).toHaveBeenCalledOnce();
  });

  it('keeps no history, a database block, and an authority read failure as distinct fail-closed states', async () => {
    commandMocks.getAvailability
      .mockResolvedValueOnce({ status: 'none' })
      .mockResolvedValueOnce({ status: 'blocked', reason: 'flagShadowed' })
      .mockRejectedValueOnce(new Error('database unavailable'));

    const none = createDurableReviewUndoController();
    await expect(none.refresh()).resolves.toBe(true);
    expect(none.state).toMatchObject({
      status: 'none',
      target: null,
      operationId: null,
      blockedReason: null,
    });
    expect(none.beginRequest()).toBeNull();

    const blocked = createDurableReviewUndoController();
    await expect(blocked.refresh()).resolves.toBe(true);
    expect(blocked.state).toMatchObject({
      status: 'blocked',
      target: null,
      operationId: null,
      blockedReason: 'flagShadowed',
    });
    expect(blocked.beginRequest()).toBeNull();

    const failed = createDurableReviewUndoController();
    await expect(failed.refresh()).resolves.toBe(false);
    expect(failed.state).toMatchObject({
      status: 'failed',
      target: null,
      operationId: null,
      blockedReason: null,
    });
    expect(failed.beginRequest()).toBeNull();
    expect(globalThis.crypto.randomUUID).not.toHaveBeenCalled();
  });

  it('ignores an older concurrent hydration after a newer database authority wins', async () => {
    const older = deferred<{ status: 'blocked'; reason: 'legacyHistory' }>();
    const newer = deferred<ReturnType<typeof available>>();
    const newestTarget = undoTarget({ effectEventId: 42, segmentId: 'segment-42' });
    commandMocks.getAvailability
      .mockReturnValueOnce(older.promise)
      .mockReturnValueOnce(newer.promise);
    const controller = createDurableReviewUndoController();

    const olderRefresh = controller.refresh();
    const newerRefresh = controller.refresh();
    newer.resolve(available(newestTarget));
    await expect(newerRefresh).resolves.toBe(true);
    expect(controller.state).toMatchObject({
      status: 'ready',
      target: newestTarget,
      operationId: null,
    });

    older.resolve({ status: 'blocked', reason: 'legacyHistory' });
    await expect(olderRefresh).resolves.toBe(false);
    expect(controller.state).toMatchObject({
      status: 'ready',
      target: newestTarget,
      operationId: null,
      blockedReason: null,
    });
    expect(globalThis.crypto.randomUUID).not.toHaveBeenCalled();
  });

  it('accepts reconciliation only when both immutable effect and segment identities match', async () => {
    const target = undoTarget();
    commandMocks.getAvailability.mockResolvedValueOnce(available(target));
    const controller = createDurableReviewUndoController();

    await expect(controller.refresh(expectedUndo(target))).resolves.toBe(true);
    expect(controller.state).toMatchObject({ status: 'ready', target, operationId: null });
  });

  it.each([
    ['generic', flagUndoTarget()],
    [
      'technical',
      flagUndoTarget({
        effectEventId: 52,
        flagKind: { kind: 'technicalUnusable', reason: 'corruptContainer' },
      }),
    ],
  ] as const)('hydrates an exact restart-safe %s flag target', async (_kind, target) => {
    commandMocks.getAvailability.mockResolvedValueOnce(available(target));
    const controller = createDurableReviewUndoController();

    await expect(controller.refresh(expectedUndo(target))).resolves.toBe(true);
    expect(controller.state).toMatchObject({ status: 'ready', target, operationId: null });
    expect(controller.beginRequest()).toEqual({ target, operationId: FIRST_UNDO_OPERATION });
  });

  it('retains a flag retry identity only for byte-exact immutable authority', async () => {
    vi.mocked(globalThis.crypto.randomUUID)
      .mockReturnValueOnce(FIRST_UNDO_OPERATION)
      .mockReturnValueOnce(SECOND_UNDO_OPERATION);
    const first = flagUndoTarget({
      flagKind: { kind: 'technicalUnusable', reason: 'missingFile' },
    });
    const identical = {
      ...first,
      flagKind: { ...first.flagKind },
    } as FlagUndoTarget;
    const changed = {
      ...identical,
      flagKind: { kind: 'technicalUnusable', reason: 'permissionDenied' as const },
    } as FlagUndoTarget;
    commandMocks.getAvailability
      .mockResolvedValueOnce(available(first))
      .mockResolvedValueOnce(available(identical))
      .mockResolvedValueOnce(available(changed));
    const controller = createDurableReviewUndoController();

    await controller.refresh();
    const firstRequest = controller.beginRequest();
    expect(firstRequest).toEqual({ target: first, operationId: FIRST_UNDO_OPERATION });
    if (!firstRequest) throw new Error('expected flag Undo request');
    controller.releaseUnsent(firstRequest);

    await controller.refresh();
    expect(controller.beginRequest()).toEqual({
      target: identical,
      operationId: FIRST_UNDO_OPERATION,
    });
    controller.releaseUnsent({ target: identical, operationId: FIRST_UNDO_OPERATION });

    await controller.refresh();
    expect(controller.state).toMatchObject({ status: 'ready', target: changed, operationId: null });
    expect(controller.beginRequest()).toEqual({
      target: changed,
      operationId: SECOND_UNDO_OPERATION,
    });
  });

  it('validates tagged flag outcomes and rejects cross-table IDs or malformed applied rows', () => {
    const target = flagUndoTarget();
    const applied = {
      status: 'applied',
      effectKind: 'flag',
      effectEventId: target.effectEventId,
      restoredRevision: target.priorRevision,
      segment: { id: target.segmentId },
    };

    expect(validatedDesktopReviewUndoOutcome(applied, target)).toBe('applied');
    expect(
      validatedDesktopReviewUndoOutcome(
        { status: 'alreadyApplied', effectKind: 'flag', effectEventId: target.effectEventId },
        target,
      ),
    ).toBe('alreadyApplied');
    expect(
      validatedDesktopReviewUndoOutcome({ ...applied, effectKind: 'decision' }, target),
    ).toBeNull();
    expect(validatedDesktopReviewUndoOutcome({ ...applied, effectEventId: 41 }, target)).toBeNull();
    expect(
      validatedDesktopReviewUndoOutcome({ ...applied, segment: { id: 'wrong-segment' } }, target),
    ).toBeNull();
    expect(
      validatedDesktopReviewUndoOutcome({ ...applied, restoredRevision: -1 }, target),
    ).toBeNull();
  });

  it('treats hostile response accessors as ambiguous instead of throwing after IPC', () => {
    const target = flagUndoTarget();
    const hostileOutcome = new Proxy(
      {},
      {
        get() {
          throw new Error('hostile undo response getter');
        },
      },
    );

    expect(() => validatedDesktopReviewUndoOutcome(hostileOutcome, target)).not.toThrow();
    expect(validatedDesktopReviewUndoOutcome(hostileOutcome, target)).toBeNull();
  });

  it.each([
    [
      'effect',
      {
        ...expectedUndo(undoTarget()),
        effectEventId: 999,
      },
    ],
    [
      'segment',
      {
        ...expectedUndo(undoTarget()),
        segmentId: 'other-segment',
      },
    ],
  ])('refuses commit reconciliation with a mismatched %s identity', async (_field, expected) => {
    commandMocks.getAvailability.mockResolvedValueOnce(available(undoTarget()));
    const controller = createDurableReviewUndoController();

    await expect(controller.refresh(expected)).resolves.toBe(false);
    expect(controller.state).toMatchObject({
      status: 'ready',
      target: undoTarget(),
      operationId: null,
      blockedReason: null,
    });
    expect(globalThis.crypto.randomUUID).not.toHaveBeenCalled();
  });

  it('retains the first click operation id across same-authority hydration and ambiguous retry', async () => {
    const firstRead = undoTarget();
    const identicalSecondRead = { ...firstRead };
    commandMocks.getAvailability
      .mockResolvedValueOnce(available(firstRead))
      .mockResolvedValueOnce(available(identicalSecondRead));
    const controller = createDurableReviewUndoController();

    await controller.refresh();
    const firstRequest = controller.beginRequest();
    expect(firstRequest).toEqual({ target: firstRead, operationId: FIRST_UNDO_OPERATION });
    if (!firstRequest) throw new Error('expected first Undo request');

    controller.releaseUnsent(firstRequest);
    expect(controller.state).toMatchObject({ status: 'ready', inFlight: false });

    await controller.refresh();
    expect(controller.state.operationId).toBe(FIRST_UNDO_OPERATION);
    const secondRequest = controller.beginRequest();
    expect(secondRequest).toEqual({
      target: identicalSecondRead,
      operationId: FIRST_UNDO_OPERATION,
    });
    if (!secondRequest) throw new Error('expected second Undo request');

    controller.markAmbiguous(secondRequest, {
      schema: 1,
      code: 'IPC_RESPONSE_LOST',
      message: 'response channel closed',
      retryable: true,
    });
    expect(controller.state).toMatchObject({
      status: 'reconciling',
      operationId: FIRST_UNDO_OPERATION,
      inFlight: false,
      errorCode: 'IPC_RESPONSE_LOST',
    });
    expect(controller.beginRequest()).toEqual(secondRequest);
    expect(globalThis.crypto.randomUUID).toHaveBeenCalledOnce();
  });

  it.each([
    ['effectEventId', { effectEventId: 42 }],
    ['segmentId', { segmentId: 'segment-42' }],
    ['decision', { decision: 'accept' as const }],
    ['sourceOperationId', { sourceOperationId: '20000000-0000-4000-8000-000000000041' }],
    ['sourcePayloadHash', { sourcePayloadHash: 'b'.repeat(64) }],
    ['databaseGeneration', { databaseGeneration: 10 }],
  ])(
    'clears retry identity when %s changes and mints only on the next click',
    async (_field, change) => {
      vi.mocked(globalThis.crypto.randomUUID)
        .mockReturnValueOnce(FIRST_UNDO_OPERATION)
        .mockReturnValueOnce(SECOND_UNDO_OPERATION);
      const firstTarget = undoTarget();
      const changedTarget = undoTarget(change);
      commandMocks.getAvailability
        .mockResolvedValueOnce(available(firstTarget))
        .mockResolvedValueOnce(available(changedTarget));
      const controller = createDurableReviewUndoController();

      await controller.refresh();
      const firstRequest = controller.beginRequest();
      expect(firstRequest?.operationId).toBe(FIRST_UNDO_OPERATION);
      if (!firstRequest) throw new Error('expected first Undo request');
      controller.releaseUnsent(firstRequest);
      await controller.refresh();

      expect(controller.state).toMatchObject({
        status: 'ready',
        target: changedTarget,
        operationId: null,
      });
      expect(globalThis.crypto.randomUUID).toHaveBeenCalledOnce();
      expect(controller.beginRequest()).toEqual({
        target: changedTarget,
        operationId: SECOND_UNDO_OPERATION,
      });
      expect(globalThis.crypto.randomUUID).toHaveBeenCalledTimes(2);
    },
  );

  it.each(['applied', 'alreadyApplied', 'conflict'] as const)(
    'keeps a terminal %s Undo outcome projection-stale until authoritative reload completes',
    async (outcome) => {
      const target = undoTarget();
      commandMocks.getAvailability
        .mockResolvedValueOnce(available(target))
        .mockResolvedValueOnce({ status: 'blocked', reason: 'latestDecisionUndone' });
      const controller = createDurableReviewUndoController();

      await controller.refresh();
      const request = controller.beginRequest();
      if (!request) throw new Error('expected Undo request');
      controller.requireProjectionReload(request, outcome);

      expect(controller.state).toMatchObject({
        status: 'projectionStale',
        target,
        operationId: FIRST_UNDO_OPERATION,
        blockedReason: null,
        inFlight: false,
        projectionOutcome: outcome,
      });
      expect(controller.pendingProjection()).toEqual({ target, outcome });
      expect(controller.beginRequest()).toBeNull();
      expect(controller.blocksNewTruth()).toBe(true);
      await expect(controller.refresh()).resolves.toBe(false);
      expect(commandMocks.getAvailability).toHaveBeenCalledOnce();

      const global = settledProjectionAuthority();
      await expect(controller.reconcileProjections(global.authority)).resolves.toBe(true);
      expect(controller.state).toMatchObject({
        status: 'blocked',
        target: null,
        operationId: null,
        blockedReason: 'latestDecisionUndone',
        projectionOutcome: null,
      });
      expect(controller.pendingProjection()).toBeNull();
      expect(controller.blocksNewTruth()).toBe(false);
      expect(commandMocks.getAvailability).toHaveBeenCalledTimes(2);
    },
  );

  it.each(['applied', 'alreadyApplied', 'conflict'] as const)(
    'rejects an unchanged exact target after terminal %s as an invalid Undo postcondition',
    async (outcome) => {
      const target = undoTarget();
      commandMocks.getAvailability
        .mockResolvedValueOnce(available(target))
        .mockResolvedValueOnce(available({ ...target }));
      const controller = createDurableReviewUndoController();
      const global = settledProjectionAuthority();

      await controller.refresh();
      const request = controller.beginRequest();
      if (!request) throw new Error('expected Undo request');
      controller.requireProjectionReload(request, outcome);

      await expect(controller.reconcileProjections(global.authority)).resolves.toBe(false);
      expect(controller.state).toMatchObject({
        status: 'projectionStale',
        target,
        operationId: FIRST_UNDO_OPERATION,
        projectionOutcome: outcome,
        errorCode: 'INVALID_UNDO_POSTCONDITION',
        inFlight: false,
      });
      expect(controller.pendingProjection()).toEqual({ target, outcome });
      expect(controller.blocksNewTruth()).toBe(true);
    },
  );

  it('invalidates settled authority for a newer action but refuses to discard an in-flight request', async () => {
    const firstTarget = undoTarget();
    const nextTarget = undoTarget({ effectEventId: 42, segmentId: 'segment-42' });
    commandMocks.getAvailability
      .mockResolvedValueOnce(available(firstTarget))
      .mockResolvedValueOnce(available(nextTarget));
    const controller = createDurableReviewUndoController();

    await controller.refresh();
    expect(controller.invalidateForNewAction()).toBe(true);
    expect(controller.state).toMatchObject({
      status: 'loading',
      target: null,
      operationId: null,
      blockedReason: null,
    });
    expect(controller.beginRequest()).toBeNull();
    expect(controller.invalidateForNewAction()).toBe(false);

    await expect(controller.refresh()).resolves.toBe(true);
    const request = controller.beginRequest();
    if (!request) throw new Error('expected Undo request');
    expect(controller.state).toMatchObject({
      status: 'reconciling',
      target: nextTarget,
      operationId: FIRST_UNDO_OPERATION,
      blockedReason: null,
      inFlight: true,
    });
    expect(controller.invalidateForNewAction()).toBe(false);
    expect(controller.state.target).toEqual(nextTarget);
    expect(controller.state.operationId).toBe(FIRST_UNDO_OPERATION);
  });

  it.each([
    ['status', { status: 'futureProtocol' }],
    [
      'target',
      {
        status: 'available',
        target: undoTarget({ sourceOperationId: 'not-a-uuid' }),
      },
    ],
    ['block reason', { status: 'blocked', reason: 'futureReason' }],
  ])('fails closed and erases retained authority for a malformed %s', async (_kind, malformed) => {
    const target = undoTarget();
    commandMocks.getAvailability
      .mockResolvedValueOnce(available(target))
      .mockResolvedValueOnce(malformed);
    const controller = createDurableReviewUndoController();

    await controller.refresh();
    const retained = controller.beginRequest();
    if (!retained) throw new Error('expected Undo request');
    controller.releaseUnsent(retained);
    await expect(controller.refresh()).resolves.toBe(false);

    expect(controller.state).toMatchObject({
      status: 'failed',
      target: null,
      operationId: null,
      blockedReason: null,
      errorCode: 'INVALID_UNDO_AVAILABILITY',
    });
    expect(controller.beginRequest()).toBeNull();
    expect(controller.blocksNewTruth()).toBe(true);
  });

  it('requires exact receipts from two registered workspaces and the final global projection', async () => {
    const trace: string[] = [];
    const target = undoTarget();
    commandMocks.getAvailability
      .mockResolvedValueOnce(available(target))
      .mockResolvedValueOnce({ status: 'blocked', reason: 'latestDecisionUndone' });
    const controller = createDurableReviewUndoController();
    const review = settledProjectionAuthority('review', trace);
    const inbox = settledProjectionAuthority('inbox', trace);
    const global = settledProjectionAuthority('global', trace);
    const unregisterReview = controller.registerProjectionConsumer(review.authority);
    const unregisterInbox = controller.registerProjectionConsumer(inbox.authority);

    await controller.refresh();
    const request = controller.beginRequest();
    if (!request) throw new Error('expected Undo request');
    controller.requireProjectionReload(request, 'applied');

    await expect(controller.reconcileProjections(global.authority)).resolves.toBe(true);
    expect(trace).toEqual(['review', 'inbox', 'global']);
    expect(review.projection.receipt()).toBe(1);
    expect(inbox.projection.receipt()).toBe(1);
    expect(global.projection.receipt()).toBe(1);
    expect(review.authority.projectionReceipt).toHaveBeenCalled();
    expect(inbox.authority.projectionReceipt).toHaveBeenCalled();
    expect(global.authority.projectionReceipt).toHaveBeenCalled();
    expect(controller.pendingProjection()).toBeNull();
    expect(controller.state).toMatchObject({
      status: 'blocked',
      blockedReason: 'latestDecisionUndone',
      inFlight: false,
    });

    unregisterReview();
    unregisterInbox();
  });

  it.each(['consumer', 'global'] as const)(
    'keeps the barrier stale when a settled consumer epoch advances while a %s reload is held',
    async (heldAt) => {
      const target = undoTarget();
      commandMocks.getAvailability.mockResolvedValueOnce(available(target));
      const controller = createDurableReviewUndoController();
      const first = settledProjectionAuthority();
      const second = settledProjectionAuthority();
      const held = deferred<void>();
      const heldProjection = new ProjectionEpoch();
      const heldAuthority = {
        reloadProjection: vi.fn(async () => {
          const epoch = heldProjection.begin();
          await held.promise;
          return heldProjection.settle(epoch, true);
        }),
        projectionReceipt: vi.fn(() => heldProjection.receipt()),
      };
      const unregisterFirst = controller.registerProjectionConsumer(first.authority);
      const unregisterSecond = controller.registerProjectionConsumer(
        heldAt === 'consumer' ? heldAuthority : second.authority,
      );
      const global = heldAt === 'global' ? heldAuthority : settledProjectionAuthority().authority;

      await controller.refresh();
      const request = controller.beginRequest();
      if (!request) throw new Error('expected Undo request');
      controller.requireProjectionReload(request, 'applied');
      const reconciliation = controller.reconcileProjections(global);
      await vi.waitFor(() => expect(heldAuthority.reloadProjection).toHaveBeenCalledOnce());

      first.projection.mutate();
      held.resolve();
      await expect(reconciliation).resolves.toBe(false);
      expect(controller.pendingProjection()).toEqual({ target, outcome: 'applied' });
      expect(controller.state).toMatchObject({ status: 'projectionStale', inFlight: false });
      expect(commandMocks.getAvailability).toHaveBeenCalledOnce();

      unregisterFirst();
      unregisterSecond();
    },
  );

  it('invalidates settlements across projection registration and unregistration churn', async () => {
    const target = undoTarget();
    commandMocks.getAvailability
      .mockResolvedValueOnce(available(target))
      .mockResolvedValueOnce({ status: 'blocked', reason: 'latestDecisionUndone' });
    const controller = createDurableReviewUndoController();
    const firstGate = deferred<void>();
    const secondGate = deferred<void>();
    const firstProjection = new ProjectionEpoch();
    let firstReloadCount = 0;
    const firstAuthority = {
      reloadProjection: vi.fn(async () => {
        const epoch = firstProjection.begin();
        ++firstReloadCount;
        if (firstReloadCount === 1) await firstGate.promise;
        if (firstReloadCount === 2) await secondGate.promise;
        return firstProjection.settle(epoch, true);
      }),
      projectionReceipt: vi.fn(() => firstProjection.receipt()),
    };
    const second = settledProjectionAuthority();
    const global = settledProjectionAuthority();
    const unregisterFirst = controller.registerProjectionConsumer(firstAuthority);

    await controller.refresh();
    const request = controller.beginRequest();
    if (!request) throw new Error('expected Undo request');
    controller.requireProjectionReload(request, 'applied');

    const registrationAttempt = controller.reconcileProjections(global.authority);
    await vi.waitFor(() => expect(firstAuthority.reloadProjection).toHaveBeenCalledTimes(1));
    const unregisterSecond = controller.registerProjectionConsumer(second.authority);
    firstGate.resolve();
    await expect(registrationAttempt).resolves.toBe(false);
    expect(controller.pendingProjection()).not.toBeNull();
    expect(global.authority.reloadProjection).not.toHaveBeenCalled();

    const unregistrationAttempt = controller.reconcileProjections(global.authority);
    await vi.waitFor(() => {
      expect(firstAuthority.reloadProjection).toHaveBeenCalledTimes(2);
      expect(second.authority.reloadProjection).toHaveBeenCalledOnce();
    });
    unregisterSecond();
    secondGate.resolve();
    await expect(unregistrationAttempt).resolves.toBe(false);
    expect(controller.pendingProjection()).not.toBeNull();
    expect(global.authority.reloadProjection).not.toHaveBeenCalled();

    await expect(controller.reconcileProjections(global.authority)).resolves.toBe(true);
    expect(firstAuthority.reloadProjection).toHaveBeenCalledTimes(3);
    expect(global.authority.reloadProjection).toHaveBeenCalledOnce();
    expect(controller.pendingProjection()).toBeNull();
    unregisterFirst();
  });

  it('contains a throwing projection receipt and leaves the exact barrier closed', async () => {
    const target = undoTarget();
    commandMocks.getAvailability.mockResolvedValueOnce(available(target));
    const controller = createDurableReviewUndoController();
    const throwingAuthority = {
      reloadProjection: vi.fn(async () => 1),
      projectionReceipt: vi.fn((): number | null => {
        throw new Error('projection receipt unavailable');
      }),
    };
    const unregister = controller.registerProjectionConsumer(throwingAuthority);
    const global = settledProjectionAuthority();

    await controller.refresh();
    const request = controller.beginRequest();
    if (!request) throw new Error('expected Undo request');
    controller.requireProjectionReload(request, 'alreadyApplied');

    await expect(controller.reconcileProjections(global.authority)).resolves.toBe(false);
    expect(controller.pendingProjection()).toEqual({ target, outcome: 'alreadyApplied' });
    expect(controller.state).toMatchObject({ status: 'projectionStale', inFlight: false });
    expect(commandMocks.getAvailability).toHaveBeenCalledOnce();
    unregister();
  });

  it('rechecks exact receipts after availability awaits and refuses an intervening epoch advance', async () => {
    const target = undoTarget();
    const availabilityAfterReload = deferred<{
      status: 'blocked';
      reason: 'latestDecisionUndone';
    }>();
    commandMocks.getAvailability
      .mockResolvedValueOnce(available(target))
      .mockReturnValueOnce(availabilityAfterReload.promise);
    const controller = createDurableReviewUndoController();
    const consumer = settledProjectionAuthority();
    const global = settledProjectionAuthority();
    const unregister = controller.registerProjectionConsumer(consumer.authority);

    await controller.refresh();
    const request = controller.beginRequest();
    if (!request) throw new Error('expected Undo request');
    controller.requireProjectionReload(request, 'applied');
    const reconciliation = controller.reconcileProjections(global.authority);
    await vi.waitFor(() => expect(commandMocks.getAvailability).toHaveBeenCalledTimes(2));

    consumer.projection.mutate();
    availabilityAfterReload.resolve({ status: 'blocked', reason: 'latestDecisionUndone' });
    await expect(reconciliation).resolves.toBe(false);
    expect(controller.pendingProjection()).toEqual({ target, outcome: 'applied' });
    expect(controller.state).toMatchObject({ status: 'projectionStale', inFlight: false });
    unregister();
  });

  it('times out a hung availability read, lets e2 recover, and ignores the late e1 value', async () => {
    vi.useFakeTimers();
    const staleTarget = undoTarget();
    const recoveredTarget = undoTarget({ effectEventId: 42, segmentId: 'segment-42' });
    const lateAvailability = deferred<ReturnType<typeof available>>();
    commandMocks.getAvailability
      .mockReturnValueOnce(lateAvailability.promise)
      .mockResolvedValueOnce(available(recoveredTarget));
    const controller = createDurableReviewUndoController();

    const first = controller.refresh();
    await vi.advanceTimersByTimeAsync(REVIEW_OPERATION_TIMEOUT_MS);
    await expect(first).resolves.toBe(false);
    expect(controller.state).toMatchObject({
      status: 'failed',
      target: null,
      operationId: null,
      errorCode: 'E_UNDO_AVAILABILITY_TIMEOUT',
    });

    await expect(controller.refresh()).resolves.toBe(true);
    expect(controller.state).toMatchObject({
      status: 'ready',
      target: recoveredTarget,
      errorCode: null,
    });

    lateAvailability.resolve(available(staleTarget));
    await Promise.resolve();
    await Promise.resolve();
    expect(controller.state).toMatchObject({
      status: 'ready',
      target: recoveredTarget,
      errorCode: null,
    });
    expect(commandMocks.getAvailability).toHaveBeenCalledTimes(2);
    expect(vi.getTimerCount()).toBe(0);
  });

  it('times out a hung projection e1, settles retry e2, and rejects the late e1 receipt', async () => {
    vi.useFakeTimers();
    const target = undoTarget();
    commandMocks.getAvailability
      .mockResolvedValueOnce(available(target))
      .mockResolvedValueOnce({ status: 'blocked', reason: 'latestDecisionUndone' });
    const controller = createDurableReviewUndoController();
    const projection = new ProjectionEpoch();
    const lateProjection = deferred<void>();
    let reloadCount = 0;
    let firstLateReceipt: number | null | undefined;
    const global = {
      reloadProjection: vi.fn(async () => {
        const epoch = projection.begin();
        const attempt = ++reloadCount;
        if (attempt === 1) await lateProjection.promise;
        const receipt = projection.settle(epoch, true);
        if (attempt === 1) firstLateReceipt = receipt;
        return receipt;
      }),
      projectionReceipt: vi.fn(() => projection.receipt()),
    };

    await controller.refresh();
    const request = controller.beginRequest();
    if (!request) throw new Error('expected Undo request');
    controller.requireProjectionReload(request, 'applied');

    const first = controller.reconcileProjections(global);
    await vi.advanceTimersByTimeAsync(0);
    expect(global.reloadProjection).toHaveBeenCalledOnce();
    await vi.advanceTimersByTimeAsync(REVIEW_OPERATION_TIMEOUT_MS);
    await expect(first).resolves.toBe(false);
    expect(controller.state).toMatchObject({ status: 'projectionStale', inFlight: false });
    expect(controller.pendingProjection()).toEqual({ target, outcome: 'applied' });

    await expect(controller.reconcileProjections(global)).resolves.toBe(true);
    const recoveredReceipt = projection.receipt();
    expect(recoveredReceipt).toBe(2);
    expect(controller.state).toMatchObject({
      status: 'blocked',
      blockedReason: 'latestDecisionUndone',
      inFlight: false,
    });

    lateProjection.resolve();
    await vi.advanceTimersByTimeAsync(0);
    expect(firstLateReceipt).toBeNull();
    expect(projection.receipt()).toBe(recoveredReceipt);
    expect(controller.state).toMatchObject({
      status: 'blocked',
      blockedReason: 'latestDecisionUndone',
      inFlight: false,
    });
    expect(global.reloadProjection).toHaveBeenCalledTimes(2);
    expect(commandMocks.getAvailability).toHaveBeenCalledTimes(2);
    expect(vi.getTimerCount()).toBe(0);
  });

  it.each([
    ['mismatched target', available(undoTarget({ effectEventId: 42 }))],
    ['wrong decision', available(undoTarget({ decision: 'accept' }))],
    [
      'wrong decision operation',
      available(undoTarget({ sourceOperationId: '20000000-0000-4000-8000-000000000041' })),
    ],
    ['none', { status: 'none' as const }],
    ['blocked', { status: 'blocked' as const, reason: 'flagShadowed' as const }],
  ])(
    'does not settle truth after an exact expected target resolves as %s',
    async (_kind, result) => {
      const expectedTarget = undoTarget();
      commandMocks.getAvailability
        .mockResolvedValueOnce(available(expectedTarget))
        .mockResolvedValueOnce(result);
      const controller = createDurableReviewUndoController();
      const global = settledProjectionAuthority();

      await controller.refresh();
      const lease = controller.beginTruthWrite();
      if (!lease) throw new Error('expected truth-write lease');
      expect(controller.invalidateForNewAction(lease)).toBe(true);
      await expect(
        controller.reconcileTruthProjections(lease, global.authority, expectedUndo(expectedTarget)),
      ).resolves.toBe(false);

      expect(controller.state).toMatchObject({
        status: 'failed',
        target: null,
        operationId: null,
        errorCode: 'TRUTH_AUTHORITY_MISMATCH',
        truthProjectionPending: true,
      });
      controller.endTruthWrite(lease);
      expect(controller.blocksNewTruth()).toBe(true);
    },
  );

  it('keeps truth projection recovery pending through a failed retry and clears it on exact success', async () => {
    const target = undoTarget();
    commandMocks.getAvailability
      .mockResolvedValueOnce(available(target))
      .mockResolvedValueOnce(available(target));
    const controller = createDurableReviewUndoController();
    const projection = new ProjectionEpoch();
    let reloadCount = 0;
    const global = {
      reloadProjection: vi.fn(async () => {
        const epoch = projection.begin();
        ++reloadCount;
        if (reloadCount <= 2) {
          projection.finish(epoch, false);
          return null;
        }
        return projection.settle(epoch, true);
      }),
      projectionReceipt: vi.fn(() => projection.receipt()),
    };

    await controller.refresh();
    const lease = controller.beginTruthWrite();
    if (!lease) throw new Error('expected truth-write lease');
    expect(controller.invalidateForNewAction(lease)).toBe(true);
    await expect(
      controller.reconcileTruthProjections(lease, global, expectedUndo(target)),
    ).resolves.toBe(false);
    controller.endTruthWrite(lease);
    expect(controller.state).toMatchObject({
      status: 'failed',
      truthProjectionPending: true,
      inFlight: false,
      errorCode: 'TRUTH_PROJECTION_RELOAD_REQUIRED',
    });

    await expect(controller.retryTruthProjections(global)).resolves.toBe(false);
    expect(controller.state).toMatchObject({
      status: 'failed',
      truthProjectionPending: true,
      inFlight: false,
      errorCode: 'TRUTH_PROJECTION_RELOAD_REQUIRED',
    });

    await expect(controller.retryTruthProjections(global)).resolves.toBe(true);
    expect(controller.state).toMatchObject({
      status: 'ready',
      target,
      truthProjectionPending: false,
      truthWriteInFlight: false,
      inFlight: false,
      errorCode: null,
    });
    expect(global.reloadProjection).toHaveBeenCalledTimes(3);
    expect(commandMocks.getAvailability).toHaveBeenCalledTimes(2);
  });

  it('grants one renderer-wide truth lease and permanently hard-stops an ambiguous write', async () => {
    commandMocks.getAvailability.mockResolvedValueOnce(available(undoTarget()));
    const controller = createDurableReviewUndoController();
    await controller.refresh();

    const lease = controller.beginTruthWrite();
    if (!lease) throw new Error('expected truth-write lease');
    expect(controller.beginTruthWrite()).toBeNull();
    expect(controller.truthWriteStillCurrent(lease)).toBe(true);
    controller.markTruthWriteAmbiguous('not-the-active-lease', new Error('ignore me'));
    expect(controller.state.truthWriteAmbiguous).toBe(false);

    controller.markTruthWriteAmbiguous(lease, {
      schema: 1,
      code: 'IPC_RESPONSE_LOST',
      message: 'two exact attempts had no response',
      retryable: false,
    });
    expect(controller.state).toMatchObject({
      status: 'failed',
      target: null,
      operationId: null,
      errorCode: 'IPC_RESPONSE_LOST',
      truthWriteInFlight: false,
      truthWriteAmbiguous: true,
      truthProjectionPending: false,
    });
    expect(controller.truthWriteStillCurrent(lease)).toBe(false);
    expect(controller.beginTruthWrite()).toBeNull();
    expect(controller.invalidateForNewAction()).toBe(false);
    await expect(controller.refresh()).resolves.toBe(false);
    expect(commandMocks.getAvailability).toHaveBeenCalledOnce();
    expect(controller.blocksNewTruth()).toBe(true);
  });

  it('exports one shared coordinator identity for every workstation consumer', async () => {
    const reimported = (await import('./durableReviewUndo.svelte')).sharedDurableReviewUndo;

    expect(reimported).toBe(sharedDurableReviewUndo);
    expect(sharedDurableReviewUndo).not.toBe(createDurableReviewUndoController());
  });
});
