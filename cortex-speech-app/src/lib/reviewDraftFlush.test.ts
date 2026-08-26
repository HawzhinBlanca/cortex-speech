import { describe, expect, it, vi } from 'vitest';
import {
  flushReviewDrafts,
  registerReviewDraftFlusher,
  registeredReviewDraftFlushers,
} from './reviewDraftFlush';

describe('review draft close barrier', () => {
  it('awaits every mounted review session and unregisters exactly', async () => {
    let release!: () => void;
    const first = vi.fn(() => new Promise<void>((resolve) => (release = resolve)));
    const second = vi.fn(async () => undefined);
    const removeFirst = registerReviewDraftFlusher(first);
    const removeSecond = registerReviewDraftFlusher(second);

    const pending = flushReviewDrafts();
    expect(first).toHaveBeenCalledOnce();
    expect(second).toHaveBeenCalledOnce();
    let completed = false;
    void pending.then(() => (completed = true));
    await Promise.resolve();
    expect(completed).toBe(false);
    release();
    await pending;

    removeFirst();
    removeSecond();
    expect(registeredReviewDraftFlushers()).toBe(0);
  });

  it('propagates a durable-write failure so close cannot claim success', async () => {
    const remove = registerReviewDraftFlusher(async () => {
      throw new Error('disk full');
    });
    await expect(flushReviewDrafts()).rejects.toThrow('disk full');
    remove();
  });
});
