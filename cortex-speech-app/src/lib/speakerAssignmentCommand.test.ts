import { invoke } from '@tauri-apps/api/core';
import { describe, expect, it, vi } from 'vitest';
import { assignSpeakersV1 } from './commands';

const invokeMock = vi.mocked(invoke);

describe('batch speaker assignment IPC contract', () => {
  it('routes one bounded generated request and reports unchanged replay rows honestly', async () => {
    invokeMock.mockReset();
    invokeMock.mockResolvedValueOnce({ requestedCount: 3, changedCount: 2, unchangedCount: 1 });

    await expect(
      assignSpeakersV1({ ids: ['one', 'two', 'three'], targetSpeakerId: 'Shara Karim' }),
    ).resolves.toEqual({ requestedCount: 3, changedCount: 2, unchangedCount: 1 });
    expect(invokeMock.mock.calls).toEqual([
      [
        'assign_speakers_v1',
        {
          request: {
            ids: ['one', 'two', 'three'],
            targetSpeakerId: 'Shara Karim',
          },
        },
      ],
    ]);
  });

  it('propagates an all-or-nothing stale-selection refusal', async () => {
    const stale = {
      schema: 1,
      code: 'STALE_SEGMENT_SELECTION',
      message: 'The selected segment set changed.',
      retryable: false,
    };
    invokeMock.mockReset();
    invokeMock.mockRejectedValueOnce(stale);

    await expect(
      assignSpeakersV1({ ids: ['gone', 'still-here'], targetSpeakerId: 'speaker-a' }),
    ).rejects.toEqual(stale);
  });
});
