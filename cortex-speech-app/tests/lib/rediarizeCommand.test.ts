import { invoke } from '@tauri-apps/api/core';
import { beforeEach, describe, expect, it, vi } from 'vitest';

import { rediarizeSegments } from '../../src/lib/commands';

const invokeMock = vi.mocked(invoke);

describe('generated owner rediarization contract', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it('passes the exact selected segment identities and preserves the updated count', async () => {
    invokeMock.mockResolvedValueOnce(2);

    await expect(rediarizeSegments(['segment-1', 'segment-2'])).resolves.toBe(2);
    expect(invokeMock).toHaveBeenCalledWith('rediarize_segments', {
      ids: ['segment-1', 'segment-2'],
    });
  });

  it('preserves a structured lifecycle refusal without retry or string coercion', async () => {
    const refusal = {
      schema: 1,
      code: 'RESTORE_IN_PROGRESS',
      message: 'Speaker analysis cannot run while database recovery is in progress.',
      retryable: true,
      suggestedAction: 'retry' as const,
      operationId: null,
      details: {},
    };
    invokeMock.mockRejectedValueOnce(refusal);

    await expect(rediarizeSegments(['segment-1'])).rejects.toBe(refusal);
    expect(invokeMock).toHaveBeenCalledTimes(1);
  });
});
