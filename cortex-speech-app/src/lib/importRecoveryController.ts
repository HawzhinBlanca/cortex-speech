export interface InterruptedImportIdentity {
  id: string;
}

export type ImportRecoveryAuthorityState = 'checking' | 'known' | 'unknown';

type RecoveryAuthorityResult<Job> =
  { kind: 'known'; job: Job | null } | { kind: 'unknown'; error: unknown } | { kind: 'stale' };

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
  setAuthorityState?: (state: ImportRecoveryAuthorityState) => void;
  delayBeforeRetry?: (attempt: number) => Promise<void>;
  attemptTimeoutMs?: number;
}

async function boundedRecoveryRead<T>(promise: Promise<T>, timeoutMs: number): Promise<T> {
  let timeout: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_, reject) => {
        timeout = setTimeout(() => reject(new Error('Import recovery check timed out')), timeoutMs);
      }),
    ]);
  } finally {
    if (timeout) clearTimeout(timeout);
  }
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
  let reconcilePending = false;
  const delayBeforeRetry =
    dependencies.delayBeforeRetry ??
    ((attempt: number) =>
      new Promise<void>((resolve) => {
        setTimeout(resolve, attempt === 1 ? 50 : 150);
      }));
  const attemptTimeoutMs = dependencies.attemptTimeoutMs ?? 5_000;

  async function reconcileNow(generation: number): Promise<RecoveryAuthorityResult<Job>> {
    dependencies.setAuthorityState?.('checking');
    let lastError: unknown;
    for (let attempt = 1; attempt <= 3; attempt += 1) {
      try {
        const job = await boundedRecoveryRead(dependencies.load(), attemptTimeoutMs);
        if (generation !== reconcileGeneration) return { kind: 'stale' };
        dependencies.replaceCurrent(job);
        dependencies.setAuthorityState?.('known');
        return { kind: 'known', job };
      } catch (error) {
        if (generation !== reconcileGeneration) return { kind: 'stale' };
        lastError = error;
        if (attempt < 3) await delayBeforeRetry(attempt);
      }
    }
    if (generation !== reconcileGeneration) return { kind: 'stale' };
    // An unreadable durable journal is never equivalent to no journal. Keep recovery actions
    // disabled and visibly unknown until a later read proves the authoritative state.
    dependencies.setAuthorityState?.('unknown');
    dependencies.onLoadFailure(lastError);
    return { kind: 'unknown', error: lastError };
  }

  async function reconcile(): Promise<void> {
    // Settlement/import-complete can arrive before an IPC mutation response. Queue exactly one
    // authoritative read behind the mutation instead of starting a read whose causal ordering is
    // unknowable and then suppressing it when the response lands.
    if (active) {
      reconcilePending = true;
      return;
    }
    await reconcileNow(++reconcileGeneration);
  }

  async function run(
    kind: 'resume' | 'discard',
    operation: (job: InterruptedImportIdentity) => Promise<unknown>,
    onSuccess: (() => void) | undefined,
    onFailure: (error: unknown) => void,
  ): Promise<void> {
    const job = dependencies.currentJob();
    if (!job || active) return;

    active = true;
    // A read started before this mutation represents pre-command authority. Invalidate it before
    // crossing the IPC boundary so that a late response cannot resurrect a journal that the
    // durable command has resumed or discarded.
    reconcileGeneration += 1;
    try {
      dependencies.setBusy(true);
      if (kind === 'discard') {
        // Discard is an immediate CAS command. Bound the transport response so a lost or wedged
        // desktop IPC can never hold the recovery UI forever; durable journal truth below decides
        // whether the desired state was actually reached.
        await boundedRecoveryRead(operation(job), attemptTimeoutMs);
      } else {
        await operation(job);
      }
      dependencies.clearIfCurrent(job.id);
      onSuccess?.();
      // The response proves command acceptance, not the complete durable recovery state. Always
      // reconcile so a lost settlement event cannot hide a successor journal until restart.
      reconcilePending = true;
    } catch (error) {
      if (kind === 'discard') {
        const authority = await reconcileNow(++reconcileGeneration);
        // A missing old journal proves discard success despite a lost response. A different ID is
        // an authoritative successor and must be preserved. Only the same journal (or unreadable
        // authority) is a retryable discard failure.
        if (authority.kind === 'known' && authority.job?.id !== job.id) {
          dependencies.clearIfCurrent(job.id);
        } else {
          onFailure(error);
        }
      } else {
        try {
          onFailure(error);
        } finally {
          reconcilePending = true;
        }
      }
    } finally {
      active = false;
      dependencies.setBusy(false);
      if (reconcilePending) {
        reconcilePending = false;
        await reconcile();
      }
    }
  }

  return {
    reconcile,
    resume: () =>
      run(
        'resume',
        (job) => dependencies.resume(job.id),
        dependencies.onResumeSuccess,
        dependencies.onResumeFailure,
      ),
    discard: () =>
      run(
        'discard',
        (job) => dependencies.discard(job.id),
        undefined,
        dependencies.onDiscardFailure,
      ),
  };
}
