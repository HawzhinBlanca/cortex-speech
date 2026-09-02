import { afterEach, describe, expect, it, vi } from 'vitest';

const { mountMock } = vi.hoisted(() => ({
  mountMock: vi.fn(() => ({ kind: 'preview-app' })),
}));

vi.mock('svelte', async (importOriginal) => {
  const actual = await importOriginal<typeof import('svelte')>();
  return { ...actual, mount: mountMock };
});

type Invoke = (command: string, args?: Record<string, unknown>) => Promise<unknown>;

interface PreviewInternals {
  invoke: Invoke;
  transformCallback: (callback: unknown) => number;
  metadata: {
    currentWindow: { label: string };
    currentWebview: { windowLabel: string; label: string };
  };
}

interface PreviewWindow {
  __TAURI__?: unknown;
  __TAURI_INTERNALS__?: PreviewInternals;
  __TAURI_CB__?: Record<number, unknown>;
  __TAURI_EVENT_PLUGIN_INTERNALS__?: {
    unregisterListener: (event: string, id: number) => void;
  };
}

const previewWindow = window as unknown as PreviewWindow;

async function installPreviewMock(): Promise<{
  app: unknown;
  infoMessages: unknown[][];
  internals: PreviewInternals;
}> {
  vi.resetModules();
  mountMock.mockClear();
  delete previewWindow.__TAURI__;
  delete previewWindow.__TAURI_INTERNALS__;
  delete previewWindow.__TAURI_CB__;
  delete previewWindow.__TAURI_EVENT_PLUGIN_INTERNALS__;
  localStorage.clear();
  document.body.innerHTML = '<div id="app"></div>';

  const infoMessages: unknown[][] = [];
  const infoSpy = vi.spyOn(console, 'info').mockImplementation((...args: unknown[]) => {
    infoMessages.push(args);
  });
  try {
    const loaded = await import('../../src/main');
    const internals = previewWindow.__TAURI_INTERNALS__;
    if (!internals) throw new Error('main.ts did not install the development Tauri mock');
    return { app: loaded.default, infoMessages, internals };
  } finally {
    infoSpy.mockRestore();
  }
}

afterEach(() => {
  delete previewWindow.__TAURI__;
  delete previewWindow.__TAURI_INTERNALS__;
  delete previewWindow.__TAURI_CB__;
  delete previewWindow.__TAURI_EVENT_PLUGIN_INTERNALS__;
  document.body.replaceChildren();
  vi.restoreAllMocks();
});

