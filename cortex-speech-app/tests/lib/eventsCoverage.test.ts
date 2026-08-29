import { get } from 'svelte/store';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const desktop = vi.hoisted(() => ({
  available: true,
  handlers: new Map<string, (event: { payload: unknown }) => void>(),
  unlisten: vi.fn(),
  listen: vi.fn(async (event: string, handler: (event: { payload: unknown }) => void) => {
    desktop.handlers.set(event, handler);
    return desktop.unlisten;
  }),
}));

vi.mock('../../src/lib/adapters/desktop', () => ({
  listen: desktop.listen,
}));

vi.mock('../../src/lib/runtime', () => ({
  isTauriRuntime: () => desktop.available,
}));

import {
  beginBatchEventScope,
  beginImportEventScope,
  closeBatchEventScope,
  closeImportEventScope,
  createBatchOperationId,
  createImportRunId,
  markBatchEventSettled,
  publicAgentStagePresentation,
  publicBatchHaltCode,
  publicBatchProgressEvent,
  publicPipelineProgressPresentation,
  setBatchWorkerSettledHandler,
  setImportCompleteHandler,
  setImportEnrichmentCompleteHandler,
  setImportWorkerSettledHandler,
  startEventListeners,
  stopEventListeners,
} from '../../src/lib/events';
import { locale } from '../../src/lib/i18n';
import { notifications } from '../../src/lib/stores/notificationStore';
import { segments } from '../../src/lib/stores/segmentStore';
import {
  agentPipelineStages,
  batchProgress,
  filesProcessed,
  isProcessing,
  pipelineCurrentFile,
  pipelinePhase,
  pipelineStatus,
  pipelineTotal,
} from '../../src/lib/stores/uiStore';

const RUN_A = '00000000-0000-4000-8000-00000000000a';
const RUN_B = '00000000-0000-4000-8000-00000000000b';

function emit(event: string, payload: unknown): void {
  const handler = desktop.handlers.get(event);
  expect(handler, `listener ${event}`).toBeTypeOf('function');
  handler?.({ payload });
}

function hostile(property: string): object {
  return Object.defineProperty({}, property, {
    get: () => {
      throw new Error('private hostile getter');
    },
  });
}

beforeEach(() => {
  desktop.available = true;
  desktop.handlers.clear();
  desktop.listen.mockClear();
  desktop.unlisten.mockReset();
  notifications.clear();
  locale.set('en');
  agentPipelineStages.set([]);
  batchProgress.set({ status: 'idle', completed: 0, total: 0, percent: 0 });
  filesProcessed.set(0);
  pipelineTotal.set(0);
  pipelineCurrentFile.set('');
  pipelineStatus.set('');
  isProcessing.set(false);
  pipelinePhase.set('idle');
});

afterEach(() => {
  stopEventListeners();
  desktop.handlers.clear();
  notifications.clear();
  vi.restoreAllMocks();
});

