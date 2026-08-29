import { get } from 'svelte/store';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { AgenticReadiness } from '../../src/lib/commands';
import type { ImportEventContext } from '../../src/lib/events';

type ContextState = {
  current: boolean;
  observed: boolean;
  terminal: boolean;
};

type ControlledContext = {
  context: ImportEventContext;
  state: ContextState;
};

const commandMocks = vi.hoisted(() => ({
  openAudioFile: vi.fn(),
  checkAgenticReadiness: vi.fn(),
  importAudioFile: vi.fn(),
  importDirectory: vi.fn(),
  resumeInterruptedImport: vi.fn(),
  getImportRunStatus: vi.fn(),
}));

const authority = vi.hoisted(() => ({
  sequence: 0,
  contexts: [] as ControlledContext[],
}));

vi.mock('../../src/lib/commands', () => commandMocks);

vi.mock('../../src/lib/events', () => ({
  createImportRunId: () => `run-${++authority.sequence}`,
  beginImportEventScope: (runId: string, source: 'file' | 'directory') => {
    const state: ContextState = { current: true, observed: false, terminal: false };
    const context: ImportEventContext = {
      runId,
      source,
      generation: authority.sequence,
      isCurrent: () => state.current,
      hasObservedEvent: () => state.current && state.observed,
      hasTerminalEvent: () => state.current && state.terminal,
    };
    authority.contexts.push({ context, state });
    return context;
  },
  closeImportEventScope: (context: ImportEventContext) => {
    const controlled = authority.contexts.find((candidate) => candidate.context === context);
    if (controlled) controlled.state.current = false;
  },
  markImportEventSettled: (context: ImportEventContext) => {
    const controlled = authority.contexts.find((candidate) => candidate.context === context);
    if (!controlled?.state.current || controlled.state.terminal) return false;
    controlled.state.observed = true;
    controlled.state.terminal = true;
    return true;
  },
}));

import {
  createImportOperationCoordinator,
  type ImportOperationCoordinatorOptions,
} from '../../src/lib/importOperationCoordinator';
import { activeOperations } from '../../src/lib/invoke';
import { locale } from '../../src/lib/i18n';
import { notifications } from '../../src/lib/stores/notificationStore';
import { selectedSegmentId } from '../../src/lib/stores/segmentStore';
import {
  agentPipelineStages,
  filesProcessed,
  isProcessing,
  pipelineCurrentFile,
  pipelinePhase,
  pipelineStatus,
  pipelineTotal,
  statusMessage,
} from '../../src/lib/stores/uiStore';

const READY: AgenticReadiness = {
  status: 'ready',
  ready: true,
  sourceReferenceModels: ['champion'],
  sourceReferenceModelCount: 1,
  availableHypothesisModels: [],
  availableHypothesisModelCount: 0,
  requiredHypothesisModels: 0,
  checks: [],
  checkCount: 0,
};

const CANCELLED_FILE = {
  schema: 1,
  code: 'E_FILE_PICKER_CANCELLED',
  message: 'cancelled',
  retryable: false,
};

const CANCELLED_DIRECTORY = {
  schema: 1,
  code: 'E_DIRECTORY_PICKER_CANCELLED',
  message: 'cancelled',
  retryable: false,
};

const coordinators: Array<{ destroy: () => void }> = [];

function controlledContext(index = authority.contexts.length - 1): ControlledContext {
  const controlled = authority.contexts[index];
  if (!controlled) throw new Error(`Missing controlled import context at ${index}`);
  return controlled;
}

function deferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function harness(overrides: Partial<ImportOperationCoordinatorOptions> = {}) {
  const options: ImportOperationCoordinatorOptions = {
    requireDesktopRuntime: vi.fn(() => true),
    canStartImport: vi.fn(() => true),
    setStarting: vi.fn(),
    loadSegments: vi.fn(async () => {}),
    loadLatestAgentHistory: vi.fn(async () => {}),
    reconcileRecovery: vi.fn(async () => {}),
    clearAgentEvidence: vi.fn(),
    ...overrides,
  };
  const coordinator = createImportOperationCoordinator(options);
  const result = { coordinator, options };
  coordinators.push(coordinator);
  return result;
}

