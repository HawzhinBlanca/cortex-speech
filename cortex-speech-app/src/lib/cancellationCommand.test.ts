import { invoke } from '@tauri-apps/api/core';
import { describe, expect, it, vi } from 'vitest';
import { cancelOperation, cancelWslRefinement } from './commands';

const invokeMock = vi.mocked(invoke);

describe('cancellation IPC contract', () => {
  it('routes both cancellation signals through generated zero-argument commands', async () => {
    invokeMock.mockReset();
    invokeMock.mockResolvedValue(undefined);

    await cancelOperation();
    await cancelWslRefinement();

    expect(invokeMock.mock.calls).toEqual([['cancel_operation'], ['cancel_wsl_refinement']]);
  });

  it('preserves a structured native refusal without coercing it to text', async () => {
    invokeMock.mockReset();
    const refusal = {
      schema: 1,
      code: 'CANCEL_FAILED',
      message: 'The active operation could not be cancelled.',
      retryable: true,
      suggestedAction: 'retry',
      operationId: null,
    };
    invokeMock.mockRejectedValueOnce(refusal);

    await expect(cancelOperation()).rejects.toEqual(refusal);
  });
});
