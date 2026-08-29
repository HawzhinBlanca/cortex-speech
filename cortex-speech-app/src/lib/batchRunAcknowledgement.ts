export type BatchAcknowledgementResult = 'acknowledged' | 'rejected' | 'stale' | 'unavailable';

async function boundedAttempt<T>(promise: Promise<T>, timeoutMs: number): Promise<T> {
  let timeout: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      promise,
      new Promise<never>((_, reject) => {
        timeout = setTimeout(() => reject(new Error('Batch acknowledgement timed out')), timeoutMs);
      }),
    ]);
  } finally {
    if (timeout) clearTimeout(timeout);
  }
}

/** Acknowledgement is an exact-id idempotent receipt: a replay after a lost successful response
 * returns true. False is therefore a hard mismatch and never counts as success. */
export async function acknowledgeBatchRunWithRetry(options: {
  operationId: string;
  acknowledge: (operationId: string) => Promise<unknown>;
  isCurrent: () => boolean;
  delayBeforeRetry?: (attempt: number) => Promise<void>;
  attemptTimeoutMs?: number;
}): Promise<BatchAcknowledgementResult> {
  const {
    operationId,
    acknowledge,
    isCurrent,
    delayBeforeRetry = (attempt) =>
      new Promise<void>((resolve) => setTimeout(resolve, attempt === 1 ? 50 : 150)),
    attemptTimeoutMs = 2_000,
  } = options;

  for (let attempt = 1; attempt <= 3; attempt += 1) {
    if (!isCurrent()) return 'stale';
    try {
      const acknowledged = await boundedAttempt(acknowledge(operationId), attemptTimeoutMs);
      if (!isCurrent()) return 'stale';
      if (acknowledged === true) return 'acknowledged';
      if (acknowledged === false) return 'rejected';
    } catch {
      if (!isCurrent()) return 'stale';
    }
    if (attempt < 3) await delayBeforeRetry(attempt);
  }
  return isCurrent() ? 'unavailable' : 'stale';
}
