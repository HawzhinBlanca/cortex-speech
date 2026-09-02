import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';

const desktop = vi.hoisted(() => ({
  handlers: new Map<string, (event: { payload: unknown }) => void>(),
  unlisten: vi.fn(),
  listen: vi.fn(async (event: string, handler: (event: { payload: unknown }) => void) => {
    desktop.handlers.set(event, handler);
    return desktop.unlisten;
  }),
}));

const api = vi.hoisted(() => ({
  batchTranscribe: vi.fn(),
  batchNormalize: vi.fn(),
  getActiveBatchRun: vi.fn(),
  getBatchRunStatus: vi.fn(),
  acknowledgeBatchRun: vi.fn(),
}));

const activity = vi.hoisted(() => ({
  start: vi.fn(),
  end: vi.fn(),
}));

const notices = vi.hoisted(() => ({
  info: vi.fn(),
  success: vi.fn(),
  warning: vi.fn(),
  error: vi.fn((_message?: string, _options?: unknown) => 'notice-error'),
  dismiss: vi.fn(),
}));

const invalidateSegmentLoad = vi.fn();

vi.mock('./adapters/desktop', () => ({ listen: desktop.listen }));
vi.mock('./runtime', () => ({ isTauriRuntime: () => true }));
vi.mock('./commands', () => api);
vi.mock('./invoke', () => ({ startOperation: activity.start, endOperation: activity.end }));
vi.mock('./stores/notificationStore', () => ({ notifications: notices }));
vi.mock('./stores/segmentStore', () => ({
  segments: { bumpLoadGeneration: vi.fn(), load: vi.fn() },
}));

import { createBatchOperationCoordinator } from './batchOperationCoordinator';
import { setBatchWorkerSettledHandler, startEventListeners, stopEventListeners } from './events';
import { t } from './i18n';
import { batchProgress, isProcessing, pipelinePhase, statusMessage } from './stores/uiStore';

function completed(operationId: string) {
  return {
    operationId,
    operation: 'transcribe',
    status: 'settled',
    total: 1,
    outcome: {
      disposition: 'completed',
      total: 1,
      succeeded: 1,
      failed: 0,
      skipped: 0,
      abandoned: 0,
      cancelled: false,
      errorCode: null,
    },
  };
}

function runningActive(
  operationId: string,
  total = 2,
  operation: 'transcribe' | 'normalize' = 'transcribe',
) {
  return { operationId, operation, status: 'running', total, outcome: null };
}

function startingActive(
  operationId: string,
  total = 2,
  operation: 'transcribe' | 'normalize' = 'transcribe',
) {
  return { operationId, operation, status: 'starting', total, outcome: null };
}

