import { invoke } from '@tauri-apps/api/core';
import { describe, expect, it, vi } from 'vitest';
import { getSpeakerInventoryV1, renameSpeakerV1 } from './commands';

const invokeMock = vi.mocked(invoke);

describe('speaker inventory IPC contract', () => {
  it('preserves a null speaker identity and binds rename to displayed source and target counts', async () => {
    invokeMock.mockReset();
    invokeMock
      .mockResolvedValueOnce([
        { speakerId: null, segmentCount: 2, totalDurationSeconds: 3 },
        { speakerId: 'unknown', segmentCount: 1, totalDurationSeconds: 1.5 },
      ])
      .mockResolvedValueOnce({
        sourceSpeakerId: null,
        targetSpeakerId: 'speaker-a',
        renamedCount: 2,
        targetCount: 3,
        merged: true,
      });

    await expect(getSpeakerInventoryV1()).resolves.toHaveLength(2);
    await expect(
      renameSpeakerV1({
        sourceSpeakerId: null,
        targetSpeakerId: 'speaker-a',
        expectedSourceCount: 2,
        expectedTargetCount: 1,
      }),
    ).resolves.toMatchObject({ renamedCount: 2, merged: true });

    expect(invokeMock.mock.calls).toEqual([
      ['get_speaker_inventory_v1'],
      [
        'rename_speaker_v1',
        {
          request: {
            sourceSpeakerId: null,
            targetSpeakerId: 'speaker-a',
            expectedSourceCount: 2,
            expectedTargetCount: 1,
          },
        },
      ],
    ]);
  });

  it('propagates a typed stale-inventory refusal', async () => {
    const stale = {
      schema: 1,
      code: 'STALE_SPEAKER_INVENTORY',
      message: 'The speaker inventory changed.',
      retryable: false,
    };
    invokeMock.mockReset();
    invokeMock.mockRejectedValueOnce(stale);

    await expect(
      renameSpeakerV1({
        sourceSpeakerId: 'speaker-a',
        targetSpeakerId: 'speaker-b',
        expectedSourceCount: 2,
        expectedTargetCount: 0,
      }),
    ).rejects.toEqual(stale);
  });
});
