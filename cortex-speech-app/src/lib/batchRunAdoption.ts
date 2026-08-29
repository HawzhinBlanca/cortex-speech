import type { BatchOperationKind } from './events';
import { exactBatchRunStatus, type BatchRunOutcomeWire } from './batchStartReconciliation';
import { t, type TranslationKey } from './i18n';
import { get } from 'svelte/store';
import { notifications } from './stores/notificationStore';

export type AdoptableBatchRun =
  | {
      operationId: string;
      operation: BatchOperationKind;
      total: number;
      status: 'starting' | 'running';
      outcome: null;
    }
  | {
      operationId: string;
      operation: BatchOperationKind;
      total: number;
      status: 'settled';
      outcome: BatchRunOutcomeWire;
    };

export type BatchRunAdoptionOptions = {
  query: () => Promise<unknown>;
  isOccupied: () => boolean;
  setDiscoveryLock: (locked: boolean) => void;
  activate: (run: AdoptableBatchRun) => void;
};

function tr(key: TranslationKey): string {
  return get(t)(key);
}

function exactAdoptableBatchRun(value: unknown): AdoptableBatchRun | null {
  try {
    if (!value || typeof value !== 'object') return null;
    const candidate = value as Record<string, unknown>;
    if (
      typeof candidate.operationId !== 'string' ||
      !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(
        candidate.operationId,
      ) ||
      (candidate.operation !== 'transcribe' && candidate.operation !== 'normalize') ||
      (candidate.status !== 'starting' &&
        candidate.status !== 'running' &&
        candidate.status !== 'settled') ||
      !Number.isSafeInteger(candidate.total) ||
      (candidate.total as number) < 1 ||
      (candidate.total as number) > 100_000
    ) {
      return null;
    }
    const exact = exactBatchRunStatus(value, {
      operationId: candidate.operationId,
      operation: candidate.operation,
      expectedTotal: candidate.total as number,
    });
    if (
      !exact ||
      (exact.status !== 'starting' && exact.status !== 'running' && exact.status !== 'settled')
    ) {
      return null;
    }
    if (exact.status === 'settled') {
      if (!exact.outcome) return null;
      return {
        operationId: exact.operationId,
        operation: candidate.operation,
        total: candidate.total as number,
        status: 'settled',
        outcome: exact.outcome,
      };
    }
    return {
      operationId: exact.operationId,
      operation: candidate.operation,
      total: candidate.total as number,
      status: exact.status,
      outcome: null,
    };
  } catch {
    return null;
  }
}

async function boundedQuery<T>(promise: Promise<T>, timeoutMs = 2_000): Promise<T> {
  let timeout: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_, reject) => {
        timeout = setTimeout(
          () => reject(new Error('Active batch discovery timed out')),
          timeoutMs,
        );
      }),
    ]);
  } finally {
    if (timeout) clearTimeout(timeout);
  }
}

/** Reconstructs one strict starting, running, or just-settled process-local authority. Unknown
 * transport state owns a temporary lock so a remounted renderer cannot start duplicate work. */
export function createBatchRunAdoption(options: BatchRunAdoptionOptions) {
  let retryTimer: ReturnType<typeof setTimeout> | null = null;
  let pending: Promise<boolean> | null = null;
  let blocked = false;
  let ownsLock = false;
  let failureKind: 'unavailable' | 'malformed' | null = null;
  let failureNoticeId: string | null = null;
  let generation = 0;
  let destroyed = false;

  function clearRetry() {
    if (retryTimer) clearTimeout(retryTimer);
    retryTimer = null;
  }

  function clearFailure() {
    failureKind = null;
    if (failureNoticeId) notifications.dismiss(failureNoticeId);
    failureNoticeId = null;
  }

  function releaseLock() {
    blocked = false;
    if (!ownsLock) return;
    ownsLock = false;
    options.setDiscoveryLock(false);
  }

  function scheduleRetry(delayMs = 5_000) {
    if (destroyed || retryTimer) return;
    retryTimer = setTimeout(() => {
      retryTimer = null;
      void adoptActive();
    }, delayMs);
  }

  function notifyFailure(kind: 'unavailable' | 'malformed', cause?: unknown) {
    if (failureKind === kind) return;
    clearFailure();
    failureKind = kind;
    const malformed = kind === 'malformed';
    failureNoticeId = notifications.error(
      tr(malformed ? 'batch.adoptionMalformed' : 'batch.adoptionUnavailable'),
      {
        ...(cause ? { cause } : {}),
        publicDetail: tr(
          malformed ? 'batch.adoptionMalformedDetail' : 'batch.adoptionUnavailableDetail',
        ),
        action: {
          label: tr('retry'),
          handler: () => {
            clearRetry();
            void adoptActive();
          },
        },
      },
    );
  }

  async function discover(expectedGeneration: number): Promise<unknown> {
    let lastError: unknown = new Error('Active batch discovery failed');
    for (let attempt = 1; attempt <= 3; attempt += 1) {
      if (destroyed || expectedGeneration !== generation) return null;
      try {
        const result = await boundedQuery(options.query());
        if (destroyed || expectedGeneration !== generation) return null;
        return result;
      } catch (error) {
        lastError = error;
        if (destroyed || expectedGeneration !== generation) return null;
        if (attempt < 3) {
          await new Promise<void>((resolve) => setTimeout(resolve, attempt === 1 ? 50 : 150));
          if (destroyed || expectedGeneration !== generation) return null;
        }
      }
    }
    throw lastError;
  }

  async function run(): Promise<boolean> {
    if (destroyed || (options.isOccupied() && !ownsLock)) return false;
    clearRetry();
    const expectedGeneration = generation;
    blocked = true;
    ownsLock = true;
    options.setDiscoveryLock(true);

    let response: unknown;
    try {
      response = await discover(expectedGeneration);
    } catch (error) {
      if (destroyed || expectedGeneration !== generation) return false;
      notifyFailure('unavailable', error);
      scheduleRetry();
      return false;
    }
    if (destroyed || expectedGeneration !== generation) return false;
    if (response === null) {
      clearFailure();
      releaseLock();
      return false;
    }

    const active = exactAdoptableBatchRun(response);
    if (!active) {
      notifyFailure('malformed');
      scheduleRetry();
      return false;
    }
    try {
      options.activate(active);
    } catch (error) {
      options.setDiscoveryLock(true);
      notifyFailure('malformed', error);
      scheduleRetry();
      return false;
    }
    blocked = false;
    ownsLock = false;
    clearFailure();
    return true;
  }

  function adoptActive(): Promise<boolean> {
    if (destroyed) return Promise.resolve(false);
    if (pending) return pending;
    const attempt = run();
    pending = attempt;
    void attempt.then(
      () => {
        if (pending === attempt) pending = null;
      },
      () => {
        if (pending === attempt) pending = null;
      },
    );
    return attempt;
  }

  return {
    adoptActive,
    blocksStart: () => pending !== null || blocked,
    destroy: () => {
      if (destroyed) return;
      destroyed = true;
      generation += 1;
      clearRetry();
      clearFailure();
      releaseLock();
    },
  };
}