describe('main development Tauri preview contract', () => {
  it('installs once, mounts the app, and mirrors the event callback lifecycle', async () => {
    vi.spyOn(Math, 'random').mockReturnValue(0.123456789);
    const callback = vi.fn();
    const { app, infoMessages, internals } = await installPreviewMock();

    expect(app).toEqual({ kind: 'preview-app' });
    expect(mountMock).toHaveBeenCalledOnce();
    expect(mountMock).toHaveBeenCalledWith(expect.anything(), {
      target: document.getElementById('app'),
    });
    expect(infoMessages).toEqual([
      ['[cortex] dev Tauri mock installed — UI preview mode (no backend)'],
    ]);
    expect(internals.metadata).toEqual({
      currentWindow: { label: 'main' },
      currentWebview: { windowLabel: 'main', label: 'main' },
    });

    const callbackId = internals.transformCallback(callback);
    expect(callbackId).toBe(123456789);
    expect(previewWindow.__TAURI_CB__?.[callbackId]).toBe(callback);
    expect(await internals.invoke('plugin:event|listen', { handler: callbackId })).toBe(callbackId);
    expect(await internals.invoke('plugin:event|listen')).toBe(0);
    expect(await internals.invoke('plugin:dialog|open')).toBeNull();

    previewWindow.__TAURI_EVENT_PLUGIN_INTERNALS__?.unregisterListener(
      'batch-progress',
      callbackId,
    );
    expect(previewWindow.__TAURI_CB__?.[callbackId]).toBeUndefined();
    delete previewWindow.__TAURI_CB__;
    expect(() =>
      previewWindow.__TAURI_EVENT_PLUGIN_INTERNALS__?.unregisterListener(
        'batch-progress',
        callbackId,
      ),
    ).not.toThrow();
  }, 15_000);

  it('does not replace an existing desktop runtime', async () => {
    vi.resetModules();
    mountMock.mockClear();
    localStorage.clear();
    document.body.innerHTML = '<div id="app"></div>';
    const sentinel = {
      invoke: vi.fn(),
      transformCallback: vi.fn(),
      metadata: {
        currentWindow: { label: 'desktop' },
        currentWebview: { windowLabel: 'desktop', label: 'desktop' },
      },
    } satisfies PreviewInternals;
    previewWindow.__TAURI_INTERNALS__ = sentinel;

    await import('../../src/main');

    expect(previewWindow.__TAURI_INTERNALS__).toBe(sentinel);
    expect(mountMock).toHaveBeenCalledOnce();
  }, 15_000);

  it('enforces one durable preview batch at a time and explicit acknowledgement', async () => {
    const { internals } = await installPreviewMock();
    const invoke = internals.invoke;

    await expect(invoke('batch_transcribe', { ids: ['seg_001'] })).rejects.toMatchObject({
      code: 'BATCH_RUN_REJECTED',
      operationId: null,
    });
    await expect(
      invoke('batch_transcribe', { operationId: 'empty-batch', ids: [] }),
    ).rejects.toMatchObject({ code: 'BATCH_RUN_REJECTED', operationId: 'empty-batch' });
    expect(await invoke('get_active_batch_run')).toBeNull();
    expect(await invoke('get_batch_run_status')).toMatchObject({
      operationId: '',
      status: 'unknown',
    });
    expect(await invoke('get_batch_run_status', { operationId: 'missing-run' })).toEqual({
      operationId: 'missing-run',
      operation: null,
      status: 'unknown',
      total: null,
      outcome: null,
    });
    expect(await invoke('acknowledge_batch_run')).toBe(false);
    expect(await invoke('acknowledge_batch_run', { operationId: 'missing-run' })).toBe(false);

    expect(
      await invoke('batch_transcribe', {
        operationId: 'batch-one',
        ids: ['seg_001', 7, null],
      }),
    ).toEqual({ status: 'started', operationId: 'batch-one', operation: 'transcribe' });
    expect(await invoke('get_active_batch_run')).toMatchObject({
      operationId: 'batch-one',
      operation: 'transcribe',
      status: 'settled',
      total: 1,
      outcome: { disposition: 'completed', succeeded: 1, failed: 0 },
    });
    expect(await invoke('get_batch_run_status', { operationId: 'batch-one' })).toMatchObject({
      operationId: 'batch-one',
      status: 'settled',
    });
    await expect(
      invoke('batch_normalize', { operationId: 'batch-two', ids: ['seg_002'] }),
    ).rejects.toMatchObject({ code: 'BATCH_RUN_REJECTED', operationId: 'batch-two' });
    expect(await invoke('acknowledge_batch_run', { operationId: 'batch-one' })).toBe(true);
    expect(await invoke('get_active_batch_run')).toBeNull();
    expect(await invoke('acknowledge_batch_run', { operationId: 'batch-one' })).toBe(true);

    expect(
      await invoke('batch_normalize', {
        operationId: 'batch-three',
        ids: ['seg_003'],
      }),
    ).toEqual({ status: 'started', operationId: 'batch-three', operation: 'normalize' });
  });

  it('serves review scopes, durable drafts, revision conflicts, and idempotent commits', async () => {
    const { internals } = await installPreviewMock();
    const invoke = internals.invoke;

    const all = (await invoke('get_review_page_v1')) as {
      items: Array<{ segment: { id: string; rawTranscript: string }; baseRevision: number }>;
      total: number;
      scopeLabel: string;
      focusNarrowed: boolean;
    };
    expect(all.total).toBe(8);
    expect(all.scopeLabel).toBe('pending');
    expect(all.focusNarrowed).toBe(false);
    expect(all.items[0].baseRevision).toBe(0);

    expect(
      (
        (await invoke('get_review_page_v1', { scope: { kind: 'pending' } })) as {
          total: number;
        }
      ).total,
    ).toBe(4);
    expect(
      (
        (await invoke('get_review_page_v1', { scope: { kind: 'escalation' } })) as {
          total: number;
        }
      ).total,
    ).toBe(2);
    expect(
      (
        (await invoke('get_review_page_v1', {
          scope: { kind: 'search', query: 'CLIP_2' },
        })) as { items: Array<{ segment: { id: string } }> }
      ).items.map((item) => item.segment.id),
    ).toEqual(['seg_002']);
    expect(
      (
        (await invoke('get_review_page_v1', {
          scope: { kind: 'search', query: 'speaker_01' },
        })) as { total: number }
      ).total,
    ).toBe(2);
    expect(
      (
        (await invoke('get_review_page_v1', { scope: { kind: 'search' } })) as {
          total: number;
        }
      ).total,
    ).toBe(8);
    expect(
      (
        (await invoke('get_review_page_v1', {
          scope: { kind: 'search', query: '   ئەمڕۆ   ' },
        })) as { items: Array<{ segment: { id: string } }> }
      ).items[0].segment.id,
    ).toBe('seg_001');
    expect(
      (
        (await invoke('get_review_page_v1', {
          scope: { kind: 'voiceFocus', focusId: 'speaker-00' },
        })) as { focusNarrowed: boolean; scopeLabel: string }
      ).focusNarrowed,
    ).toBe(true);

    expect(await invoke('get_review_draft_v1', { segmentId: 'seg_003' })).toBeNull();
    expect(await invoke('get_review_draft_v1')).toBeNull();
    await expect(
      invoke('save_review_draft_v1', {
        segmentId: 'missing',
        baseRevision: 0,
        text: 'draft',
      }),
    ).rejects.toThrow('Unknown preview segment: missing');
    const draft = (await invoke('save_review_draft_v1', {
      segmentId: 'seg_003',
      baseRevision: 0,
      text: 'owner draft',
    })) as { updatedAt: string };
    expect(draft).toMatchObject({
      segmentId: 'seg_003',
      baseRevision: 0,
      text: 'owner draft',
    });
    expect(Number.isNaN(Date.parse(draft.updatedAt))).toBe(false);
    expect(await invoke('get_review_draft_v1', { segmentId: 'seg_003' })).toEqual(draft);
    await expect(
      invoke('save_review_draft_v1', {
        segmentId: 'seg_003',
        baseRevision: 7,
        text: 'stale',
      }),
    ).rejects.toMatchObject({ code: 'STALE_REVIEW_DRAFT', suggestedAction: 'reloadClip' });
    expect(await invoke('delete_review_draft_v1', { segmentId: 'seg_003', baseRevision: 7 })).toBe(
      false,
    );
    expect(
      await invoke('save_review_draft_v1', {
        segmentId: 'seg_004',
        baseRevision: 0,
      }),
    ).toMatchObject({ segmentId: 'seg_004', text: '' });
    expect(await invoke('delete_review_draft_v1', { segmentId: 'seg_004', baseRevision: 0 })).toBe(
      true,
    );
    expect(await invoke('delete_review_draft_v1')).toBe(false);

    await expect(
      invoke('commit_review_v1', {
        request: {
          operationId: 'missing-segment',
          segmentId: 'missing',
          baseRevision: 0,
          decision: 'accept',
        },
      }),
    ).rejects.toThrow('Unknown preview segment: missing');
    await expect(
      invoke('commit_review_v1', { request: { operationId: 'missing-request-segment' } }),
    ).rejects.toThrow('Unknown preview segment: ');
    await expect(
      invoke('commit_review_v1', {
        request: {
          operationId: 'stale-op',
          segmentId: 'seg_003',
          baseRevision: 9,
          decision: 'accept',
        },
      }),
    ).rejects.toMatchObject({
      code: 'STALE_REVIEW_REVISION',
      operationId: 'stale-op',
    });

    const accepted = await invoke('commit_review_v1', {
      request: {
        operationId: 'accept-op',
        segmentId: 'seg_003',
        baseRevision: 0,
        decision: 'accept',
      },
    });
    expect(accepted).toMatchObject({
      segmentId: 'seg_003',
      committedRevision: 1,
      decisionId: 'preview-accept-op',
    });
    expect(await invoke('get_review_draft_v1', { segmentId: 'seg_003' })).toBeNull();
    expect(
      await invoke('commit_review_v1', {
        request: {
          operationId: 'accept-op',
          segmentId: 'seg_003',
          baseRevision: 0,
          decision: 'accept',
        },
      }),
    ).toEqual(accepted);

    const edited = await invoke('commit_review_v1', {
      request: {
        operationId: 'edit-op',
        segmentId: 'seg_003',
        baseRevision: 1,
        decision: 'edit',
        transcript: 'exact owner correction',
      },
    });
    expect(edited).toMatchObject({
      committedRevision: 2,
      authoritativeTranscript: 'exact owner correction',
    });
    expect(await invoke('get_segment', { segmentId: 'seg_003' })).toMatchObject({
      rawTranscript: 'exact owner correction',
      annotatedTranscript: 'exact owner correction',
      verified: true,
    });

    await invoke('commit_review_v1', {
      request: {
        operationId: 'reject-op',
        segmentId: 'seg_003',
        baseRevision: 2,
        decision: 'reject',
      },
    });
    expect(await invoke('get_segment', { segmentId: 'seg_003' })).toMatchObject({
      rawTranscript: 'exact owner correction',
      annotatedTranscript: null,
      verified: false,
    });
    expect(await invoke('delete_review_draft_v1', { segmentId: 'seg_003', baseRevision: 0 })).toBe(
      false,
    );
  });

  it('returns detached segment pages and applies metadata with compare-and-set semantics', async () => {
    const { internals } = await installPreviewMock();
    const invoke = internals.invoke;

    const all = (await invoke('get_segments_page')) as {
      items: Array<Record<string, unknown>>;
      total: number;
      nextCursor: null;
    };
    expect(all).toMatchObject({ total: 8, nextCursor: null });
    all.items[0].rawTranscript = 'mutated caller copy';
    expect(await invoke('get_segment', { segmentId: 'seg_001' })).not.toMatchObject({
      rawTranscript: 'mutated caller copy',
    });
    expect(
      ((await invoke('get_segments_page', { verified: true })) as { total: number }).total,
    ).toBe(4);
    expect(
      ((await invoke('get_segments_page', { verified: false })) as { total: number }).total,
    ).toBe(4);
    expect(
      ((await invoke('get_segments_page', { query: 'fixtures/clip_8' })) as { total: number })
        .total,
    ).toBe(1);
    expect(
      ((await invoke('get_segments_page', { query: 'speaker_02' })) as { total: number }).total,
    ).toBe(2);
    expect(
      ((await invoke('get_segments_page', { query: 'does not exist' })) as { total: number }).total,
    ).toBe(0);
    await expect(invoke('get_segment', { segmentId: 'missing' })).rejects.toThrow(
      "Segment 'missing' no longer exists",
    );
    await expect(invoke('get_segment')).rejects.toThrow("Segment '' no longer exists");

    await expect(
      invoke('update_segment_metadata_v1', {
        request: { segmentId: 'missing', changes: [{ field: 'speakerId', value: 'X' }] },
      }),
    ).rejects.toMatchObject({ code: 'SEGMENT_NOT_FOUND' });
    await expect(
      invoke('update_segment_metadata_v1', {
        request: { segmentId: 'seg_001', changes: [] },
      }),
    ).rejects.toThrow('metadata changes cannot be empty');
    await expect(
      invoke('update_segment_metadata_v1', { request: { segmentId: 'seg_001' } }),
    ).rejects.toThrow('metadata changes cannot be empty');
    await expect(
      invoke('update_segment_metadata_v1', {
        request: {
          segmentId: 'seg_001',
          changes: [{ field: 'speakerId', expected: 'WRONG', value: 'SPEAKER_10' }],
        },
      }),
    ).rejects.toMatchObject({ code: 'STALE_SEGMENT_METADATA' });

    expect(
      await invoke('update_segment_metadata_v1', {
        request: {
          segmentId: 'seg_001',
          changes: [
            { field: 'speakerId', expected: 'SPEAKER_00', value: 'SPEAKER_10' },
            {
              field: 'alignmentJson',
              expected: (all.items[0].alignmentJson as string) ?? null,
              value: '{"words":[]}',
            },
          ],
        },
      }),
    ).toEqual({
      segmentId: 'seg_001',
      speakerId: 'SPEAKER_10',
      alignmentJson: '{"words":[]}',
      changed: true,
    });
    expect(
      await invoke('update_segment_metadata_v1', {
        request: {
          segmentId: 'seg_001',
          changes: [
            { field: 'speakerId', expected: 'old-value', value: 'SPEAKER_10' },
            { field: 'alignmentJson', expected: 'old-value', value: '{"words":[]}' },
          ],
        },
      }),
    ).toMatchObject({ changed: false });
  });

  it('renames, merges, assigns, inventories, and atomically validates delete requests', async () => {
    const { internals } = await installPreviewMock();
    const invoke = internals.invoke;

    const inventory = (await invoke('get_speaker_inventory_v1')) as Array<{
      speakerId: string | null;
      segmentCount: number;
      totalDurationSeconds: number;
    }>;
    expect(inventory[0]).toMatchObject({ speakerId: 'SPEAKER_00', segmentCount: 3 });
    expect(inventory.some((row) => row.speakerId === null && row.segmentCount === 1)).toBe(true);

    await expect(
      invoke('rename_speaker_v1', {
        request: {
          sourceSpeakerId: 'SPEAKER_00',
          targetSpeakerId: '',
          expectedSourceCount: 3,
          expectedTargetCount: 0,
        },
      }),
    ).rejects.toMatchObject({ code: 'STALE_SPEAKER_INVENTORY' });
    await expect(
      invoke('rename_speaker_v1', {
        request: {
          sourceSpeakerId: 'SPEAKER_00',
          targetSpeakerId: 'SPEAKER_00',
          expectedSourceCount: 3,
          expectedTargetCount: 3,
        },
      }),
    ).rejects.toMatchObject({ code: 'STALE_SPEAKER_INVENTORY' });
    await expect(
      invoke('rename_speaker_v1', {
        request: {
          sourceSpeakerId: 'SPEAKER_00',
          targetSpeakerId: 'SPEAKER_01',
          expectedSourceCount: 99,
          expectedTargetCount: 2,
        },
      }),
    ).rejects.toMatchObject({ code: 'STALE_SPEAKER_INVENTORY' });

    expect(
      await invoke('rename_speaker_v1', {
        request: {
          sourceSpeakerId: 'SPEAKER_00',
          targetSpeakerId: 'SPEAKER_01',
          expectedSourceCount: 3,
          expectedTargetCount: 2,
        },
      }),
    ).toEqual({
      sourceSpeakerId: 'SPEAKER_00',
      targetSpeakerId: 'SPEAKER_01',
      renamedCount: 3,
      targetCount: 5,
      merged: true,
    });
    expect(
      await invoke('rename_speaker_v1', {
        request: {
          targetSpeakerId: 'NEW_SPEAKER',
          expectedSourceCount: 1,
          expectedTargetCount: 0,
        },
      }),
    ).toMatchObject({ renamedCount: 1, merged: false });

    await expect(
      invoke('assign_speakers_v1', { request: { ids: [], targetSpeakerId: null } }),
    ).rejects.toMatchObject({ code: 'INVALID_SPEAKER_ASSIGNMENT' });
    await expect(
      invoke('assign_speakers_v1', {
        request: { ids: ['seg_001', 'seg_001'], targetSpeakerId: null },
      }),
    ).rejects.toMatchObject({ code: 'INVALID_SPEAKER_ASSIGNMENT' });
    expect(
      await invoke('assign_speakers_v1', {
        request: { ids: ['seg_001', 'seg_002', 'not-present'], targetSpeakerId: 'SPEAKER_01' },
      }),
    ).toEqual({ requestedCount: 3, changedCount: 0, unchangedCount: 3 });
    expect(
      await invoke('assign_speakers_v1', {
        request: { ids: ['seg_001', 'seg_003'], targetSpeakerId: null },
      }),
    ).toEqual({ requestedCount: 2, changedCount: 2, unchangedCount: 0 });
    expect(
      await invoke('assign_speakers_v1', {
        request: { ids: ['seg_004'] },
      }),
    ).toEqual({ requestedCount: 1, changedCount: 1, unchangedCount: 0 });

    await expect(invoke('delete_segments_v1')).rejects.toMatchObject({
      code: 'INVALID_DELETE_REQUEST',
    });
    await expect(invoke('delete_segments_v1', { request: { ids: [] } })).rejects.toMatchObject({
      code: 'INVALID_DELETE_REQUEST',
    });
    await expect(
      invoke('delete_segments_v1', { request: { ids: ['seg_001', 'seg_001'] } }),
    ).rejects.toMatchObject({ code: 'INVALID_DELETE_REQUEST' });
    expect(
      await invoke('delete_segments_v1', { request: { ids: ['seg_001', 'not-present'] } }),
    ).toEqual({ requestedCount: 2, deletedCount: 1 });
    await expect(invoke('get_segment', { segmentId: 'seg_001' })).rejects.toThrow(
      "Segment 'seg_001' no longer exists",
    );
  });

  it('provides correctly shaped media, playback, statistics, quality, and validation previews', async () => {
    const { internals } = await installPreviewMock();
    const invoke = internals.invoke;

    expect(await invoke('get_training_grade_breakdown')).toMatchObject({
      summary: {
        totalSegments: 8,
        trainingReadySegments: 4,
        goldSegments: 4,
        reviewSegments: 4,
      },
      reasonCounts: { human_verified: 4 },
    });
    const waveform = (await invoke('get_waveform')) as number[];
    expect(waveform).toHaveLength(400);
    expect(waveform.every((sample) => sample >= 0.02 && sample <= 1)).toBe(true);
    expect(await invoke('get_audio_duration')).toBe(6.2);
    expect(await invoke('get_audio_health')).toEqual({
      totalFiles: 8,
      missingFiles: 0,
      missingPaths: [],
    });
    const mediaAsset = (await invoke('register_media_asset', {
      audioPath: 'fixtures/clip_1.wav',
    })) as { id: string; expiresAt: string };
    expect(mediaAsset.id).toBe('preview-fixtures/clip_1.wav');
    expect(Number.isNaN(Date.parse(mediaAsset.expiresAt))).toBe(false);
    expect(await invoke('register_review_media_asset')).toMatchObject({ id: 'preview-' });
    expect(await invoke('get_media_asset_url')).toMatch(/^data:audio\/wav;base64,/);

    const playback = (await invoke('begin_desktop_playback_session_v1', {
      segmentId: 'seg_001',
      expectedRevision: 3,
    })) as Record<string, unknown>;
    expect(playback).toMatchObject({
      segmentId: 'seg_001',
      segmentRevision: 3,
      clipDurationMs: 6200,
    });
    expect(playback.playbackReceiptId).toEqual(expect.any(String));
    expect(
      await invoke('begin_desktop_playback_session_v1', { segmentId: 'seg_002' }),
    ).toMatchObject({ segmentRevision: 0 });
    expect(
      await invoke('finalize_desktop_playback_session_v1', {
        playbackReceiptId: 'receipt-one',
        intervals: [{ startMs: 0, endMs: 7000 }, { startMs: 10, endMs: 5 }, {}],
      }),
    ).toEqual({
      playbackReceiptId: 'receipt-one',
      segmentId: 'dev-preview-segment',
      segmentRevision: 0,
      uniquePlayedMs: 7000,
      clipDurationMs: 6200,
      coverageRatio: 1,
    });
    expect(await invoke('finalize_desktop_playback_session_v1')).toMatchObject({
      playbackReceiptId: '',
      uniquePlayedMs: 0,
      coverageRatio: 0,
    });

    for (const command of ['get_stats', 'compute_stats', 'get_dataset_stats']) {
      expect(await invoke(command)).toMatchObject({
        totalSegments: 8,
        verifiedCount: 4,
        pendingCount: 4,
        uniqueSpeakers: 3,
        durationHistogram: { under5s: 3, under10s: 4, under15s: 1 },
      });
    }
    expect(await invoke('get_dataset_quality')).toMatchObject({
      totalSegments: 8,
      lowConfidenceCount: 1,
      annotatedSegmentCount: 4,
      qualityGatePassed: true,
    });
    expect(await invoke('get_label_quality_lift')).toBeNull();
    expect(await invoke('get_dataset_certificate')).toEqual({
      targetError: 0.05,
      confidenceLevel: 0.95,
      threshold: 0.35,
      totalCertified: 0,
      certifiedSegmentIds: [],
      expectedErrorBound: 0.05,
      isCalibrated: false,
    });

    const validation = (await invoke('validate_dataset_cmd')) as {
      totalSegments: number;
      warnings: Array<Record<string, unknown>>;
      errors: Array<Record<string, unknown>>;
      summary: string;
    };
    expect(validation.totalSegments).toBe(8);
    expect(validation.warnings).toEqual([
      expect.objectContaining({ segmentId: 'seg_006', field: 'snrDb', severity: 'Warning' }),
    ]);
    expect(validation.errors).toEqual([
      expect.objectContaining({
        segmentId: 'seg_006',
        field: 'confidence',
        severity: 'Error',
      }),
    ]);
    expect(validation.summary).toBe('8 segments checked · 1 error(s) · 1 warning(s)');
  });

  it('returns explicit workstation support contracts and fails loudly for unknown commands', async () => {
    const { internals } = await installPreviewMock();
    const invoke = internals.invoke;

    for (const command of ['restore_session', 'take_last_crash', 'get_interrupted_import']) {
      expect(await invoke(command)).toBeNull();
    }
    expect(await invoke('check_agentic_readiness')).toMatchObject({
      status: 'ready',
      ready: true,
      sourceReferenceModelCount: 0,
      availableHypothesisModels: ['omniasr-wsl-7b'],
      availableHypothesisModelCount: 1,
      requiredHypothesisModels: 1,
      checkCount: 4,
    });
    expect(await invoke('save_session')).toBeNull();
    expect(await invoke('update_settings')).toBeNull();
    expect(await invoke('get_history_status_v1')).toEqual({
      undoAction: null,
      redoAction: null,
    });
    expect(await invoke('undo')).toEqual({
      action: null,
      status: { undoAction: null, redoAction: null },
    });
    expect(await invoke('redo')).toEqual({
      action: null,
      status: { undoAction: null, redoAction: null },
    });
    expect(await invoke('get_configured_providers')).toEqual([]);
    expect(await invoke('couch_review_status')).toEqual({ running: false, reviewers: [] });
    expect(await invoke('spot_check_report')).toEqual([]);
    expect(await invoke('reviewer_throughput')).toEqual([]);
    expect(await invoke('count_segments')).toBe(8);
    expect(await invoke('get_segment_count')).toBe(8);

    for (const command of [
      'get_active_learning_queue',
      'get_escalation_queue',
      'get_escalation_rate_trend',
      'get_jobs',
      'list_agent_import_reports',
      'list_agent_stage_events',
      'list_db_snapshots',
      'list_eval_runs',
      'list_model_versions',
      'models_status',
    ]) {
      expect(await invoke(command)).toEqual([]);
    }
    for (const command of ['app_health', 'db_info', 'get_settings', 'import_status']) {
      expect(await invoke(command)).toEqual({});
    }
    await expect(invoke('new_command_without_preview_contract')).rejects.toThrow(
      'Unknown development mock command: new_command_without_preview_contract',
    );
  });
});
