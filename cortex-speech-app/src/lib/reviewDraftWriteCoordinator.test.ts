import { describe, expect, it, vi } from 'vitest';
import {
  ReviewDraftWriteCoordinator,
  ReviewDraftWriteIdentityError,
  type ReviewDraftWriteCoordinatorOptions,
} from './reviewDraftWriteCoordinator';

function echo(segmentId: string, baseRevision: number, text: string) {
  return { segmentId, baseRevision, text };
}

function coordinator(overrides: Partial<ReviewDraftWriteCoordinatorOptions> = {}) {
  return new ReviewDraftWriteCoordinator({
    save: vi.fn(async (segmentId, baseRevision, text) => echo(segmentId, baseRevision, text)),
    delete: vi.fn(async () => true),
    ...overrides,
  });
}

describe('revision-bound review draft write coordinator', () => {
  it('retries an off-screen A failure during close after navigation to B', async () => {
    const save = vi
      .fn<ReviewDraftWriteCoordinatorOptions['save']>()
      .mockRejectedValueOnce(new Error('disk full'))
      .mockImplementation(async (segmentId, baseRevision, text) =>
        echo(segmentId, baseRevision, text),
      );
    const writes = coordinator({ save });

    await expect(
      writes.request({ kind: 'save', segmentId: 'A', baseRevision: 7, text: 'exact A' }),
    ).rejects.toThrow('disk full');
    await writes.request({ kind: 'save', segmentId: 'B', baseRevision: 9, text: 'exact B' });

    expect(writes.hasDesired('A')).toBe(true);
    await writes.flushAll();
    expect(save.mock.calls).toEqual([
      ['A', 7, 'exact A'],
      ['B', 9, 'exact B'],
      ['A', 7, 'exact A'],
    ]);
    expect(writes.hasDesired()).toBe(false);
  });

  it('keeps a fail-once intent dirty and retries the exact request', async () => {
    const save = vi
      .fn<ReviewDraftWriteCoordinatorOptions['save']>()
      .mockRejectedValueOnce(new Error('transport lost'))
      .mockImplementation(async (segmentId, baseRevision, text) =>
        echo(segmentId, baseRevision, text),
      );
    const writes = coordinator({ save });
    const intent = {
      kind: 'save' as const,
      segmentId: 'clip',
      baseRevision: 12,
      text: 'human text',
    };

    await expect(writes.request(intent)).rejects.toThrow('transport lost');
    expect(writes.desiredIntent('clip')).toEqual(intent);
    await writes.flushSegment('clip');

    expect(save).toHaveBeenNthCalledWith(1, 'clip', 12, 'human text');
    expect(save).toHaveBeenNthCalledWith(2, 'clip', 12, 'human text');
    expect(writes.isDurable(intent)).toBe(true);
  });

  it('propagates a repeated retry failure so the close barrier stays blocked', async () => {
    const save = vi.fn<ReviewDraftWriteCoordinatorOptions['save']>(async () => {
      throw new Error('storage remains unavailable');
    });
    const writes = coordinator({ save });
    const intent = { kind: 'save' as const, segmentId: 'clip', baseRevision: 12, text: 'keep me' };

    await expect(writes.request(intent)).rejects.toThrow('storage remains unavailable');
    await expect(writes.flushAll()).rejects.toThrow('storage remains unavailable');

    expect(save).toHaveBeenCalledTimes(2);
    expect(writes.desiredIntent('clip')).toEqual(intent);
  });

  it('does not let a stale success clear a newer draft', async () => {
    let releaseOld!: () => void;
    const oldPending = new Promise<void>((resolve) => (releaseOld = resolve));
    const save = vi.fn<ReviewDraftWriteCoordinatorOptions['save']>(
      async (segmentId, baseRevision, text) => {
        if (text === 'older') await oldPending;
        return echo(segmentId, baseRevision, text);
      },
    );
    const writes = coordinator({ save });

    const oldWrite = writes.request({
      kind: 'save',
      segmentId: 'clip',
      baseRevision: 3,
      text: 'older',
    });
    const newWrite = writes.request({
      kind: 'save',
      segmentId: 'clip',
      baseRevision: 3,
      text: 'newer',
    });
    releaseOld();
    await Promise.all([oldWrite, newWrite]);

    expect(save.mock.calls).toEqual([
      ['clip', 3, 'older'],
      ['clip', 3, 'newer'],
    ]);
    expect(
      writes.isDurable({ kind: 'save', segmentId: 'clip', baseRevision: 3, text: 'newer' }),
    ).toBe(true);
  });

  it.each([
    ['segment', echo('other', 4, 'exact')],
    ['revision', echo('clip', 5, 'exact')],
    ['text', echo('clip', 4, 'wrong')],
  ])('rejects a wrong %s echo and leaves the exact intent dirty', async (_label, response) => {
    const save = vi.fn(async () => response);
    const writes = coordinator({ save });
    const intent = { kind: 'save' as const, segmentId: 'clip', baseRevision: 4, text: 'exact' };

    await expect(writes.request(intent)).rejects.toBeInstanceOf(ReviewDraftWriteIdentityError);
    expect(writes.desiredIntent('clip')).toEqual(intent);
  });
});