function exactStatus(status: 'running' | 'settled' | 'rejected' | 'unknown') {
  return { runId: controlledContext().context.runId, status };
}

function notices(type?: 'success' | 'error' | 'info' | 'warning') {
  const current = get(notifications);
  return type ? current.filter((notice) => notice.type === type) : current;
}

function resetUi(): void {
  isProcessing.set(false);
  pipelinePhase.set('idle');
  pipelineCurrentFile.set('');
  pipelineStatus.set('');
  pipelineTotal.set(0);
  filesProcessed.set(0);
  agentPipelineStages.set([]);
  statusMessage.set('Ready');
  selectedSegmentId.set(null);
  activeOperations.set(new Set());
}

beforeEach(() => {
  authority.sequence = 0;
  authority.contexts.length = 0;
  commandMocks.openAudioFile.mockReset().mockResolvedValue('C:\\audio\\clip.wav');
  commandMocks.checkAgenticReadiness.mockReset().mockResolvedValue(READY);
  commandMocks.importAudioFile.mockReset().mockResolvedValue({ accepted: true });
  commandMocks.importDirectory.mockReset().mockResolvedValue({ accepted: true });
  commandMocks.resumeInterruptedImport.mockReset().mockResolvedValue({ resumed: true });
  commandMocks.getImportRunStatus.mockReset().mockImplementation(async (runId: string) => ({
    runId,
    status: 'rejected',
  }));
  locale.set('en');
  notifications.clear();
  resetUi();
  vi.spyOn(console, 'error').mockImplementation(() => {});
});

