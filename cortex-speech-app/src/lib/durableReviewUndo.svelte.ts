import * as api from './commands';
import { publicErrorReference } from './errorText';
import { withReviewOperationTimeout } from './reviewOperationTimeout';

export { withReviewOperationTimeout } from './reviewOperationTimeout';

export type DurableReviewUndoStatus =
  'loading' | 'ready' | 'none' | 'blocked' | 'failed' | 'reconciling' | 'projectionStale';

export type ExpectedDesktopReviewUndo =
  | {
      kind: 'decision';
      effectEventId: number;
      segmentId: string;
      decision: 'accept' | 'edit' | 'reject';
      sourceOperationId: string;
    }
  | {
      kind: 'flag';
      effectEventId: number;
      segmentId: string;
      sourceOperationId: string;
      flagKind: api.DesktopReviewFlagKindV1;
    };

export interface ExactDesktopReviewUndoRequest {
  target: api.DesktopReviewUndoTargetV1;
  operationId: string;
}

export type TerminalDesktopReviewUndoStatus = 'applied' | 'alreadyApplied' | 'conflict';

/**
 * A reload receipt is valid only while `projectionReceipt()` still returns the same epoch.
 * Implementations return `null` while a projection operation is pending or its latest reload
 * failed. This closes the race where one view completed its reload, then began a newer reload while
 * another view was still settling.
 */
export interface ReviewProjectionAuthority {
  reloadProjection(): Promise<number | null>;
  projectionReceipt(): number | null;
}

type DesktopReviewUndoAvailability = Awaited<
  ReturnType<typeof api.getDesktopReviewUndoAvailabilityV1>
>;

interface ProjectionSettlement {
  registryVersion: number;
  authorities: ReviewProjectionAuthority[];
  receipts: number[];
}

const UUID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const SHA256_PATTERN = /^[0-9a-f]{64}$/i;
const UNDO_DECISIONS = new Set(['accept', 'edit', 'reject']);
const UNDO_BLOCK_REASONS = new Set([
  'legacyHistory',
  'latestDecisionUndone',
  'latestFlagUndone',
  'decisionShadowed',
  'flagShadowed',
]);