describe('batch operation coordinator', () => {
  beforeEach(async () => {
    vi.useRealTimers();
    desktop.handlers.clear();
    desktop.listen.mockClear();
    desktop.unlisten.mockClear();
    vi.clearAllMocks();
    api.batchTranscribe.mockReset();
    api.batchNormalize.mockReset();
    api.getBatchRunStatus.mockReset();
    api.acknowledgeBatchRun.mockReset().mockResolvedValue(true);
    api.getActiveBatchRun.mockReset().mockResolvedValue(null);
    isProcessing.set(false);
    pipelinePhase.set('idle');
    batchProgress.set({ status: 'idle', completed: 0, total: 0, percent: 0 });
    statusMessage.set('');
    await startEventListeners();
  });

  afterEach(() => {
    stopEventListeners();
    vi.useRealTimers();
  });

  it('keeps controls locked after terminal result until the exact worker settles and refreshes', async () => {
    let operationId = '';
    api.batchTranscribe.mockImplementation(async (_ids: string[], exactId: string) => {
      operationId = exactId;
      return { status: 'started', operationId: exactId, operation: 'transcribe' };
    });
    api.getBatchRunStatus.mockImplementation(async () => completed(operationId));
    const loadSegments = vi.fn().mockResolvedValue(undefined);
    const refreshHistory = vi.fn().mockResolvedValue(undefined);
    const coordinator = createBatchOperationCoordinator({
      loadSegments,
      invalidateSegmentLoad,
      refreshHistory,
    });
    setBatchWorkerSettledHandler(coordinator.settleFromWorker);

    await coordinator.startTranscription(['segment-1']);
    desktop.handlers.get('batch-progress')?.({
      payload: {
        type: 'completed',
        operationId,
        operation: 'transcribe',
        total: 1,
        succeeded: 1,
        failed: 0,
        skipped: 0,
        abandoned: 0,
        cancelled: false,
      },
    });
    await Promise.resolve();
    expect(get(isProcessing)).toBe(true);
    expect(activity.end).not.toHaveBeenCalled();

    desktop.handlers.get('batch-worker-settled')?.({
      payload: { operationId, operation: 'transcribe' },
    });
    await vi.waitFor(() => expect(loadSegments).toHaveBeenCalledOnce());
    await vi.waitFor(() => expect(refreshHistory).toHaveBeenCalledOnce());
    await vi.waitFor(() => expect(activity.end).toHaveBeenCalledOnce());
    expect(get(isProcessing)).toBe(false);
    expect(get(batchProgress).status).toBe('idle');
  });

  it('recovers a lost terminal event from retained panic outcome without claiming success', async () => {
    let operationId = '';
    api.batchTranscribe.mockImplementation(async (_ids: string[], exactId: string) => {
      operationId = exactId;
      return { status: 'started', operationId: exactId, operation: 'transcribe' };
    });
    api.getBatchRunStatus.mockImplementation(async () => ({
      operationId,
      operation: 'transcribe',
      status: 'settled',
      total: 1,
      outcome: {
        disposition: 'panicked',
        total: 1,
        succeeded: 0,
        failed: 0,
        skipped: 0,
        abandoned: 1,
        cancelled: false,
        errorCode: 'BATCH_WORKER_PANICKED',
      },
    }));
    const coordinator = createBatchOperationCoordinator({
      loadSegments: vi.fn().mockResolvedValue(undefined),
      invalidateSegmentLoad,
      refreshHistory: vi.fn().mockResolvedValue(undefined),
    });
    setBatchWorkerSettledHandler(coordinator.settleFromWorker);

    await coordinator.startTranscription(['segment-1']);
    desktop.handlers.get('batch-worker-settled')?.({
      payload: { operationId, operation: 'transcribe' },
    });
    await vi.waitFor(() => expect(activity.end).toHaveBeenCalledOnce());
    expect(notices.error).toHaveBeenCalled();
    expect(notices.success).not.toHaveBeenCalled();
    expect(get(isProcessing)).toBe(false);
  });

  it('recognizes halted panic telemetry as matching the exact durable panic outcome', async () => {
    let operationId = '';
    api.batchTranscribe.mockImplementation(async (_ids: string[], exactId: string) => {
      operationId = exactId;
      return { status: 'started', operationId: exactId, operation: 'transcribe' };
    });
    api.getBatchRunStatus.mockImplementation(async () => ({
      operationId,
      operation: 'transcribe',
      status: 'settled',
      total: 1,
      outcome: {
        disposition: 'panicked',
        total: 1,
        succeeded: 0,
        failed: 0,
        skipped: 0,
        abandoned: 1,
        cancelled: false,
        errorCode: 'BATCH_WORKER_PANICKED',
      },
    }));
    const coordinator = createBatchOperationCoordinator({
      loadSegments: vi.fn().mockResolvedValue(undefined),
      invalidateSegmentLoad,
      refreshHistory: vi.fn().mockResolvedValue(undefined),
    });
    setBatchWorkerSettledHandler(coordinator.settleFromWorker);

    await coordinator.startTranscription(['segment-1']);
    desktop.handlers.get('batch-progress')?.({
      payload: {
        type: 'halted',
        operationId,
        operation: 'transcribe',
        total: 1,
        succeeded: 0,
        failed: 0,
        skipped: 0,
        abandoned: 1,
        cancelled: false,
        error: { code: 'BATCH_WORKER_PANICKED' },
      },
    });
    desktop.handlers.get('batch-worker-settled')?.({
      payload: { operationId, operation: 'transcribe' },
    });

    await vi.waitFor(() => expect(activity.end).toHaveBeenCalledOnce());
    expect(notices.error).toHaveBeenCalledOnce();
    expect(notices.success).not.toHaveBeenCalled();
  });

  it('ignores a flattering terminal event when exact retained status says the worker panicked', async () => {
    let operationId = '';
    api.batchTranscribe.mockImplementation(async (_ids: string[], exactId: string) => {
      operationId = exactId;
      return { status: 'started', operationId: exactId, operation: 'transcribe' };
    });
    api.getBatchRunStatus.mockImplementation(async () => ({
      operationId,
      operation: 'transcribe',
      status: 'settled',
      total: 1,
      outcome: {
        disposition: 'panicked',
        total: 1,
        succeeded: 0,
        failed: 0,
        skipped: 0,
        abandoned: 1,
        cancelled: false,
        errorCode: 'BATCH_WORKER_PANICKED',
      },
    }));
    const coordinator = createBatchOperationCoordinator({
      loadSegments: vi.fn().mockResolvedValue(undefined),
      invalidateSegmentLoad,
      refreshHistory: vi.fn().mockResolvedValue(undefined),
    });
    setBatchWorkerSettledHandler(coordinator.settleFromWorker);

    await coordinator.startTranscription(['segment-1']);
    desktop.handlers.get('batch-progress')?.({
      payload: {
        type: 'completed',
        operationId,
        operation: 'transcribe',
        total: 1,
        succeeded: 1,
        failed: 0,
        skipped: 0,
        abandoned: 0,
        cancelled: false,
      },
    });
    expect(notices.success).not.toHaveBeenCalled();

    desktop.handlers.get('batch-worker-settled')?.({
      payload: { operationId, operation: 'transcribe' },
    });
    await vi.waitFor(() => expect(activity.end).toHaveBeenCalledOnce());
    expect(notices.error).toHaveBeenCalledTimes(2);
    expect(notices.success).not.toHaveBeenCalled();
    expect(get(isProcessing)).toBe(false);
  });

  it('reports abandoned terminal telemetry that disagrees with durable outcome accounting', async () => {
    let operationId = '';
    api.batchTranscribe.mockImplementation(async (_ids: string[], exactId: string) => {
      operationId = exactId;
      return { status: 'started', operationId: exactId, operation: 'transcribe' };
    });
    api.getBatchRunStatus.mockImplementation(async () => ({
      operationId,
      operation: 'transcribe',
      status: 'settled',
      total: 1,
      outcome: {
        disposition: 'halted',
        total: 1,
        succeeded: 0,
        failed: 0,
        skipped: 0,
        abandoned: 1,
        cancelled: false,
        errorCode: 'BATCH_TRANSCRIPTION_FAILED',
      },
    }));
    const coordinator = createBatchOperationCoordinator({
      loadSegments: vi.fn().mockResolvedValue(undefined),
      invalidateSegmentLoad,
      refreshHistory: vi.fn().mockResolvedValue(undefined),
    });
    setBatchWorkerSettledHandler(coordinator.settleFromWorker);

    await coordinator.startTranscription(['segment-1']);
    desktop.handlers.get('batch-progress')?.({
      payload: {
        type: 'halted',
        operationId,
        operation: 'transcribe',
        total: 1,
        succeeded: 0,
        failed: 1,
        skipped: 0,
        abandoned: 0,
        cancelled: false,
        error: { code: 'BATCH_TRANSCRIPTION_FAILED' },
      },
    });
    desktop.handlers.get('batch-worker-settled')?.({
      payload: { operationId, operation: 'transcribe' },
    });

    await vi.waitFor(() => expect(activity.end).toHaveBeenCalledOnce());
    expect(notices.error).toHaveBeenCalledTimes(2);
    expect(api.acknowledgeBatchRun).toHaveBeenCalledWith(operationId);
    expect(notices.success).not.toHaveBeenCalled();
  });

  it('fails closed and stays recoverable when settlement arrives but outcome status is unavailable', async () => {
    let operationId = '';
    api.batchTranscribe.mockImplementation(async (_ids: string[], exactId: string) => {
      operationId = exactId;
      return { status: 'started', operationId: exactId, operation: 'transcribe' };
    });
    api.getBatchRunStatus.mockRejectedValue(new Error('status transport unavailable'));
    const loadSegments = vi.fn().mockResolvedValue(undefined);
    const coordinator = createBatchOperationCoordinator({
      loadSegments,
      invalidateSegmentLoad,
      refreshHistory: vi.fn().mockResolvedValue(undefined),
    });
    setBatchWorkerSettledHandler(coordinator.settleFromWorker);

    await coordinator.startTranscription(['segment-1']);
    desktop.handlers.get('batch-worker-settled')?.({
      payload: { operationId, operation: 'transcribe' },
    });
    await vi.waitFor(() => expect(notices.error).toHaveBeenCalled(), { timeout: 2_000 });
    expect(notices.info).not.toHaveBeenCalled();
    expect(notices.success).not.toHaveBeenCalled();
    expect(loadSegments).not.toHaveBeenCalled();
    expect(activity.end).not.toHaveBeenCalled();
    expect(get(isProcessing)).toBe(true);
    coordinator.destroy();
  });

  it('invalidates a hung segment refresh but retains terminal authority for retry', async () => {
    vi.useFakeTimers();
    let operationId = '';
    api.batchTranscribe.mockImplementation(async (_ids: string[], exactId: string) => {
      operationId = exactId;
      return { status: 'started', operationId: exactId, operation: 'transcribe' };
    });
    api.getBatchRunStatus.mockImplementation(async () => completed(operationId));
    const loadSegments = vi.fn(() => new Promise<void>(() => undefined));
    const refreshHistory = vi.fn().mockResolvedValue(undefined);
    const coordinator = createBatchOperationCoordinator({
      loadSegments,
      invalidateSegmentLoad,
      refreshHistory,
    });
    setBatchWorkerSettledHandler(coordinator.settleFromWorker);

    await coordinator.startTranscription(['segment-1']);
    desktop.handlers.get('batch-worker-settled')?.({
      payload: { operationId, operation: 'transcribe' },
    });
    await Promise.resolve();
    await vi.advanceTimersByTimeAsync(15_000);

    expect(invalidateSegmentLoad).toHaveBeenCalledOnce();
    expect(refreshHistory).not.toHaveBeenCalled();
    expect(api.acknowledgeBatchRun).not.toHaveBeenCalled();
    expect(activity.end).not.toHaveBeenCalled();
    expect(get(isProcessing)).toBe(true);
    coordinator.destroy();
  });

  it.each(['segments', 'history'] as const)(
    'retains terminal authority and withholds acknowledgement while the %s refresh fails',
    async (failingRefresh) => {
      vi.useFakeTimers();
      const operationId = '00000000-0000-4000-8000-000000000110';
      api.getActiveBatchRun.mockResolvedValue(completed(operationId));
      const loadSegments = vi
        .fn()
        .mockImplementationOnce(() =>
          failingRefresh === 'segments'
            ? Promise.reject(new Error('segment refresh unavailable'))
            : Promise.resolve(),
        )
        .mockResolvedValue(undefined);
      const refreshHistory = vi
        .fn()
        .mockImplementationOnce(() =>
          failingRefresh === 'history'
            ? Promise.reject(new Error('history refresh unavailable'))
            : Promise.resolve(),
        )
        .mockResolvedValue(undefined);
      const coordinator = createBatchOperationCoordinator({
        loadSegments,
        invalidateSegmentLoad,
        refreshHistory,
      });

      await coordinator.adoptActive();
      await vi.advanceTimersByTimeAsync(0);

      expect(api.acknowledgeBatchRun).not.toHaveBeenCalled();
      expect(activity.end).not.toHaveBeenCalled();
      expect(get(isProcessing)).toBe(true);

      await vi.advanceTimersByTimeAsync(2_000);
      await vi.runAllTimersAsync();

      expect(api.acknowledgeBatchRun).toHaveBeenCalledOnce();
      expect(activity.end).toHaveBeenCalledOnce();
      expect(loadSegments).toHaveBeenCalledTimes(2);
      expect(refreshHistory).toHaveBeenCalledTimes(failingRefresh === 'history' ? 2 : 1);
      expect(notices.success).toHaveBeenCalledOnce();
      expect(notices.dismiss).toHaveBeenCalledWith('notice-error');
      expect(get(isProcessing)).toBe(false);
    },
  );

  it.each([
    {
      code: 'BATCH_START_CANCELLED',
      retryable: true,
      suggestedAction: 'retry',
      title: 'batch.startCancelled',
      detail: 'batch.startCancelledDetail',
    },
    {
      code: 'BATCH_START_AUTHORITY_LOST',
      retryable: false,
      suggestedAction: 'openHealth',
      title: 'batch.startAuthorityLost',
      detail: 'batch.startAuthorityLostDetail',
    },
    {
      code: 'RESTORE_GENERATION_CHANGED',
      retryable: true,
      suggestedAction: 'retry',
      title: 'batch.restoreGenerationChanged',
      detail: 'batch.restoreGenerationChangedDetail',
    },
  ] as const)(
    'localizes $code without exposing backend prose or the raw code',
    async ({ code, retryable, suggestedAction, title, detail }) => {
      let operationId = '';
      const failure = {
        schema: 1,
        code,
        message: 'private backend path C:\\owner\\library.db',
        retryable,
        suggestedAction,
        operationId: null,
        details: {},
      };
      api.batchTranscribe.mockImplementation(async (_ids: string[], exactId: string) => {
        operationId = exactId;
        throw failure;
      });
      api.getBatchRunStatus.mockImplementation(async () => ({
        operationId,
        operation: 'transcribe',
        status: 'rejected',
        total: null,
        outcome: null,
      }));
      const coordinator = createBatchOperationCoordinator({
        loadSegments: vi.fn().mockResolvedValue(undefined),
        invalidateSegmentLoad,
        refreshHistory: vi.fn().mockResolvedValue(undefined),
      });

      await coordinator.startTranscription(['segment-1']);

      const [message, options] = notices.error.mock.calls.at(-1) ?? [];
      const publicDetail = (options as { publicDetail?: string } | undefined)?.publicDetail;
      expect(message).toBe(get(t)(title));
      expect(publicDetail).toBe(get(t)(detail));
      expect(`${message} ${publicDetail}`).not.toMatch(new RegExp(`${code}|private|library`, 'i'));
      expect(options).toEqual(expect.objectContaining({ cause: failure }));
      expect(get(isProcessing)).toBe(false);
    },
  );

  it('keeps a preflight reservation alive after a lost command response and settles from exact status', async () => {
    vi.useFakeTimers();
    let operationId = '';
    api.batchTranscribe.mockImplementation(async (_ids: string[], exactId: string) => {
      operationId = exactId;
      throw new Error('response channel lost');
    });
    api.getBatchRunStatus.mockImplementation(async () => ({
      operationId,
      operation: 'transcribe',
      status: 'starting',
      total: 1,
      outcome: null,
    }));
    const loadSegments = vi.fn().mockResolvedValue(undefined);
    const coordinator = createBatchOperationCoordinator({
      loadSegments,
      invalidateSegmentLoad,
      refreshHistory: vi.fn().mockResolvedValue(undefined),
    });
    setBatchWorkerSettledHandler(coordinator.settleFromWorker);

    await coordinator.startTranscription(['segment-1']);
    expect(get(isProcessing)).toBe(true);
    expect(notices.warning).toHaveBeenCalled();
    expect(activity.end).not.toHaveBeenCalled();

    api.getBatchRunStatus.mockImplementation(async () => completed(operationId));
    await vi.advanceTimersByTimeAsync(5_000);
    await vi.runAllTimersAsync();
    expect(loadSegments).toHaveBeenCalledOnce();
    expect(activity.end).toHaveBeenCalledOnce();
    expect(get(isProcessing)).toBe(false);
  });

  it('settles from one already-validated exact status even if every later status call is lost', async () => {
    let operationId = '';
    api.batchTranscribe.mockImplementation(async (_ids: string[], exactId: string) => {
      operationId = exactId;
      throw new Error('start response lost after admission');
    });
    api.getBatchRunStatus
      .mockImplementationOnce(async () => completed(operationId))
      .mockRejectedValue(new Error('later status transport unavailable'));
    const loadSegments = vi.fn().mockResolvedValue(undefined);
    const refreshHistory = vi.fn().mockResolvedValue(undefined);
    const coordinator = createBatchOperationCoordinator({
      loadSegments,
      invalidateSegmentLoad,
      refreshHistory,
    });

    await coordinator.startTranscription(['segment-1']);

    expect(api.getBatchRunStatus).toHaveBeenCalledOnce();
    expect(loadSegments).toHaveBeenCalledOnce();
    expect(refreshHistory).toHaveBeenCalledOnce();
    expect(activity.end).toHaveBeenCalledOnce();
    expect(get(isProcessing)).toBe(false);
  });

  it('cleans up only after exact rejected authority and blocks a concurrent second start', async () => {
    let operationId = '';
    let rejectFirst!: (error: unknown) => void;
    api.batchTranscribe.mockImplementation(
      (_ids: string[], exactId: string) =>
        new Promise((_resolve, reject) => {
          operationId = exactId;
          rejectFirst = reject;
        }),
    );
    api.getBatchRunStatus.mockImplementation(async () => ({
      operationId,
      operation: 'transcribe',
      status: 'rejected',
      total: null,
      outcome: null,
    }));
    const coordinator = createBatchOperationCoordinator({
      loadSegments: vi.fn().mockResolvedValue(undefined),
      invalidateSegmentLoad,
      refreshHistory: vi.fn().mockResolvedValue(undefined),
    });

    const first = coordinator.startTranscription(['segment-1']);
    await vi.waitFor(() => expect(api.batchTranscribe).toHaveBeenCalledOnce());
    await coordinator.startNormalization(['segment-2']);
    expect(api.batchNormalize).not.toHaveBeenCalled();
    rejectFirst(new Error('definitive refusal'));
    await first;
    expect(activity.end).toHaveBeenCalledOnce();
    expect(notices.error).toHaveBeenCalled();
    expect(get(isProcessing)).toBe(false);
  });

  it('adopts the exact durable running batch and blocks a duplicate start after renderer reload', async () => {
    const operationId = '00000000-0000-4000-8000-000000000101';
    api.getActiveBatchRun.mockResolvedValue(runningActive(operationId, 2));
    api.getBatchRunStatus.mockResolvedValue(runningActive(operationId, 2));
    const coordinator = createBatchOperationCoordinator({
      loadSegments: vi.fn().mockResolvedValue(undefined),
      invalidateSegmentLoad,
      refreshHistory: vi.fn().mockResolvedValue(undefined),
    });
    setBatchWorkerSettledHandler(coordinator.settleFromWorker);

    await expect(coordinator.adoptActive()).resolves.toBe(true);
    expect(activity.start).toHaveBeenCalledWith(`batch-transcribe:${operationId}`);
    expect(get(isProcessing)).toBe(true);
    expect(get(pipelinePhase)).toBe('transcribing');
    expect(get(batchProgress)).toMatchObject({ status: 'running', completed: 0, total: 2 });
    await coordinator.startNormalization(['segment-3']);
    expect(api.batchNormalize).not.toHaveBeenCalled();

    desktop.handlers.get('batch-progress')?.({
      payload: {
        type: 'progress',
        operationId,
        operation: 'transcribe',
        total: 2,
        current: 1,
        status: 'transcribing',
        file: 'clip.wav',
      },
    });
    expect(get(batchProgress).completed).toBe(1);
    coordinator.destroy();
  });

  it('adopts an exact preflight starting reservation and never unlocks or acknowledges it', async () => {
    vi.useFakeTimers();
    const operationId = '00000000-0000-4000-8000-000000000109';
    api.getActiveBatchRun.mockResolvedValue(startingActive(operationId, 2));
    api.getBatchRunStatus.mockResolvedValue(startingActive(operationId, 2));
    const coordinator = createBatchOperationCoordinator({
      loadSegments: vi.fn().mockResolvedValue(undefined),
      invalidateSegmentLoad,
      refreshHistory: vi.fn().mockResolvedValue(undefined),
    });

    await expect(coordinator.adoptActive()).resolves.toBe(true);
    await vi.advanceTimersByTimeAsync(0);

    expect(activity.start).toHaveBeenCalledWith(`batch-transcribe:${operationId}`);
    expect(api.getBatchRunStatus).toHaveBeenCalledWith(operationId);
    expect(api.acknowledgeBatchRun).not.toHaveBeenCalled();
    expect(get(isProcessing)).toBe(true);
    await coordinator.startNormalization(['segment-3']);
    expect(api.batchNormalize).not.toHaveBeenCalled();
    coordinator.destroy();
  });

  it('settles terminalization-before-discovery directly from the exact discovery response', async () => {
    const operationId = '00000000-0000-4000-8000-000000000106';
    api.getActiveBatchRun.mockResolvedValue(completed(operationId));
    const loadSegments = vi.fn().mockResolvedValue(undefined);
    const refreshHistory = vi.fn().mockResolvedValue(undefined);
    const coordinator = createBatchOperationCoordinator({
      loadSegments,
      invalidateSegmentLoad,
      refreshHistory,
    });

    await expect(coordinator.adoptActive()).resolves.toBe(true);
    await vi.waitFor(() => expect(activity.end).toHaveBeenCalledOnce());

    expect(api.getBatchRunStatus).not.toHaveBeenCalled();
    expect(api.acknowledgeBatchRun).toHaveBeenCalledWith(operationId);
    expect(loadSegments).toHaveBeenCalledOnce();
    expect(refreshHistory).toHaveBeenCalledOnce();
    expect(notices.success).toHaveBeenCalledOnce();
    expect(get(isProcessing)).toBe(false);
  });

  it('replays an exact acknowledgement after response loss without repeating outcome handling', async () => {
    const operationId = '00000000-0000-4000-8000-000000000107';
    api.getActiveBatchRun.mockResolvedValue(completed(operationId));
    api.acknowledgeBatchRun
      .mockRejectedValueOnce(new Error('response lost after durable acknowledgement'))
      .mockResolvedValue(true);
    const loadSegments = vi.fn().mockResolvedValue(undefined);
    const refreshHistory = vi.fn().mockResolvedValue(undefined);
    const coordinator = createBatchOperationCoordinator({
      loadSegments,
      invalidateSegmentLoad,
      refreshHistory,
    });

    await coordinator.adoptActive();
    await vi.waitFor(() => expect(activity.end).toHaveBeenCalledOnce());

    expect(api.acknowledgeBatchRun).toHaveBeenCalledTimes(2);
    expect(api.acknowledgeBatchRun).toHaveBeenNthCalledWith(1, operationId);
    expect(api.acknowledgeBatchRun).toHaveBeenNthCalledWith(2, operationId);
    expect(loadSegments).toHaveBeenCalledOnce();
    expect(refreshHistory).toHaveBeenCalledOnce();
    expect(notices.success).toHaveBeenCalledOnce();
    expect(get(isProcessing)).toBe(false);
  });

  it('keeps the workstation locked when exact acknowledgement is rejected', async () => {
    const operationId = '00000000-0000-4000-8000-000000000108';
    api.getActiveBatchRun.mockResolvedValue(completed(operationId));
    api.acknowledgeBatchRun.mockResolvedValue(false);
    const loadSegments = vi.fn().mockResolvedValue(undefined);
    const refreshHistory = vi.fn().mockResolvedValue(undefined);
    const coordinator = createBatchOperationCoordinator({
      loadSegments,
      invalidateSegmentLoad,
      refreshHistory,
    });

    await coordinator.adoptActive();
    await vi.waitFor(() =>
      expect(notices.error).toHaveBeenCalledWith(
        expect.any(String),
        expect.objectContaining({ action: expect.any(Object) }),
      ),
    );

    expect(activity.end).not.toHaveBeenCalled();
    expect(get(isProcessing)).toBe(true);
    await coordinator.startNormalization(['segment-2']);
    expect(api.batchNormalize).not.toHaveBeenCalled();

    const acknowledgementOptions = notices.error.mock.calls.at(-1)?.[1] as
      { action?: { handler: () => void } } | undefined;
    api.acknowledgeBatchRun.mockResolvedValue(true);
    acknowledgementOptions?.action?.handler();
    await vi.waitFor(() => expect(activity.end).toHaveBeenCalledOnce());
    expect(notices.dismiss).toHaveBeenCalledWith('notice-error');
    expect(loadSegments).toHaveBeenCalledOnce();
    expect(refreshHistory).toHaveBeenCalledOnce();
    expect(notices.success).toHaveBeenCalledOnce();
  });

  it('settles an adopted run from durable interrupted status without relying on old events', async () => {
    vi.useFakeTimers();
    const operationId = '00000000-0000-4000-8000-000000000102';
    api.getActiveBatchRun.mockResolvedValue(runningActive(operationId, 2));
    api.getBatchRunStatus.mockResolvedValue({
      operationId,
      operation: 'transcribe',
      status: 'settled',
      total: 2,
      outcome: {
        disposition: 'halted',
        total: 2,
        succeeded: 0,
        failed: 0,
        skipped: 0,
        abandoned: 2,
        cancelled: false,
        errorCode: 'PROCESS_INTERRUPTED',
      },
    });
    const loadSegments = vi.fn().mockResolvedValue(undefined);
    const refreshHistory = vi.fn().mockResolvedValue(undefined);
    const coordinator = createBatchOperationCoordinator({
      loadSegments,
      invalidateSegmentLoad,
      refreshHistory,
    });

    await coordinator.adoptActive();
    await vi.advanceTimersByTimeAsync(0);

    expect(loadSegments).toHaveBeenCalledOnce();
    expect(refreshHistory).toHaveBeenCalledOnce();
    expect(notices.error).toHaveBeenCalled();
    expect(notices.success).not.toHaveBeenCalled();
    expect(activity.end).toHaveBeenCalledWith(`batch-transcribe:${operationId}`);
    expect(get(isProcessing)).toBe(false);
  });

  it('retries a transient active-discovery transport loss and adopts the proven run once', async () => {
    const operationId = '00000000-0000-4000-8000-000000000103';
    api.getActiveBatchRun
      .mockRejectedValueOnce(new Error('renderer channel restarted'))
      .mockResolvedValue(runningActive(operationId, 3, 'normalize'));
    api.getBatchRunStatus.mockResolvedValue(runningActive(operationId, 3, 'normalize'));
    const coordinator = createBatchOperationCoordinator({
      loadSegments: vi.fn().mockResolvedValue(undefined),
      invalidateSegmentLoad,
      refreshHistory: vi.fn().mockResolvedValue(undefined),
    });

    await expect(coordinator.adoptActive()).resolves.toBe(true);

    expect(api.getActiveBatchRun).toHaveBeenCalledTimes(2);
    expect(activity.start).toHaveBeenCalledTimes(1);
    expect(get(batchProgress)).toMatchObject({ status: 'running', total: 3 });
    expect(notices.error).not.toHaveBeenCalled();
    coordinator.destroy();
  });

  it('keeps starts blocked after repeated discovery loss and recovers through its retry action', async () => {
    api.getActiveBatchRun.mockRejectedValue(new Error('desktop backend unavailable'));
    const coordinator = createBatchOperationCoordinator({
      loadSegments: vi.fn().mockResolvedValue(undefined),
      invalidateSegmentLoad,
      refreshHistory: vi.fn().mockResolvedValue(undefined),
    });

    await expect(coordinator.adoptActive()).resolves.toBe(false);
    expect(api.getActiveBatchRun).toHaveBeenCalledTimes(3);
    expect(get(isProcessing)).toBe(true);
    const failureOptions = notices.error.mock.calls.at(-1)?.[1] as
      { action?: { handler: () => void } } | undefined;
    expect(failureOptions?.action).toBeDefined();

    api.getActiveBatchRun.mockResolvedValue(null);
    failureOptions?.action?.handler();
    await vi.waitFor(() => expect(get(isProcessing)).toBe(false));
    expect(notices.dismiss).toHaveBeenCalled();
    api.batchNormalize.mockImplementation(async (_ids: string[], operationId: string) => ({
      status: 'started',
      operationId,
      operation: 'normalize',
    }));
    await coordinator.startNormalization(['segment-1']);
    expect(api.batchNormalize).toHaveBeenCalledOnce();
    coordinator.destroy();
  });

  it('fails closed with a retry action when settled discovery has malformed accounting', async () => {
    const operationId = '00000000-0000-4000-8000-000000000104';
    api.getActiveBatchRun.mockResolvedValue({
      ...completed(operationId),
      outcome: { ...completed(operationId).outcome, abandoned: 1 },
    });
    const coordinator = createBatchOperationCoordinator({
      loadSegments: vi.fn().mockResolvedValue(undefined),
      invalidateSegmentLoad,
      refreshHistory: vi.fn().mockResolvedValue(undefined),
    });

    await expect(coordinator.adoptActive()).resolves.toBe(false);
    expect(get(isProcessing)).toBe(true);
    expect(activity.start).not.toHaveBeenCalled();
    expect(notices.error).toHaveBeenCalledWith(
      expect.any(String),
      expect.objectContaining({
        action: expect.objectContaining({ handler: expect.any(Function) }),
      }),
    );
    await coordinator.startTranscription(['segment-1']);
    expect(api.batchTranscribe).not.toHaveBeenCalled();
    coordinator.destroy();
    expect(get(isProcessing)).toBe(false);
  });

  it('drops a late discovery response and releases its lock after destroy', async () => {
    const operationId = '00000000-0000-4000-8000-000000000105';
    let resolveDiscovery!: (value: unknown) => void;
    api.getActiveBatchRun.mockImplementation(
      () => new Promise((resolve) => (resolveDiscovery = resolve)),
    );
    const coordinator = createBatchOperationCoordinator({
      loadSegments: vi.fn().mockResolvedValue(undefined),
      invalidateSegmentLoad,
      refreshHistory: vi.fn().mockResolvedValue(undefined),
    });

    const adoption = coordinator.adoptActive();
    expect(get(isProcessing)).toBe(true);
    coordinator.destroy();
    resolveDiscovery(runningActive(operationId));

    await expect(adoption).resolves.toBe(false);
    expect(activity.start).not.toHaveBeenCalled();
    expect(api.getBatchRunStatus).not.toHaveBeenCalled();
    expect(get(isProcessing)).toBe(false);
  });
});
