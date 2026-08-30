import { publicBatchHaltCode, type BatchEventContext, type BatchOperationKind } from './events';

export type BatchRunDispositionWire = 'completed' | 'halted' | 'cancelled' | 'panicked';

export interface BatchRunOutcomeWire {
  disposition: BatchRunDispositionWire;
  total: number;
  succeeded: number;
  failed: number;
  skipped: number;
  abandoned: number;
  cancelled: boolean;
  errorCode: string | null;
}

export interface BatchRunStatusWire {
  operationId: string;
  operation: BatchOperationKind | null;
  status: 'starting' | 'running' | 'settled' | 'rejected' | 'unknown';
  total: number | null;
  outcome: BatchRunOutcomeWire | null;
}

export type BatchStartReconciliation = {
  disposition:
    'stale' | 'starting' | 'running' | 'settled' | 'rejected' | 'uncertain' | 'outcome-unknown';
  outcome?: BatchRunOutcomeWire;
};

type ReconciliationOptions = {
  context: BatchEventContext;
  getStatus: (operationId: string) => Promise<unknown>;
  delayBeforeRetry?: (attempt: number) => Promise<void>;
  attemptTimeoutMs?: number;
};

const closedStatuses = new Set(['starting', 'running', 'settled', 'rejected', 'unknown']);
const closedDispositions = new Set<BatchRunDispositionWire>([
  'completed',
  'halted',
  'cancelled',
  'panicked',
]);
function count(value: unknown): number | null {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0 ? value : null;
}

function exactOutcome(
  value: unknown,
  context: Pick<BatchEventContext, 'expectedTotal'>,
): BatchRunOutcomeWire | null {
  if (!value || typeof value !== 'object') return null;
  const candidate = value as Record<string, unknown>;
  if (!closedDispositions.has(candidate.disposition as BatchRunDispositionWire)) return null;
  const total = count(candidate.total);
  const succeeded = count(candidate.succeeded);
  const failed = count(candidate.failed);
  const skipped = count(candidate.skipped);
  const abandoned = count(candidate.abandoned);
  if (
    total === null ||
    total !== context.expectedTotal ||
    succeeded === null ||
    failed === null ||
    skipped === null ||
    abandoned === null ||
    succeeded + failed + skipped + abandoned !== total ||
    typeof candidate.cancelled !== 'boolean' ||
    (candidate.errorCode !== null && typeof candidate.errorCode !== 'string')
  ) {
    return null;
  }
  const disposition = candidate.disposition as BatchRunDispositionWire;
  const errorCode = candidate.errorCode as string | null;
  if (
    disposition === 'completed' &&
    (candidate.cancelled || errorCode !== null || failed !== 0 || abandoned !== 0)
  ) {
    return null;
  }
  if (disposition === 'cancelled' && (!candidate.cancelled || errorCode !== null)) return null;
  if (
    disposition === 'halted' &&
    (candidate.cancelled ||
      !errorCode ||
      errorCode === 'BATCH_WORKER_PANICKED' ||
      !publicBatchHaltCode(errorCode))
  ) {
    return null;
  }
  if (
    disposition === 'panicked' &&
    (candidate.cancelled || errorCode !== 'BATCH_WORKER_PANICKED')
  ) {
    return null;
  }
  return {
    disposition,
    total,
    succeeded,
    failed,
    skipped,
    abandoned,
    cancelled: candidate.cancelled,
    errorCode,
  };
}

export function exactBatchRunStatus(
  value: unknown,
  context: Pick<BatchEventContext, 'operationId' | 'operation' | 'expectedTotal'>,
): BatchRunStatusWire | null {
  if (!value || typeof value !== 'object') return null;
  const candidate = value as Record<string, unknown>;
  if (
    candidate.operationId !== context.operationId ||
    !closedStatuses.has(String(candidate.status))
  ) {
    return null;
  }
  const status = candidate.status as BatchRunStatusWire['status'];
  if (status === 'unknown') {
    return candidate.operation === null && candidate.total === null && candidate.outcome === null
      ? { operationId: context.operationId, operation: null, status, total: null, outcome: null }
      : null;
  }
  if (candidate.operation !== context.operation) return null;
  const total = candidate.total === null ? null : count(candidate.total);
  if (status === 'rejected') {
    return candidate.total === null && candidate.outcome === null
      ? {
          operationId: context.operationId,
          operation: context.operation,
          status,
          total: null,
          outcome: null,
        }
      : null;
  }
  if (total !== context.expectedTotal) return null;
  const outcome = candidate.outcome === null ? null : exactOutcome(candidate.outcome, context);
  if (candidate.outcome !== null && !outcome) return null;
  if ((status === 'starting' || status === 'running') && outcome !== null) return null;
  if (status === 'settled' && outcome === null) return null;
  return {
    operationId: context.operationId,
    operation: context.operation,
    status,
    total,
    outcome,
  };
}

async function boundedAttempt<T>(promise: Promise<T>, timeoutMs: number): Promise<T> {
  let timeout: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_, reject) => {
        timeout = setTimeout(() => reject(new Error('Batch status check timed out')), timeoutMs);
      }),
    ]);
  } finally {
    if (timeout) clearTimeout(timeout);
  }
}

/** Admission commands should return immediately. A timeout never means rejection: the exact status
 * authority decides whether native work was admitted. */
export async function boundedBatchCommandResponse(
  promise: Promise<unknown>,
  context: BatchEventContext,
  timeoutMs = 5_000,
): Promise<void> {
  const value = await boundedAttempt(promise, timeoutMs);
  if (!value || typeof value !== 'object') throw new Error('Invalid batch start response');
  const response = value as Record<string, unknown>;
  if (
    response.status !== 'started' ||
    response.operationId !== context.operationId ||
    response.operation !== context.operation
  ) {
    throw new Error('Invalid batch start response');
  }
}

export async function reconcileBatchStartResponse({
  context,
  getStatus,
  delayBeforeRetry = (attempt) =>
    new Promise<void>((resolve) => setTimeout(resolve, attempt === 1 ? 50 : 150)),
  attemptTimeoutMs = 2_000,
}: ReconciliationOptions): Promise<BatchStartReconciliation> {
  if (!context.isCurrent()) return { disposition: 'stale' };

  for (let attempt = 1; attempt <= 3; attempt += 1) {
    try {
      const wire = exactBatchRunStatus(
        await boundedAttempt(getStatus(context.operationId), attemptTimeoutMs),
        context,
      );
      if (!context.isCurrent()) return { disposition: 'stale' };
      if (!wire) throw new Error('Invalid batch run status response');
      if (wire.status === 'starting' || wire.status === 'running') {
        return { disposition: wire.status };
      }
      if (wire.status === 'settled' && wire.outcome) {
        return { disposition: 'settled', outcome: wire.outcome };
      }
      if (context.hasSettledEvent()) return { disposition: 'outcome-unknown' };
      if (context.hasObservedEvent()) return { disposition: 'uncertain' };
      if (wire.status === 'rejected') return { disposition: 'rejected' };
      if (attempt < 3) await delayBeforeRetry(attempt);
      else return { disposition: 'uncertain' };
    } catch {
      if (!context.isCurrent()) return { disposition: 'stale' };
      if (attempt < 3) await delayBeforeRetry(attempt);
    }
  }

  if (!context.isCurrent()) return { disposition: 'stale' };
  return context.hasSettledEvent()
    ? { disposition: 'outcome-unknown' }
    : { disposition: 'uncertain' };
}