function record(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

function validFlagKind(value: unknown): value is api.DesktopReviewFlagKindV1 {
  const flagKind = record(value);
  return (
    flagKind?.kind === 'generic' ||
    (flagKind?.kind === 'technicalUnusable' &&
      typeof flagKind.reason === 'string' &&
      ['decodeFailed', 'missingFile', 'permissionDenied', 'corruptContainer'].includes(
        flagKind.reason,
      ))
  );
}

function sameFlagKind(
  left: api.DesktopReviewFlagKindV1,
  right: api.DesktopReviewFlagKindV1,
): boolean {
  if (left.kind !== right.kind) return false;
  if (left.kind === 'generic' && right.kind === 'generic') return true;
  return (
    left.kind === 'technicalUnusable' &&
    right.kind === 'technicalUnusable' &&
    left.reason === right.reason
  );
}

function validTarget(value: unknown): value is api.DesktopReviewUndoTargetV1 {
  const target = record(value);
  const validCommon =
    target !== null &&
    Number.isSafeInteger(target.effectEventId) &&
    (target.effectEventId as number) > 0 &&
    typeof target.segmentId === 'string' &&
    target.segmentId.length > 0 &&
    target.segmentId.length <= 1024 &&
    typeof target.sourceOperationId === 'string' &&
    UUID_PATTERN.test(target.sourceOperationId) &&
    typeof target.sourcePayloadHash === 'string' &&
    SHA256_PATTERN.test(target.sourcePayloadHash) &&
    Number.isSafeInteger(target.databaseGeneration) &&
    (target.databaseGeneration as number) >= 0;
  if (!validCommon) return false;
  if (target.kind === 'decision') {
    return typeof target.decision === 'string' && UNDO_DECISIONS.has(target.decision);
  }
  return (
    target.kind === 'flag' &&
    Number.isSafeInteger(target.priorRevision) &&
    (target.priorRevision as number) >= 0 &&
    Number.isSafeInteger(target.flagRevision) &&
    target.flagRevision === (target.priorRevision as number) + 1 &&
    validFlagKind(target.flagKind)
  );
}

function validatedAvailability(value: unknown): DesktopReviewUndoAvailability | null {
  const availability = record(value);
  if (!availability || typeof availability.status !== 'string') return null;
  if (availability.status === 'available') {
    return validTarget(availability.target)
      ? (availability as unknown as DesktopReviewUndoAvailability)
      : null;
  }
  if (availability.status === 'blocked') {
    return typeof availability.reason === 'string' && UNDO_BLOCK_REASONS.has(availability.reason)
      ? (availability as unknown as DesktopReviewUndoAvailability)
      : null;
  }
  return availability.status === 'none'
    ? (availability as unknown as DesktopReviewUndoAvailability)
    : null;
}

function sameAuthority(
  left: api.DesktopReviewUndoTargetV1 | null,
  right: api.DesktopReviewUndoTargetV1,
) {
  if (
    left?.effectEventId === right.effectEventId &&
    left.segmentId === right.segmentId &&
    left.kind === right.kind &&
    left.sourceOperationId === right.sourceOperationId &&
    left.sourcePayloadHash === right.sourcePayloadHash &&
    left.databaseGeneration === right.databaseGeneration
  ) {
    if (left.kind === 'decision' && right.kind === 'decision') {
      return left.decision === right.decision;
    }
    if (left.kind === 'flag' && right.kind === 'flag') {
      return (
        left.priorRevision === right.priorRevision &&
        left.flagRevision === right.flagRevision &&
        sameFlagKind(left.flagKind, right.flagKind)
      );
    }
  }
  return false;
}

/** Validate a terminal response against both the table-local effect id and its closed action kind. */
export function validatedDesktopReviewUndoOutcome(
  value: unknown,
  target: api.DesktopReviewUndoTargetV1,
): TerminalDesktopReviewUndoStatus | null {
  try {
    // A successful IPC boundary still returns an untrusted runtime value. Validation must be total:
    // a throwing getter is an ambiguous response, never proof that the Undo was unsent.
    const outcome = record(value);
    if (
      !outcome ||
      (outcome.status !== 'applied' &&
        outcome.status !== 'alreadyApplied' &&
        outcome.status !== 'conflict') ||
      outcome.effectKind !== target.kind ||
      !Number.isSafeInteger(outcome.effectEventId) ||
      outcome.effectEventId !== target.effectEventId
    ) {
      return null;
    }
    if (outcome.status === 'applied') {
      const segment = record(outcome.segment);
      if (
        segment?.id !== target.segmentId ||
        !Number.isSafeInteger(outcome.restoredRevision) ||
        (outcome.restoredRevision as number) < 0
      ) {
        return null;
      }
    }
    return outcome.status;
  } catch {
    return null;
  }
}

function sameRequest(
  state: {
    target: api.DesktopReviewUndoTargetV1 | null;
    operationId: string | null;
  },
  request: ExactDesktopReviewUndoRequest,
) {
  return state.operationId === request.operationId && sameAuthority(state.target, request.target);
}

/**
 * The renderer's view of the database-owned, restart-safe desktop Undo authority.
 *
 * Targets always come from generated typed IPC. An operation UUID is minted only when the owner
 * actually invokes Undo, then remains attached to that exact immutable target across transport
 * ambiguity, view changes, and harmless re-hydration. A known backend outcome is not considered
 * reconciled until the caller has reloaded its authoritative queue projection.
 */
export function createDurableReviewUndoController() {
  const state = $state({
    status: 'loading' as DurableReviewUndoStatus,
    target: null as api.DesktopReviewUndoTargetV1 | null,
    operationId: null as string | null,
    blockedReason: null as api.DesktopReviewUndoBlockReasonV1 | null,
    errorCode: null as string | null,
    inFlight: false,
    truthWriteInFlight: false,
    truthWriteAmbiguous: false,
    truthProjectionPending: false,
    projectionOutcome: null as TerminalDesktopReviewUndoStatus | null,
  });
  let refreshSequence = 0;
  let truthWriteLease: string | null = null;
  let projectionRegistryVersion = 0;
  let projectionReconcilePromise: Promise<boolean> | null = null;
  const projectionConsumers = new Map<symbol, ReviewProjectionAuthority>();
  let truthProjectionExpected: ExpectedDesktopReviewUndo | undefined;

  function failAvailability(errorCode: string) {
    state.status = 'failed';
    state.target = null;
    state.operationId = null;
    state.blockedReason = null;
    state.errorCode = errorCode;
    state.projectionOutcome = null;
  }

  function availabilityMatchesExpected(
    availability: DesktopReviewUndoAvailability,
    expected: ExpectedDesktopReviewUndo | undefined,
  ) {
    if (!expected) return true;
    if (
      availability.status !== 'available' ||
      availability.target.kind !== expected.kind ||
      availability.target.effectEventId !== expected.effectEventId ||
      availability.target.segmentId !== expected.segmentId ||
      availability.target.sourceOperationId !== expected.sourceOperationId
    ) {
      return false;
    }
    if (availability.target.kind === 'decision' && expected.kind === 'decision') {
      return availability.target.decision === expected.decision;
    }
    return (
      availability.target.kind === 'flag' &&
      expected.kind === 'flag' &&
      sameFlagKind(availability.target.flagKind, expected.flagKind)
    );
  }

  function applyAvailability(
    availability: DesktopReviewUndoAvailability,
    expected: ExpectedDesktopReviewUndo | undefined,
    retainedTarget: api.DesktopReviewUndoTargetV1 | null,
    retainedOperationId: string | null,
  ): boolean {
    if (availability.status === 'available') {
      const exactExpected = availabilityMatchesExpected(availability, expected);
      state.status = 'ready';
      state.target = availability.target;
      state.operationId = sameAuthority(retainedTarget, availability.target)
        ? retainedOperationId
        : null;
      state.blockedReason = null;
      state.errorCode = null;
      state.projectionOutcome = null;
      return exactExpected;
    }
    state.target = null;
    state.operationId = null;
    state.errorCode = null;
    state.projectionOutcome = null;
    if (availability.status === 'blocked') {
      state.status = 'blocked';
      state.blockedReason = availability.reason;
    } else {
      state.status = 'none';
      state.blockedReason = null;
    }
    return expected === undefined;
  }

  async function refresh(expected?: ExpectedDesktopReviewUndo): Promise<boolean> {
    // A routine mount/queue refresh must never erase an exact retry or a known mutation whose
    // projection has not yet reloaded. Those states have explicit reconciliation paths below.
    if (
      state.inFlight ||
      state.truthWriteInFlight ||
      state.truthWriteAmbiguous ||
      state.truthProjectionPending ||
      state.status === 'reconciling' ||
      state.status === 'projectionStale'
    ) {
      return false;
    }

    const sequence = ++refreshSequence;
    const retainedTarget = state.target;
    const retainedOperationId = state.operationId;
    state.status = 'loading';
    state.blockedReason = null;
    state.errorCode = null;
    state.projectionOutcome = null;
    try {
      const rawAvailability: unknown = await withReviewOperationTimeout(
        api.getDesktopReviewUndoAvailabilityV1(),
        'E_UNDO_AVAILABILITY_TIMEOUT',
      );
      if (sequence !== refreshSequence) return false;
      const availability = validatedAvailability(rawAvailability);
      if (!availability) {
        failAvailability('INVALID_UNDO_AVAILABILITY');
        return false;
      }
      return applyAvailability(availability, expected, retainedTarget, retainedOperationId);
    } catch (error) {
      if (sequence !== refreshSequence) return false;
      failAvailability(publicErrorReference(error).code ?? 'UNDO_AVAILABILITY_FAILED');
      return false;
    }
  }

  /** Reserve the one exact request before any asynchronous draft flush or IPC begins. */
  function beginRequest(): ExactDesktopReviewUndoRequest | null {
    if (
      state.inFlight ||
      state.truthWriteInFlight ||
      state.truthWriteAmbiguous ||
      (state.status !== 'ready' && state.status !== 'reconciling') ||
      !state.target
    ) {
      return null;
    }
    state.operationId ??= crypto.randomUUID();
    state.status = 'reconciling';
    state.inFlight = true;
    state.errorCode = null;
    state.projectionOutcome = null;
    return { target: state.target, operationId: state.operationId };
  }

  /** Release only a request proven never to have crossed the IPC boundary. */
  function releaseUnsent(request: ExactDesktopReviewUndoRequest) {
    if (!sameRequest(state, request)) return;
    state.inFlight = false;
    state.status = 'ready';
  }

  /** Preserve the exact request after a thrown transport or malformed response. */
  function markAmbiguous(request: ExactDesktopReviewUndoRequest, error?: unknown) {
    if (!sameRequest(state, request)) return;
    state.inFlight = false;
    state.status = 'reconciling';
    state.errorCode = publicErrorReference(error).code ?? null;
    state.projectionOutcome = null;
  }

  /** A valid terminal outcome still needs an authoritative queue reload before new truth writes. */
  function requireProjectionReload(
    request: ExactDesktopReviewUndoRequest,
    outcome: TerminalDesktopReviewUndoStatus,
  ) {
    if (!sameRequest(state, request)) return;
    state.inFlight = false;
    state.status = 'projectionStale';
    state.errorCode = null;
    state.projectionOutcome = outcome;
  }

  function pendingProjection() {
    return state.status === 'projectionStale' && state.target && state.projectionOutcome
      ? { target: state.target, outcome: state.projectionOutcome }
      : null;
  }

  function registerProjectionConsumer(authority: ReviewProjectionAuthority): () => void {
    const token = Symbol('desktop-review-projection');
    projectionConsumers.set(token, authority);
    ++projectionRegistryVersion;
    return () => {
      if (projectionConsumers.delete(token)) ++projectionRegistryVersion;
    };
  }

  async function reloadAllProjections(
    globalAuthority: ReviewProjectionAuthority,
  ): Promise<ProjectionSettlement | null> {
    const registryVersion = projectionRegistryVersion;
    const consumerAuthorities = [...projectionConsumers.values()];
    const safeReload = async (authority: ReviewProjectionAuthority) => {
      try {
        return await withReviewOperationTimeout(
          authority.reloadProjection(),
          'E_REVIEW_PROJECTION_TIMEOUT',
        );
      } catch {
        return null;
      }
    };
    // Review projections may hydrate the shared Library row. Settle them first, then make the
    // Library reload the final writer so the two authorities cannot invalidate each other by design.
    const consumerReceipts = await Promise.all(
      consumerAuthorities.map((authority) => safeReload(authority)),
    );
    if (
      registryVersion !== projectionRegistryVersion ||
      consumerReceipts.some((receipt) => receipt === null)
    )
      return null;
    const globalReceipt = await safeReload(globalAuthority);
    const authorities = [...consumerAuthorities, globalAuthority];
    const receipts = [...consumerReceipts, globalReceipt];
    if (receipts.some((receipt) => receipt === null)) return null;
    const settlement = {
      registryVersion,
      authorities,
      receipts: receipts as number[],
    };
    return settlementCurrent(settlement) ? settlement : null;
  }

  function settlementCurrent(settlement: ProjectionSettlement): boolean {
    if (settlement.registryVersion !== projectionRegistryVersion) return false;
    try {
      return settlement.receipts.every(
        (receipt, index) => settlement.authorities[index].projectionReceipt() === receipt,
      );
    } catch {
      return false;
    }
  }

  /**
   * Reload every mounted review projection plus the current global Library scope. A registration
   * change or any superseded/failed reload keeps the barrier closed and requires another attempt.
   */
  async function reconcileProjections(
    globalAuthority: ReviewProjectionAuthority,
  ): Promise<boolean> {
    if (projectionReconcilePromise) return projectionReconcilePromise;
    const pending = pendingProjection();
    if (!pending || state.inFlight) return false;
    state.inFlight = true;
    const attempt = (async () => {
      const settlement = await reloadAllProjections(globalAuthority);
      if (
        state.status !== 'projectionStale' ||
        !sameAuthority(state.target, pending.target) ||
        state.projectionOutcome !== pending.outcome ||
        !settlement
      ) {
        return false;
      }
      let availability: DesktopReviewUndoAvailability | null;
      try {
        availability = validatedAvailability(
          await withReviewOperationTimeout(
            api.getDesktopReviewUndoAvailabilityV1(),
            'E_UNDO_AVAILABILITY_TIMEOUT',
          ),
        );
      } catch (error) {
        state.errorCode = publicErrorReference(error).code ?? 'UNDO_AVAILABILITY_FAILED';
        return false;
      }
      if (!availability) {
        state.errorCode = 'INVALID_UNDO_AVAILABILITY';
        return false;
      }
      // This is the final synchronous check before the barrier opens. `inFlight` remains true during
      // the availability await, so no renderer truth writer can slip between validation and apply.
      if (
        state.status !== 'projectionStale' ||
        !sameAuthority(state.target, pending.target) ||
        state.projectionOutcome !== pending.outcome ||
        !settlementCurrent(settlement)
      ) {
        return false;
      }
      if (
        availability.status === 'available' &&
        sameAuthority(availability.target, pending.target)
      ) {
        state.errorCode = 'INVALID_UNDO_POSTCONDITION';
        return false;
      }
      ++refreshSequence;
      applyAvailability(availability, undefined, null, null);
      state.inFlight = false;
      return true;
    })().finally(() => {
      if (projectionReconcilePromise === attempt) projectionReconcilePromise = null;
      if (state.status === 'projectionStale') state.inFlight = false;
    });
    projectionReconcilePromise = attempt;
    return attempt;
  }

  /** Settle every mounted projection after any desktop review write, successful or ambiguous. */
  async function reconcileTruthProjections(
    lease: string,
    globalAuthority: ReviewProjectionAuthority,
    expected?: ExpectedDesktopReviewUndo,
  ): Promise<boolean> {
    if (truthWriteLease !== lease || !state.truthWriteInFlight) return false;
    state.truthProjectionPending = true;
    truthProjectionExpected = expected;
    const settlement = await reloadAllProjections(globalAuthority);
    if (truthWriteLease !== lease || !state.truthWriteInFlight || !settlement) {
      failAvailability('TRUTH_PROJECTION_RELOAD_REQUIRED');
      return false;
    }
    let availability: DesktopReviewUndoAvailability | null;
    try {
      availability = validatedAvailability(
        await withReviewOperationTimeout(
          api.getDesktopReviewUndoAvailabilityV1(),
          'E_UNDO_AVAILABILITY_TIMEOUT',
        ),
      );
    } catch (error) {
      failAvailability(publicErrorReference(error).code ?? 'UNDO_AVAILABILITY_FAILED');
      return false;
    }
    if (!availability) {
      failAvailability('INVALID_UNDO_AVAILABILITY');
      return false;
    }
    if (
      truthWriteLease !== lease ||
      !state.truthWriteInFlight ||
      !settlementCurrent(settlement) ||
      !availabilityMatchesExpected(availability, expected)
    ) {
      failAvailability(
        settlementCurrent(settlement)
          ? 'TRUTH_AUTHORITY_MISMATCH'
          : 'TRUTH_PROJECTION_RELOAD_REQUIRED',
      );
      return false;
    }
    ++refreshSequence;
    applyAvailability(availability, expected, null, null);
    state.truthProjectionPending = false;
    truthProjectionExpected = undefined;
    endTruthWrite(lease);
    return true;
  }

  async function retryTruthProjections(
    globalAuthority: ReviewProjectionAuthority,
  ): Promise<boolean> {
    if (!state.truthProjectionPending || state.inFlight || state.truthWriteInFlight) return false;
    state.inFlight = true;
    try {
      const settlement = await reloadAllProjections(globalAuthority);
      if (!settlement) {
        failAvailability('TRUTH_PROJECTION_RELOAD_REQUIRED');
        return false;
      }
      let availability: DesktopReviewUndoAvailability | null;
      try {
        availability = validatedAvailability(
          await withReviewOperationTimeout(
            api.getDesktopReviewUndoAvailabilityV1(),
            'E_UNDO_AVAILABILITY_TIMEOUT',
          ),
        );
      } catch (error) {
        failAvailability(publicErrorReference(error).code ?? 'UNDO_AVAILABILITY_FAILED');
        return false;
      }
      if (!availability) {
        failAvailability('INVALID_UNDO_AVAILABILITY');
        return false;
      }
      const expected = truthProjectionExpected;
      if (!settlementCurrent(settlement) || !availabilityMatchesExpected(availability, expected)) {
        failAvailability(
          settlementCurrent(settlement)
            ? 'TRUTH_AUTHORITY_MISMATCH'
            : 'TRUTH_PROJECTION_RELOAD_REQUIRED',
        );
        return false;
      }
      ++refreshSequence;
      applyAvailability(availability, expected, null, null);
      state.truthProjectionPending = false;
      truthProjectionExpected = undefined;
      state.inFlight = false;
      return true;
    } finally {
      if (state.truthProjectionPending) state.inFlight = false;
    }
  }

  /** Retained only as a fail-closed compatibility surface; reconciliation requires all authorities. */
  async function finishProjectionReload(): Promise<boolean> {
    return false;
  }

  function statusBlocksNewTruth(): boolean {
    return (
      state.status === 'loading' ||
      state.status === 'failed' ||
      state.status === 'reconciling' ||
      state.status === 'projectionStale'
    );
  }

  function blocksNewTruth(): boolean {
    return state.truthWriteInFlight || state.truthWriteAmbiguous || statusBlocksNewTruth();
  }

  /** A mounted review surface owns the only visible outcome/focus authority for these states. */
  function blocksSurfaceTransition(): boolean {
    return (
      state.inFlight ||
      state.truthWriteInFlight ||
      state.truthWriteAmbiguous ||
      state.truthProjectionPending ||
      state.status === 'reconciling' ||
      state.status === 'projectionStale'
    );
  }

  /** Mutually exclusive renderer-wide lease held across draft/playback awaits and the writer IPC. */
  function beginTruthWrite(): string | null {
    if (state.inFlight || truthWriteLease || state.truthWriteAmbiguous || statusBlocksNewTruth())
      return null;
    truthWriteLease = crypto.randomUUID();
    state.truthWriteInFlight = true;
    return truthWriteLease;
  }

  function truthWriteStillCurrent(lease: string): boolean {
    return (
      truthWriteLease === lease &&
      state.truthWriteInFlight &&
      !state.inFlight &&
      !statusBlocksNewTruth()
    );
  }

  function endTruthWrite(lease: string) {
    if (truthWriteLease !== lease) return;
    truthWriteLease = null;
    state.truthWriteInFlight = false;
  }

  /**
   * Two byte-identical transport attempts failed or returned a malformed success. The database may
   * have committed, so no different truth operation is safe in this renderer process. A restart is
   * the recovery boundary: no worker survives it and startup re-reads database authority.
   */
  function markTruthWriteAmbiguous(lease: string, error?: unknown) {
    if (truthWriteLease !== lease || !state.truthWriteInFlight) return;
    ++refreshSequence;
    state.truthWriteAmbiguous = true;
    state.truthProjectionPending = false;
    truthProjectionExpected = undefined;
    failAvailability(publicErrorReference(error).code ?? 'TRUTH_WRITE_OUTCOME_UNKNOWN');
    endTruthWrite(lease);
  }

  function invalidateForNewAction(lease?: string): boolean {
    if (
      state.truthWriteAmbiguous ||
      statusBlocksNewTruth() ||
      (truthWriteLease !== null && truthWriteLease !== lease)
    )
      return false;
    ++refreshSequence;
    state.status = 'loading';
    state.target = null;
    state.operationId = null;
    state.blockedReason = null;
    state.errorCode = null;
    state.inFlight = false;
    state.projectionOutcome = null;
    return true;
  }

  return {
    state,
    refresh,
    beginRequest,
    releaseUnsent,
    markAmbiguous,
    requireProjectionReload,
    pendingProjection,
    registerProjectionConsumer,
    reconcileProjections,
    reconcileTruthProjections,
    retryTruthProjections,
    finishProjectionReload,
    invalidateForNewAction,
    blocksNewTruth,
    blocksSurfaceTransition,
    beginTruthWrite,
    truthWriteStillCurrent,
    endTruthWrite,
    markTruthWriteAmbiguous,
  };
}

/** One process-wide authority shared by Review and Inbox, which can be mounted simultaneously. */
export const sharedDurableReviewUndo = createDurableReviewUndoController();

export type DurableReviewUndoController = ReturnType<typeof createDurableReviewUndoController>;
