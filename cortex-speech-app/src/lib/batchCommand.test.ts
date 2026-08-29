import { invoke } from '@tauri-apps/api/core';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { acknowledgeBatchRun, batchNormalize, batchTranscribe } from './commands';
import type { BatchStartedV1, CommandErrorV1 } from './generated/ipc';

const invokeMock = vi.mocked(invoke);
const OPERATION_ID = '00000000-0000-4000-8000-000000000201';

describe('generated durable batch command boundary', () => {
  beforeEach(() => invokeMock.mockReset());

  it('uses generated exact admission and acknowledgement arguments', async () => {
    const transcribe: BatchStartedV1 = {
      status: 'started',
      operationId: OPERATION_ID,
      operation: 'transcribe',
    };
    const normalize: BatchStartedV1 = {
      status: 'started',
      operationId: OPERATION_ID,
      operation: 'normalize',
    };
    invokeMock
      .mockResolvedValueOnce(transcribe)
      .mockResolvedValueOnce(normalize)
      .mockResolvedValueOnce(true);

    await expect(batchTranscribe(['s1', 's2'], OPERATION_ID)).resolves.toEqual(transcribe);
    await expect(batchNormalize(['s1'], OPERATION_ID)).resolves.toEqual(normalize);
    await expect(acknowledgeBatchRun(OPERATION_ID)).resolves.toBe(true);

    expect(invokeMock.mock.calls).toEqual([
      ['batch_transcribe', { ids: ['s1', 's2'], operationId: OPERATION_ID }],
      ['batch_normalize', { ids: ['s1'], operationId: OPERATION_ID }],
      ['acknowledge_batch_run', { operationId: OPERATION_ID }],
    ]);
  });

  it('preserves a typed acknowledgement refusal', async () => {
    const refusal: CommandErrorV1 = {
      schema: 1,
      code: 'BATCH_ACKNOWLEDGEMENT_INVALID',
      message: 'The exact terminal batch could not be acknowledged.',
      retryable: false,
      suggestedAction: 'openHealth',
      operationId: OPERATION_ID,
      details: {},
    };
    invokeMock.mockRejectedValueOnce(refusal);

    await expect(acknowledgeBatchRun(OPERATION_ID)).rejects.toEqual(refusal);
  });
});
