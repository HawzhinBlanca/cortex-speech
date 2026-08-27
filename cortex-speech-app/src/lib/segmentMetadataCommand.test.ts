import { invoke } from '@tauri-apps/api/core';
import { describe, expect, it, vi } from 'vitest';
import { updateSegmentMetadataV1 } from './commands';

const invokeMock = vi.mocked(invoke);

describe('segment metadata IPC contract', () => {
  it('sends an explicit expected and replacement value only for edited fields', async () => {
    invokeMock.mockReset();
    invokeMock.mockResolvedValueOnce({
      segmentId: 'segment-a',
      speakerId: 'speaker-new',
      alignmentJson: '{"source_start_ms":0,"source_end_ms":1000}',
      changed: true,
    });

    await expect(
      updateSegmentMetadataV1(
        'segment-a',
        {
          speakerId: 'speaker-old',
          alignmentJson: '{"source_start_ms":0,"source_end_ms":1000}',
        },
        { speakerId: 'speaker-new' },
      ),
    ).resolves.toMatchObject({ changed: true, speakerId: 'speaker-new' });

    expect(invokeMock).toHaveBeenCalledWith('update_segment_metadata_v1', {
      request: {
        segmentId: 'segment-a',
        changes: [{ field: 'speakerId', expected: 'speaker-old', value: 'speaker-new' }],
      },
    });
  });

  it('preserves null as an explicit compare-and-clear operation', async () => {
    invokeMock.mockReset();
    invokeMock.mockResolvedValueOnce({
      segmentId: 'segment-a',
      speakerId: null,
      alignmentJson: null,
      changed: true,
    });

    await updateSegmentMetadataV1(
      'segment-a',
      { speakerId: 'speaker-old', alignmentJson: '{"words":[]}' },
      { speakerId: null, alignmentJson: null },
    );

    expect(invokeMock).toHaveBeenCalledWith('update_segment_metadata_v1', {
      request: {
        segmentId: 'segment-a',
        changes: [
          { field: 'speakerId', expected: 'speaker-old', value: null },
          { field: 'alignmentJson', expected: '{"words":[]}', value: null },
        ],
      },
    });
  });

  it('propagates a structured stale conflict without converting it to success', async () => {
    invokeMock.mockReset();
    const conflict = {
      schema: 1,
      code: 'STALE_SEGMENT_METADATA',
      message: 'Reload the segment.',
      retryable: false,
      suggestedAction: 'reloadClip',
      operationId: null,
      details: { field: 'speakerId' },
    };
    invokeMock.mockRejectedValueOnce(conflict);

    await expect(
      updateSegmentMetadataV1(
        'segment-a',
        { speakerId: 'speaker-old', alignmentJson: null },
        { speakerId: 'speaker-new' },
      ),
    ).rejects.toEqual(conflict);
  });

  it('refuses an empty update before invoking the backend', async () => {
    invokeMock.mockReset();
    await expect(
      updateSegmentMetadataV1('segment-a', { speakerId: 'speaker-old', alignmentJson: null }, {}),
    ).rejects.toThrow('empty segment metadata update');
    expect(invokeMock).not.toHaveBeenCalled();
  });
});
