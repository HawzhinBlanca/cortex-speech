import { describe, expect, it, vi } from 'vitest';
import type { BatchEventContext } from './events';
import {
  boundedBatchCommandResponse,
  reconcileBatchStartResponse,
  type BatchRunOutcomeWire,
} from './batchStartReconciliation';

const OPERATION_ID = '00000000-0000-4000-8000-000000000001';

function context(overrides: Partial<BatchEventContext> = {}): BatchEventContext {
  return {
    operationId: OPERATION_ID,
    operation: 'transcribe',
    expectedTotal: 2,
    generation: 1,
    isCurrent: () => true,
    hasObservedEvent: () => false,
    hasTerminalEvent: () => false,
    terminalEvent: () => null,
    hasSettledEvent: () => false,
    ...overrides,
  };
}

const completed: BatchRunOutcomeWire = {
  disposition: 'completed',
  total: 2,
  succeeded: 2,
  failed: 0,
  skipped: 0,
  abandoned: 0,
  cancelled: false,
  errorCode: null,
};

describe('batch start reconciliation', () => {
  it('accepts only the exact start response identity and kind', async () => {
    await expect(
      boundedBatchCommandResponse(
        Promise.resolve({
          status: 'started',
          operationId: OPERATION_ID,
          operation: 'transcribe',
        }),
        context(),
      ),
    ).resolves.toBeUndefined();
    await expect(
      boundedBatchCommandResponse(
        Promise.resolve({ status: 'started', operationId: OPERATION_ID, operation: 'normalize' }),
        context(),
      ),
    ).rejects.toThrow('Invalid batch start response');
  });

  it('bounds a lost admission response without classifying it as rejection', async () => {
    await expect(
      boundedBatchCommandResponse(new Promise<never>(() => undefined), context(), 1),
    ).rejects.toThrow('timed out');
  });

  it('returns exact starting, running, and outcome-bearing settled authority', async () => {
    await expect(
      reconcileBatchStartResponse({
        context: context(),
        getStatus: async () => ({
          operationId: OPERATION_ID,
          operation: 'transcribe',
          status: 'starting',
          total: 2,
          outcome: null,
        }),
      }),
    ).resolves.toEqual({ disposition: 'starting' });

    await expect(
      reconcileBatchStartResponse({
        context: context(),
        getStatus: async () => ({
          operationId: OPERATION_ID,
          operation: 'transcribe',
          status: 'running',
          total: 2,
          outcome: null,
        }),
      }),
    ).resolves.toEqual({ disposition: 'running' });

    await expect(
      reconcileBatchStartResponse({
        context: context(),
        getStatus: async () => ({
          operationId: OPERATION_ID,
          operation: 'transcribe',
          status: 'settled',
          total: 2,
          outcome: completed,
        }),
      }),
    ).resolves.toEqual({ disposition: 'settled', outcome: completed });
  });

  it('retains explicit panic truth when the terminal event was lost', async () => {
    const panicked: BatchRunOutcomeWire = {
      disposition: 'panicked',
      total: 2,
      succeeded: 0,
      failed: 0,
      skipped: 0,
      abandoned: 2,
      cancelled: false,
      errorCode: 'BATCH_WORKER_PANICKED',
    };
    await expect(
      reconcileBatchStartResponse({
        context: context(),
        getStatus: async () => ({
          operationId: OPERATION_ID,
          operation: 'transcribe',
          status: 'settled',
          total: 2,
          outcome: panicked,
        }),
      }),
    ).resolves.toEqual({ disposition: 'settled', outcome: panicked });
  });

  it('fails closed on mismatched identity, kind, malformed counters, or missing settled outcome', async () => {
    const invalid = [
      {
        operationId: '00000000-0000-4000-8000-000000000099',
        operation: 'transcribe',
        status: 'settled',
        total: 2,
        outcome: completed,
      },
      {
        operationId: OPERATION_ID,
        operation: 'normalize',
        status: 'settled',
        total: 2,
        outcome: completed,
      },
      {
        operationId: OPERATION_ID,
        operation: 'transcribe',
        status: 'starting',
        total: 2,
        outcome: completed,
      },
      {
        operationId: OPERATION_ID,
        operation: 'transcribe',
        status: 'starting',
        total: null,
        outcome: null,
      },
      {
        operationId: OPERATION_ID,
        operation: 'transcribe',
        status: 'settled',
        total: 2,
        outcome: { ...completed, succeeded: 3 },
      },
      {
        operationId: OPERATION_ID,
        operation: 'transcribe',
        status: 'settled',
        total: 2,
        outcome: { ...completed, total: 1, succeeded: 1 },
      },
      {
        operationId: OPERATION_ID,
        operation: 'transcribe',
        status: 'settled',
        total: 2,
        outcome: { ...completed, succeeded: 1, failed: 1 },
      },
      {
        operationId: OPERATION_ID,
        operation: 'transcribe',
        status: 'settled',
        total: 2,
        outcome: { ...completed, succeeded: 1 },
      },
      {
        operationId: OPERATION_ID,
        operation: 'transcribe',
        status: 'settled',
        total: 2,
        outcome: null,
      },
      {
        operationId: OPERATION_ID,
        operation: 'transcribe',
        status: 'rejected',
        outcome: null,
      },
    ];
    for (const value of invalid) {
      await expect(
        reconcileBatchStartResponse({
          context: context(),
          getStatus: async () => value,
          delayBeforeRetry: async () => undefined,
          attemptTimeoutMs: 10,
        }),
      ).resolves.toEqual({ disposition: 'uncertain' });
    }
  });

  it('accepts rejection only without contradictory exact event evidence', async () => {
    const rejected = {
      operationId: OPERATION_ID,
      operation: 'transcribe',
      status: 'rejected',
      total: null,
      outcome: null,
    };
    await expect(
      reconcileBatchStartResponse({ context: context(), getStatus: async () => rejected }),
    ).resolves.toEqual({ disposition: 'rejected' });
    await expect(
      reconcileBatchStartResponse({
        context: context({ hasObservedEvent: () => true }),
        getStatus: async () => rejected,
      }),
    ).resolves.toEqual({ disposition: 'uncertain' });
  });

  it('never treats physical settlement alone as proof of the terminal outcome', async () => {
    const getStatus = vi.fn().mockRejectedValue(new Error('lost'));
    await expect(
      reconcileBatchStartResponse({
        context: context({ hasObservedEvent: () => true, hasSettledEvent: () => true }),
        getStatus,
        delayBeforeRetry: async () => undefined,
      }),
    ).resolves.toEqual({ disposition: 'outcome-unknown' });
    expect(getStatus).toHaveBeenCalledTimes(3);
  });

  it('drops a late status response after a newer batch takes authority', async () => {
    let current = true;
    await expect(
      reconcileBatchStartResponse({
        context: context({ isCurrent: () => current }),
        getStatus: async () => {
          current = false;
          return {
            operationId: OPERATION_ID,
            operation: 'transcribe',
            status: 'running',
            total: 2,
            outcome: null,
          };
        },
      }),
    ).resolves.toEqual({ disposition: 'stale' });
  });
});
