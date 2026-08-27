export interface InterruptedImportIdentity {
  id: string;
}

interface ImportRecoveryDependencies<Job extends InterruptedImportIdentity> {
  currentJob: () => Job | null;
  setBusy: (busy: boolean) => void;
  clearIfCurrent: (jobId: string) => void;
  load: () => Promise<Job | null>;
  replaceCurrent: (job: Job | null) => void;
  resume: (jobId: string) => Promise<unknown>;
  discard: (jobId: string) => Promise<unknown>;
  onResumeSuccess: () => void;
  onResumeFailure: (error: unknown) => void;
  onDiscardFailure: (error: unknown) => void;
  onLoadFailure: (error: unknown) => void;
}

/**
 * Single-flight recovery orchestration. A journal is cleared only after the matching durable
 * backend command succeeds; a rejection or ambiguous transport failure leaves it visible.
 */
export function createImportRecoveryController<Job extends InterruptedImportIdentity>(
  dependencies: ImportRecoveryDependencies<Job>,
) {
  let active = false;
  let reconcileGeneration = 0;

  async function reconcile(): Promise<void> {
    const generation = ++reconcileGeneration;
    try {
      const job = await dependencies.load();
      if (generation === reconcileGeneration) dependencies.replaceCurrent(job);
    } catch (error) {
      if (generation === reconcileGeneration) dependencies.onLoadFailure(error);
    }
  }

  async function run(
    operation: (job: InterruptedImportIdentity) => Promise<unknown>,
    onSuccess: (() => void) | undefined,
    onFailure: (error: unknown) => void,
  ): Promise<void> {
    const job = dependencies.currentJob();
    if (!job || active) return;

    active = true;
    try {
      dependencies.setBusy(true);
      await operation(job);
      dependencies.clearIfCurrent(job.id);
      onSuccess?.();
    } catch (error) {
      try {
        onFailure(error);
      } finally {
        await reconcile();
      }
    } finally {
      active = false;
      dependencies.setBusy(false);
    }
  }

  return {
    reconcile,
    resume: () =>
      run(
        (job) => dependencies.resume(job.id),
        dependencies.onResumeSuccess,
        dependencies.onResumeFailure,
      ),
    discard: () =>
      run((job) => dependencies.discard(job.id), undefined, dependencies.onDiscardFailure),
  };
}
