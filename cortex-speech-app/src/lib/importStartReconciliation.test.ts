import { describe, expect, it, vi } from 'vitest';
import type { ImportEventContext } from './events';
import {
  boundedImportCommandResponse,
  reconcileImportStartResponse,
} from './importStartReconciliation';

function context(overrides: Partial<ImportEventContext> = {}): ImportEventContext {
  return {
    runId: '00000000-0000-4000-8000-000000000001',
    source: 'directory',
    generation: 1,
    isCurrent: () => true,
    hasObservedEvent: () => false,
    hasTerminalEvent: () => false,
    ...overrides,
  };
}

describe('reconcileImportStartResponse', () => {
  it('bounds a never-returning immediate-admission command response', async () => {
    await expect(
      boundedImportCommandResponse(new Promise<never>(() => undefined), 1),
    ).rejects.toThrow('timed out');
  });

  it.each(['running', 'settled'] as const)('accepts exact backend %s authority', async (status) => {
    await expect(
      reconcileImportStartResponse({
        context: context(),
        getStatus: async (runId) => ({ runId, status }),
      }),
    ).resolves.toBe(status);
  });

  it('treats an exact rejection without event evidence as definite', async () => {
    await expect(
      reconcileImportStartResponse({
        context: context(),
        getStatus: async (runId) => ({ runId, status: 'rejected' }),
      }),
    ).resolves.toBe('rejected');
  });

  it('fails closed when event evidence contradicts rejected authority', async () => {
    await expect(
      reconcileImportStartResponse({
        context: context({ hasObservedEvent: () => true }),
        getStatus: async (runId) => ({ runId, status: 'rejected' }),
      }),
    ).resolves.toBe('uncertain');
  });

  it('never treats unknown authority as a definite rejection', async () => {
    const getStatus = vi.fn(async (runId: string) => ({ runId, status: 'unknown' }));
    const delayBeforeRetry = vi.fn().mockResolvedValue(undefined);
    await expect(
      reconcileImportStartResponse({ context: context(), getStatus, delayBeforeRetry }),
    ).resolves.toBe('uncertain');
    expect(getStatus).toHaveBeenCalledTimes(3);
    expect(delayBeforeRetry).toHaveBeenCalledTimes(2);
  });

  it('uses an exact terminal event as accepted settlement when status is unavailable', async () => {
    await expect(
      reconcileImportStartResponse({
        context: context({ hasObservedEvent: () => true, hasTerminalEvent: () => true }),
        getStatus: async () => {
          throw new Error('response lost');
        },
        delayBeforeRetry: async () => undefined,
      }),
    ).resolves.toBe('settled');
  });

  it('keeps a completion-only run uncertain when settlement authority is unavailable', async () => {
    await expect(
      reconcileImportStartResponse({
        context: context({ hasObservedEvent: () => true, hasTerminalEvent: () => false }),
        getStatus: async () => {
          throw new Error('status unavailable');
        },
        delayBeforeRetry: async () => undefined,
      }),
    ).resolves.toBe('uncertain');
  });

  it('retries status three times then retains an uncertain operation fail-closed', async () => {
    const getStatus = vi.fn().mockRejectedValue(new Error('offline'));
    const delayBeforeRetry = vi.fn().mockResolvedValue(undefined);
    await expect(
      reconcileImportStartResponse({ context: context(), getStatus, delayBeforeRetry }),
    ).resolves.toBe('uncertain');
    expect(getStatus).toHaveBeenCalledTimes(3);
    expect(delayBeforeRetry).toHaveBeenCalledTimes(2);
  });

  it('times out a never-settling status IPC and still completes bounded reconciliation', async () => {
    const getStatus = vi.fn(() => new Promise<never>(() => undefined));
    await expect(
      reconcileImportStartResponse({
        context: context(),
        getStatus,
        attemptTimeoutMs: 1,
        delayBeforeRetry: async () => undefined,
      }),
    ).resolves.toBe('uncertain');
    expect(getStatus).toHaveBeenCalledTimes(3);
  });

  it('rejects mismatched run identities as untrusted wire data', async () => {
    await expect(
      reconcileImportStartResponse({
        context: context(),
        getStatus: async () => ({
          runId: '00000000-0000-4000-8000-000000000099',
          status: 'settled',
        }),
        delayBeforeRetry: async () => undefined,
      }),
    ).resolves.toBe('uncertain');
  });

  it('snapshots status once so a hostile getter cannot forge a definite rejection', async () => {
    let statusReads = 0;
    const wire = new Proxy(
      { runId: context().runId },
      {
        get(target, property, receiver) {
          if (property === 'status') {
            statusReads += 1;
            return statusReads % 2 === 1 ? 'running' : 'rejected';
          }
          return Reflect.get(target, property, receiver);
        },
      },
    );

    await expect(
      reconcileImportStartResponse({
        context: context(),
        getStatus: async () => wire,
      }),
    ).resolves.toBe('running');
    expect(statusReads).toBe(1);
  });

  it('keeps throwing status accessors uncertain and bounded', async () => {
    const getStatus = vi.fn(
      async () =>
        new Proxy(
          { runId: context().runId },
          {
            get(target, property, receiver) {
              if (property === 'status') throw new Error('hostile status getter');
              return Reflect.get(target, property, receiver);
            },
          },
        ),
    );

    await expect(
      reconcileImportStartResponse({
        context: context(),
        getStatus,
        delayBeforeRetry: async () => undefined,
      }),
    ).resolves.toBe('uncertain');
    expect(getStatus).toHaveBeenCalledTimes(3);
  });

  it('drops a late status response after a newer scope takes authority', async () => {
    let current = true;
    const getStatus = vi.fn(async (runId: string) => {
      current = false;
      return { runId, status: 'running' };
    });
    await expect(
      reconcileImportStartResponse({
        context: context({ isCurrent: () => current }),
        getStatus,
      }),
    ).resolves.toBe('stale');
  });
});
