import { invoke } from '@tauri-apps/api/core';
import { describe, expect, it, vi } from 'vitest';
import { deleteSegment, deleteSegmentsBatch, deleteSegmentsV1 } from './commands';

const invokeMock = vi.mocked(invoke);

describe('segment deletion IPC contract', () => {
  it('routes single and batch deletion through one generated request shape', async () => {
    invokeMock.mockReset();
    invokeMock
      .mockResolvedValueOnce({ requestedCount: 1, deletedCount: 1 })
      .mockResolvedValueOnce({ requestedCount: 2, deletedCount: 2 });

    await deleteSegment('segment-a');
    await deleteSegmentsBatch(['segment-b', 'segment-c']);

    expect(invokeMock.mock.calls).toEqual([
      ['delete_segments_v1', { request: { ids: ['segment-a'] } }],
      ['delete_segments_v1', { request: { ids: ['segment-b', 'segment-c'] } }],
    ]);
  });

  it('returns the idempotent replay count and propagates structured refusal', async () => {
    invokeMock.mockReset();
    invokeMock.mockResolvedValueOnce({ requestedCount: 1, deletedCount: 0 });
    await expect(deleteSegmentsV1(['already-gone'])).resolves.toEqual({
      requestedCount: 1,
      deletedCount: 0,
    });

    const blocked = {
      schema: 1,
      code: 'SEGMENT_DELETE_BLOCKED',
      message: 'Reviewed segments are append-only.',
      retryable: false,
    };
    invokeMock.mockRejectedValueOnce(blocked);
    await expect(deleteSegmentsV1(['reviewed'])).rejects.toEqual(blocked);
  });
});