describe('events closed wire vocabulary', () => {
  it('covers every progress phase, bounded labels, secure IDs, and fail-closed identity generation', () => {
    for (const [status, phase] of [
      ['resuming', 'importing'],
      ['processing', 'importing'],
      ['reference_transcribing', 'reference_transcribing'],
      ['transcribing', 'transcribing'],
      ['adjudicating', 'adjudicating'],
      ['unknown', 'importing'],
    ] as const) {
      expect(
        publicPipelineProgressPresentation({
          runId: RUN_A,
          current: 20_000_000,
          total: 1,
          fileLabel: `C:\\private\\${'a'.repeat(200)}.wav`,
          status,
        }),
      ).toMatchObject({ runId: RUN_A, current: 10_000_000, total: 1, phase });
    }
    expect(publicPipelineProgressPresentation(7)).toMatchObject({
      runId: null,
      file: 'unknown file',
    });
    expect(
      publicPipelineProgressPresentation({
        runId: `${RUN_A}${'x'.repeat(100)}`,
        status: 'processing',
      }),
    ).toMatchObject({ runId: null });

    expect(createImportRunId()).toMatch(/^[A-Za-z0-9][A-Za-z0-9_.:@+-]+$/);
    expect(createBatchOperationId()).toMatch(
      /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/,
    );
    const randomUuid = vi.spyOn(globalThis.crypto, 'randomUUID');
    randomUuid.mockReturnValueOnce(
      'not a valid run id' as `${string}-${string}-${string}-${string}-${string}`,
    );
    expect(() => createImportRunId()).toThrow(
      'Secure import run identity generation is unavailable',
    );
    randomUuid.mockReturnValueOnce(
      'not-a-uuid' as `${string}-${string}-${string}-${string}-${string}`,
    );
    expect(() => createBatchOperationId()).toThrow(
      'Secure batch operation identity generation is unavailable',
    );
  });

  it('rejects malformed stage and batch payloads while accepting every closed terminal shape', () => {
    expect(publicAgentStagePresentation(null)).toBeNull();
    expect(publicAgentStagePresentation(3)).toBeNull();
    expect(
      publicAgentStagePresentation({
        runId: `${RUN_A}${'x'.repeat(100)}`,
        stage: 'agent_report',
        status: 'completed',
      }),
    ).toBeNull();

    for (const code of [
      'CHAMPION_UNAVAILABLE',
      'CHAMPION_IDENTITY_MISMATCH',
      'MODEL_IDENTITY_CHANGED',
      'TRANSCRIPTION_SOURCE_CHANGED',
      'AUDIO_DECODE_FAILED',
      'BATCH_SEGMENT_MISSING',
      'BATCH_TRANSCRIPT_WRITE_FAILED',
      'BATCH_NORMALIZATION_FAILED',
      'BATCH_REFINEMENT_FAILED',
      'BATCH_JURY_FAILED',
      'BATCH_WORKER_START_FAILED',
      'BATCH_WORKER_PANICKED',
      'PROCESS_INTERRUPTED',
      'BATCH_EVIDENCE_INVALID',
      'BATCH_TRANSCRIPTION_FAILED',
    ] as const) {
      expect(publicBatchHaltCode(code)).toBe(code);
    }
    expect(publicBatchHaltCode(null)).toBeNull();
    expect(publicBatchHaltCode('__proto__')).toBeNull();

    expect(publicBatchProgressEvent(null)).toBeNull();
    expect(publicBatchProgressEvent('not an event')).toBeNull();
    expect(
      publicBatchProgressEvent({
        type: 'invented',
        operationId: RUN_A,
        operation: 'transcribe',
        total: 1,
      }),
    ).toBeNull();
    expect(
      publicBatchProgressEvent({
        type: 'progress',
        operationId: RUN_A,
        operation: 'normalize',
        total: 2,
        current: 1,
        status: 'failed',
        file: `${'b'.repeat(200)}.wav`,
      }),
    ).toMatchObject({ operation: 'normalize', current: 1, status: 'failed' });
    for (const invalid of [
      { current: -1, status: 'normalizing' },
      { current: 3, status: 'normalizing' },
      { current: 1, status: 'transcribing' },
      { current: 1, status: 4 },
    ]) {
      expect(
        publicBatchProgressEvent({
          type: 'progress',
          operationId: RUN_A,
          operation: 'normalize',
          total: 2,
          ...invalid,
        }),
      ).toBeNull();
    }

    expect(
      publicBatchProgressEvent({
        type: 'completed',
        operationId: RUN_A,
        operation: 'normalize',
        total: 2,
        succeeded: 1,
        failed: 0,
        abandoned: 1,
        cancelled: true,
      }),
    ).toEqual({
      type: 'completed',
      operationId: RUN_A,
      operation: 'normalize',
      total: 2,
      succeeded: 1,
      failed: 0,
      abandoned: 1,
      cancelled: true,
    });
    expect(
      publicBatchProgressEvent({
        type: 'halted',
        operationId: RUN_A,
        operation: 'transcribe',
        total: 1,
        succeeded: 0,
        failed: 1,
        skipped: 0,
        abandoned: 0,
        cancelled: false,
        error: hostile('code'),
      }),
    ).toMatchObject({ error: { code: 'BATCH_TRANSCRIPTION_FAILED', message: '' } });
    expect(publicBatchProgressEvent(hostile('operationId'))).toBeNull();
  });
});

