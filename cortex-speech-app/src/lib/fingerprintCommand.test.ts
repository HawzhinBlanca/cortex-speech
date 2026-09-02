import { invoke } from '@tauri-apps/api/core';
import { describe, expect, it, vi } from 'vitest';
import { getFingerprintCount } from './commands';

const invokeMock = vi.mocked(invoke);

describe('fingerprint diagnostics IPC contract', () => {
  it('uses the generated zero-argument command and returns its exact count', async () => {
    invokeMock.mockReset();
    invokeMock.mockResolvedValueOnce(42);

    await expect(getFingerprintCount()).resolves.toBe(42);
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith('get_fingerprint_count');
  });

  it('propagates the renderer-safe typed refusal without string coercion', async () => {
    invokeMock.mockReset();
    const refusal = {
      schema: 1,
      code: 'RATE_LIMITED',
      message: 'The duplicate-audio summary is busy. Retry in a moment.',
      retryable: true,
      suggestedAction: 'retry',
      operationId: null,
    };
    invokeMock.mockRejectedValueOnce(refusal);

    await expect(getFingerprintCount()).rejects.toEqual(refusal);
  });
});
