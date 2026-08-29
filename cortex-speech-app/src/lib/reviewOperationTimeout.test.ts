import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { withReviewOperationTimeout } from './reviewOperationTimeout';

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

describe('review operation timeout', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it.each(['resolve', 'reject'] as const)(
    'settles once at timeout and ignores a late source %s',
    async (lateOutcome) => {
      const source = deferred<string>();
      const onResolved = vi.fn();
      const onRejected = vi.fn();
      void withReviewOperationTimeout(source.promise, 'E_TEST_TIMEOUT', 25).then(
        onResolved,
        onRejected,
      );

      await vi.advanceTimersByTimeAsync(25);
      expect(onResolved).not.toHaveBeenCalled();
      expect(onRejected).toHaveBeenCalledOnce();
      expect(onRejected.mock.calls[0]?.[0]).toMatchObject({ message: 'E_TEST_TIMEOUT' });
      expect(vi.getTimerCount()).toBe(0);

      if (lateOutcome === 'resolve') source.resolve('late success');
      else source.reject(new Error('late failure'));
      await Promise.resolve();
      await Promise.resolve();

      expect(onResolved).not.toHaveBeenCalled();
      expect(onRejected).toHaveBeenCalledOnce();
      expect(onRejected.mock.calls[0]?.[0]).toMatchObject({ message: 'E_TEST_TIMEOUT' });
    },
  );

  it('forwards an early resolution and clears its timer', async () => {
    const source = deferred<string>();
    const bounded = withReviewOperationTimeout(source.promise, 'E_TEST_TIMEOUT', 25);

    source.resolve('settled');
    await expect(bounded).resolves.toBe('settled');
    expect(vi.getTimerCount()).toBe(0);
  });

  it('forwards the exact early rejection and clears its timer', async () => {
    const source = deferred<string>();
    const failure = new Error('source failed');
    const bounded = withReviewOperationTimeout(source.promise, 'E_TEST_TIMEOUT', 25);

    source.reject(failure);
    await expect(bounded).rejects.toBe(failure);
    expect(vi.getTimerCount()).toBe(0);
  });
});