describe('events listener failure containment and terminal truth', () => {
  it('renders directory/file completion outcomes and drops malformed private event shapes', async () => {
    await startEventListeners();

    beginImportEventScope(RUN_A, 'directory');
    emit('import-complete', {
      runId: RUN_A,
      source: 'directory',
      total: 3,
      succeeded: 2,
      failed: 1,
      segmentCount: 2,
      segmentIds: ['valid-id', 'invalid id', ...Array.from({ length: 130 }, (_, i) => `id-${i}`)],
    });
    await vi.waitFor(() => expect(get(notifications).at(-1)?.type).toBe('warning'));

    beginImportEventScope(RUN_B, 'directory');
    emit('import-complete', {
      runId: RUN_B,
      source: 'directory',
      total: 2,
      succeeded: 2,
      failed: 0,
    });
    await vi.waitFor(() => expect(get(notifications).at(-1)?.type).toBe('success'));

    beginImportEventScope(RUN_A, 'file');
    emit('import-complete', {
      runId: RUN_A,
      source: 'file',
      total: 1,
      succeeded: 0,
      failed: 1,
    });
    await vi.waitFor(() => expect(get(notifications).at(-1)?.type).toBe('error'));

    const before = get(notifications).length;
    for (const payload of [null, 4, hostile('runId')]) emit('import-complete', payload);
    for (const payload of [null, hostile('runId'), { runId: RUN_A, source: 'directory' }]) {
      emit('import-enrichment-complete', payload);
      emit('import-worker-settled', payload);
    }
    emit('batch-worker-settled', hostile('operationId'));
    expect(get(notifications)).toHaveLength(before);
  });

  it('contains sync and async callback failures only while their exact context remains current', async () => {
    await startEventListeners();

    setImportCompleteHandler(() => {
      throw new Error('complete refresh failed');
    });
    beginImportEventScope(RUN_A, 'file');
    emit('import-complete', {
      runId: RUN_A,
      source: 'file',
      total: 1,
      succeeded: 1,
      failed: 0,
    });
    await vi.waitFor(() => expect(get(notifications).at(-1)?.message).toContain('refresh'));

    setImportEnrichmentCompleteHandler(() => {
      throw new Error('sync enrichment failure');
    });
    beginImportEventScope(RUN_A, 'file');
    emit('import-enrichment-complete', {
      runId: RUN_A,
      source: 'file',
      segmentCount: 1,
      segmentIds: ['segment-a'],
    });
    await vi.waitFor(() => expect(get(notifications).at(-1)?.type).toBe('error'));

    setImportEnrichmentCompleteHandler(vi.fn().mockRejectedValue(new Error('async enrichment')));
    beginImportEventScope(RUN_A, 'file');
    emit('import-enrichment-complete', { runId: RUN_A, source: 'file' });
    await vi.waitFor(() =>
      expect(get(notifications).filter((item) => item.type === 'error').length).toBeGreaterThan(2),
    );

    setImportWorkerSettledHandler(() => {
      throw new Error('sync worker settlement');
    });
    beginImportEventScope(RUN_A, 'directory');
    emit('import-worker-settled', { runId: RUN_A, source: 'directory' });
    await vi.waitFor(() => expect(get(notifications).at(-1)?.type).toBe('error'));

    setImportWorkerSettledHandler(vi.fn().mockRejectedValue(new Error('async worker settlement')));
    beginImportEventScope(RUN_B, 'directory');
    emit('import-worker-settled', { runId: RUN_B, source: 'directory' });
    await vi.waitFor(() => expect(get(notifications).at(-1)?.type).toBe('error'));
  });

  it('maps progress, enrichment errors, all allowed phases, and every WSL terminal outcome', async () => {
    const load = vi.spyOn(segments, 'load').mockResolvedValue(true);
    await startEventListeners();
    beginImportEventScope(RUN_A, 'file');

    emit('pipeline-progress', {
      runId: RUN_A,
      current: 2,
      total: 4,
      fileLabel: 'C:\\private\\clip.wav',
      status: 'reference_transcribing',
    });
    expect(get(filesProcessed)).toBe(2);
    expect(get(pipelineTotal)).toBe(4);
    expect(get(pipelineCurrentFile)).toBe('clip.wav');
    expect(get(pipelinePhase)).toBe('reference_transcribing');
    expect(get(pipelineStatus)).toBe('Building reference transcript...');

    emit('pipeline-error', {
      runId: RUN_A,
      file: 'C:\\private\\clip.wav',
      code: 'IMPORT_ENRICHMENT_FAILED',
    });
    expect(get(notifications).at(-1)).toMatchObject({ type: 'error' });
    emit('pipeline-error', hostile('file'));

    for (const phase of [
      'importing',
      'reference_transcribing',
      'detecting',
      'transcribing',
      'adjudicating',
    ]) {
      emit('pipeline-phase', { runId: RUN_A, phase });
      expect(get(pipelinePhase)).toBe(phase);
    }
    emit('pipeline-phase', { runId: RUN_A, phase: 'private-backend-phase' });
    expect(get(pipelinePhase)).toBe('adjudicating');

    emit('wsl-status', { status: 'completed', transcribed: 2, failed: 1, exit_code: 0 });
    emit('wsl-status', { status: 'completed', transcribed: 2, failed: 0, exit_code: 0 });
    emit('wsl-status', { status: 'completed', exit_code: 0 });
    emit('wsl-status', { status: 'cancelled', transcribed: 1, exit_code: 1 });
    emit('wsl-status', { status: 'failed', exit_code: 7 });
    await vi.waitFor(() => expect(load).toHaveBeenCalledTimes(4));
    expect(get(notifications).some((item) => item.type === 'success')).toBe(true);
    expect(get(notifications).some((item) => item.type === 'warning')).toBe(true);
    expect(get(notifications).some((item) => item.type === 'info')).toBe(true);
    expect(get(notifications).some((item) => item.message.includes('7'))).toBe(true);
  });

  it('contains batch settlement callback failures and invalidates stale authorities exactly once', async () => {
    await startEventListeners();
    setBatchWorkerSettledHandler(() => {
      throw new Error('sync batch reconciliation');
    });
    const first = beginBatchEventScope(RUN_A, 'normalize', 2);
    emit('batch-progress', {
      type: 'started',
      operationId: RUN_A,
      operation: 'normalize',
      total: 2,
    });
    emit('batch-progress', {
      type: 'progress',
      operationId: RUN_A,
      operation: 'normalize',
      total: 2,
      current: 1,
      status: 'normalizing',
    });
    emit('batch-worker-settled', { operationId: RUN_A, operation: 'normalize' });
    await vi.waitFor(() => expect(get(notifications).at(-1)?.type).toBe('error'));
    expect(first.hasSettledEvent()).toBe(true);
    expect(markBatchEventSettled(first)).toBe(false);
    closeBatchEventScope(first);
    expect(first.isCurrent()).toBe(false);
    closeBatchEventScope(first);

    setBatchWorkerSettledHandler(
      vi.fn().mockRejectedValue(new Error('async batch reconciliation')),
    );
    const second = beginBatchEventScope(RUN_B, 'transcribe', 1);
    emit('batch-progress', {
      type: 'halted',
      operationId: RUN_B,
      operation: 'transcribe',
      total: 1,
      succeeded: 0,
      failed: 1,
      abandoned: 0,
      cancelled: false,
      error: { code: 'CHAMPION_UNAVAILABLE' },
    });
    emit('batch-worker-settled', { operationId: RUN_B, operation: 'transcribe' });
    await vi.waitFor(() => expect(get(notifications).at(-1)?.type).toBe('error'));
    expect(second.terminalEvent()).toMatchObject({ type: 'halted' });

    expect(() => beginImportEventScope('invalid id', 'file')).toThrow(
      'Invalid import event authority',
    );
    expect(() => beginBatchEventScope('invalid', 'transcribe', 1)).toThrow(
      'Invalid batch event authority',
    );
    closeBatchEventScope(second);
    for (const count of [0, -1, 100_001, 1.5]) {
      expect(() => beginBatchEventScope(RUN_A, 'normalize', count)).toThrow(
        'Invalid batch event authority',
      );
    }
  });

  it('is inert outside desktop and tears down every listener even when one unsubscriber throws', async () => {
    desktop.available = false;
    await startEventListeners();
    expect(desktop.listen).not.toHaveBeenCalled();

    desktop.available = true;
    await startEventListeners();
    const registrations = desktop.listen.mock.calls.length;
    expect(registrations).toBeGreaterThan(5);
    desktop.unlisten.mockImplementationOnce(() => {
      throw new Error('one teardown refused');
    });
    stopEventListeners();
    expect(desktop.unlisten).toHaveBeenCalledTimes(registrations);

    const stale = beginImportEventScope(RUN_A, 'file');
    const current = beginImportEventScope(RUN_B, 'file');
    closeImportEventScope(stale);
    expect(current.isCurrent()).toBe(true);
    closeImportEventScope(current);
    expect(current.isCurrent()).toBe(false);
  });
});
