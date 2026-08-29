import { describe, expect, it, vi } from 'vitest';
import { acknowledgeBatchRunWithRetry } from './batchRunAcknowledgement';

const OPERATION_ID = '00000000-0000-4000-8000-000000000301';
const noDelay = async () => undefined;

describe('batch terminal acknowledgement', () => {
  it('replays the exact id after response loss and accepts only explicit true', async () => {
    const acknowledge = vi
      .fn()
      .mockRejectedValueOnce(new Error('response lost'))
      .mockResolvedValueOnce(true);

    await expect(
      acknowledgeBatchRunWithRetry({
        operationId: OPERATION_ID,
        acknowledge,
        isCurrent: () => true,
        delayBeforeRetry: noDelay,
      }),
    ).resolves.toBe('acknowledged');
    expect(acknowledge.mock.calls).toEqual([[OPERATION_ID], [OPERATION_ID]]);
  });

  it('treats false as a definitive mismatch rather than lost-success proof', async () => {
    const acknowledge = vi.fn().mockResolvedValue(false);
    await expect(
      acknowledgeBatchRunWithRetry({
        operationId: OPERATION_ID,
        acknowledge,
        isCurrent: () => true,
        delayBeforeRetry: noDelay,
      }),
    ).resolves.toBe('rejected');
    expect(acknowledge).toHaveBeenCalledOnce();
  });

  it('fails closed after three malformed or unavailable responses', async () => {
    const acknowledge = vi.fn().mockResolvedValue('yes');
    await expect(
      acknowledgeBatchRunWithRetry({
        operationId: OPERATION_ID,
        acknowledge,
        isCurrent: () => true,
        delayBeforeRetry: noDelay,
      }),
    ).resolves.toBe('unavailable');
    expect(acknowledge).toHaveBeenCalledTimes(3);
  });

  it('drops a late acknowledgement after the event scope is destroyed', async () => {
    let current = true;
    const acknowledge = vi.fn(async () => {
      current = false;
      return true;
    });
    await expect(
      acknowledgeBatchRunWithRetry({
        operationId: OPERATION_ID,
        acknowledge,
        isCurrent: () => current,
        delayBeforeRetry: noDelay,
      }),
    ).resolves.toBe('stale');
  });
});
