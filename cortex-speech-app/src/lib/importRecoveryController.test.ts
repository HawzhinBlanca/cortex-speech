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

  it('retains the journal after discard failure and does nothing when no journal exists', async () => {
    const onDiscardFailure = vi.fn();
    const discard = vi.fn().mockRejectedValue(new Error('transport unavailable'));
    const replaceCurrent = vi.fn();
    let currentJob: { id: string } | null = { id: 'job-2' };
    const controller = createImportRecoveryController({
      currentJob: () => currentJob,
      setBusy: vi.fn(),
      clearIfCurrent: vi.fn(),
      load: vi.fn().mockResolvedValue(null),
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
    expect(replaceCurrent).toHaveBeenCalledWith(null);
    currentJob = null;
    await controller.discard();
    expect(discard).toHaveBeenCalledOnce();
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
});
