import type { ImportEventContext } from './events';

export type ImportStartDisposition = 'stale' | 'running' | 'settled' | 'rejected' | 'uncertain';

export interface ImportRunStatusWire {
  runId: string;
  status: 'running' | 'settled' | 'rejected' | 'unknown';
}

type ReconciliationOptions = {
  context: ImportEventContext;
  getStatus: (runId: string) => Promise<unknown>;
  delayBeforeRetry?: (attempt: number) => Promise<void>;
  attemptTimeoutMs?: number;
};

const closedStatuses = new Set(['running', 'settled', 'rejected', 'unknown']);

function exactStatus(value: unknown, runId: string): ImportRunStatusWire | null {
  if (!value || typeof value !== 'object') return null;
  try {
    // Snapshot untrusted IPC fields once. Returning the original object let a stateful getter pass
    // validation as `running` and later read as `rejected`, incorrectly turning an ambiguous start
    // into a definite non-admission and clearing its exact run scope.
    const candidate = value as Record<string, unknown>;
    const candidateRunId = candidate.runId;
    const candidateStatus = candidate.status;
    if (
      candidateRunId !== runId ||
      typeof candidateStatus !== 'string' ||
      !closedStatuses.has(candidateStatus)
    ) {
      return null;
    }
    return { runId: candidateRunId, status: candidateStatus as ImportRunStatusWire['status'] };
  } catch {
    return null;
  }
}

async function boundedAttempt<T>(promise: Promise<T>, timeoutMs: number): Promise<T> {
  let timeout: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_, reject) => {
        timeout = setTimeout(() => reject(new Error('Import status check timed out')), timeoutMs);
      }),
    ]);
  } finally {
    if (timeout) clearTimeout(timeout);
  }
}

/** Bound commands that should return immediately after worker admission (file import and resume).
 * The underlying desktop invocation may still complete later; exact run status, not this timeout,
 * decides whether work was accepted. */
export function boundedImportCommandResponse<T>(
  promise: Promise<T>,
  timeoutMs = 5_000,
): Promise<T> {
  return boundedAttempt(promise, timeoutMs);
}

/**
 * Reconcile a rejected import IPC response against backend admission truth. A transport rejection is
 * not proof that the command was refused: Windows/Tauri can lose the response after the worker was
 * admitted and began emitting events. Only an exact backend `rejected` response with no event
 * evidence is safe to treat as a definite failure; `unknown` can still mean the async command is
 * pending before admission.
 */
export async function reconcileImportStartResponse({
  context,
  getStatus,
  delayBeforeRetry = (attempt) =>
    new Promise<void>((resolve) => setTimeout(resolve, attempt === 1 ? 50 : 150)),
  attemptTimeoutMs = 2_000,
}: ReconciliationOptions): Promise<ImportStartDisposition> {
  if (!context.isCurrent()) return 'stale';

  for (let attempt = 1; attempt <= 3; attempt += 1) {
    try {
      const wire = exactStatus(
        await boundedAttempt(getStatus(context.runId), attemptTimeoutMs),
        context.runId,
      );
      if (!context.isCurrent()) return 'stale';
      if (!wire) throw new Error('Invalid import run status response');

      if (wire.status === 'running') return 'running';
      if (wire.status === 'settled') return 'settled';

      // An exact event from this caller-created scope contradicts rejected/unknown admission. Never
      // clear the operation on that contradiction; a later status/settlement can resolve it safely.
      if (context.hasTerminalEvent()) return 'settled';
      if (context.hasObservedEvent()) return 'uncertain';
      if (wire.status === 'rejected') return 'rejected';
      // `unknown` is not rejection: the async directory command can still be pending in the native
      // picker before admission, or an older compatible backend may not have registered the run yet.
      // Retry the bounded window, then retain the operation fail-closed.
      if (attempt < 3) await delayBeforeRetry(attempt);
      else return 'uncertain';
    } catch {
      if (!context.isCurrent()) return 'stale';
      if (attempt < 3) await delayBeforeRetry(attempt);
    }
  }

  if (!context.isCurrent()) return 'stale';
  if (context.hasTerminalEvent()) return 'settled';
  return 'uncertain';
}
