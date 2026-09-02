import type { BatchRunOutcomeWire } from './batchStartReconciliation';
import type { BatchProgressEvent } from './events';

export function terminalEventMatchesOutcome(
  event: BatchProgressEvent | null,
  outcome: BatchRunOutcomeWire,
): boolean {
  if (!event) return true;
  if (
    event.total !== outcome.total ||
    (event.succeeded ?? 0) !== outcome.succeeded ||
    (event.failed ?? 0) !== outcome.failed ||
    (event.skipped ?? 0) !== outcome.skipped ||
    (event.abandoned ?? 0) !== outcome.abandoned ||
    (event.cancelled ?? false) !== outcome.cancelled
  ) {
    return false;
  }
  if (event.type === 'halted') {
    return (
      event.error?.code === outcome.errorCode &&
      (outcome.disposition === 'halted' || outcome.disposition === 'panicked')
    );
  }
  if (event.type !== 'completed') return false;
  return outcome.disposition === (event.cancelled ? 'cancelled' : 'completed');
}

export function sameBatchOutcome(left: BatchRunOutcomeWire, right: BatchRunOutcomeWire): boolean {
  return (
    left.disposition === right.disposition &&
    left.total === right.total &&
    left.succeeded === right.succeeded &&
    left.failed === right.failed &&
    left.skipped === right.skipped &&
    left.abandoned === right.abandoned &&
    left.cancelled === right.cancelled &&
    left.errorCode === right.errorCode
  );
}

export async function boundedBatchRefresh(
  promise: Promise<void>,
  timeoutMs = 15_000,
  onTimeout?: () => void,
): Promise<void> {
  let timeout: ReturnType<typeof setTimeout> | undefined;
  try {
    await Promise.race([
      promise,
      new Promise<never>((_, reject) => {
        timeout = setTimeout(() => {
          onTimeout?.();
          reject(new Error('Batch settlement refresh timed out'));
        }, timeoutMs);
      }),
    ]);
  } finally {
    if (timeout) clearTimeout(timeout);
  }
}