afterEach(() => {
  for (const coordinator of coordinators.splice(0)) coordinator.destroy();
  notifications.clear();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe('import operation admission and exact-run reconciliation', () => {
  it('blocks duplicate, busy, non-desktop, and disallowed starts before native mutation', async () => {
    isProcessing.set(true);
    await harness().coordinator.openFile();

    isProcessing.set(false);
    await harness({ requireDesktopRuntime: () => false }).coordinator.openFile();
    await harness({ canStartImport: () => false }).coordinator.importDirectory();
    expect(commandMocks.openAudioFile).not.toHaveBeenCalled();
    expect(commandMocks.importDirectory).not.toHaveBeenCalled();

    const picker = deferred<string | null>();
    commandMocks.openAudioFile.mockReturnValueOnce(picker.promise);
    const concurrent = harness();
    const first = concurrent.coordinator.openFile();
    await Promise.resolve();
    await concurrent.coordinator.openFile();
    expect(commandMocks.openAudioFile).toHaveBeenCalledOnce();
    picker.resolve(null);
    await first;
    expect(get(isProcessing)).toBe(false);
    expect(concurrent.options.setStarting).toHaveBeenLastCalledWith(false);
  });

  it('reports blocked, degraded, and failed advisory readiness without publishing false success', async () => {
    commandMocks.checkAgenticReadiness.mockResolvedValueOnce({
      ...READY,
      ready: false,
      status: 'blocked',
      checks: [
        { code: 'champion', status: 'blocked' },
        { code: 'storage', status: 'unknown' },
        { code: 'healthy', status: 'ready' },
      ],
      checkCount: 3,
    });
    await harness().coordinator.openFile();
    expect(notices('warning').at(-1)).toMatchObject({ detail: '2 blocker(s)' });

    resetUi();
    notifications.clear();
    commandMocks.checkAgenticReadiness.mockResolvedValueOnce({
      ...READY,
      ready: false,
      status: 'degraded',
      checks: [{ code: 'optional', status: 'degraded' }],
      checkCount: 1,
    });
    await harness().coordinator.openFile();
    expect(notices('info').at(-1)).toMatchObject({ detail: '1 blocker(s)' });

    resetUi();
    notifications.clear();
    commandMocks.checkAgenticReadiness.mockRejectedValueOnce(new Error('probe unavailable'));
    await harness().coordinator.openFile();
    expect(notices('warning').at(-1)?.message).toContain('readiness');
    expect(commandMocks.importAudioFile).toHaveBeenCalledTimes(3);
  });

  it('treats picker cancellation as neutral and a definite file rejection as actionable failure', async () => {
    commandMocks.openAudioFile.mockRejectedValueOnce(CANCELLED_FILE);
    await harness().coordinator.openFile();
    expect(notices('error')).toHaveLength(0);
    expect(get(isProcessing)).toBe(false);

    commandMocks.importAudioFile.mockRejectedValueOnce({
      schema: 1,
      code: 'CHAMPION_UNAVAILABLE',
      message: 'private backend detail',
      retryable: true,
      suggestedAction: 'openModels',
    });
    await harness().coordinator.openFile();
    expect(notices('error').at(-1)).toMatchObject({
      suggestedAction: 'openModels',
      retryable: true,
    });
    expect(get(isProcessing)).toBe(false);
    expect(controlledContext().state.current).toBe(false);
  });

  it('retains exact running/ambiguous starts, suppresses duplicate resume errors, then settles by status', async () => {
    vi.useFakeTimers();
    commandMocks.importAudioFile.mockImplementationOnce(async () => {
      throw new Error('lost response');
    });
    commandMocks.getImportRunStatus.mockImplementationOnce(async () => exactStatus('running'));
    const running = harness();
    await running.coordinator.openFile();
    expect(get(isProcessing)).toBe(true);
    expect(notices('warning')).toHaveLength(1);
    expect(running.coordinator.shouldSuppressResumeFailure()).toBe(false);

    commandMocks.getImportRunStatus.mockImplementation(async () => exactStatus('settled'));
    await vi.advanceTimersByTimeAsync(5_000);
    expect(get(isProcessing)).toBe(false);
    expect(running.options.loadSegments).toHaveBeenCalledOnce();
    expect(notices('info').some((notice) => notice.message.includes('refreshed'))).toBe(true);

    resetUi();
    notifications.clear();
    commandMocks.importAudioFile.mockImplementationOnce(async () => {
      controlledContext().state.observed = true;
      throw new Error('event raced response');
    });
    commandMocks.getImportRunStatus.mockImplementation(async () => exactStatus('rejected'));
    const ambiguous = harness();
    await ambiguous.coordinator.openFile();
    expect(ambiguous.coordinator.shouldSuppressResumeFailure()).toBe(true);
    expect(get(isProcessing)).toBe(true);
    ambiguous.coordinator.destroy();
    expect(ambiguous.coordinator.shouldSuppressResumeFailure()).toBe(false);
  });

  it('handles directory cancellation, definite failure, and response-loss recovery independently', async () => {
    commandMocks.importDirectory.mockRejectedValueOnce(CANCELLED_DIRECTORY);
    const cancelled = harness();
    await cancelled.coordinator.importDirectory();
    expect(cancelled.options.loadLatestAgentHistory).toHaveBeenCalledWith();
    expect(notices('error')).toHaveLength(0);
    expect(get(statusMessage)).toBe('Ready');

    resetUi();
    commandMocks.importDirectory.mockRejectedValueOnce(new Error('directory refused'));
    const rejected = harness();
    await rejected.coordinator.importDirectory();
    expect(notices('error').at(-1)?.message).toContain('Import failed');
    expect(get(statusMessage)).toContain('Import failed');

    resetUi();
    notifications.clear();
    commandMocks.importDirectory.mockRejectedValueOnce(new Error('lost directory response'));
    commandMocks.getImportRunStatus.mockImplementationOnce(async () => exactStatus('running'));
    const accepted = harness();
    await accepted.coordinator.importDirectory();
    expect(get(isProcessing)).toBe(true);
    expect(notices('warning')).toHaveLength(1);
    expect(accepted.options.reconcileRecovery).not.toHaveBeenCalled();
  });

  it('returns accepted resume responses, clears rejected runs, and preserves uncertain run authority', async () => {
    const accepted = harness();
    await expect(accepted.coordinator.resume('job-a')).resolves.toEqual({ resumed: true });

    resetUi();
    const refusal = new Error('resume refused');
    commandMocks.resumeInterruptedImport.mockRejectedValueOnce(refusal);
    const rejected = harness();
    await expect(rejected.coordinator.resume('job-b')).rejects.toBe(refusal);
    expect(get(isProcessing)).toBe(false);
    expect(rejected.coordinator.shouldSuppressResumeFailure()).toBe(false);

    resetUi();
    commandMocks.resumeInterruptedImport.mockImplementationOnce(async () => {
      controlledContext().state.observed = true;
      throw new Error('resume response lost');
    });
    const uncertain = harness();
    await expect(uncertain.coordinator.resume('job-c')).rejects.toThrow('resume response lost');
    expect(uncertain.coordinator.shouldSuppressResumeFailure()).toBe(true);
    expect(get(isProcessing)).toBe(true);

    resetUi();
    commandMocks.resumeInterruptedImport.mockRejectedValueOnce(new Error('lost but running'));
    commandMocks.getImportRunStatus.mockImplementationOnce(async () => exactStatus('running'));
    const running = harness();
    await expect(running.coordinator.resume('job-d')).resolves.toBeUndefined();
    expect(get(isProcessing)).toBe(true);
  });
});

describe('import completion, enrichment, and worker settlement', () => {
  it('refreshes file outcomes, selects the first exact segment, and reports all terminal shapes', async () => {
    const setup = harness();
    await setup.coordinator.openFile();
    const { context } = controlledContext();

    await setup.coordinator.handleComplete(
      {
        runId: context.runId,
        source: 'file',
        total: 1,
        succeeded: 0,
        failed: 1,
        segmentIds: ['segment-a'],
      },
      context,
    );
    expect(get(selectedSegmentId)).toBe('segment-a');
    expect(notices('error').at(-1)?.message).toContain('open');

    await setup.coordinator.handleComplete(
      {
        runId: context.runId,
        source: 'file',
        total: 3,
        succeeded: 3,
        failed: 0,
        segmentCount: 3,
      },
      context,
    );
    expect(notices('success').at(-1)?.message).toContain('3 segments');

    await setup.coordinator.handleComplete(
      {
        runId: context.runId,
        source: 'file',
        total: 1,
        succeeded: 1,
        failed: 0,
      },
      context,
    );
    expect(notices('success').at(-1)?.message).toContain('imported');
  });

  it('refreshes directory history/recovery and abandons stale refreshes without false completion', async () => {
    const setup = harness();
    await setup.coordinator.importDirectory();
    const controlled = controlledContext();
    await setup.coordinator.handleComplete(
      {
        runId: controlled.context.runId,
        source: 'directory',
        total: 2,
        succeeded: 2,
        failed: 0,
      },
      controlled.context,
    );
    expect(setup.options.loadLatestAgentHistory).toHaveBeenCalledWith(
      controlled.context.runId,
      controlled.context,
    );
    expect(setup.options.reconcileRecovery).toHaveBeenCalledOnce();
    expect(get(statusMessage)).toContain('complete');

    const staleAfterSegments = harness({
      loadSegments: vi.fn(async () => {
        controlledContext().state.current = false;
      }),
    });
    resetUi();
    await staleAfterSegments.coordinator.importDirectory();
    const staleContext = controlledContext();
    await staleAfterSegments.coordinator.handleComplete(
      {
        runId: staleContext.context.runId,
        source: 'directory',
        total: 1,
        succeeded: 1,
        failed: 0,
      },
      staleContext.context,
    );
    expect(staleAfterSegments.options.loadLatestAgentHistory).not.toHaveBeenCalled();
    expect(staleAfterSegments.options.reconcileRecovery).not.toHaveBeenCalled();
  });

  it('contains current refresh failures, ignores stale failures, and guards enrichment history', async () => {
    const current = harness({
      loadSegments: vi.fn(async () => {
        throw new Error('refresh failed');
      }),
    });
    await current.coordinator.openFile();
    await current.coordinator.handleComplete(
      {
        runId: controlledContext().context.runId,
        source: 'file',
        total: 1,
        succeeded: 1,
        failed: 0,
      },
      controlledContext().context,
    );
    expect(notices('error').at(-1)?.message).toContain('refresh');

    notifications.clear();
    const stale = harness({
      loadSegments: vi.fn(async () => {
        controlledContext().state.current = false;
        throw new Error('stale refresh failed');
      }),
    });
    resetUi();
    await stale.coordinator.openFile();
    await stale.coordinator.handleComplete(
      {
        runId: controlledContext().context.runId,
        source: 'file',
        total: 1,
        succeeded: 1,
        failed: 0,
      },
      controlledContext().context,
    );
    expect(notices('error')).toHaveLength(0);

    const enrichment = harness();
    resetUi();
    await enrichment.coordinator.openFile();
    const enrichmentContext = controlledContext();
    await enrichment.coordinator.handleEnrichment(
      { runId: enrichmentContext.context.runId, source: 'file' },
      enrichmentContext.context,
    );
    expect(enrichment.options.loadLatestAgentHistory).toHaveBeenCalledWith(
      enrichmentContext.context.runId,
      enrichmentContext.context,
    );
    enrichment.options.loadSegments = vi.fn(async () => {
      enrichmentContext.state.current = false;
    });
    await enrichment.coordinator.handleEnrichment(
      { runId: enrichmentContext.context.runId, source: 'file' },
      enrichmentContext.context,
    );
  });

  it('settles each exact worker once, starts directory recovery immediately, and suppresses duplicate recovery copy', async () => {
    const segmentLoad = deferred<void>();
    const recovery = deferred<void>();
    const setup = harness({
      loadSegments: vi.fn(() => segmentLoad.promise),
      reconcileRecovery: vi.fn(() => recovery.promise),
    });
    await setup.coordinator.importDirectory();
    const controlled = controlledContext();
    const first = setup.coordinator.settleFromWorker(
      { runId: controlled.context.runId, source: 'directory' },
      controlled.context,
    );
    const duplicate = setup.coordinator.settleFromWorker(
      { runId: controlled.context.runId, source: 'directory' },
      controlled.context,
    );
    expect(setup.options.reconcileRecovery).toHaveBeenCalledOnce();
    expect(setup.options.loadSegments).toHaveBeenCalledOnce();
    expect(get(isProcessing)).toBe(false);
    await duplicate;
    segmentLoad.resolve();
    recovery.resolve();
    await first;
    expect(setup.options.loadLatestAgentHistory).toHaveBeenCalledWith(
      controlled.context.runId,
      controlled.context,
    );
    expect(notices('info').at(-1)?.message).toContain('refreshed');

    resetUi();
    notifications.clear();
    const observed = harness();
    await observed.coordinator.openFile();
    const observedContext = controlledContext();
    await observed.coordinator.handleComplete(
      {
        runId: observedContext.context.runId,
        source: 'file',
        total: 1,
        succeeded: 1,
        failed: 0,
      },
      observedContext.context,
    );
    const infoBefore = notices('info').length;
    await observed.coordinator.settleFromWorker(
      { runId: observedContext.context.runId, source: 'file' },
      observedContext.context,
    );
    expect(notices('info')).toHaveLength(infoBefore);
  });

  it('reports only current settlement refresh failures and refuses stale/source-mismatched settlement', async () => {
    const setup = harness({
      loadSegments: vi.fn(async () => {
        throw new Error('post-settlement load failed');
      }),
    });
    await setup.coordinator.openFile();
    const controlled = controlledContext();
    await setup.coordinator.settleFromWorker(
      { runId: 'other-run', source: 'file' },
      controlled.context,
    );
    await setup.coordinator.settleFromWorker(
      { runId: controlled.context.runId, source: 'directory' },
      controlled.context,
    );
    expect(setup.options.loadSegments).not.toHaveBeenCalled();

    await setup.coordinator.settleFromWorker(
      { runId: controlled.context.runId, source: 'file' },
      controlled.context,
    );
    expect(notices('error').at(-1)?.message).toContain('refresh');
  });
});

describe('import status monitor', () => {
  it('gives a still-pending directory command a bounded rejection grace period, then fails closed', async () => {
    vi.useFakeTimers();
    commandMocks.importDirectory.mockReturnValueOnce(new Promise(() => {}));
    const setup = harness();
    void setup.coordinator.importDirectory();
    await vi.advanceTimersByTimeAsync(0);
    expect(authority.contexts).toHaveLength(1);

    await vi.advanceTimersByTimeAsync(7_100);
    expect(commandMocks.getImportRunStatus.mock.calls.length).toBeGreaterThanOrEqual(4);
    expect(get(isProcessing)).toBe(false);
    expect(notices('error')).toHaveLength(1);
    expect(setup.options.reconcileRecovery).toHaveBeenCalledOnce();
  });
});
