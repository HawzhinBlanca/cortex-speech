import type { UpdatedSegmentMetadataV1 } from './generated/ipc';
import type { SegmentMetadataBaseline, SegmentMetadataFields } from './commands';

interface SegmentMetadataCoordinatorDeps {
  save: (
    segmentId: string,
    expected: SegmentMetadataBaseline,
    fields: SegmentMetadataFields,
  ) => Promise<UpdatedSegmentMetadataV1>;
  applyServerTruth: (updated: UpdatedSegmentMetadataV1) => void;
  onReadinessChanged: () => void;
}

export interface SegmentMetadataCoordinator {
  remember: (segmentId: string, metadata: SegmentMetadataBaseline) => void;
  forget: (segmentId: string) => void;
  pruneExcept: (segmentIds: Iterable<string>) => void;
  isReady: (segmentId: string | null) => boolean;
  saveFields: (segmentId: string, fields: Record<string, unknown>) => Promise<boolean>;
}

/**
 * Owns the renderer side of versioned segment-metadata compare-and-set.
 *
 * Baselines come only from a hydrated server row or a successful server ACK. Saves are serialized so
 * a second local edit cannot start until the first ACK has rebased its expected values. Runtime key
 * and value checks keep review truth unrepresentable even if an untyped UI caller reaches this API.
 */
export function createSegmentMetadataCoordinator({
  save,
  applyServerTruth,
  onReadinessChanged,
}: SegmentMetadataCoordinatorDeps): SegmentMetadataCoordinator {
  const baselines = new Map<string, SegmentMetadataBaseline>();
  let saveTail: Promise<void> = Promise.resolve();

  function remember(segmentId: string, metadata: SegmentMetadataBaseline): void {
    const existing = baselines.get(segmentId);
    if (existing) {
      existing.speakerId = metadata.speakerId;
      existing.alignmentJson = metadata.alignmentJson;
    } else {
      baselines.set(segmentId, {
        speakerId: metadata.speakerId,
        alignmentJson: metadata.alignmentJson,
      });
    }
    onReadinessChanged();
  }

  function forget(segmentId: string): void {
    if (baselines.delete(segmentId)) onReadinessChanged();
  }

  function pruneExcept(segmentIds: Iterable<string>): void {
    const retained = new Set(segmentIds);
    let changed = false;
    for (const segmentId of baselines.keys()) {
      if (!retained.has(segmentId)) changed = baselines.delete(segmentId) || changed;
    }
    if (changed) onReadinessChanged();
  }

  function isReady(segmentId: string | null): boolean {
    return segmentId !== null && baselines.has(segmentId);
  }

  function validatedFields(fields: Record<string, unknown>): SegmentMetadataFields {
    const unexpected = Object.keys(fields).filter(
      (key) => key !== 'speakerId' && key !== 'alignmentJson',
    );
    if (unexpected.length > 0) {
      throw new Error(`Refusing non-metadata autosave fields: ${unexpected.join(', ')}`);
    }
    const metadata: SegmentMetadataFields = {};
    if ('speakerId' in fields) {
      if (fields.speakerId !== null && typeof fields.speakerId !== 'string') {
        throw new Error('Refusing invalid speakerId metadata');
      }
      metadata.speakerId = fields.speakerId;
    }
    if ('alignmentJson' in fields) {
      if (fields.alignmentJson !== null && typeof fields.alignmentJson !== 'string') {
        throw new Error('Refusing invalid alignmentJson metadata');
      }
      metadata.alignmentJson = fields.alignmentJson;
    }
    return metadata;
  }

  function saveFields(segmentId: string, fields: Record<string, unknown>): Promise<boolean> {
    let queuedFields: SegmentMetadataFields;
    try {
      queuedFields = validatedFields(fields);
    } catch (error) {
      return Promise.reject(error);
    }
    // Capture the mutable baseline cell before queueing. Pruning may remove it from the readiness map
    // after the user changes clips, but already-issued writes still retain the exact expected values.
    // Earlier serialized ACKs update this same cell, so later edits rebase instead of conflicting.
    const baseline = baselines.get(segmentId);
    if (!baseline) {
      return Promise.reject(
        new Error('Segment metadata is not hydrated; reload the selected segment before saving'),
      );
    }
    const run = saveTail.then(async () => {
      const expected = {
        speakerId: baseline.speakerId,
        alignmentJson: baseline.alignmentJson,
      };
      const updated = await save(segmentId, expected, queuedFields);
      baseline.speakerId = updated.speakerId;
      baseline.alignmentJson = updated.alignmentJson;
      const current = baselines.get(segmentId);
      if (current && current !== baseline) {
        current.speakerId = updated.speakerId;
        current.alignmentJson = updated.alignmentJson;
      }
      applyServerTruth(updated);
      return updated.changed;
    });
    saveTail = run.then(
      () => undefined,
      () => undefined,
    );
    return run;
  }

  return { remember, forget, pruneExcept, isReady, saveFields };
}
