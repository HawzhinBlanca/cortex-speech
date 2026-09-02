import { describe, expect, it, vi } from 'vitest';
import { createImportRecoveryController } from './importRecoveryController';

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

describe('import recovery controller', () => {
  it('clears only the exact journal after success and suppresses a duplicate in-flight action', async () => {
    const pending = deferred<void>();
    const clearIfCurrent = vi.fn();
    const setBusy = vi.fn();
    const resume = vi.fn(() => pending.promise);
    const controller = createImportRecoveryController({
      currentJob: () => ({ id: 'job-1' }),
      setBusy,
      clearIfCurrent,
      load: vi.fn(),
      replaceCurrent: vi.fn(),
      resume,
      discard: vi.fn(),
      onResumeSuccess: vi.fn(),
      onResumeFailure: vi.fn(),
      onDiscardFailure: vi.fn(),
      onLoadFailure: vi.fn(),
    });

    const first = controller.resume();
    await controller.resume();
    expect(resume).toHaveBeenCalledOnce();
    expect(resume).toHaveBeenCalledWith('job-1');
    expect(setBusy).toHaveBeenNthCalledWith(1, true);
    expect(clearIfCurrent).not.toHaveBeenCalled();

    pending.resolve();
    await first;
    expect(clearIfCurrent).toHaveBeenCalledWith('job-1');
    expect(setBusy).toHaveBeenLastCalledWith(false);
  });

  it('retains the journal and forwards the exact structured refusal after resume failure', async () => {
    const refusal = { schema: 1, code: 'IMPORT_SOURCE_MISSING' };
    const clearIfCurrent = vi.fn();
    const successor = { id: 'job-2' };
    const replaceCurrent = vi.fn();
    const onResumeFailure = vi.fn();
    const controller = createImportRecoveryController({
      currentJob: () => ({ id: 'job-1' }),
      setBusy: vi.fn(),
      clearIfCurrent,
      load: vi.fn().mockResolvedValue(successor),
      replaceCurrent,
      resume: vi.fn().mockRejectedValue(refusal),
      discard: vi.fn(),
      onResumeSuccess: vi.fn(),
      onResumeFailure,
      onDiscardFailure: vi.fn(),
      onLoadFailure: vi.fn(),
    });

    await controller.resume();
    expect(clearIfCurrent).not.toHaveBeenCalled();
    expect(onResumeFailure).toHaveBeenCalledWith(refusal);
    expect(replaceCurrent).toHaveBeenCalledWith(successor);
  });

  it('reconciles a lost resume response to no interrupted job while the backend worker is live', async () => {
    const responseLost = { schema: 1, code: 'IPC_RESPONSE_LOST' };
    const clearIfCurrent = vi.fn();
    const replaceCurrent = vi.fn();
    const onResumeFailure = vi.fn();
    const controller = createImportRecoveryController({
      currentJob: () => ({ id: 'crashed-job' }),
      setBusy: vi.fn(),
      clearIfCurrent,
      // The hardened backend returns None while ImportState::Running, so a live successor cannot
      // be rendered with an enabled Discard action after an ambiguous transport failure.
      load: vi.fn().mockResolvedValue(null),
      replaceCurrent,
      resume: vi.fn().mockRejectedValue(responseLost),
      discard: vi.fn(),
      onResumeSuccess: vi.fn(),
      onResumeFailure,
      onDiscardFailure: vi.fn(),
      onLoadFailure: vi.fn(),
    });

    await controller.resume();
    expect(clearIfCurrent).not.toHaveBeenCalled();
    expect(onResumeFailure).toHaveBeenCalledWith(responseLost);
    expect(replaceCurrent).toHaveBeenCalledWith(null);
  });

  it('retains the same journal after a proven discard failure and does nothing when none exists', async () => {
    const onDiscardFailure = vi.fn();
    const discard = vi.fn().mockRejectedValue(new Error('transport unavailable'));
    const replaceCurrent = vi.fn();
    let currentJob: { id: string } | null = { id: 'job-2' };
    const controller = createImportRecoveryController({
      currentJob: () => currentJob,
      setBusy: vi.fn(),
      clearIfCurrent: vi.fn(),
      load: vi.fn().mockResolvedValue({ id: 'job-2' }),
      replaceCurrent,
      resume: vi.fn(),
      discard,
      onResumeSuccess: vi.fn(),
      onResumeFailure: vi.fn(),
      onDiscardFailure,
      onLoadFailure: vi.fn(),
    });

    await controller.discard();
    expect(onDiscardFailure).toHaveBeenCalledOnce();
    expect(replaceCurrent).toHaveBeenCalledWith({ id: 'job-2' });
    currentJob = null;
    await controller.discard();
    expect(discard).toHaveBeenCalledOnce();
  });

  it('bounds a lost discard response and accepts authoritative absence as success', async () => {
    const replaceCurrent = vi.fn();
    const setBusy = vi.fn();
    const onDiscardFailure = vi.fn();
    const controller = createImportRecoveryController({
      currentJob: () => ({ id: 'job-1' }),
      setBusy,
      clearIfCurrent: vi.fn(),
      load: vi.fn().mockResolvedValue(null),
      replaceCurrent,
      discard: vi.fn(() => new Promise<never>(() => undefined)),
      resume: vi.fn(),
      onResumeSuccess: vi.fn(),
      onResumeFailure: vi.fn(),
      onDiscardFailure,
      onLoadFailure: vi.fn(),
      delayBeforeRetry: async () => undefined,
      attemptTimeoutMs: 1,
    });

    await controller.discard();

    expect(replaceCurrent).toHaveBeenCalledWith(null);
    expect(onDiscardFailure).not.toHaveBeenCalled();
    expect(setBusy).toHaveBeenNthCalledWith(1, true);
    expect(setBusy).toHaveBeenLastCalledWith(false);
  });

  it('preserves a successor journal after an ambiguous discard response', async () => {
    const successor = { id: 'job-2' };
    const replaceCurrent = vi.fn();
    const clearIfCurrent = vi.fn();
    const onDiscardFailure = vi.fn();
    const controller = createImportRecoveryController({
      currentJob: () => ({ id: 'job-1' }),
      setBusy: vi.fn(),
      clearIfCurrent,
      load: vi.fn().mockResolvedValue(successor),
      replaceCurrent,
      discard: vi.fn(() => new Promise<never>(() => undefined)),
      resume: vi.fn(),
      onResumeSuccess: vi.fn(),
      onResumeFailure: vi.fn(),
      onDiscardFailure,
      onLoadFailure: vi.fn(),
      delayBeforeRetry: async () => undefined,
      attemptTimeoutMs: 1,
    });

    await controller.discard();

    expect(replaceCurrent).toHaveBeenCalledWith(successor);
    expect(clearIfCurrent).toHaveBeenCalledWith('job-1');
    expect(onDiscardFailure).not.toHaveBeenCalled();
  });

  it('fails closed when both discard response and journal authority are unavailable', async () => {
    const authorityStates: string[] = [];
    const onDiscardFailure = vi.fn();
    const onLoadFailure = vi.fn();
    const controller = createImportRecoveryController({
      currentJob: () => ({ id: 'job-1' }),
      setBusy: vi.fn(),
      clearIfCurrent: vi.fn(),
      load: vi.fn().mockRejectedValue(new Error('journal unreadable')),
      replaceCurrent: vi.fn(),
      discard: vi.fn(() => new Promise<never>(() => undefined)),
      resume: vi.fn(),
      onResumeSuccess: vi.fn(),
      onResumeFailure: vi.fn(),
      onDiscardFailure,
      onLoadFailure,
      setAuthorityState: (state) => authorityStates.push(state),
      delayBeforeRetry: async () => undefined,
      attemptTimeoutMs: 1,
    });

    await controller.discard();

    expect(onDiscardFailure).toHaveBeenCalledOnce();
    expect(onLoadFailure).toHaveBeenCalledOnce();
    expect(authorityStates).toEqual(['checking', 'unknown']);
  });

  it('keeps the current journal when reconciliation itself fails', async () => {
    const loadFailure = { schema: 1, code: 'IMPORT_JOURNAL_READ_FAILED' };
    const replaceCurrent = vi.fn();
    const onLoadFailure = vi.fn();
    const controller = createImportRecoveryController({
      currentJob: () => ({ id: 'job-3' }),
      setBusy: vi.fn(),
      clearIfCurrent: vi.fn(),
      load: vi.fn().mockRejectedValue(loadFailure),
      replaceCurrent,
      resume: vi.fn(),
      discard: vi.fn(),
      onResumeSuccess: vi.fn(),
      onResumeFailure: vi.fn(),
      onDiscardFailure: vi.fn(),
      onLoadFailure,
    });

    await controller.reconcile();
    expect(replaceCurrent).not.toHaveBeenCalled();
    expect(onLoadFailure).toHaveBeenCalledWith(loadFailure);
  });

  it('never lets an older journal read overwrite a newer reconciliation', async () => {
    const older = deferred<{ id: string } | null>();
    const newer = deferred<{ id: string } | null>();
    const replaceCurrent = vi.fn();
    const load = vi.fn().mockReturnValueOnce(older.promise).mockReturnValueOnce(newer.promise);
    const controller = createImportRecoveryController({
      currentJob: () => ({ id: 'job-1' }),
      setBusy: vi.fn(),
      clearIfCurrent: vi.fn(),
      load,
      replaceCurrent,
      resume: vi.fn(),
      discard: vi.fn(),
      onResumeSuccess: vi.fn(),
      onResumeFailure: vi.fn(),
      onDiscardFailure: vi.fn(),
      onLoadFailure: vi.fn(),
    });

    const first = controller.reconcile();
    const second = controller.reconcile();
    newer.resolve(null);
    await second;
    older.resolve({ id: 'stale-job' });
    await first;

    expect(replaceCurrent).toHaveBeenCalledOnce();
    expect(replaceCurrent).toHaveBeenCalledWith(null);
  });

  it('never lets a read started before a successful mutation resurrect the cleared journal', async () => {
    const staleRead = deferred<{ id: string } | null>();
    const replaceCurrent = vi.fn();
    const clearIfCurrent = vi.fn();
    const controller = createImportRecoveryController({
      currentJob: () => ({ id: 'job-1' }),
      setBusy: vi.fn(),
      clearIfCurrent,
      load: vi.fn().mockReturnValueOnce(staleRead.promise).mockResolvedValueOnce(null),
      replaceCurrent,
      resume: vi.fn(),
      discard: vi.fn().mockResolvedValue(undefined),
      onResumeSuccess: vi.fn(),
      onResumeFailure: vi.fn(),
      onDiscardFailure: vi.fn(),
      onLoadFailure: vi.fn(),
    });

    const reconcile = controller.reconcile();
    await controller.discard();
    expect(clearIfCurrent).toHaveBeenCalledWith('job-1');

    staleRead.resolve({ id: 'job-1' });
    await reconcile;

    expect(replaceCurrent).toHaveBeenCalledOnce();
    expect(replaceCurrent).toHaveBeenCalledWith(null);
    expect(replaceCurrent).not.toHaveBeenCalledWith({ id: 'job-1' });
  });

  it('queues a causally newer worker-settled reconciliation behind the resume response', async () => {
    const resumeResponse = deferred<void>();
    const settledRead = deferred<{ id: string } | null>();
    const successor = { id: 'successor-job' };
    const replaceCurrent = vi.fn();
    const clearIfCurrent = vi.fn();
    const load = vi.fn().mockReturnValue(settledRead.promise);
    const controller = createImportRecoveryController({
      currentJob: () => ({ id: 'crashed-job' }),
      setBusy: vi.fn(),
      clearIfCurrent,
      load,
      replaceCurrent,
      resume: vi.fn().mockReturnValue(resumeResponse.promise),
      discard: vi.fn(),
      onResumeSuccess: vi.fn(),
      onResumeFailure: vi.fn(),
      onDiscardFailure: vi.fn(),
      onLoadFailure: vi.fn(),
    });

    const resume = controller.resume();
    await controller.reconcile();
    expect(load).not.toHaveBeenCalled();

    resumeResponse.resolve();
    await vi.waitFor(() => expect(load).toHaveBeenCalledOnce());
    settledRead.resolve(successor);
    await resume;

    expect(clearIfCurrent).toHaveBeenCalledWith('crashed-job');
    expect(replaceCurrent).toHaveBeenCalledWith(successor);
  });

  it('retries a post-resume authority read and exposes the durable successor', async () => {
    const transientFailure = new Error('one transient read failure');
    const successor = { id: 'failed-successor' };
    const replaceCurrent = vi.fn();
    const authorityStates: string[] = [];
    const onLoadFailure = vi.fn();
    const load = vi.fn().mockRejectedValueOnce(transientFailure).mockResolvedValueOnce(successor);
    const controller = createImportRecoveryController({
      currentJob: () => ({ id: 'crashed-job' }),
      setBusy: vi.fn(),
      clearIfCurrent: vi.fn(),
      load,
      replaceCurrent,
      resume: vi.fn().mockResolvedValue(undefined),
      discard: vi.fn(),
      onResumeSuccess: vi.fn(),
      onResumeFailure: vi.fn(),
      onDiscardFailure: vi.fn(),
      onLoadFailure,
      setAuthorityState: (state) => authorityStates.push(state),
      delayBeforeRetry: async () => {},
    });

    await controller.resume();

    expect(load).toHaveBeenCalledTimes(2);
    expect(replaceCurrent).toHaveBeenCalledWith(successor);
    expect(onLoadFailure).not.toHaveBeenCalled();
    expect(authorityStates).toEqual(['checking', 'known']);
  });

  it('fails closed as recovery-unknown after all bounded authority reads fail', async () => {
    const failure = new Error('journal unavailable');
    const authorityStates: string[] = [];
    const replaceCurrent = vi.fn();
    const onLoadFailure = vi.fn();
    const controller = createImportRecoveryController({
      currentJob: () => null,
      setBusy: vi.fn(),
      clearIfCurrent: vi.fn(),
      load: vi.fn().mockRejectedValue(failure),
      replaceCurrent,
      resume: vi.fn(),
      discard: vi.fn(),
      onResumeSuccess: vi.fn(),
      onResumeFailure: vi.fn(),
      onDiscardFailure: vi.fn(),
      onLoadFailure,
      setAuthorityState: (state) => authorityStates.push(state),
      delayBeforeRetry: async () => {},
    });

    await controller.reconcile();

    expect(replaceCurrent).not.toHaveBeenCalled();
    expect(onLoadFailure).toHaveBeenCalledOnce();
    expect(authorityStates).toEqual(['checking', 'unknown']);
  });

  it('times out a never-settling journal IPC and exposes retryable unknown authority', async () => {
    const authorityStates: string[] = [];
    const load = vi.fn(() => new Promise<never>(() => undefined));
    const onLoadFailure = vi.fn();
    const controller = createImportRecoveryController({
      currentJob: () => null,
      setBusy: vi.fn(),
      clearIfCurrent: vi.fn(),
      load,
      replaceCurrent: vi.fn(),
      resume: vi.fn(),
      discard: vi.fn(),
      onResumeSuccess: vi.fn(),
      onResumeFailure: vi.fn(),
      onDiscardFailure: vi.fn(),
      onLoadFailure,
      setAuthorityState: (state) => authorityStates.push(state),
      delayBeforeRetry: async () => undefined,
      attemptTimeoutMs: 1,
    });

    await controller.reconcile();

    expect(load).toHaveBeenCalledTimes(3);
    expect(onLoadFailure).toHaveBeenCalledOnce();
    expect(authorityStates).toEqual(['checking', 'unknown']);
  });
});
