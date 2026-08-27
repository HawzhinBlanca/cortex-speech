import { describe, expect, it, vi } from 'vitest';
import { createSegmentMetadataCoordinator } from './segmentMetadataCoordinator';
import type { SegmentMetadataBaseline, SegmentMetadataFields } from './commands';

describe('segment metadata coordinator', () => {
  it('serializes local edits and rebases the second request on the first server ACK', async () => {
    let releaseFirst!: () => void;
    const calls: Array<{ expectedSpeaker: string | null; value: unknown }> = [];
    const applyServerTruth = vi.fn();
    const save = vi.fn(
      async (
        segmentId: string,
        expected: SegmentMetadataBaseline,
        fields: SegmentMetadataFields,
      ) => {
        calls.push({ expectedSpeaker: expected.speakerId, value: fields.speakerId });
        if (calls.length === 1) {
          await new Promise<void>((resolve) => {
            releaseFirst = resolve;
          });
        }
        return {
          segmentId,
          speakerId: fields.speakerId ?? null,
          alignmentJson: expected.alignmentJson,
          changed: true,
        };
      },
    );
    const coordinator = createSegmentMetadataCoordinator({
      save,
      applyServerTruth,
      onReadinessChanged: vi.fn(),
    });
    coordinator.remember('segment-a', { speakerId: 'speaker-a', alignmentJson: null });

    const first = coordinator.saveFields('segment-a', { speakerId: 'speaker-b' });
    const second = coordinator.saveFields('segment-a', { speakerId: 'speaker-c' });
    await Promise.resolve();
    expect(save).toHaveBeenCalledTimes(1);
    releaseFirst();
    await Promise.all([first, second]);

    expect(calls).toEqual([
      { expectedSpeaker: 'speaker-a', value: 'speaker-b' },
      { expectedSpeaker: 'speaker-b', value: 'speaker-c' },
    ]);
    expect(applyServerTruth).toHaveBeenCalledTimes(2);
  });

  it('does not rebase or apply server truth after a failed compare-and-set', async () => {
    const conflict = { schema: 1, code: 'STALE_SEGMENT_METADATA', retryable: false };
    const save = vi.fn().mockRejectedValueOnce(conflict).mockResolvedValueOnce({
      segmentId: 'segment-a',
      speakerId: 'speaker-c',
      alignmentJson: null,
      changed: true,
    });
    const applyServerTruth = vi.fn();
    const coordinator = createSegmentMetadataCoordinator({
      save,
      applyServerTruth,
      onReadinessChanged: vi.fn(),
    });
    coordinator.remember('segment-a', { speakerId: 'speaker-a', alignmentJson: null });

    await expect(coordinator.saveFields('segment-a', { speakerId: 'speaker-b' })).rejects.toBe(
      conflict,
    );
    await coordinator.saveFields('segment-a', { speakerId: 'speaker-c' });
    expect(save.mock.calls[1][1]).toEqual({ speakerId: 'speaker-a', alignmentJson: null });
    expect(applyServerTruth).toHaveBeenCalledTimes(1);
  });

  it('fails closed on review fields and on saves without a hydrated baseline', async () => {
    const save = vi.fn();
    const coordinator = createSegmentMetadataCoordinator({
      save,
      applyServerTruth: vi.fn(),
      onReadinessChanged: vi.fn(),
    });
    coordinator.remember('segment-a', { speakerId: null, alignmentJson: null });

    await expect(
      coordinator.saveFields('segment-a', { annotatedTranscript: 'must not cross' }),
    ).rejects.toThrow('Refusing non-metadata autosave fields');
    coordinator.forget('segment-a');
    expect(coordinator.isReady('segment-a')).toBe(false);
    await expect(coordinator.saveFields('segment-a', { speakerId: 'speaker-b' })).rejects.toThrow(
      'not hydrated',
    );
    expect(save).not.toHaveBeenCalled();
  });

  it('prunes visited baselines while an already-issued save retains and rebases its cell', async () => {
    let releaseFirst!: () => void;
    const calls: SegmentMetadataBaseline[] = [];
    const save = vi.fn(async (segmentId: string, expected: SegmentMetadataBaseline) => {
      calls.push({ ...expected });
      if (calls.length === 1) {
        await new Promise<void>((resolve) => {
          releaseFirst = resolve;
        });
      }
      return {
        segmentId,
        speakerId: calls.length === 1 ? 'speaker-b' : 'speaker-c',
        alignmentJson: null,
        changed: true,
      };
    });
    const coordinator = createSegmentMetadataCoordinator({
      save,
      applyServerTruth: vi.fn(),
      onReadinessChanged: vi.fn(),
    });
    coordinator.remember('segment-a', { speakerId: 'speaker-a', alignmentJson: null });

    const first = coordinator.saveFields('segment-a', { speakerId: 'speaker-b' });
    const second = coordinator.saveFields('segment-a', { speakerId: 'speaker-c' });
    coordinator.remember('segment-b', { speakerId: null, alignmentJson: null });
    coordinator.pruneExcept(['segment-b']);
    expect(coordinator.isReady('segment-a')).toBe(false);
    expect(coordinator.isReady('segment-b')).toBe(true);

    await Promise.resolve();
    releaseFirst();
    await Promise.all([first, second]);
    expect(calls).toEqual([
      { speakerId: 'speaker-a', alignmentJson: null },
      { speakerId: 'speaker-b', alignmentJson: null },
    ]);
    expect(coordinator.isReady('segment-a')).toBe(false);
  });
});
