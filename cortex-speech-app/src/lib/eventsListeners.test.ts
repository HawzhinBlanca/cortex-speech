import { afterEach, describe, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';

const desktop = vi.hoisted(() => ({
  handlers: new Map<string, (event: { payload: unknown }) => void>(),
  unlisten: vi.fn(),
  listen: vi.fn(async (event: string, handler: (event: { payload: unknown }) => void) => {
    desktop.handlers.set(event, handler);
    return desktop.unlisten;
  }),
}));

vi.mock('./adapters/desktop', () => ({
  listen: desktop.listen,
}));

vi.mock('./runtime', () => ({ isTauriRuntime: () => true }));

import {
  beginBatchEventScope,
  beginImportEventScope,
  closeBatchEventScope,
  markBatchEventSettled,
  markImportEventSettled,
  publicBatchProgressEvent,
  setBatchWorkerSettledHandler,
  setImportCompleteHandler,
  setImportEnrichmentCompleteHandler,
  setImportWorkerSettledHandler,
  startEventListeners,
  stopEventListeners,
} from './events';
import { agentPipelineStages, batchProgress, isProcessing, pipelinePhase } from './stores/uiStore';

const RUN_A = '00000000-0000-4000-8000-00000000000a';
const RUN_B = '00000000-0000-4000-8000-00000000000b';

describe('desktop event settlement boundary', () => {
  afterEach(() => {
    stopEventListeners();
    desktop.handlers.clear();
    desktop.unlisten.mockClear();
    desktop.listen.mockClear();
    agentPipelineStages.set([]);
    batchProgress.set({ status: 'idle', completed: 0, total: 0, percent: 0 });
    isProcessing.set(false);
    pipelinePhase.set('idle');
  });

  it('validates and scrubs the complete batch event wire boundary', () => {
    expect(
      publicBatchProgressEvent({
        type: 'progress',
        operationId: RUN_A,
        operation: 'transcribe',
        total: 2,
        current: 1,
        status: 'transcribing',
        file: String.raw`C:\private\owner\clip.wav`,
      }),
    ).toMatchObject({ file: 'clip.wav', current: 1, total: 2 });

    const halted = publicBatchProgressEvent({
      type: 'halted',
      operationId: RUN_A,
      operation: 'transcribe',
      total: 2,
      succeeded: 1,
      failed: 1,
      skipped: 0,
      abandoned: 0,
      cancelled: false,
      error: {
        code: 'BATCH_TRANSCRIPT_WRITE_FAILED',
        message: String.raw`C:\private\owner\db.sqlite secret-token`,
      },
    });
    expect(halted?.error).toMatchObject({
      code: 'BATCH_TRANSCRIPT_WRITE_FAILED',
      message: '',
      operationId: RUN_A,
    });
    expect(halted).toMatchObject({ failed: 1, abandoned: 0 });
    expect(JSON.stringify(halted)).not.toContain('private');
    expect(JSON.stringify(halted)).not.toContain('secret-token');

    for (const invalid of [
      { type: 'started', operationId: RUN_A.toUpperCase(), operation: 'transcribe', total: 1 },
      { type: 'started', operationId: RUN_A, operation: 'verify', total: 1 },
      { type: 'started', operationId: RUN_A, operation: 'transcribe', total: Infinity },
      {
        type: 'completed',
        operationId: RUN_A,
        operation: 'transcribe',
        total: 1,
        succeeded: 2,
        failed: 0,
        cancelled: false,
      },
      {
        type: 'completed',
        operationId: RUN_A,
        operation: 'transcribe',
        total: 2,
        succeeded: 1,
        failed: 0,
        skipped: 0,
        cancelled: false,
      },
      {
        type: 'completed',
        operationId: RUN_A,
        operation: 'transcribe',
        total: 2,
        succeeded: 1,
        failed: 1,
        skipped: 0,
        abandoned: 0,
        cancelled: false,
      },
    ]) {
      expect(publicBatchProgressEvent(invalid)).toBeNull();
    }
  });

  it('reconciles recovery only from the post-import settlement event', async () => {
    const reconcile = vi.fn().mockResolvedValue(undefined);
    setImportWorkerSettledHandler(reconcile);
    await startEventListeners();
    beginImportEventScope(RUN_A, 'directory');

    const settled = desktop.handlers.get('import-worker-settled');
    expect(settled).toBeTypeOf('function');
    settled?.({ payload: { runId: RUN_A, source: 'directory' } });
    await vi.waitFor(() => expect(reconcile).toHaveBeenCalledOnce());

    stopEventListeners();
    expect(desktop.unlisten).toHaveBeenCalled();
    settled?.({ payload: { runId: RUN_A, source: 'directory' } });
    expect(reconcile).toHaveBeenCalledOnce();
  });

  it('does not treat pre-release import completion as terminal settlement', async () => {
    await startEventListeners();
    const context = beginImportEventScope(RUN_A, 'file');

    desktop.handlers.get('import-complete')?.({
      payload: { runId: RUN_A, total: 1, succeeded: 1, failed: 0, source: 'file' },
    });
    expect(context.hasObservedEvent()).toBe(true);
    expect(context.hasTerminalEvent()).toBe(false);

    desktop.handlers.get('import-worker-settled')?.({
      payload: { runId: RUN_A, source: 'file' },
    });
    expect(context.hasTerminalEvent()).toBe(true);
  });

  it('never accepts delayed same-run primary events after terminal settlement', async () => {
    const complete = vi.fn().mockResolvedValue(undefined);
    const enriched = vi.fn().mockResolvedValue(undefined);
    const settled = vi.fn().mockResolvedValue(undefined);
    setImportCompleteHandler(complete);
    setImportEnrichmentCompleteHandler(enriched);
    setImportWorkerSettledHandler(settled);
    await startEventListeners();
    const context = beginImportEventScope(RUN_A, 'file');

    desktop.handlers.get('import-complete')?.({
      payload: {
        runId: RUN_A,
        total: Number.POSITIVE_INFINITY,
        succeeded: 1,
        failed: 0,
        source: 'file',
      },
    });
    desktop.handlers.get('import-worker-settled')?.({
      payload: { runId: RUN_A, source: 'untrusted-source' },
    });
    await Promise.resolve();
    expect(complete).not.toHaveBeenCalled();
    expect(settled).not.toHaveBeenCalled();
    expect(context.hasTerminalEvent()).toBe(false);

    desktop.handlers.get('import-worker-settled')?.({
      payload: { runId: RUN_A, source: 'file' },
    });
    await vi.waitFor(() => expect(settled).toHaveBeenCalledOnce());
    isProcessing.set(false);

    desktop.handlers.get('pipeline-started')?.({ payload: { runId: RUN_A, total: 1 } });
    desktop.handlers.get('pipeline-progress')?.({
      payload: {
        runId: RUN_A,
        current: 1,
        total: 1,
        fileLabel: 'late.wav',
        status: 'processing',
      },
    });
    desktop.handlers.get('import-complete')?.({
      payload: { runId: RUN_A, total: 1, succeeded: 1, failed: 0, source: 'directory' },
    });
    desktop.handlers.get('import-enrichment-complete')?.({
      payload: { runId: RUN_A, source: 'directory', segmentIds: ['late'] },
    });
    desktop.handlers.get('pipeline-agent-stage')?.({
      payload: {
        runId: RUN_A,
        stage: 'audio_chunking',
        status: 'completed',
        fileLabel: 'late.wav',
        current: 1,
        total: 1,
      },
    });

    await Promise.resolve();
    expect(get(isProcessing)).toBe(false);
    expect(complete).not.toHaveBeenCalled();
    expect(enriched).not.toHaveBeenCalled();
    expect(settled).toHaveBeenCalledOnce();
    expect(get(agentPipelineStages)).toEqual([]);
  });

  it('accepts exactly one file enrichment after worker settlement while primary events stay sealed', async () => {
    const enriched = vi.fn().mockResolvedValue(undefined);
    setImportEnrichmentCompleteHandler(enriched);
    await startEventListeners();
    const context = beginImportEventScope(RUN_A, 'file');

    desktop.handlers.get('import-worker-settled')?.({
      payload: { runId: RUN_A, source: 'file' },
    });
    expect(context.hasTerminalEvent()).toBe(true);

    desktop.handlers.get('pipeline-progress')?.({
      payload: {
        runId: RUN_A,
        current: 1,
        total: 1,
        fileLabel: 'late-primary.wav',
        status: 'processing',
      },
    });
    desktop.handlers.get('pipeline-phase')?.({
      payload: { runId: RUN_A, phase: 'adjudicating' },
    });
    desktop.handlers.get('pipeline-agent-stage')?.({
      payload: {
        runId: RUN_A,
        stage: 'jury_adjudication',
        status: 'completed',
        fileLabel: 'clip.wav',
        current: 1,
        total: 1,
      },
    });
    desktop.handlers.get('import-enrichment-complete')?.({
      payload: { runId: RUN_A, source: 'file', segmentIds: ['segment-a'] },
    });
    desktop.handlers.get('import-enrichment-complete')?.({
      payload: { runId: RUN_A, source: 'file', segmentIds: ['duplicate'] },
    });

    await vi.waitFor(() => expect(enriched).toHaveBeenCalledOnce());
    expect(get(pipelinePhase)).toBe('idle');
    expect(get(agentPipelineStages)).toMatchObject([
      { runId: RUN_A, stage: 'jury_adjudication', file: 'clip.wav' },
    ]);
    expect(get(isProcessing)).toBe(false);
  });

  it('binds terminal authority to both the exact run and declared source', async () => {
    const complete = vi.fn().mockResolvedValue(undefined);
    const settled = vi.fn().mockResolvedValue(undefined);
    setImportCompleteHandler(complete);
    setImportWorkerSettledHandler(settled);
    await startEventListeners();
    const context = beginImportEventScope(RUN_A, 'file');

    desktop.handlers.get('import-complete')?.({
      payload: { runId: RUN_A, total: 1, succeeded: 1, failed: 0, source: 'directory' },
    });
    desktop.handlers.get('import-worker-settled')?.({
      payload: { runId: RUN_A, source: 'directory' },
    });
    await Promise.resolve();
    expect(complete).not.toHaveBeenCalled();
    expect(settled).not.toHaveBeenCalled();
    expect(context.hasTerminalEvent()).toBe(false);

    desktop.handlers.get('import-worker-settled')?.({
      payload: { runId: RUN_A, source: 'file' },
    });
    await vi.waitFor(() => expect(settled).toHaveBeenCalledOnce());
    expect(context.hasTerminalEvent()).toBe(true);
  });

  it('lets exact status reconciliation seal the primary event lane', async () => {
    await startEventListeners();
    const context = beginImportEventScope(RUN_A, 'directory');
    expect(markImportEventSettled(context)).toBe(true);
    expect(markImportEventSettled(context)).toBe(false);

    desktop.handlers.get('pipeline-started')?.({ payload: { runId: RUN_A, total: 1 } });
    desktop.handlers.get('pipeline-progress')?.({
      payload: {
        runId: RUN_A,
        current: 1,
        total: 1,
        fileLabel: 'late.wav',
        status: 'processing',
      },
    });
    expect(get(isProcessing)).toBe(false);
  });

  it('drops every late completion and stage from run A after run B starts', async () => {
    const complete = vi.fn().mockResolvedValue(undefined);
    const enriched = vi.fn().mockResolvedValue(undefined);
    setImportCompleteHandler(complete);
    setImportEnrichmentCompleteHandler(enriched);
    await startEventListeners();

    beginImportEventScope(RUN_A, 'file');
    desktop.handlers.get('pipeline-started')?.({ payload: { runId: RUN_A, total: 1 } });
    desktop.handlers.get('import-complete')?.({
      payload: { runId: RUN_A, total: 1, succeeded: 1, failed: 0, source: 'file' },
    });
    await vi.waitFor(() => expect(complete).toHaveBeenCalledOnce());

    beginImportEventScope(RUN_B, 'file');
    desktop.handlers.get('pipeline-started')?.({ payload: { runId: RUN_B, total: 1 } });
    desktop.handlers.get('import-complete')?.({
      payload: { runId: RUN_A, total: 1, succeeded: 1, failed: 0, source: 'file' },
    });
    desktop.handlers.get('import-enrichment-complete')?.({
      payload: { runId: RUN_A, source: 'file', segmentIds: ['segment-a'] },
    });
    desktop.handlers.get('pipeline-agent-stage')?.({
      payload: {
        runId: RUN_A,
        stage: 'jury_adjudication',
        status: 'completed',
        fileLabel: 'a.wav',
        current: 1,
        total: 1,
      },
    });

    expect(complete).toHaveBeenCalledOnce();
    expect(enriched).not.toHaveBeenCalled();
    expect(get(agentPipelineStages)).toEqual([]);

    desktop.handlers.get('pipeline-agent-stage')?.({
      payload: {
        runId: RUN_B,
        stage: 'audio_chunking',
        status: 'completed',
        fileLabel: 'b.wav',
        current: 1,
        total: 1,
      },
    });
    desktop.handlers.get('import-enrichment-complete')?.({
      payload: { runId: RUN_B, source: 'file', segmentIds: ['segment-b'] },
    });

    await vi.waitFor(() => expect(enriched).toHaveBeenCalledOnce());
    expect(get(agentPipelineStages)).toMatchObject([
      { runId: RUN_B, stage: 'audio_chunking', file: 'b.wav' },
    ]);
  });

  it('invalidates an accepted run-A callback while it is awaiting after run B begins', async () => {
    let releaseA!: () => void;
    const paused = new Promise<void>((resolve) => (releaseA = resolve));
    let contextStillCurrentAfterAwait: boolean | null = null;
    const complete = vi.fn(async (_payload: unknown, context: { isCurrent: () => boolean }) => {
      await paused;
      contextStillCurrentAfterAwait = context.isCurrent();
    });
    setImportCompleteHandler(complete);
    await startEventListeners();

    beginImportEventScope(RUN_A, 'file');
    desktop.handlers.get('import-complete')?.({
      payload: { runId: RUN_A, total: 1, succeeded: 1, failed: 0, source: 'file' },
    });
    await vi.waitFor(() => expect(complete).toHaveBeenCalledOnce());

    beginImportEventScope(RUN_B, 'file');
    releaseA();
    await vi.waitFor(() => expect(contextStillCurrentAfterAwait).toBe(false));

    desktop.handlers.get('import-complete')?.({
      payload: { runId: RUN_B, total: 1, succeeded: 1, failed: 0, source: 'file' },
    });
    await vi.waitFor(() => expect(complete).toHaveBeenCalledTimes(2));
    expect(complete.mock.calls[1][1].isCurrent()).toBe(true);
  });

  it('does not let an old exact batch terminal event act on a newer batch', async () => {
    await startEventListeners();
    const first = beginBatchEventScope(RUN_A, 'transcribe', 1);

    desktop.handlers.get('batch-progress')?.({
      payload: {
        type: 'started',
        total: 1,
        operation: 'transcribe',
        operationId: RUN_A,
      },
    });
    desktop.handlers.get('batch-progress')?.({
      payload: {
        type: 'completed',
        total: 1,
        succeeded: 1,
        failed: 0,
        skipped: 0,
        abandoned: 0,
        cancelled: false,
        operation: 'transcribe',
        operationId: RUN_A,
      },
    });
    expect(first.hasTerminalEvent()).toBe(true);
    expect(get(isProcessing)).toBe(true);

    expect(markBatchEventSettled(first)).toBe(true);
    closeBatchEventScope(first);
    const second = beginBatchEventScope(RUN_B, 'normalize', 2);
    desktop.handlers.get('batch-progress')?.({
      payload: { type: 'started', total: 2, operation: 'normalize', operationId: RUN_B },
    });
    desktop.handlers.get('batch-progress')?.({
      payload: {
        type: 'completed',
        total: 1,
        succeeded: 1,
        failed: 0,
        skipped: 0,
        abandoned: 0,
        cancelled: false,
        operation: 'transcribe',
        operationId: RUN_A,
      },
    });
    expect(first.isCurrent()).toBe(false);
    expect(second.hasTerminalEvent()).toBe(false);
    expect(get(isProcessing)).toBe(true);
    expect(get(batchProgress)).toMatchObject({ status: 'running', total: 2 });

    desktop.handlers.get('batch-progress')?.({
      payload: {
        type: 'completed',
        total: 2,
        succeeded: 2,
        failed: 0,
        abandoned: 0,
        cancelled: false,
        operation: 'normalize',
        operationId: RUN_B,
      },
    });
    expect(second.hasTerminalEvent()).toBe(true);
    expect(get(isProcessing)).toBe(true);
  });

  it('never regresses exact-run progress when concurrent worker events arrive out of order', async () => {
    await startEventListeners();
    beginBatchEventScope(RUN_A, 'transcribe', 10);

    desktop.handlers.get('batch-progress')?.({
      payload: {
        type: 'progress',
        total: 10,
        current: 8,
        status: 'transcribing',
        file: 'eight.wav',
        operation: 'transcribe',
        operationId: RUN_A,
      },
    });
    desktop.handlers.get('batch-progress')?.({
      payload: {
        type: 'progress',
        total: 10,
        current: 3,
        status: 'transcribing',
        file: 'three.wav',
        operation: 'transcribe',
        operationId: RUN_A,
      },
    });

    expect(get(batchProgress)).toEqual({
      status: 'running',
      completed: 8,
      total: 10,
      percent: 80,
    });
  });

  it('requires exact batch identity and physical worker settlement before reconciliation', async () => {
    const settled = vi.fn().mockResolvedValue(undefined);
    setBatchWorkerSettledHandler(settled);
    await startEventListeners();
    const context = beginBatchEventScope(RUN_A, 'transcribe', 1);

    desktop.handlers.get('batch-progress')?.({
      payload: {
        type: 'completed',
        total: 2,
        succeeded: 2,
        failed: 0,
        skipped: 0,
        abandoned: 0,
        cancelled: false,
        operation: 'transcribe',
        operationId: RUN_A,
      },
    });
    expect(context.hasTerminalEvent()).toBe(false);

    // A terminal can arrive without `started`; caller-created scope remains the authority.
    desktop.handlers.get('batch-progress')?.({
      payload: {
        type: 'completed',
        total: 1,
        succeeded: 1,
        failed: 0,
        skipped: 0,
        abandoned: 0,
        cancelled: false,
        operation: 'transcribe',
        operationId: RUN_A,
      },
    });
    expect(context.hasTerminalEvent()).toBe(true);
    expect(context.terminalEvent()).toMatchObject({ type: 'completed', total: 1, succeeded: 1 });
    expect(context.hasSettledEvent()).toBe(false);

    desktop.handlers.get('batch-worker-settled')?.({
      payload: { operationId: RUN_A, operation: 'normalize' },
    });
    desktop.handlers.get('batch-worker-settled')?.({
      payload: { operationId: RUN_B, operation: 'transcribe' },
    });
    await Promise.resolve();
    expect(settled).not.toHaveBeenCalled();

    desktop.handlers.get('batch-worker-settled')?.({
      payload: { operationId: RUN_A, operation: 'transcribe' },
    });
    await vi.waitFor(() => expect(settled).toHaveBeenCalledOnce());
    expect(context.hasSettledEvent()).toBe(true);
  });

  it('refuses to replace a live batch event authority', async () => {
    await startEventListeners();
    const current = beginBatchEventScope(RUN_A, 'transcribe', 1);
    expect(() => beginBatchEventScope(RUN_B, 'normalize', 1)).toThrow(
      'A batch event authority is already active',
    );
    expect(current.isCurrent()).toBe(true);
  });

  it('rolls back every staged listener when one registration rejects', async () => {
    desktop.listen
      .mockImplementationOnce(async (event, handler) => {
        desktop.handlers.set(event, handler);
        return desktop.unlisten;
      })
      .mockRejectedValueOnce(new Error('registration failed'));

    await expect(startEventListeners()).rejects.toThrow('registration failed');
    expect(desktop.unlisten).toHaveBeenCalledOnce();

    beginImportEventScope(RUN_A, 'directory');
    desktop.handlers.get('pipeline-progress')?.({
      payload: {
        runId: RUN_A,
        current: 1,
        total: 1,
        fileLabel: 'ghost.wav',
        status: 'processing',
      },
    });
    expect(get(isProcessing)).toBe(false);
  });

  it('unsubscribes a listener that resolves after teardown and never activates it', async () => {
    let releaseRegistration!: (unlisten: typeof desktop.unlisten) => void;
    const lateUnlisten = vi.fn();
    desktop.listen.mockImplementationOnce(
      (event, handler) =>
        new Promise<typeof desktop.unlisten>((resolve) => {
          desktop.handlers.set(event, handler);
          releaseRegistration = resolve;
        }),
    );

    const starting = startEventListeners();
    await vi.waitFor(() => expect(desktop.listen).toHaveBeenCalledOnce());
    stopEventListeners();
    releaseRegistration(lateUnlisten);
    await starting;
    expect(lateUnlisten).toHaveBeenCalledOnce();

    beginImportEventScope(RUN_A, 'directory');
    desktop.handlers.get('pipeline-progress')?.({
      payload: {
        runId: RUN_A,
        current: 1,
        total: 1,
        fileLabel: 'ghost.wav',
        status: 'processing',
      },
    });
    expect(get(isProcessing)).toBe(false);
  });
});
